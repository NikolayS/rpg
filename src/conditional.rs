//! Conditional execution engine for `\if` / `\elif` / `\else` / `\endif`.
//!
//! psql-style conditional blocks allow scripts to execute sections
//! conditionally based on boolean expressions.  Variable interpolation is
//! expected to have already occurred before expressions reach this module.
//!
//! # State machine
//!
//! Each `\if` pushes a [`CondBlock`] onto the stack.  The block records:
//!
//! - `any_true` — whether any branch in this block has already been taken
//!   (prevents multiple branches from executing).
//! - `active` — whether the *current* branch is executing.
//!
//! `\elif` / `\else` / `\endif` mutate or pop the top-of-stack entry.
//!
//! The REPL is in "active" state when **every** block on the stack is active.
//! A suppressed outer block makes all inner blocks suppressed regardless of
//! their own condition.

// ---------------------------------------------------------------------------
// Boolean evaluator
// ---------------------------------------------------------------------------

/// Evaluate a boolean expression string.
///
/// Recognised truthy values (case-insensitive, leading/trailing whitespace
/// is trimmed): `true`, `on`, `1`, `yes`, `t`, `y`.
///
/// Everything else — including the empty string — is falsy.
///
/// Variable interpolation must have already occurred; this function only
/// interprets the final string value.
pub fn eval_bool(expr: &str) -> bool {
    matches!(
        expr.trim().to_lowercase().as_str(),
        "true" | "on" | "1" | "yes" | "t" | "y"
    )
}

/// Like `eval_bool` but distinguishes invalid expressions from valid false.
///
/// Returns `Some(true)` for recognised truthy values, `Some(false)` for
/// recognised falsy values (`false`, `off`, `0`, `no`, `f`, `n`, ``),
/// and `None` for anything that is not a recognised boolean.
pub fn eval_bool_strict(expr: &str) -> Option<bool> {
    match expr.trim().to_lowercase().as_str() {
        "true" | "on" | "1" | "yes" | "t" | "y" => Some(true),
        "false" | "off" | "0" | "no" | "f" | "n" | "" => Some(false),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// CondBlock
// ---------------------------------------------------------------------------

/// One level of a conditional block (`\if` … `\endif`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CondBlock {
    /// Has any branch in this `\if`/`\elif`/`\else` chain already been true?
    any_true: bool,
    /// Is the current branch active (executing)?
    active: bool,
    /// Has an `\else` branch been seen for this block?
    ///
    /// Used to detect an erroneous second `\else` or an `\elif` after `\else`.
    seen_else: bool,
    /// Was this block opened with an invalid boolean expression?
    ///
    /// When true, psql echoes (but does not execute) the lines inside the
    /// suppressed branch — mirroring psql's error-conditional behaviour.
    error: bool,
}

// ---------------------------------------------------------------------------
// ConditionalState
// ---------------------------------------------------------------------------

/// Stack-based conditional execution state.
///
/// One `ConditionalState` is owned by the REPL and shared across all
/// execution paths (interactive, `-f` file, `\i` include).  The stack grows
/// with each `\if` and shrinks with each `\endif`.
#[derive(Debug, Default)]
pub struct ConditionalState {
    stack: Vec<CondBlock>,
    /// Number of currently-open blocks that were opened with an invalid
    /// boolean expression (error-conditional mode).
    error_depth: usize,
}

impl ConditionalState {
    /// Create a new, empty (unconstrained) state.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            error_depth: 0,
        }
    }

    /// Return `true` when every open block is currently active.
    ///
    /// An empty stack means no conditional is in effect, so execution is
    /// always active.
    pub fn is_active(&self) -> bool {
        self.stack.iter().all(|b| b.active)
    }

    /// Return the current nesting depth.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Discard all open blocks, resetting to the unconstrained state.
    ///
    /// Used when `\quit` fires inside a conditional block: psql does not
    /// emit "unterminated \\if" warnings when the script exits via `\quit`.
    pub fn reset(&mut self) {
        self.stack.clear();
        self.error_depth = 0;
    }

    /// Return `true` when at least one currently-open block was opened with
    /// an invalid boolean expression (psql error-conditional mode).
    ///
    /// In this mode the REPL echoes (but does not execute) SQL lines inside
    /// the suppressed block, matching psql's `-a` behaviour.
    pub fn is_error_conditional(&self) -> bool {
        self.error_depth > 0
    }

    /// Process `\if <condition>`.
    ///
    /// Pushes a new block.  The block is active only when the condition is
    /// true **and** we are already in an active context (i.e. not suppressed
    /// by an outer block).
    pub fn push_if(&mut self, condition: bool) {
        let outer_active = self.is_active();
        let active = outer_active && condition;
        self.stack.push(CondBlock {
            any_true: active,
            active,
            seen_else: false,
            error: false,
        });
    }

    /// Like `push_if(false)` but marks the block as an error-conditional.
    ///
    /// Called when `\if` receives an expression that is not a recognised
    /// boolean value.  The block behaves as `\if false` but the caller is
    /// responsible for printing the appropriate error message, and the REPL
    /// will echo (without executing) SQL inside the suppressed branch.
    pub fn push_if_error(&mut self) {
        let outer_active = self.is_active();
        self.stack.push(CondBlock {
            any_true: false,
            active: false,
            seen_else: false,
            error: outer_active, // only track error when outer is active
        });
        if outer_active {
            self.error_depth += 1;
        }
    }

    /// Process `\elif <condition>`.
    ///
    /// # Errors
    /// Returns an error string when:
    /// - no `\if` is open (`\elif` without `\if`)
    /// - an `\else` has already been seen for this block
    pub fn handle_elif(&mut self, condition: bool) -> Result<(), String> {
        // Check outer active state before mutating — we need the state that
        // was active *before* this `\if` block was pushed.
        let outer_active = self
            .stack
            .len()
            .checked_sub(2)
            .is_none_or(|i| self.stack[i].active);

        match self.stack.last_mut() {
            None => Err("\\elif without \\if".to_owned()),
            Some(block) if block.seen_else => Err("\\elif after \\else".to_owned()),
            Some(block) => {
                // Activate this branch only if no prior branch was true and
                // the outer context is active.
                if !block.any_true && outer_active && condition {
                    block.active = true;
                    block.any_true = true;
                } else {
                    block.active = false;
                }
                Ok(())
            }
        }
    }

    /// Process `\else`.
    ///
    /// # Errors
    /// Returns an error string when:
    /// - no `\if` is open
    /// - an `\else` has already been seen for this block
    pub fn handle_else(&mut self) -> Result<(), String> {
        let outer_active = self
            .stack
            .len()
            .checked_sub(2)
            .is_none_or(|i| self.stack[i].active);

        match self.stack.last_mut() {
            None => Err("\\else without \\if".to_owned()),
            Some(block) if block.seen_else => Err("\\else after \\else".to_owned()),
            Some(block) => {
                // The else branch is active iff no prior branch was true and
                // the outer context is active.
                block.active = !block.any_true && outer_active;
                block.seen_else = true;
                Ok(())
            }
        }
    }

    /// Process `\endif`.
    ///
    /// # Errors
    /// Returns an error string when no `\if` is open.
    pub fn pop_endif(&mut self) -> Result<(), String> {
        match self.stack.pop() {
            Some(block) => {
                if block.error {
                    self.error_depth = self.error_depth.saturating_sub(1);
                }
                Ok(())
            }
            None => Err("\\endif without \\if".to_owned()),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- eval_bool -----------------------------------------------------------

    #[test]
    fn eval_bool_truthy_values() {
        for val in &[
            "true", "True", "TRUE", "on", "ON", "1", "yes", "YES", "t", "T", "y", "Y",
        ] {
            assert!(eval_bool(val), "{val} should be truthy");
        }
    }

    #[test]
    fn eval_bool_falsy_values() {
        for val in &[
            "false", "False", "FALSE", "off", "OFF", "0", "no", "NO", "f", "F", "n", "N", "",
        ] {
            assert!(!eval_bool(val), "{val} should be falsy");
        }
    }

    #[test]
    fn eval_bool_unknown_is_false() {
        assert!(!eval_bool("maybe"));
        assert!(!eval_bool("yes_please"));
        assert!(!eval_bool("2"));
    }

    #[test]
    fn eval_bool_trims_whitespace() {
        assert!(eval_bool("  true  "));
        assert!(!eval_bool("  false  "));
    }

    // -- ConditionalState: basic if/endif ------------------------------------

    #[test]
    fn empty_stack_is_active() {
        let s = ConditionalState::new();
        assert!(s.is_active());
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn push_true_if_is_active() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        assert!(s.is_active());
        assert_eq!(s.depth(), 1);
    }

    #[test]
    fn push_false_if_is_inactive() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        assert!(!s.is_active());
    }

    #[test]
    fn endif_restores_active() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        assert!(!s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
        assert_eq!(s.depth(), 0);
    }

    // -- elif ----------------------------------------------------------------

    #[test]
    fn elif_taken_when_if_false() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_elif(true).unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn elif_not_taken_when_if_true() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        s.handle_elif(true).unwrap();
        assert!(!s.is_active());
    }

    #[test]
    fn elif_not_taken_when_prior_elif_taken() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_elif(true).unwrap();
        s.handle_elif(true).unwrap();
        assert!(!s.is_active());
    }

    #[test]
    fn elif_without_if_is_error() {
        let mut s = ConditionalState::new();
        assert!(s.handle_elif(true).is_err());
    }

    #[test]
    fn elif_after_else_is_error() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_else().unwrap();
        assert!(s.handle_elif(true).is_err());
    }

    // -- else ----------------------------------------------------------------

    #[test]
    fn else_taken_when_if_false() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_else().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn else_not_taken_when_if_true() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        s.handle_else().unwrap();
        assert!(!s.is_active());
    }

    #[test]
    fn else_without_if_is_error() {
        let mut s = ConditionalState::new();
        assert!(s.handle_else().is_err());
    }

    #[test]
    fn double_else_is_error() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_else().unwrap();
        assert!(s.handle_else().is_err());
    }

    // -- endif ---------------------------------------------------------------

    #[test]
    fn endif_without_if_is_error() {
        let mut s = ConditionalState::new();
        assert!(s.pop_endif().is_err());
    }

    // -- nested conditionals -------------------------------------------------

    #[test]
    fn nested_both_true() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        s.push_if(true);
        assert!(s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn nested_outer_false_suppresses_inner() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        // Inner true — but outer is false so result must still be inactive.
        s.push_if(true);
        assert!(!s.is_active());
        s.pop_endif().unwrap();
        assert!(!s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn nested_inner_false_suppresses_body() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        s.push_if(false);
        assert!(!s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn nested_elif_in_outer_false() {
        // Even if elif condition is true, outer suppression wins.
        let mut s = ConditionalState::new();
        s.push_if(false); // outer: inactive
        s.push_if(false); // inner
        s.handle_elif(true).unwrap(); // inner elif true — but outer inactive
        assert!(!s.is_active());
    }

    // -- if/elif/else full chain ----------------------------------------------

    #[test]
    fn full_chain_if_true() {
        let mut s = ConditionalState::new();
        s.push_if(true);
        assert!(s.is_active()); // \if branch
        s.handle_elif(true).unwrap();
        assert!(!s.is_active()); // \elif branch (skipped)
        s.handle_else().unwrap();
        assert!(!s.is_active()); // \else branch (skipped)
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn full_chain_elif_true() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        assert!(!s.is_active()); // \if false
        s.handle_elif(true).unwrap();
        assert!(s.is_active()); // \elif taken
        s.handle_else().unwrap();
        assert!(!s.is_active()); // \else skipped
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }

    #[test]
    fn full_chain_else_taken() {
        let mut s = ConditionalState::new();
        s.push_if(false);
        s.handle_elif(false).unwrap();
        assert!(!s.is_active()); // neither branch taken yet
        s.handle_else().unwrap();
        assert!(s.is_active()); // \else taken
        s.pop_endif().unwrap();
        assert!(s.is_active());
    }
}
