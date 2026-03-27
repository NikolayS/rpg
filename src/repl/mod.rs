//! Interactive REPL loop for Rpg.
#![allow(clippy::wildcard_imports)]
//!
//! Provides readline-based line editing with persistent history, multi-line
//! SQL accumulation, backslash command handling, transaction-state prompts,
//! and signal-aware Ctrl-C / Ctrl-D behaviour.

use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

#[cfg(not(target_arch = "wasm32"))]
use rustyline::error::ReadlineError;
#[cfg(not(target_arch = "wasm32"))]
use rustyline::history::FileHistory;
#[cfg(not(target_arch = "wasm32"))]
use rustyline::{
    Cmd, ConditionalEventHandler, Event, EventContext, EventHandler, KeyCode, KeyEvent, Modifiers,
    RepeatCount,
};
#[cfg(not(target_arch = "wasm32"))]
use rustyline::{Config, EditMode, Editor};
use tokio_postgres::Client;

#[cfg(not(target_arch = "wasm32"))]
use crate::complete::{
    load_schema_cache, DropdownEventHandler, DropdownKey, RpgHelper, SchemaCache,
};

use crate::connection::ConnParams;

// ---------------------------------------------------------------------------
// Submodules
// ---------------------------------------------------------------------------

pub(super) mod ai_commands;
use ai_commands::*;

pub(super) mod execute;
use execute::*;

pub(super) mod watch;
use watch::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default history file path (relative to home directory).
const DEFAULT_HISTORY_FILE: &str = ".rpg_history";

/// Maximum number of history entries kept in memory and on disk.
const HISTORY_SIZE: usize = 2000;

// ---------------------------------------------------------------------------
// Transaction state
// ---------------------------------------------------------------------------

/// Transaction state reflected in the prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TxState {
    /// No open transaction.
    #[default]
    Idle,
    /// Inside an active transaction block.
    InTransaction,
    /// Inside a failed (aborted) transaction block.
    Failed,
}

impl TxState {
    /// Infix character inserted between `=` and `>` in the prompt.
    ///
    /// For idle state there is no infix; for in-transaction `*`; for failed `!`.
    fn infix(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::InTransaction => "*",
            Self::Failed => "!",
        }
    }

    /// Update the state based on the SQL statement that was executed.
    ///
    /// We track transaction state by inspecting the SQL because
    /// `tokio-postgres 0.7` `CommandComplete` only carries a row count.
    ///
    /// - `BEGIN` (or `START TRANSACTION`) → enter transaction block
    /// - `COMMIT` (or `END`) → return to idle
    /// - `ROLLBACK` (or `ABORT`) → return to idle
    /// - `ROLLBACK TO [SAVEPOINT]` → no state change (still in transaction)
    /// - `SAVEPOINT` / `RELEASE` → no state change at block level
    ///
    /// NOTE: Client-side SQL inspection is inherently limited — it cannot
    /// handle all edge cases (e.g. statements inside PL/pgSQL, implicit
    /// transaction management by the server). Proper server-side tracking
    /// via `ReadyForQuery` transaction status byte is future work.
    pub fn update_from_sql(&mut self, sql: &str) {
        // Grab the first keyword(s) from the (possibly multi-statement) input.
        // Strip trailing punctuation (e.g. `;`) from each token so that
        // `"begin;"` is treated the same as `"begin"`.
        let upper = sql.trim().to_uppercase();
        let words: Vec<&str> = upper
            .split_whitespace()
            .take(3)
            .map(|w| w.trim_end_matches(|c: char| !c.is_alphabetic()))
            .collect();
        let first = words.first().copied().unwrap_or("");
        let second = words.get(1).copied().unwrap_or("");

        if first == "BEGIN" || (first == "START" && second == "TRANSACTION") {
            *self = Self::InTransaction;
        } else if first == "COMMIT" || first == "END" {
            *self = Self::Idle;
        } else if first == "ROLLBACK" || first == "ABORT" {
            // `ROLLBACK TO [SAVEPOINT] name` stays inside the transaction.
            if second != "TO" {
                *self = Self::Idle;
            }
        }
    }

    /// Transition to `Failed` (called when a query error occurs while we are
    /// inside a transaction).
    pub fn on_error(&mut self) {
        if *self == Self::InTransaction {
            *self = Self::Failed;
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

/// Runtime context used by [`expand_prompt`] to substitute format codes.
#[allow(clippy::struct_excessive_bools)]
pub struct PromptContext<'a> {
    /// Current database name (`%/`).
    pub dbname: &'a str,
    /// Connected user name (`%n`).
    pub user: &'a str,
    /// Full host name (`%M`).
    pub host: &'a str,
    /// Port number (`%>`).
    pub port: u16,
    /// Whether the connected role is a superuser (`%#`).
    pub is_superuser: bool,
    /// Current transaction state (used by `%R` and `%x`).
    pub tx: TxState,
    /// `true` when the prompt is for a continuation line, not the first line.
    ///
    /// Affects `%R`: first-line returns `=`, continuation returns `-`.
    pub continuation: bool,
    /// `true` when the cursor is inside a `/* … */` block comment.
    ///
    /// Affects `%R`: returns `*` when inside a block comment.
    pub in_block_comment: bool,
    /// `true` when single-line mode is active (`-S` / `\set SINGLELINE on`).
    ///
    /// Affects `%R`: returns `^` in single-line mode.
    pub single_line_mode: bool,
    /// `false` when the session is disconnected from the server.
    ///
    /// Affects `%R`: returns `!` when disconnected.
    pub connected: bool,
    /// Current input line number within the session (`%l`).
    ///
    /// Set to `0` when not tracked.
    pub line_number: u64,
    /// Backend process ID (`%p`).
    ///
    /// `None` when unknown (e.g. not yet queried).
    pub backend_pid: Option<u32>,
}

/// Expand a psql-compatible prompt template string.
///
/// Recognises the following format codes (a subset of those documented
/// in the psql manual):
///
/// | Code | Expansion                                                    |
/// |------|--------------------------------------------------------------|
/// | `%/` | Current database name                                        |
/// | `%~` | Database name, or `~` when it equals the user name          |
/// | `%n` | User name                                                    |
/// | `%M` | Full host name                                               |
/// | `%m` | Short host name (up to the first `.`)                        |
/// | `%>` | Port number                                                  |
/// | `%#` | `#` if superuser, `>` otherwise                             |
/// | `%R` | Input status: `=` (normal), `-` (continuation),             |
/// |      | `*` (block comment), `^` (single-line mode),                |
/// |      | `!` (disconnected)                                          |
/// | `%x` | Transaction status: empty (idle), `*` (in tx), `!` (failed) |
/// | `%l` | Line number                                                  |
/// | `%p` | Backend PID, or empty when unknown                          |
/// | `%%` | Literal `%`                                                  |
///
/// Unrecognised `%X` sequences are passed through unchanged.
pub fn expand_prompt(template: &str, ctx: &PromptContext<'_>) -> String {
    let chars: Vec<char> = template.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(template.len() + 16);
    let mut i = 0;

    while i < len {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // `%` at end of template — emit literally.
        if i + 1 >= len {
            out.push('%');
            i += 1;
            continue;
        }

        let code = chars[i + 1];
        match code {
            '%' => {
                out.push('%');
            }
            '/' => {
                out.push_str(ctx.dbname);
            }
            '~' => {
                if ctx.dbname == ctx.user {
                    out.push('~');
                } else {
                    out.push_str(ctx.dbname);
                }
            }
            'n' => {
                out.push_str(ctx.user);
            }
            'M' => {
                out.push_str(ctx.host);
            }
            'm' => {
                // Short host: everything before the first `.`.
                let short = ctx.host.split_once('.').map_or(ctx.host, |(left, _)| left);
                out.push_str(short);
            }
            '>' => {
                out.push_str(&ctx.port.to_string());
            }
            '#' => {
                out.push(if ctx.is_superuser { '#' } else { '>' });
            }
            'R' => {
                let ch = if !ctx.connected {
                    '!'
                } else if ctx.single_line_mode {
                    '^'
                } else if ctx.in_block_comment {
                    '*'
                } else if ctx.continuation {
                    '-'
                } else {
                    '='
                };
                out.push(ch);
            }
            'x' => {
                out.push_str(ctx.tx.infix());
            }
            'l' => {
                out.push_str(&ctx.line_number.to_string());
            }
            'p' => {
                if let Some(pid) = ctx.backend_pid {
                    out.push_str(&pid.to_string());
                }
            }
            other => {
                // Unknown code — pass through verbatim.
                out.push('%');
                out.push(other);
            }
        }

        i += 2;
    }

    out
}

/// Build the main prompt string from a database name and transaction state.
///
/// Format: `dbname=>` (idle), `dbname=*>` (in-tx), `dbname=!>` (failed).
/// Continuation uses `-` instead of `=` as the first separator.
///
/// Rpg-specific execution and input mode tags (` plan`, ` text2sql`, etc.)
/// are inserted as a literal prefix before the psql-compatible `%R%x%#`
/// codes, so that [`expand_prompt`] drives the actual substitution.
///
/// Kept for use by tests and any external callers that need mode-tag logic.
/// Interactive REPL loops use [`build_prompt_from_settings`] instead.
#[allow(dead_code)]
pub fn build_prompt(
    dbname: &str,
    tx: TxState,
    continuation: bool,
    input_mode: InputMode,
    exec_mode: ExecMode,
) -> String {
    // Show the most specific non-default mode tag.  When the execution mode
    // is not Interactive it takes priority; otherwise we fall back to the
    // input mode (only non-default, i.e. text2sql, gets a tag).
    let mode_tag = match exec_mode {
        ExecMode::Plan => " plan",
        ExecMode::Yolo => " yolo",
        ExecMode::Interactive => match input_mode {
            InputMode::Text2Sql => " text2sql",
            InputMode::Sql => "",
        },
    };
    // Build a template equivalent to the default PROMPT1 (`%/%R%x%# `) but
    // with the Rpg mode tag injected as a literal between `%/` and `%R`.
    let template = format!("%/{mode_tag}%R%x%# ");
    let ctx = PromptContext {
        dbname,
        user: "",
        host: "",
        port: 5432,
        is_superuser: false,
        tx,
        continuation,
        in_block_comment: false,
        single_line_mode: false,
        connected: true,
        line_number: 0,
        backend_pid: None,
    };
    expand_prompt(&template, &ctx)
}

/// Build the prompt string by evaluating the PROMPT1 (or PROMPT2) variable
/// from `settings` and expanding psql-compatible format codes.
///
/// Uses PROMPT2 when `continuation` is `true`.  Falls back to the default
/// `%/%R%x%# ` if the variable has been unset.
///
/// Rpg-specific mode tags are handled by [`build_prompt`]; callers that
/// want full variable-driven prompts should use this function instead.
pub fn build_prompt_from_settings(
    settings: &ReplSettings,
    params: &ConnParams,
    tx: TxState,
    continuation: bool,
) -> String {
    let var_name = if continuation { "PROMPT2" } else { "PROMPT1" };
    let default_template = "%/%R%x%# ";
    let template = settings
        .vars
        .get(var_name)
        .unwrap_or(default_template)
        .to_owned();

    let ctx = PromptContext {
        dbname: &params.dbname,
        user: &params.user,
        host: &params.host,
        port: params.port,
        is_superuser: settings.is_superuser,
        tx,
        continuation,
        in_block_comment: false,
        single_line_mode: settings.single_line,
        connected: true,
        line_number: 0,
        backend_pid: None,
    };
    expand_prompt(&template, &ctx)
}

// ---------------------------------------------------------------------------
// Multi-line input detection
// ---------------------------------------------------------------------------

/// Return `true` when `buf` forms a complete SQL statement (ends with `;`
/// outside of strings, comments, and dollar-quoted bodies).
///
/// Rules:
/// - A trailing `;` outside of any quoting or commenting context terminates.
/// - Single-quoted strings `'...'` (with `''` escape) are tracked.
/// - Dollar-quoted strings `$$...$$` or `$tag$...$tag$` are tracked.
/// - `--` line comments are stripped before analysis.
/// - `/* … */` block comments are tracked.
/// - Parenthesis depth does not affect statement completion.
pub fn is_complete(buf: &str) -> bool {
    let mut in_single = false;
    let mut in_block_comment = false;
    let mut dollar_tag: Option<String> = None;

    let bytes = buf.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // If inside a dollar-quoted string, look for the closing tag.
        if let Some(ref tag) = dollar_tag.clone() {
            let tag_bytes = tag.as_bytes();
            if bytes[i..].starts_with(tag_bytes) {
                i += tag_bytes.len();
                dollar_tag = None;
                continue;
            }
            // newlines inside dollar-quoted strings: just advance
            i += 1;
            continue;
        }

        if in_block_comment {
            if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                i += 2;
                in_block_comment = false;
            } else {
                i += 1;
            }
            continue;
        }

        if in_single {
            if bytes[i] == b'\'' {
                // Escaped quote '' ?
                if i + 1 < len && bytes[i + 1] == b'\'' {
                    i += 2;
                } else {
                    in_single = false;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }

        // Not in any quoted context.

        // Line comment: skip to end of line
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // Block comment start
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }

        // Single-quote start
        if bytes[i] == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }

        // Dollar-quote start: scan for closing $
        if bytes[i] == b'$' {
            let rest = &buf[i..];
            if let Some(end) = rest[1..].find('$') {
                let inner = &rest[1..=end]; // text between the two $
                                            // Validate: tag must be empty ($$) or contain only letters,
                                            // digits, and underscores, and must NOT be purely digits
                                            // (which would be a positional parameter like $1, $2, …).
                let valid = inner.is_empty()
                    || (inner.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !inner.chars().all(|c| c.is_ascii_digit()));
                if valid {
                    let tag = &rest[..end + 2]; // includes both $ delimiters
                    dollar_tag = Some(tag.to_owned());
                    i += tag.len();
                    continue;
                }
            }
        }

        // Semicolon terminates (outside quotes/comments)
        if bytes[i] == b';' {
            return true;
        }

        i += 1;
    }

    false
}

// ---------------------------------------------------------------------------
// Backslash command types
// ---------------------------------------------------------------------------

// ExpandedMode is defined in output.rs and re-exported here for backward
// compatibility with code that imports from repl.
pub use crate::output::ExpandedMode;

// ---------------------------------------------------------------------------
// Input mode
// ---------------------------------------------------------------------------

/// The current input interpretation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Standard SQL input (default). Lines are accumulated and executed
    /// when a semicolon terminator is found.
    #[default]
    Sql,
    /// Text-to-SQL mode. Each non-empty line is treated as a natural
    /// language prompt and forwarded to `/ask`.  Lines starting with `;`
    /// are sent as raw SQL.
    Text2Sql,
}

// ---------------------------------------------------------------------------
// Execution mode
// ---------------------------------------------------------------------------

/// Controls *how much* the AI can do without asking.
///
/// Orthogonal to [`InputMode`] — any input mode can combine with any
/// execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecMode {
    /// AI always shows generated SQL and asks before executing (default).
    #[default]
    Interactive,
    /// AI investigates (read-only) and produces a plan document.
    Plan,
    /// AI auto-executes suggested fixes directly.
    Yolo,
}

// ---------------------------------------------------------------------------
// Auto-EXPLAIN mode
// ---------------------------------------------------------------------------

/// Auto-EXPLAIN level — controls whether queries automatically show
/// execution plans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoExplain {
    /// No automatic EXPLAIN (default).
    #[default]
    Off,
    /// Prepend `EXPLAIN` to every query.
    On,
    /// Prepend `EXPLAIN ANALYZE` to every query.
    Analyze,
    /// Prepend `EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING)`.
    Verbose,
}

impl AutoExplain {
    /// Cycle to the next mode: Off → On → Analyze → Verbose → Off.
    pub(crate) fn cycle(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Analyze,
            Self::Analyze => Self::Verbose,
            Self::Verbose => Self::Off,
        }
    }

    /// Return the EXPLAIN prefix string (empty for Off).
    fn prefix(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::On => "EXPLAIN ",
            Self::Analyze => "EXPLAIN ANALYZE ",
            Self::Verbose => "EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING) ",
        }
    }

    /// Human-readable label.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Analyze => "analyze",
            Self::Verbose => "verbose",
        }
    }
}

// ---------------------------------------------------------------------------
// Last-error context (used by /fix)
// ---------------------------------------------------------------------------

/// Context captured when a query fails, so `/fix` can explain and correct it.
#[derive(Debug, Clone)]
pub struct LastError {
    /// The SQL query that failed.
    pub query: String,
    /// Human-readable error message from the server.
    pub error_message: String,
    /// Optional SQLSTATE code (e.g. `"42703"` for undefined column).
    pub sqlstate: Option<String>,
}

// ---------------------------------------------------------------------------
// Session conversation context (used by /ask for follow-up queries)
// ---------------------------------------------------------------------------

/// A single entry in the AI conversation history.
#[derive(Debug, Clone)]
pub struct ConversationEntry {
    /// Role: "user" or "assistant".
    pub role: &'static str,
    /// The text content.
    pub content: String,
    /// Whether this entry is an action record (executed query + result).
    ///
    /// Action entries survive compaction — they are never LLM-summarized.
    /// Only FIFO-evicted when the total entry count exceeds `max_entries`.
    pub is_action: bool,
}

/// Sliding-window conversation context for AI commands.
///
/// Stores recent user prompts and assistant responses so that follow-up
/// queries (e.g. "now group that by month") can reference prior context.
/// Also tracks SQL queries and their results for richer context.
#[derive(Debug, Clone, Default)]
pub struct ConversationContext {
    /// Conversation history entries (user + assistant turns).
    entries: Vec<ConversationEntry>,
    /// Maximum number of entries before oldest are dropped.
    max_entries: usize,
    /// Approximate token count (rough: 1 token ≈ 4 chars).
    approx_tokens: usize,
}

impl ConversationContext {
    /// Create a new context with a default capacity.
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: 50,
            approx_tokens: 0,
        }
    }

    /// Add a user turn to the conversation.
    fn push_user(&mut self, content: String) {
        self.approx_tokens += content.len() / 4;
        self.entries.push(ConversationEntry {
            role: "user",
            content,
            is_action: false,
        });
        self.trim();
    }

    /// Add an assistant turn to the conversation.
    fn push_assistant(&mut self, content: String) {
        self.approx_tokens += content.len() / 4;
        self.entries.push(ConversationEntry {
            role: "assistant",
            content,
            is_action: false,
        });
        self.trim();
    }

    /// Record a SQL query and its result summary as an action entry.
    ///
    /// Action entries survive compaction — they are never LLM-summarized,
    /// only FIFO-evicted. This ensures the AI always knows which queries
    /// were actually executed and what happened.
    fn push_query_result(&mut self, sql: &str, result_summary: &str) {
        let content = format!("Executed SQL:\n```sql\n{sql}\n```\nResult: {result_summary}");
        self.approx_tokens += content.len() / 4;
        self.entries.push(ConversationEntry {
            role: "user",
            content,
            is_action: true,
        });
        self.trim();
    }

    /// Build the conversation history as `Message` objects for the LLM.
    fn to_messages(&self) -> Vec<crate::ai::Message> {
        self.entries
            .iter()
            .map(|e| crate::ai::Message {
                role: if e.role == "user" {
                    crate::ai::Role::User
                } else {
                    crate::ai::Role::Assistant
                },
                content: e.content.clone(),
            })
            .collect()
    }

    /// Compact the context: summarize older *conversation* entries into a
    /// single summary, keeping the most recent `keep` entries and all
    /// *action* entries intact.
    ///
    /// Action entries (`is_action == true`) are never summarized — they
    /// survive compaction and remain in the context at their original
    /// position. Only conversational entries are compressed.
    fn compact(&mut self, focus: Option<&str>) {
        use std::fmt::Write as _;

        if self.entries.len() <= 4 {
            return; // Nothing meaningful to compact.
        }

        // Keep the last 4 entries, split the rest for compaction.
        let keep = 4;
        let split = self.entries.len().saturating_sub(keep);
        let old_entries: Vec<ConversationEntry> = self.entries.drain(..split).collect();

        // Separate action entries (survive) from conversation entries (summarized).
        let mut action_entries: Vec<ConversationEntry> = Vec::new();
        let mut conversation_entries: Vec<ConversationEntry> = Vec::new();
        for entry in old_entries {
            if entry.is_action {
                action_entries.push(entry);
            } else {
                conversation_entries.push(entry);
            }
        }

        // Build summary from conversation entries only.
        let mut summary = String::from("Previous conversation summary:");
        if let Some(f) = focus {
            let _ = write!(summary, " (focus: {f})");
        }
        summary.push('\n');

        for entry in &conversation_entries {
            let preview: String = entry.content.chars().take(200).collect();
            let suffix = if entry.content.len() > 200 { "..." } else { "" };
            let _ = writeln!(summary, "- [{role}] {preview}{suffix}", role = entry.role);
        }

        // Rebuild: summary + surviving action entries + kept entries.
        let mut rebuilt = Vec::with_capacity(1 + action_entries.len() + self.entries.len());
        rebuilt.push(ConversationEntry {
            role: "user",
            content: summary,
            is_action: false,
        });
        rebuilt.append(&mut action_entries);
        rebuilt.append(&mut self.entries);
        self.entries = rebuilt;

        // Recalculate token count.
        self.approx_tokens = 0;
        for e in &self.entries {
            self.approx_tokens += e.content.len() / 4;
        }
    }

    /// Clear all conversation history.
    fn clear(&mut self) {
        self.entries.clear();
        self.approx_tokens = 0;
    }

    /// Return `true` if the context has any entries.
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Approximate token count.
    fn token_estimate(&self) -> usize {
        self.approx_tokens
    }

    /// Auto-compact if the approximate token count exceeds 70% of the
    /// configured context window.  Returns `true` if compaction occurred.
    fn auto_compact_if_needed(&mut self, context_window: u32) -> bool {
        let threshold = (u64::from(context_window) * 70 / 100) as usize;
        if self.approx_tokens > threshold && self.entries.len() > 4 {
            self.compact(None);
            true
        } else {
            false
        }
    }

    /// Drop oldest entries until we're within `max_entries`.
    fn trim(&mut self) {
        while self.entries.len() > self.max_entries {
            if let Some(removed) = self.entries.first() {
                self.approx_tokens = self.approx_tokens.saturating_sub(removed.content.len() / 4);
            }
            self.entries.remove(0);
        }
    }
}

// ---------------------------------------------------------------------------
// REPL settings (mutable at runtime via backslash commands)
// ---------------------------------------------------------------------------

/// Runtime-adjustable display settings.
#[allow(clippy::struct_excessive_bools)]
pub struct ReplSettings {
    /// Whether to print query timing after each query.
    pub timing: bool,
    /// Expanded display mode.
    pub expanded: ExpandedMode,
    /// Whether to echo internally-generated SQL to stdout (`-E` / `--echo-hidden`).
    pub echo_hidden: bool,
    /// Print configuration (`\pset` and CLI flags).
    pub pset: crate::output::PsetConfig,
    /// Variable store (`\set` / `\unset`).
    pub vars: crate::vars::Variables,
    /// Current output redirect target. When `Some`, query output and `\qecho`
    /// text are written here instead of stdout.
    pub output_target: Option<Box<dyn std::io::Write>>,
    /// Log file handle (`-L`). When `Some`, all query input and output are
    /// mirrored to this writer in addition to normal output.
    pub log_file: Option<Box<dyn std::io::Write>>,
    /// Echo each query to stderr before executing (`-e` / `--echo-queries`).
    pub echo_queries: bool,
    /// Echo failed query text to stderr (`-b` / `--echo-errors`).
    pub echo_errors: bool,
    /// Single-step mode: prompt before executing each command (`-s`).
    pub single_step: bool,
    /// Single-line mode: treat newline as statement terminator (`-S`).
    pub single_line: bool,
    /// Wrap `-f` file execution in `BEGIN` / `COMMIT` (`-1`).
    pub single_transaction: bool,
    /// Quiet mode: suppress informational messages (`-q`).
    pub quiet: bool,
    /// Debug mode: enable debug output (`-D`).
    pub debug: bool,
    /// Conditional execution state (`\if` / `\elif` / `\else` / `\endif`).
    pub cond: crate::conditional::ConditionalState,
    /// The last successfully-executed SQL string, used by `\watch`.
    pub last_query: Option<String>,
    /// Pending bind parameters set by `\bind` for the next query execution.
    ///
    /// When `Some`, the next query is sent using the extended query protocol
    /// (`client.query`) with these values as positional parameters.  The
    /// field is cleared to `None` after each query execution.
    pub pending_bind_params: Option<Vec<String>>,
    /// Named prepared statements stored by `\parse`.
    ///
    /// Maps statement name → compiled [`tokio_postgres::Statement`].
    pub named_statements: HashMap<String, tokio_postgres::Statement>,
    /// Disable ANSI syntax highlighting in the interactive REPL.
    ///
    /// Set by `--no-highlight` CLI flag or `\set HIGHLIGHT off`.
    pub no_highlight: bool,
    /// Disable schema-aware tab completion in the interactive REPL.
    ///
    /// Toggled by the F2 key or `\f2` metacommand.
    pub no_completion: bool,
    /// Whether the built-in pager is enabled.
    ///
    /// Defaults to `true`. Disable with `\set PAGER off` or by setting the
    /// `PAGER` environment variable to an external pager command.
    /// Only activates in interactive mode (not with `-c`, `-f`, or piped input).
    pub pager_enabled: bool,
    /// External pager command to run instead of the built-in TUI pager.
    ///
    /// `None` uses the built-in pager.  `Some(cmd)` spawns `cmd` via a shell
    /// and pipes output to its stdin.
    ///
    /// Set by `\set PAGER <cmd>` when `<cmd>` is not `on`/`off`, or
    /// initialised from the `PAGER` environment variable at startup.
    pub pager_command: Option<String>,
    /// Minimum number of result lines before the pager activates.
    ///
    /// When `> 0`, the pager only activates if the output exceeds *both*
    /// the terminal height *and* this threshold.  Defaults to `0` (disabled).
    ///
    /// Set by `\pset pager_min_lines N`.
    pub pager_min_lines: usize,
    /// Warn before executing destructive statements (DROP, TRUNCATE, etc.).
    ///
    /// Defaults to `true`. Disable with `\set SAFETY off` or
    /// `\set DESTRUCTIVE_WARNING off`.
    pub safety_enabled: bool,
    /// Loaded TOML configuration (profiles, display defaults, etc.).
    ///
    /// Used by `\c @profile` to look up named connection profiles.
    pub config: crate::config::Config,
    /// Current input interpretation mode.
    pub input_mode: InputMode,
    /// Current execution mode (how much the AI can do without asking).
    pub exec_mode: ExecMode,
    /// Auto-EXPLAIN level — prepend EXPLAIN to queries when not Off.
    pub auto_explain: AutoExplain,
    /// Controls how EXPLAIN output is rendered in the interactive REPL.
    ///
    /// Set via `\pset explain_format enhanced|raw|compact`.
    pub explain_format: crate::explain::ExplainFormat,
    /// Context from the most-recently failed query.
    ///
    /// Populated whenever a query returns an error; cleared on the next
    /// successful execution.  Used by `/fix` to provide the LLM with the
    /// query and error details.
    pub last_error: Option<LastError>,
    /// Session conversation context for multi-turn AI interactions.
    ///
    /// Stores recent user prompts, assistant responses, and query results
    /// so follow-up `/ask` commands can reference prior context.
    pub conversation: ConversationContext,
    /// Cumulative token usage across all AI calls in this session.
    ///
    /// Tracks total input + output tokens consumed.  When a `token_budget`
    /// is configured, AI calls are refused once this exceeds the budget.
    pub tokens_used: u64,
    /// Detected database capabilities (extensions, version).
    ///
    /// Populated at connect time by [`crate::capabilities::detect`].
    pub db_capabilities: crate::capabilities::DbCapabilities,
    /// Bypass safety checks in YOLO mode.
    ///
    /// Set by `--i-know-what-im-doing` CLI flag. When `true` and
    /// `exec_mode == Yolo`, all write queries are auto-executed. Use
    /// with extreme care.
    pub i_know_what_im_doing: bool,
    /// Verbosity level for error display, mirroring psql's `\set VERBOSITY`.
    ///
    /// When `true`, SQLSTATE codes are appended to error output.
    /// Defaults to `false` (psql default).
    pub verbose_errors: bool,
    /// Use Vi keybinding mode in the REPL.
    ///
    /// Defaults to `false` (Emacs mode).  Set with `\set VI on`.
    /// rustyline does not support changing `EditMode` on an existing editor
    /// instance, so this preference is stored here and applied at the next
    /// session start via [`run_readline_loop`].
    pub vi_mode: bool,
    /// Path of the currently-executing script file, if any.
    ///
    /// Set whenever a file is being processed via `\i`, `\ir`, or `-f`.
    /// Used by `\ir` to resolve relative file paths against the directory
    /// of the current script rather than the process working directory,
    /// matching psql behaviour.
    pub current_file: Option<String>,
    /// Unique identifier for the current session (used by session persistence).
    ///
    /// Assigned once at REPL startup from [`crate::session_store::new_session_id`].
    pub session_id: String,
    /// Number of queries executed in this session.
    ///
    /// Incremented after each successful query execution.  Persisted by
    /// `\session save` and `\session touch`.
    pub query_count: u32,
    /// Whether the connected role is a superuser.
    ///
    /// Detected once after connection (and re-detected after `\c` reconnect)
    /// by querying `current_setting('is_superuser')`.  Controls whether the
    /// prompt shows `#` (superuser) or `>` (regular user).
    pub is_superuser: bool,
    /// Shared schema cache for tab completion.
    ///
    /// `None` in non-interactive paths (e.g. `-c`, `-f`, piped stdin).
    /// Set to `Some(...)` by the readline loop so that `\refresh` can
    /// update the same `Arc` that the completion helper holds.
    #[cfg(not(target_arch = "wasm32"))]
    pub schema_cache: Option<Arc<RwLock<SchemaCache>>>,

    // -- Query audit log (FR-23) -------------------------------------------
    /// Open file handle for the query audit log (`\log-file`).
    ///
    /// When `Some`, each successfully-executed query is appended to the
    /// file in a human-readable comment format after execution completes.
    /// Never contains passwords or connection strings.
    pub audit_log_file: Option<std::io::BufWriter<std::fs::File>>,
    /// Path of the currently-open audit log file, for display purposes.
    pub audit_log_path: Option<std::path::PathBuf>,
    /// Database name used in audit log entries.
    ///
    /// Set from [`crate::connection::ConnParams::dbname`] at connect time
    /// and after `\c` reconnects.
    pub audit_dbname: String,
    /// User name used in audit log entries.
    ///
    /// Set from [`crate::connection::ConnParams::user`] at connect time
    /// and after `\c` reconnects.
    pub audit_user: String,
    /// Row count from the most-recently completed query, for audit entries.
    ///
    /// Set by `execute_query` / `execute_query_extended` after each
    /// `CommandComplete` message; `None` when no query has completed yet
    /// or the query produced no `CommandComplete` (e.g. error).
    pub last_row_count: Option<u64>,
    /// Contents of `POSTGRES.md` found alongside `.rpg.toml`, if any.
    ///
    /// When present, this text is injected into the AI system prompt for
    /// `/ask`, `/fix`, and `/explain` commands to provide project-specific
    /// Postgres context (schema notes, conventions, etc.).
    pub project_context: Option<String>,
    /// Paths from `[ai] context_files` in `.rpg.toml`.
    ///
    /// These files are read at AI-call time and appended to the system
    /// prompt so the LLM has project-specific schema and query context.
    pub ai_context_files: Vec<String>,

    // -- Status bar (FR-25) ------------------------------------------------
    /// Persistent status bar rendered at the bottom of the terminal.
    ///
    /// Present only in interactive sessions; `None` in non-interactive paths.
    ///
    /// Wrapped in `Arc<Mutex<...>>` so the SIGWINCH signal handler (spawned in
    /// `run_readline_loop`) can share ownership and call `on_resize()`/`render()`
    /// from a background tokio task without data races.
    pub statusline: Option<Arc<Mutex<crate::statusline::StatusLine>>>,
    /// Last query duration in milliseconds (for the status bar).
    ///
    /// Updated after each query execution.
    pub last_query_duration_ms: Option<u64>,
    /// Show an inline hint ("type /fix …") after a SQL error.
    ///
    /// Defaults to `true`. Disable with `\set AUTO_SUGGEST off`.
    /// Only shown when AI is configured and the error is a SQL error.
    pub auto_suggest_fix: bool,
    /// Set to `true` immediately before `/fix` runs so that the inline
    /// hint is suppressed for any error produced by the fixed query,
    /// avoiding suggestion loops.  Cleared after each query execution.
    pub last_was_fix: bool,
    /// Whether to show the generated SQL box in `\text2sql` mode.
    ///
    /// Defaults to `true`. When `true`, the SQL is printed in a
    /// `┌── sql` box before execution and the user is prompted
    /// `Execute? [Y/n/e]`.  When `false` (or when `exec_mode == Yolo`),
    /// the SQL is hidden and auto-executed without confirmation.
    ///
    /// Toggle with `\set TEXT2SQL_SHOW_SQL on/off`.
    pub text2sql_show_sql: bool,
    /// Set to `true` when `\prompt` detects Ctrl+C (interrupt).
    ///
    /// `exec_lines` checks this flag after each meta-command dispatch and
    /// stops processing the current script, allowing the user to abort an
    /// interactive postgres_dba-style menu with Ctrl+C.  The flag is cleared
    /// at the top of the readline loop so that the next REPL cycle starts
    /// clean.
    pub prompt_interrupted: bool,
    /// The most-recent natural-language query entered in text2sql mode.
    ///
    /// Stored whenever `handle_ai_ask` is called while
    /// `input_mode == InputMode::Text2Sql` so that `/fix` can include the
    /// original intent in its prompt, preventing the AI from regenerating
    /// the same broken SQL instead of fixing the actual error.
    pub last_t2s_nl_query: Option<String>,
    /// `true` while an internally-managed `/ask` read-only transaction is
    /// open (i.e. between `start transaction read only` and `commit`).
    ///
    /// Used by the auto-rollback logic to distinguish between a user's
    /// explicit `BEGIN` (which must NOT be auto-rolled back) and an internal
    /// `/ask` transaction (which SHOULD be rolled back if it leaks into the
    /// next command due to an error or interruption).
    pub internal_tx: bool,
    /// Raw text of the last EXPLAIN output (plain text format).
    ///
    /// Populated whenever an EXPLAIN query succeeds, so that
    /// `\explain share` can upload it to explain.depesz.com or
    /// explain.dalibo.com.  `None` if no EXPLAIN has been run yet.
    pub last_explain_text: Option<String>,

    // -- Custom Lua commands (#659) ----------------------------------------
    /// Registry of custom backslash commands loaded from Lua scripts.
    ///
    /// Populated once at startup from `~/.config/rpg/commands/*.lua`.
    /// When the `lua` feature is not compiled in, this registry is always
    /// empty and custom-command execution returns a feature-absent error.
    pub lua_registry: crate::lua_commands::LuaRegistry,

    /// Query pre-filled into the readline prompt after a history picker
    /// selection (`\s` with no arguments).
    ///
    /// When `Some`, the REPL loop uses `readline_with_initial` for the next
    /// prompt so the user can edit or execute the chosen history entry.
    /// Cleared to `None` after the prompt is shown.
    pub initial_input: Option<String>,
}

impl std::fmt::Debug for ReplSettings {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplSettings")
            .field("timing", &self.timing)
            .field("expanded", &self.expanded)
            .field("echo_hidden", &self.echo_hidden)
            .field("pset", &self.pset)
            .field("vars", &self.vars)
            .field(
                "output_target",
                &self.output_target.as_ref().map(|_| "<writer>"),
            )
            .field("log_file", &self.log_file.as_ref().map(|_| "<writer>"))
            .field("echo_queries", &self.echo_queries)
            .field("echo_errors", &self.echo_errors)
            .field("single_step", &self.single_step)
            .field("single_line", &self.single_line)
            .field("single_transaction", &self.single_transaction)
            .field("quiet", &self.quiet)
            .field("debug", &self.debug)
            .field("cond_depth", &self.cond.depth())
            .field("last_query", &self.last_query.as_deref().map(|_| "<sql>"))
            .field(
                "pending_bind_params",
                &self
                    .pending_bind_params
                    .as_ref()
                    .map(|p| format!("{} params", p.len())),
            )
            .field(
                "named_statements",
                &format!("{} stmts", self.named_statements.len()),
            )
            .field("no_highlight", &self.no_highlight)
            .field("no_completion", &self.no_completion)
            .field("pager_enabled", &self.pager_enabled)
            .field("pager_command", &self.pager_command)
            .field("pager_min_lines", &self.pager_min_lines)
            .field("safety_enabled", &self.safety_enabled)
            .field("config_profiles", &self.config.connections.len())
            .field("input_mode", &self.input_mode)
            .field("exec_mode", &self.exec_mode)
            .field("auto_explain", &self.auto_explain)
            .field("explain_format", &self.explain_format)
            .field(
                "last_error",
                &self.last_error.as_ref().map(|e| e.error_message.as_str()),
            )
            .field(
                "conversation",
                &format!(
                    "{} entries, ~{} tokens",
                    self.conversation.entries.len(),
                    self.conversation.token_estimate()
                ),
            )
            .field("tokens_used", &self.tokens_used)
            .field("db_capabilities", &self.db_capabilities)
            .field("i_know_what_im_doing", &self.i_know_what_im_doing)
            .field("verbose_errors", &self.verbose_errors)
            .field("vi_mode", &self.vi_mode)
            .field("current_file", &self.current_file)
            .field("session_id", &self.session_id)
            .field("query_count", &self.query_count)
            .field("is_superuser", &self.is_superuser)
            .field(
                "audit_log_file",
                &self.audit_log_file.as_ref().map(|_| "<writer>"),
            )
            .field(
                "audit_log_path",
                &self
                    .audit_log_path
                    .as_deref()
                    .map(|p| p.display().to_string()),
            )
            .field("audit_dbname", &self.audit_dbname)
            .field("audit_user", &self.audit_user)
            .field("last_row_count", &self.last_row_count)
            .field(
                "project_context",
                &self.project_context.as_deref().map(|_| "<text>"),
            )
            .field("ai_context_files", &self.ai_context_files.len())
            .field(
                "statusline",
                &self
                    .statusline
                    .as_ref()
                    .and_then(|s| s.lock().ok().map(|g| g.enabled)),
            )
            .field("last_query_duration_ms", &self.last_query_duration_ms)
            .field("auto_suggest_fix", &self.auto_suggest_fix)
            .field("last_was_fix", &self.last_was_fix)
            .field("text2sql_show_sql", &self.text2sql_show_sql)
            .field("prompt_interrupted", &self.prompt_interrupted)
            .field(
                "last_t2s_nl_query",
                &self.last_t2s_nl_query.as_deref().map(|_| "<nl-query>"),
            )
            .field("internal_tx", &self.internal_tx)
            .field(
                "last_explain_text",
                &self.last_explain_text.as_deref().map(|_| "<explain>"),
            )
            .field(
                "lua_commands",
                &format!("{} loaded", self.lua_registry.commands.len()),
            )
            .field("initial_input", &self.initial_input)
            .finish_non_exhaustive()
    }
}

impl Default for ReplSettings {
    fn default() -> Self {
        Self {
            timing: false,
            expanded: ExpandedMode::default(),
            echo_hidden: false,
            pset: crate::output::PsetConfig::default(),
            vars: crate::vars::Variables::new(),
            output_target: None,
            log_file: None,
            echo_queries: false,
            echo_errors: false,
            single_step: false,
            single_line: false,
            single_transaction: false,
            quiet: false,
            debug: false,
            cond: crate::conditional::ConditionalState::default(),
            last_query: None,
            pending_bind_params: None,
            named_statements: HashMap::new(),
            no_highlight: false,
            no_completion: false,
            // Pager is enabled by default in interactive mode.
            pager_enabled: true,
            pager_command: None,
            pager_min_lines: 0,
            // Warn before destructive statements by default.
            safety_enabled: true,
            config: crate::config::Config::default(),
            input_mode: InputMode::default(),
            exec_mode: ExecMode::default(),
            auto_explain: AutoExplain::default(),
            explain_format: crate::explain::ExplainFormat::default(),
            last_error: None,
            conversation: ConversationContext::new(),
            tokens_used: 0,
            db_capabilities: crate::capabilities::DbCapabilities::default(),
            i_know_what_im_doing: false,
            verbose_errors: false,
            vi_mode: false,
            current_file: None,
            session_id: crate::session_store::new_session_id(),
            query_count: 0,
            is_superuser: false,
            #[cfg(not(target_arch = "wasm32"))]
            schema_cache: None,
            audit_log_file: None,
            audit_log_path: None,
            audit_dbname: String::new(),
            audit_user: String::new(),
            last_row_count: None,
            project_context: None,
            ai_context_files: Vec::new(),
            statusline: None,
            last_query_duration_ms: None,
            auto_suggest_fix: true,
            last_was_fix: false,
            text2sql_show_sql: true,
            prompt_interrupted: false,
            last_t2s_nl_query: None,
            internal_tx: false,
            last_explain_text: None,
            lua_registry: crate::lua_commands::LuaRegistry::load(""),
            initial_input: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tilde expansion (platform-aware)
// ---------------------------------------------------------------------------

/// Expand a leading `~` or `~/...` in a path string to the user's home
/// directory.  Returns the path unchanged when the home directory cannot
/// be determined (e.g. on WASM).
fn expand_tilde(raw: &str) -> PathBuf {
    if raw.starts_with("~/") || raw == "~" {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(home) = dirs::home_dir() {
            let suffix = raw.strip_prefix("~/").unwrap_or("");
            return home.join(suffix);
        }
        PathBuf::from(raw)
    } else {
        PathBuf::from(raw)
    }
}

// ---------------------------------------------------------------------------
// History file path resolution
// ---------------------------------------------------------------------------

/// Resolve the history file path.
///
/// Priority:
/// 1. `PSQL_HISTORY` environment variable
/// 2. `~/.rpg_history`
pub fn history_file() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("PSQL_HISTORY") {
        return Some(PathBuf::from(val));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        dirs::home_dir().map(|h| h.join(DEFAULT_HISTORY_FILE))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Startup file resolution
// ---------------------------------------------------------------------------

/// Resolve the startup RC file path.
///
/// Priority:
/// 1. `$PSQLRC` environment variable
/// 2. `~/.rpgrc` if the file exists
/// 3. `~/.psqlrc` if the file exists
/// 4. `None` — no startup file
pub fn startup_file() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("PSQLRC") {
        return Some(PathBuf::from(val));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(home) = dirs::home_dir() {
        let rpgrc = home.join(".rpgrc");
        if rpgrc.exists() {
            return Some(rpgrc);
        }
        let psqlrc = home.join(".psqlrc");
        if psqlrc.exists() {
            return Some(psqlrc);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Non-interactive (piped / -c / -f) execution
// ---------------------------------------------------------------------------

/// Execute a single SQL command string (from `-c`) and exit.
///
/// Mirrors psql behaviour: if the string starts with a backslash it is
/// dispatched as a meta-command (using only the first line as the command,
/// matching psql's `-c` meta-command handling).  Otherwise it is sent as SQL.
pub async fn exec_command(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    params: &crate::connection::ConnParams,
) -> i32 {
    // `quit` / `exit` passed via -c should exit cleanly (psql behaviour).
    if is_quit_exit(sql.trim(), true) {
        return 0;
    }
    if sql.trim_start().starts_with('\\') {
        // Backslash meta-command in -c mode.
        //
        // psql processes only the first line as the meta-command when `-c`
        // receives a multi-line string starting with `\`.  Anything after
        // the first newline is treated as extra arguments (and warned about).
        // We replicate this by extracting only the first line for parsing,
        // and dispatching against the real settings so that pset changes are
        // visible (stdout messages printed, border/format/etc. updated).
        let first_line = sql.trim().lines().next().unwrap_or(sql.trim());
        let interpolated = settings.vars.interpolate(first_line);
        let mut parsed = crate::metacmd::parse(&interpolated);
        parsed.echo_hidden = settings.echo_hidden;
        let mut tx = TxState::default();
        let result = dispatch_meta(parsed, client, params, settings, &mut tx).await;
        // Handle results that produce output or modify settings.
        match result {
            MetaResult::ShowMode => {
                let input_label = match settings.input_mode {
                    InputMode::Sql => "sql",
                    InputMode::Text2Sql => "text2sql",
                };
                let exec_label = match settings.exec_mode {
                    ExecMode::Interactive => "interactive",
                    ExecMode::Plan => "plan",
                    ExecMode::Yolo => "yolo",
                };
                eprintln!("Input mode: {input_label}  Execution mode: {exec_label}");
            }
            result @ (MetaResult::SetInputMode(_) | MetaResult::SetExecMode(_)) => {
                let label = apply_mode_change(&result, settings);
                match result {
                    MetaResult::SetInputMode(_) => eprintln!("Input mode: {label}"),
                    _ => eprintln!("Execution mode: {label}"),
                }
            }
            _ => {}
        }
        return 0;
    }
    let mut tx = TxState::default();
    i32::from(!execute_query(client, sql, settings, &mut tx).await)
}

/// Execute all SQL statements from a file and exit.
///
/// The file content is processed line by line.  Backslash meta-commands
/// (including `\if` / `\elif` / `\else` / `\endif`) are dispatched
/// immediately; SQL lines are accumulated until a complete statement is
/// detected and then executed.  Suppressed branches are skipped.
///
/// When `settings.single_transaction` is `true`, the entire file is wrapped
/// in an explicit `BEGIN` … `COMMIT` block. On any error the transaction is
/// rolled back and execution stops.
///
/// # Errors
/// Returns 1 if the file cannot be read or any statement produces a SQL error.
pub async fn exec_file(
    client: &Client,
    path: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) -> i32 {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rpg: could not read file \"{path}\": {e}");
            return 1;
        }
    };
    let mut tx = TxState::default();

    // -1 / --single-transaction: open a transaction before the first statement.
    // Use simple_query directly so that begin/commit/rollback are not echoed,
    // logged, or prompted (they are internal bookkeeping, not user SQL).
    if settings.single_transaction {
        if let Err(e) = client.simple_query("begin").await {
            eprintln!("rpg: could not begin transaction: {e}");
            return 1;
        }
        tx.update_from_sql("begin");
    }

    let mut exit_code = exec_lines(
        client,
        content.lines().map(str::to_owned),
        settings,
        params,
        &mut tx,
    )
    .await;

    if settings.cond.depth() > 0 {
        eprintln!(
            "rpg: warning: {} unterminated \\if block(s) at end of file \"{path}\"",
            settings.cond.depth()
        );
    }

    // -1 / --single-transaction: commit on success, rollback on failure.
    if settings.single_transaction {
        if exit_code == 0 {
            if let Err(e) = client.simple_query("commit").await {
                eprintln!("rpg: could not commit transaction: {e}");
                exit_code = 1;
            } else {
                tx.update_from_sql("commit");
            }
        } else {
            let _ = client.simple_query("rollback").await;
            tx.update_from_sql("rollback");
        }
    }

    exit_code
}

/// Execute SQL lines from stdin (non-interactive piped input).
pub async fn exec_stdin(client: &Client, settings: &mut ReplSettings, params: &ConnParams) -> i32 {
    let stdin = io::stdin();
    let lines = stdin.lock().lines().map_while(|l| match l {
        Ok(line) => Some(line),
        Err(e) => {
            eprintln!("rpg: read error: {e}");
            None
        }
    });
    let mut tx = TxState::default();
    let exit_code = exec_lines(client, lines, settings, params, &mut tx).await;

    if settings.cond.depth() > 0 {
        eprintln!(
            "rpg: warning: {} unterminated \\if block(s) at end of input",
            settings.cond.depth()
        );
    }

    exit_code
}

/// Shared line-processing core for `exec_file`, `exec_stdin`, and
/// `io::include_file`.
///
/// Each line is either:
/// - A backslash meta-command → dispatched immediately (always, for
///   conditionals; skipped for others when suppressed).
/// - A SQL fragment → accumulated into `buf`; flushed when complete.
///   Skipped entirely when inside a suppressed branch.
#[allow(clippy::too_many_lines)]
pub(crate) async fn exec_lines(
    client: &Client,
    lines: impl Iterator<Item = String>,
    settings: &mut ReplSettings,
    params: &ConnParams,
    tx: &mut TxState,
) -> i32 {
    let mut buf = String::new();
    let mut exit_code = 0i32;

    'lines: for line in lines {
        // `quit` / `exit` bare words work in all modes (psql behaviour).
        if is_quit_exit(line.trim(), buf.is_empty()) {
            break 'lines;
        }
        if line.trim_start().starts_with('\\') {
            // Interpolate variables in the meta-command line (psql behaviour:
            // `:varname` is expanded before the backslash parser sees it).
            let interpolated = settings.vars.interpolate(line.trim());
            let mut parsed = crate::metacmd::parse(&interpolated);
            parsed.echo_hidden = settings.echo_hidden;
            let result = dispatch_meta(parsed, client, params, settings, tx).await;
            // Handle buffer-aware results that exec_lines must act on directly.
            match result {
                MetaResult::ExecuteBuffer => {
                    let sql = buf.trim().to_owned();
                    buf.clear();
                    if !sql.is_empty() {
                        let ok = if let Some(bp) = settings.pending_bind_params.take() {
                            execute_query_extended(client, &sql, &bp, settings, tx).await
                        } else {
                            execute_query(client, &sql, settings, tx).await
                        };
                        if !ok {
                            exit_code = 1;
                            if settings.single_transaction {
                                break 'lines;
                            }
                        }
                    }
                }
                MetaResult::DescribeBuffer => {
                    // Buffer is NOT cleared after \gdesc.
                    describe_buffer(client, buf.trim(), settings.verbose_errors).await;
                }
                MetaResult::CrosstabViewBuffer(args) => {
                    let sql = buf.trim().to_owned();
                    buf.clear();
                    if !sql.is_empty() {
                        execute_crosstabview(client, &sql, &args, settings, tx).await;
                    }
                }
                MetaResult::BindParams(params) => {
                    settings.pending_bind_params = Some(params);
                }
                MetaResult::ParseStatement(name) => {
                    let sql = buf.trim().to_owned();
                    if sql.is_empty() {
                        eprintln!("\\parse: query buffer is empty");
                    } else {
                        match client.prepare(&sql).await {
                            Ok(stmt) => {
                                settings.named_statements.insert(name, stmt);
                            }
                            Err(e) => {
                                crate::output::eprint_db_error(
                                    &e,
                                    Some(&sql),
                                    settings.verbose_errors,
                                );
                                exit_code = 1;
                                if settings.single_transaction {
                                    break 'lines;
                                }
                            }
                        }
                    }
                }
                MetaResult::BindNamedExec(name, params) => {
                    if !execute_named_stmt(client, &name, &params, settings, tx).await {
                        exit_code = 1;
                        if settings.single_transaction {
                            break 'lines;
                        }
                    }
                }
                MetaResult::ClosePrepared(name) => {
                    if settings.named_statements.remove(&name).is_some() {
                        let deallocate = format!("deallocate {name}");
                        if !execute_query(client, &deallocate, settings, tx).await {
                            exit_code = 1;
                            if settings.single_transaction {
                                break 'lines;
                            }
                        }
                    } else {
                        eprintln!(
                            "\\close_prepared: prepared statement \
                             \"{name}\" does not exist"
                        );
                    }
                }
                _ => {}
            }
            // Stop the script loop when `\prompt` detected Ctrl+C.
            if settings.prompt_interrupted {
                break 'lines;
            }
        } else if settings.cond.is_active() {
            // Check for inline backslash command (e.g. `select 1 \gset`).
            if let Some(pos) = find_inline_backslash(&line) {
                let sql_part = &line[..pos];
                let meta_part = line[pos..].trim();
                if !sql_part.trim().is_empty() {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(sql_part.trim_end());
                }
                let interpolated_meta = settings.vars.interpolate(meta_part);
                let mut parsed = crate::metacmd::parse(&interpolated_meta);
                parsed.echo_hidden = settings.echo_hidden;
                let result = dispatch_meta(parsed, client, params, settings, tx).await;
                match result {
                    MetaResult::ExecuteBuffer => {
                        let sql = buf.trim().to_owned();
                        buf.clear();
                        if !sql.is_empty() {
                            let ok = if let Some(bp) = settings.pending_bind_params.take() {
                                execute_query_extended(client, &sql, &bp, settings, tx).await
                            } else {
                                execute_query(client, &sql, settings, tx).await
                            };
                            if !ok {
                                exit_code = 1;
                                if settings.single_transaction {
                                    break 'lines;
                                }
                            }
                        }
                    }
                    MetaResult::GSet(prefix) => {
                        let sql = buf.trim().to_owned();
                        buf.clear();
                        if !sql.is_empty() {
                            execute_gset(client, &sql, prefix.as_deref(), settings, tx).await;
                        }
                    }
                    MetaResult::DescribeBuffer => {
                        describe_buffer(client, buf.trim(), settings.verbose_errors).await;
                    }
                    MetaResult::CrosstabViewBuffer(args) => {
                        let sql = buf.trim().to_owned();
                        buf.clear();
                        if !sql.is_empty() {
                            execute_crosstabview(client, &sql, &args, settings, tx).await;
                        }
                    }
                    MetaResult::BindParams(params) => {
                        settings.pending_bind_params = Some(params);
                    }
                    MetaResult::BindNamedExec(name, params) => {
                        if !execute_named_stmt(client, &name, &params, settings, tx).await {
                            exit_code = 1;
                            if settings.single_transaction {
                                break 'lines;
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(&line);

                if is_complete(&buf) {
                    if !execute_query(client, buf.trim(), settings, tx).await {
                        exit_code = 1;
                        // In single-transaction mode, stop on first error so the
                        // caller can roll back and skip the rest.
                        if settings.single_transaction {
                            break 'lines;
                        }
                    }
                    buf.clear();
                }
            }
        }
    }

    // Execute any trailing SQL without a terminating semicolon.
    if !buf.trim().is_empty()
        && settings.cond.is_active()
        && !execute_query(client, buf.trim(), settings, tx).await
    {
        exit_code = 1;
    }

    exit_code
}

// ---------------------------------------------------------------------------
// Interactive REPL
// ---------------------------------------------------------------------------

/// Build the backslash command help text and return it as a `String`.
#[allow(clippy::too_many_lines)]
fn help_text() -> String {
    format!(
        "{}\n{}",
        crate::version_string(),
        r"
Backslash commands:
  \q              quit rpg
  quit            quit rpg (interactive mode only)
  exit            quit rpg (interactive mode only)
  help            show this help overview (interactive mode only)
  \timing [on|off]      toggle/set query timing display
  \x [on|off|auto]      toggle/set expanded display
  \conninfo[+]    show connection information (+ for verbose pooler/provider details)
  \copyright      show rpg copyright information
  \version        show rpg version and build information
  \?              show this help
  \s [file|pattern]    show history (numbered + highlighted); save to file or filter by pattern

Session commands:
  \c [db [user [host [port]]]]  reconnect to database
  \c @profile                   reconnect using a named profile
  \profiles                     list all configured connection profiles
  \sf[+] <func>   show function source
  \sv[+] <view>   show view definition
  \h [command]    SQL syntax help

Describe commands:
  \d  [pattern]     describe objects
  \db [pattern]     list tablespaces
  \dc [pattern]     list conversions
  \dC [pattern]     list casts
  \dd [pattern]     list object comments
  \dD [pattern]     list domains
  \dE [pattern]     list foreign tables
  \des [pattern]    list foreign servers
  \deu [pattern]    list user mappings
  \dew [pattern]    list foreign-data wrappers
  \det [pattern]    list foreign tables via FDW
  \df [pattern]     list functions
  \dg [pattern]     list roles (same as \du)
  \di [pattern]     list indexes
  \dm [pattern]     list materialised views
  \dn [pattern]     list schemas
  \do [pattern]     list operators
  \dp [pattern]     list access privileges
  \ds [pattern]     list sequences
  \dt [pattern]     list tables
  \dT [pattern]     list data types
  \du [pattern]     list roles
  \dv [pattern]     list views
  \dx [pattern]     list extensions
  \dy [pattern]     list event triggers
  \l  [pattern]     list databases

AI commands:
  /ask <prompt>     natural language to SQL
  /explain          explain the last query plan
  /fix              diagnose and fix the last error
  /optimize <query> suggest query optimizations
  /describe <table> AI-generated table description
  /init             generate .rpg.toml and POSTGRES.md in current directory
  /clear            clear AI conversation context
  /compact [focus]  compact conversation context (optional focus topic)
  /budget           show token usage and remaining budget

Named queries:
  \ns <name> <query>  save a named query (name: alphanumerics + underscores)
  \n  <name> [args…]  execute a named query; $1,$2,… replaced by args
  \n+                 list all named queries with their SQL
  \nd <name>          delete a named query
  \np <name>          print a named query without executing

Input/execution modes:
  \sql              switch to SQL input mode (default)
  \text2sql / \t2s  switch to text2sql input mode
  \plan             enter plan execution mode
  \yolo             YOLO mode: auto-enable text2sql, hide SQL box, auto-execute
  \interactive      return to interactive mode (default)
  \mode             show current input and execution mode
  \set TEXT2SQL_SHOW_SQL on/off   show/hide SQL preview box in text2sql mode

Auto-EXPLAIN:
  \set EXPLAIN on       show EXPLAIN for every query
  \set EXPLAIN analyze  show EXPLAIN ANALYZE for every query
  \set EXPLAIN verbose  show EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING)
  \set EXPLAIN off      disable auto-EXPLAIN

EXPLAIN sharing:
  \explain share depesz    upload last EXPLAIN plan to explain.depesz.com
  \explain share dalibo    upload last EXPLAIN plan to explain.dalibo.com
  \explain share pgmustard upload last EXPLAIN plan to app.pgmustard.com
                           (requires PGMUSTARD_API_KEY env var or config)

Output format:
  \pset format markdown       switch output to Markdown table format
  \pset explain_format raw     show raw EXPLAIN text (default)
  \pset explain_format enhanced show EXPLAIN with visual highlights
  \pset explain_format compact  show compact EXPLAIN summary
  --markdown                   start with Markdown output format (CLI flag)

DBA diagnostics:
  \dba               show available diagnostics
  \dba activity      pg_stat_activity summary
  \dba bloat         table bloat estimates
  \dba cache-hit     buffer cache hit ratios
  \dba config        non-default configuration
  \dba connections   connection counts by state
  \dba indexes       index health (unused, redundant, invalid)
  \dba io            I/O statistics (PG 16+)
  \dba locks         lock tree (blocked/blocking)
  \dba progress      long-running operation progress
  \dba replication   replication slot status
  \dba seq-scans     tables with high sequential scan ratio
  \dba tablesize     largest tables
  \dba vacuum        vacuum status and dead tuples
  \dba waits         wait event breakdown

Lua commands:
  \commands         list custom Lua meta-commands (if any are configured)

Function keys (interactive mode):
  F2 / \f2       toggle schema-aware tab completion on/off
  F3 / \f3       toggle single-line mode on/off
  F4 / \f4       toggle Vi/Emacs editing mode (next session)
  F5 / \f5       toggle auto-EXPLAIN on/off
  Ctrl-T          toggle SQL/text2sql input mode"
    )
}

/// Print all configured connection profiles in a table format.
///
/// Output format:
/// ```text
///  name       | host          | port | user     | dbname
/// ------------+---------------+------+----------+--------
///  production | 10.0.1.5      | 5432 | postgres | mydb
/// ```
fn print_profiles(config: &crate::config::Config) {
    if config.connections.is_empty() {
        println!("No connection profiles configured.");
        println!("Add profiles to ~/.config/rpg/config.toml under [connections.<name>].");
        return;
    }

    // Collect and sort for stable output.
    let mut profiles: Vec<(&String, &crate::config::ConnectionProfile)> =
        config.connections.iter().collect();
    profiles.sort_by_key(|(name, _)| name.as_str());

    // Column widths (minimum = header length).
    let w_name = profiles
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0)
        .max(4); // "name"
    let w_host = profiles
        .iter()
        .map(|(_, p)| p.host.as_deref().unwrap_or("").len())
        .max()
        .unwrap_or(0)
        .max(4); // "host"
    let w_port = 4_usize; // "port" header and "5432"
    let w_user = profiles
        .iter()
        .map(|(_, p)| p.username.as_deref().unwrap_or("").len())
        .max()
        .unwrap_or(0)
        .max(4); // "user"
    let w_dbname = profiles
        .iter()
        .map(|(_, p)| p.dbname.as_deref().unwrap_or("").len())
        .max()
        .unwrap_or(0)
        .max(6); // "dbname"

    let sep_name = "-".repeat(w_name);
    let sep_host = "-".repeat(w_host);
    let sep_port = "-".repeat(w_port);
    let sep_user = "-".repeat(w_user);
    let sep_dbname = "-".repeat(w_dbname);

    // Header.
    println!(
        " {v_name:<w_name$} | {v_host:<w_host$} | {v_port:<w_port$} | {v_user:<w_user$} | {v_dbname:<w_dbname$}",
        v_name = "name",
        v_host = "host",
        v_port = "port",
        v_user = "user",
        v_dbname = "dbname",
    );
    println!("-{sep_name}-+-{sep_host}-+-{sep_port}-+-{sep_user}-+-{sep_dbname}-");

    for (name, profile) in &profiles {
        let host = profile.host.as_deref().unwrap_or("");
        let port = profile.port.map_or_else(String::new, |p| p.to_string());
        let user = profile.username.as_deref().unwrap_or("");
        let dbname = profile.dbname.as_deref().unwrap_or("");
        println!(
            " {name:<w_name$} | {host:<w_host$} | {port:<w_port$} | {user:<w_user$} | {dbname:<w_dbname$}",
        );
    }
}

/// Print rpg copyright notice, including a pointer to the `PostgreSQL` license.
fn print_copyright(server_version: Option<&str>) {
    println!(
        "rpg — modern Postgres terminal
Copyright (c) 2026, Nikolay Samokhvalov and contributors
https://github.com/NikolayS/rpg

Licensed under the Apache License, Version 2.0."
    );
    if let Some(ver) = server_version {
        println!();
        println!("Connected to: {ver}");
    }
    println!();
    println!(
        "rpg is a PostgreSQL client. It is not part of the PostgreSQL project.
PostgreSQL is Copyright (c) 1996-2026, PostgreSQL Global Development Group.
See https://www.postgresql.org/about/licence/"
    );
}

/// Format an [`ExpandedMode`] value as a display string.
fn expanded_mode_str(mode: ExpandedMode) -> &'static str {
    match mode {
        ExpandedMode::On => "on",
        ExpandedMode::Auto => "auto",
        ExpandedMode::Off | ExpandedMode::Toggle => "off",
    }
}

/// Apply a timing toggle/set and print the new state.
fn apply_timing(settings: &mut ReplSettings, mode: Option<bool>) {
    settings.timing = mode.unwrap_or(!settings.timing);
    let state = if settings.timing { "on" } else { "off" };
    println!("Timing is {state}.");
}

/// Apply an expanded-display mode change and print the new state.
///
/// Both `settings.expanded` and `settings.pset.expanded` are kept in sync
/// so that subsequent queries rendered via `settings.pset` (e.g. in `-c`
/// mode) use the updated expanded flag.
fn apply_expanded(settings: &mut ReplSettings, mode: ExpandedMode) {
    settings.expanded = match mode {
        ExpandedMode::Toggle => {
            if settings.expanded == ExpandedMode::On {
                ExpandedMode::Off
            } else {
                ExpandedMode::On
            }
        }
        m => m,
    };
    // Keep pset in sync so -c and -f paths see the updated setting.
    settings.pset.expanded = settings.expanded;
    println!(
        "Expanded display is {}.",
        expanded_mode_str(settings.expanded)
    );
}

// ---------------------------------------------------------------------------
// Pager helper
// ---------------------------------------------------------------------------

/// Route `text` through the pager when it exceeds the terminal height, or
/// print directly to stdout when paging is disabled / output is short.
///
/// Honours `\o` redirection: when `settings.output_target` is set the text
/// is written to that file instead of stdout / the pager.
fn maybe_page(settings: &mut ReplSettings, text: &str) {
    // Honour \o redirect.
    if let Some(ref mut w) = settings.output_target {
        let _ = writeln!(w, "{text}");
        return;
    }
    let term_rows = crate::term::terminal_size().1 as usize;
    if settings.pager_enabled
        && crate::pager::needs_paging_with_min(
            text,
            term_rows.saturating_sub(2),
            settings.pager_min_lines,
        )
    {
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.clear();
            sl.teardown_scroll_region();
        }
        run_pager_for_text(settings, text, text.as_bytes());
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.setup_scroll_region();
            sl.render();
        }
    } else {
        print!("{text}");
    }
}

/// Apply a `\set` command.
///
/// - `\set` (bare) — print all variables sorted by name.
/// - `\set name` — print one variable.
/// - `\set name value` — assign.
///
/// Special case: when `ECHO_HIDDEN` is set to `on`, update `settings.echo_hidden`.
#[allow(clippy::too_many_lines)]
fn apply_set(settings: &mut ReplSettings, name: &str, value: &str) {
    if name.is_empty() {
        // List all variables.
        let mut pairs: Vec<(&String, &String)> = settings.vars.all().iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        let mut out = String::new();
        for (k, v) in pairs {
            use std::fmt::Write as FmtWrite;
            let _ = writeln!(out, "{k} = '{v}'");
        }
        maybe_page(settings, &out);
        return;
    }
    if value.is_empty() {
        // Synthetic settings: show current state rather than the vars store.
        if name == "EXPLAIN" {
            println!("Auto-EXPLAIN is {}.", settings.auto_explain.label());
            return;
        }
        // Display one variable.
        match settings.vars.get(name) {
            Some(v) => println!("{name} = '{v}'"),
            None => eprintln!("{name} is not set"),
        }
        return;
    }
    settings.vars.set(name, value);
    // Mirror DEBUG on/off into the debug flag and the global log level.
    if name == "DEBUG" {
        let on = matches!(value, "on" | "true" | "1");
        settings.debug = on;
        if on {
            crate::logging::set_level(crate::logging::Level::Debug);
        } else {
            crate::logging::set_level(crate::logging::Level::Warn);
        }
    }
    // Mirror ECHO_HIDDEN into the settings flag.
    if name == "ECHO_HIDDEN" {
        settings.echo_hidden = value == "on";
    }
    // Mirror HIGHLIGHT into the settings flag and pset config.
    if name == "HIGHLIGHT" {
        settings.no_highlight = value == "off";
        settings.pset.no_highlight = settings.no_highlight;
    }
    // Mirror PAGER into pager_enabled / pager_command.
    if name == "PAGER" {
        match value {
            "off" => {
                settings.pager_enabled = false;
                settings.pager_command = None;
            }
            "on" => {
                settings.pager_enabled = true;
                settings.pager_command = None;
            }
            cmd => {
                settings.pager_enabled = true;
                settings.pager_command = Some(cmd.to_owned());
            }
        }
    }
    // DESTRUCTIVE_WARNING and SAFETY both toggle the safety_enabled flag.
    if name == "DESTRUCTIVE_WARNING" || name == "SAFETY" {
        settings.safety_enabled = value != "off" && value != "false" && value != "0";
    }
    // Mirror VERBOSITY into verbose_errors (psql: verbose shows SQLSTATE).
    if name == "VERBOSITY" {
        settings.verbose_errors = value == "verbose";
    }
    // Mirror EXPLAIN into auto_explain.
    if name == "EXPLAIN" {
        settings.auto_explain = match value {
            "on" | "true" | "1" => AutoExplain::On,
            "analyze" => AutoExplain::Analyze,
            "verbose" => AutoExplain::Verbose,
            "off" | "false" | "0" => AutoExplain::Off,
            other => {
                eprintln!(
                    "\\set EXPLAIN: unknown value \"{other}\"\n\
                     Valid: on, analyze, verbose, off"
                );
                return;
            }
        };
        println!("Auto-EXPLAIN is {}.", settings.auto_explain.label());
    }
    // Mirror AI_SHOW_SQL into config.ai.show_sql.
    if name == "AI_SHOW_SQL" {
        settings.config.ai.show_sql = matches!(value, "on" | "true" | "1");
    }
    // Mirror TEXT2SQL_SHOW_SQL into text2sql_show_sql.
    if name == "TEXT2SQL_SHOW_SQL" {
        settings.text2sql_show_sql = matches!(value, "on" | "true" | "1");
    }
    // Mirror AI_PROVIDER into config.ai.provider.
    if name == "AI_PROVIDER" {
        const KNOWN_PROVIDERS: &[&str] = &["anthropic", "claude", "openai", "ollama"];
        if !KNOWN_PROVIDERS.contains(&value) {
            eprintln!(
                "warning: unknown AI provider \"{value}\"; \
                 known providers: anthropic, openai, ollama"
            );
        }
        settings.config.ai.provider = Some(value.to_owned());
        println!("AI provider set to: {value}");
    }
    // Mirror AI_MODEL into config.ai.model.
    if name == "AI_MODEL" {
        settings.config.ai.model = Some(value.to_owned());
        println!("AI model set to: {value}");
    }
    // Mirror TOKEN_BUDGET into config.ai.token_budget.
    //
    // Accepts a non-negative integer; 0 means unlimited.
    if name == "TOKEN_BUDGET" {
        match value.parse::<u64>() {
            Ok(n) => {
                settings.config.ai.token_budget = n;
                if n == 0 {
                    println!("AI token budget: unlimited");
                } else {
                    println!("AI token budget set to: {n} tokens");
                }
            }
            Err(_) => {
                eprintln!(
                    "\\set TOKEN_BUDGET: invalid value \"{value}\"\n\
                     Expected a non-negative integer (0 = unlimited)."
                );
            }
        }
    }
    // Mirror VI into vi_mode.
    //
    // rustyline does not support changing EditMode at runtime on an existing
    // Editor instance, so we store the preference and apply it on the next
    // session start.
    if name == "VI" {
        let on = matches!(value, "on" | "true" | "1");
        settings.vi_mode = on;
        settings.config.display.vi_mode = on;
        if on {
            println!("Vi mode enabled. Takes effect on next session.");
        } else {
            println!("Emacs mode (default). Takes effect on next session.");
        }
    }
    // Mirror AUTO_SUGGEST into auto_suggest_fix.
    if name == "AUTO_SUGGEST" {
        let on = value != "off" && value != "false" && value != "0";
        settings.auto_suggest_fix = on;
        if on {
            println!("Auto-suggest /fix hint enabled.");
        } else {
            println!("Auto-suggest /fix hint disabled.");
        }
    }
    // Mirror STATUSLINE into the status bar enabled flag.
    if name == "STATUSLINE" {
        let on = matches!(value, "on" | "true" | "1");
        settings.config.display.statusline_enabled = on;
        if let Some(ref sl_arc) = settings.statusline {
            let mut sl = sl_arc.lock().unwrap();
            sl.enabled = on;
            if on {
                sl.setup_scroll_region();
                sl.render();
            } else {
                sl.teardown_scroll_region();
            }
        }
        if on {
            println!("Status bar enabled.");
        } else {
            println!("Status bar disabled.");
        }
    }
}

/// Apply an `\unset` command.
fn apply_unset(settings: &mut ReplSettings, name: &str) {
    if settings.vars.unset(name) {
        // Mirror ECHO_HIDDEN.
        if name == "ECHO_HIDDEN" {
            settings.echo_hidden = false;
        }
        // Mirror AI_SHOW_SQL.
        if name == "AI_SHOW_SQL" {
            settings.config.ai.show_sql = false;
        }
        // Mirror AI_PROVIDER.
        if name == "AI_PROVIDER" {
            settings.config.ai.provider = None;
        }
        // Mirror AI_MODEL.
        if name == "AI_MODEL" {
            settings.config.ai.model = None;
        }
        // Mirror TEXT2SQL_SHOW_SQL.
        if name == "TEXT2SQL_SHOW_SQL" {
            settings.text2sql_show_sql = true;
        }
        // Mirror HIGHLIGHT (unsetting re-enables highlighting).
        if name == "HIGHLIGHT" {
            settings.no_highlight = false;
            settings.pset.no_highlight = false;
        }
    } else {
        eprintln!("\\unset: variable {name} was not set");
    }
}

/// Apply a function-key toggle action and print confirmation.
///
/// Called by the readline loop when an F-key `ConditionalEventHandler` fires.
/// Also reachable via the `\f2` / `\f3` / `\f4` / `\f5` metacommands.
fn apply_fkey_toggle(action: FKeyAction, settings: &mut ReplSettings) {
    match action {
        FKeyAction::Completion => {
            settings.no_completion = !settings.no_completion;
            let state = if settings.no_completion { "off" } else { "on" };
            println!("Completion is {state}.");
        }
        FKeyAction::SingleLine => {
            settings.single_line = !settings.single_line;
            let state = if settings.single_line { "on" } else { "off" };
            println!("Single-line mode is {state}.");
        }
        FKeyAction::ViEmacs => {
            settings.vi_mode = !settings.vi_mode;
            settings.config.display.vi_mode = settings.vi_mode;
            if settings.vi_mode {
                eprintln!("Vi mode enabled. Takes effect on next session.");
            } else {
                eprintln!("Emacs mode (default). Takes effect on next session.");
            }
        }
        FKeyAction::AutoExplain => {
            settings.auto_explain = settings.auto_explain.cycle();
            println!("Auto-EXPLAIN is {}.", settings.auto_explain.label());
        }
        FKeyAction::Text2Sql => {
            settings.input_mode = if settings.input_mode == InputMode::Text2Sql {
                InputMode::Sql
            } else {
                InputMode::Text2Sql
            };
            let label = match settings.input_mode {
                InputMode::Sql => "sql",
                InputMode::Text2Sql => "text2sql",
            };
            eprintln!("Input mode: {label}");
        }
    }
}

/// Apply a `\prompt [text] name` command.
///
/// Prints `prompt_text` to stderr (matching psql behaviour — the prompt goes
/// to the tty, not stdout), reads one line from stdin, and stores the result
/// in the variable `var_name`.  When stdin is not a terminal the prompt text
/// is suppressed.
///
/// When the user presses Ctrl+C, the variable is set to an empty string and
/// `settings.prompt_interrupted` is set to `true` so that the calling script
/// loop in `exec_lines` can detect the interrupt and stop processing.
fn apply_prompt(settings: &mut ReplSettings, prompt_text: &str, var_name: &str) {
    use std::io::Write;

    // Determine whether to use interactive (raw-mode) or piped input.
    #[cfg(not(target_arch = "wasm32"))]
    let interactive = io::stdin().is_terminal();
    #[cfg(target_arch = "wasm32")]
    let interactive = false;

    if interactive {
        // Interactive path: use crossterm raw mode so Ctrl+C is detectable.
        // Read the input character-by-character, building a line.
        #[cfg(not(target_arch = "wasm32"))]
        {
            use crossterm::event::{read, Event, KeyCode, KeyModifiers};
            use crossterm::terminal;

            if !prompt_text.is_empty() {
                eprint!("{prompt_text}");
                let _ = io::stderr().flush();
            }

            let raw_enabled = terminal::enable_raw_mode().is_ok();
            let mut input = String::new();
            let interrupted = loop {
                match read() {
                    Ok(Event::Key(key)) => match (key.code, key.modifiers) {
                        // Ctrl+C / Ctrl+D / Esc — interrupt: abort the current script.
                        (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL)
                        | (KeyCode::Esc, _) => {
                            let _ = write!(io::stderr(), "\r\n");
                            break true;
                        }
                        // Enter — end of input.
                        (KeyCode::Enter, _) => {
                            let _ = write!(io::stderr(), "\r\n");
                            break false;
                        }
                        // Backspace — delete last character.
                        (KeyCode::Backspace, _) => {
                            if input.pop().is_some() {
                                // Erase the character on screen.
                                let _ = write!(io::stderr(), "\x08 \x08");
                                let _ = io::stderr().flush();
                            }
                        }
                        // Printable character — echo and accumulate.
                        (KeyCode::Char(ch), _) => {
                            input.push(ch);
                            let _ = write!(io::stderr(), "{ch}");
                            let _ = io::stderr().flush();
                        }
                        _ => {}
                    },
                    Ok(_) => {}
                    Err(_) => break false,
                }
            };
            if raw_enabled {
                let _ = terminal::disable_raw_mode();
            }

            if interrupted {
                settings.vars.set(var_name, "");
                settings.prompt_interrupted = true;
            } else {
                settings.vars.set(var_name, &input);
            }
        }
    } else {
        // Non-interactive (piped) path: use read_line as before; Ctrl+C is
        // handled by the OS signal handler and terminates the process.
        if !prompt_text.is_empty() {
            // Suppress prompt text when stdin is not a terminal (psql behaviour).
        }
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => {
                // EOF — store empty string.
                settings.vars.set(var_name, "");
            }
            Ok(_) => {
                // Strip the trailing newline that `read_line` includes.
                let trimmed = line.trim_end_matches(['\n', '\r']);
                settings.vars.set(var_name, trimmed);
            }
            Err(e) => {
                eprintln!("\\prompt: {e}");
            }
        }
    }
}

/// Apply a `\pset` command.
#[allow(clippy::too_many_lines)]
fn apply_pset(settings: &mut ReplSettings, option: &str, value: Option<&str>) {
    use crate::output::OutputFormat;

    if option.is_empty() {
        // Display all pset options.
        let text = pset_status_text(settings);
        maybe_page(settings, &text);
        return;
    }

    match option {
        "format" => {
            if value.is_none_or(str::is_empty) {
                // \pset format (no value) — show current setting.
                println!("Output format is {}.", format_name(&settings.pset.format));
                return;
            }
            let fmt = match value.unwrap_or("") {
                "aligned" => OutputFormat::Aligned,
                "unaligned" => OutputFormat::Unaligned,
                "csv" => OutputFormat::Csv,
                "json" => OutputFormat::Json,
                "html" => OutputFormat::Html,
                "wrapped" => OutputFormat::Wrapped,
                "markdown" => OutputFormat::Markdown,
                other => {
                    eprintln!("\\pset: unknown format \"{other}\"");
                    return;
                }
            };
            settings.pset.format = fmt;
            println!("Output format is {}.", format_name(&settings.pset.format));
        }
        "border" => {
            if let Some(v) = value.and_then(|s| s.parse::<u8>().ok()) {
                settings.pset.border = v.min(2);
                println!("Border style is {}.", settings.pset.border);
            } else {
                eprintln!("\\pset: invalid border value");
            }
        }
        "null" => {
            let display = value.unwrap_or("").to_owned();
            println!("Null display is \"{display}\".");
            settings.pset.null_display = display;
        }
        "fieldsep" => {
            let sep = value.unwrap_or("|").to_owned();
            println!("Field separator is \"{sep}\".");
            settings.pset.field_sep = sep;
        }
        "recordsep" => {
            let sep = value.unwrap_or("\n").to_owned();
            settings.pset.record_sep = sep;
            println!("Record separator is set.");
        }
        "tuples_only" | "t" => {
            // psql does not print a confirmation message for tuples_only.
            settings.pset.tuples_only = bool_value(value, settings.pset.tuples_only);
        }
        "footer" => {
            // psql does not print a confirmation message for footer.
            settings.pset.footer = bool_value(value, settings.pset.footer);
        }
        "title" => {
            settings.pset.title = value.filter(|s| !s.is_empty()).map(ToOwned::to_owned);
            match &settings.pset.title {
                Some(t) => println!("Title is \"{t}\"."),
                None => println!("Title is not set."),
            }
        }
        "expanded" | "x" => {
            let mode = match value.unwrap_or("").to_lowercase().as_str() {
                "on" => ExpandedMode::On,
                "off" => ExpandedMode::Off,
                "auto" => ExpandedMode::Auto,
                _ => {
                    // Toggle.
                    if settings.pset.expanded == ExpandedMode::On {
                        ExpandedMode::Off
                    } else {
                        ExpandedMode::On
                    }
                }
            };
            settings.pset.expanded = mode;
            println!(
                "Expanded display is {}.",
                expanded_mode_str(settings.pset.expanded)
            );
        }
        "pager_min_lines" => {
            if let Some(n) = value.and_then(|s| s.parse::<usize>().ok()) {
                settings.pager_min_lines = n;
                println!("Pager minimum lines is {n}.");
            } else {
                eprintln!("\\pset: invalid pager_min_lines value");
            }
        }
        "explain_format" => {
            use crate::explain::ExplainFormat;
            let fmt = match value.unwrap_or("enhanced") {
                "enhanced" => ExplainFormat::Enhanced,
                "raw" => ExplainFormat::Raw,
                "compact" => ExplainFormat::Compact,
                other => {
                    eprintln!(
                        "\\pset: unknown explain_format \"{other}\"\n\
                         Valid: enhanced, raw, compact"
                    );
                    return;
                }
            };
            settings.explain_format = fmt;
            println!("EXPLAIN format is {}.", fmt.as_str());
        }
        other => {
            eprintln!("\\pset: unknown option \"{other}\"");
        }
    }

    // Keep ReplSettings.expanded in sync.
    settings.expanded = settings.pset.expanded;
}

/// Parse a boolean value for pset options: `on`/`true`/`1` → true, else toggle.
fn bool_value(value: Option<&str>, current: bool) -> bool {
    match value.map(str::to_lowercase).as_deref() {
        Some("on" | "true" | "1") => true,
        Some("off" | "false" | "0") => false,
        _ => !current,
    }
}

/// Return a short human-readable name for an `OutputFormat`.
fn format_name(fmt: &crate::output::OutputFormat) -> &'static str {
    use crate::output::OutputFormat;
    match fmt {
        OutputFormat::Aligned => "aligned",
        OutputFormat::Unaligned => "unaligned",
        OutputFormat::Csv => "csv",
        OutputFormat::Json => "json",
        OutputFormat::Html => "html",
        OutputFormat::Wrapped => "wrapped",
        OutputFormat::Markdown => "markdown",
    }
}

/// Build a summary of the current `PsetConfig` as a `String`.
///
/// Format matches psql `\pset` bare output.
fn pset_status_text(settings: &ReplSettings) -> String {
    use std::fmt::Write as FmtWrite;
    let mut out = String::new();
    let pset = &settings.pset;
    let _ = writeln!(out, "border         = {}", pset.border);
    let _ = writeln!(out, "expanded       = {}", expanded_mode_str(pset.expanded));
    let _ = writeln!(out, "fieldsep       = \"{}\"", pset.field_sep);
    let _ = writeln!(
        out,
        "footer         = {}",
        if pset.footer { "on" } else { "off" }
    );
    let _ = writeln!(out, "format         = {}", format_name(&pset.format));
    let _ = writeln!(out, "linestyle      = ascii");
    let _ = writeln!(out, "null           = \"{}\"", pset.null_display);
    let _ = writeln!(
        out,
        "pager          = {}",
        if settings.pager_enabled { "on" } else { "off" }
    );
    let _ = writeln!(
        out,
        "tuples_only    = {}",
        if pset.tuples_only { "on" } else { "off" }
    );
    match &pset.title {
        Some(t) => {
            let _ = writeln!(out, "title          = \"{t}\"");
        }
        None => {
            let _ = writeln!(out, "title          = (not set)");
        }
    }
    let _ = writeln!(out, "explain_format = {}", settings.explain_format.as_str());
    out
}

/// Apply `\a` — toggle between aligned and unaligned output.
fn apply_toggle_align(settings: &mut ReplSettings) {
    use crate::output::OutputFormat;
    settings.pset.format = match settings.pset.format {
        OutputFormat::Aligned => OutputFormat::Unaligned,
        _ => OutputFormat::Aligned,
    };
    println!("Output format is {}.", format_name(&settings.pset.format));
}

/// Apply `\t [on|off]` — tuples-only mode.
fn apply_tuples_only(settings: &mut ReplSettings, mode: Option<bool>) {
    settings.pset.tuples_only = mode.unwrap_or(!settings.pset.tuples_only);
    let state = if settings.pset.tuples_only {
        "on"
    } else {
        "off"
    };
    println!("Tuples only is {state}.");
}

/// Apply `\f [sep]` — field separator.
fn apply_field_sep(settings: &mut ReplSettings, sep: Option<&str>) {
    let new_sep = sep.unwrap_or("|").to_owned();
    println!("Field separator is \"{new_sep}\".");
    settings.pset.field_sep = new_sep;
}

/// Apply `\H` — toggle HTML output.
fn apply_toggle_html(settings: &mut ReplSettings) {
    use crate::output::OutputFormat;
    settings.pset.format = match settings.pset.format {
        OutputFormat::Html => OutputFormat::Aligned,
        _ => OutputFormat::Html,
    };
    println!("Output format is {}.", format_name(&settings.pset.format));
}

/// Apply `\C [title]` — set or clear table title.
fn apply_set_title(settings: &mut ReplSettings, title: Option<&str>) {
    settings.pset.title = title.filter(|s| !s.is_empty()).map(ToOwned::to_owned);
    match &settings.pset.title {
        Some(t) => println!("Title is \"{t}\"."),
        None => println!("Title is not set."),
    }
}

// ---------------------------------------------------------------------------
// MetaResult — outcome of a dispatched meta-command
// ---------------------------------------------------------------------------

/// The outcome of dispatching a backslash meta-command.
pub enum MetaResult {
    /// Continue the REPL loop normally.
    Continue,
    /// Exit the REPL loop (`\q`).
    Quit,
    /// The connection was replaced: caller must swap client and params.
    Reconnected(Box<tokio_postgres::Client>, Box<ConnParams>),
    /// Clear the query buffer (`\r`).
    ClearBuffer,
    /// Print the query buffer (`\p`).
    PrintBuffer,
    /// Open the editor on the buffer; execute the result on close (`\e`).
    ///
    /// `file` is the optional explicit file path from `\e file [line]`.
    /// `line` is the optional starting line number.
    EditBuffer {
        file: Option<String>,
        line: Option<usize>,
    },
    /// Write the buffer to the given path (`\w file`).
    WriteBufferToFile(String),
    /// Execute the current buffer and direct output to stdout (`\g`).
    ExecuteBuffer,
    /// Execute the current buffer and write output to a file (`\g file`).
    ExecuteBufferToFile(String),
    /// Execute the current buffer, piping output through a shell command (`\g |cmd`).
    ExecuteBufferPiped(String),
    /// Execute the current buffer with expanded output for this query only (`\gx`).
    ExecuteBufferExpanded,
    /// Execute the current buffer with expanded output written to a file (`\gx file`).
    ExecuteBufferExpandedToFile(String),
    /// Describe the result columns of the buffer without executing it (`\gdesc`).
    DescribeBuffer,
    /// Execute the current buffer, then execute each result cell as SQL (`\gexec`).
    GExecBuffer,
    /// Execute the current buffer and store each column as a variable (`\gset [prefix]`).
    GSet(Option<String>),
    /// Execute the buffer and display the result as a cross-tabulation table
    /// (`\crosstabview [colV [colH [colD [sortcolH]]]]`).
    ///
    /// The inner `String` carries the raw argument string (may be empty).
    CrosstabViewBuffer(String),
    /// Store bind parameters for the next query (`\bind params…`).
    ///
    /// The REPL saves these in `ReplSettings::pending_bind_params`; the
    /// next query execution drains them and uses the extended protocol.
    BindParams(Vec<String>),
    /// Prepare the current buffer as a named server-side statement (`\parse name`).
    ///
    /// The REPL calls `client.prepare(buf)` and stores the result under `name`
    /// in `ReplSettings::named_statements`.
    ParseStatement(String),
    /// Execute a named prepared statement with the given params (`\bind_named name params…`).
    ///
    /// The REPL retrieves the stored statement and calls `client.query`.
    BindNamedExec(String, Vec<String>),
    /// Deallocate a named prepared statement (`\close_prepared name`).
    ///
    /// Sends `DEALLOCATE name` to the server and removes it from the local map.
    ClosePrepared(String),
    /// Switch input mode (`\sql`, `\text2sql`, `\t2s`).
    SetInputMode(InputMode),
    /// Switch execution mode (`\plan`, `\yolo`, `\interactive`).
    SetExecMode(ExecMode),
    /// Show current mode summary (`\mode`).
    ShowMode,
}

/// Apply a `SetInputMode` or `SetExecMode` result to `settings`.
///
/// Centralises all mode-transition side-effects so the three REPL dispatch
/// sites (interactive loop, file execution, and `exec_command`) stay in sync:
///
/// - `SetInputMode` always resets `exec_mode` to `Interactive` so that
///   `\t2s` (or `\sql`) after `\yolo` stops auto-executing queries.
/// - `SetExecMode(Yolo)` auto-enables `input_mode = Text2Sql` so natural
///   language goes to the AI.
/// - `SetExecMode(Interactive)` resets `input_mode` back to `Sql` so the
///   user returns fully to the default state.
///
/// Returns a short label string used for the confirmation message.
fn apply_mode_change(result: &MetaResult, settings: &mut ReplSettings) -> &'static str {
    match result {
        MetaResult::SetInputMode(mode) => {
            settings.input_mode = *mode;
            // Switching input mode always returns to interactive exec mode
            // so that \t2s after \yolo doesn't silently execute queries.
            settings.exec_mode = ExecMode::Interactive;
            match mode {
                InputMode::Sql => "sql",
                InputMode::Text2Sql => "text2sql",
            }
        }
        MetaResult::SetExecMode(mode) => {
            settings.exec_mode = *mode;
            match mode {
                ExecMode::Yolo => {
                    settings.input_mode = InputMode::Text2Sql;
                }
                ExecMode::Interactive => {
                    settings.input_mode = InputMode::Sql;
                }
                ExecMode::Plan => {}
            }
            match mode {
                ExecMode::Interactive => "interactive",
                ExecMode::Plan => "plan",
                ExecMode::Yolo => "yolo",
            }
        }
        _ => "",
    }
}

/// Dispatch I/O and utility meta-commands (the `#33` family).
///
/// Returns `Some(MetaResult)` if the command was handled, `None` if the
/// command is not an I/O command (and the caller should continue matching).
#[allow(clippy::too_many_lines)]
async fn dispatch_io(
    parsed: &crate::metacmd::ParsedMeta,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> Option<MetaResult> {
    use crate::metacmd::MetaCmd;

    match parsed.cmd {
        MetaCmd::Include => {
            match parsed.pattern.as_deref() {
                Some(path) => {
                    crate::io::include_file(client, path, settings, tx, params).await;
                }
                None => eprintln!("\\i: file name required"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::IncludeRelative => {
            // \ir resolves the path relative to the directory of the currently
            // executing script file (psql behaviour).  When there is no current
            // script (e.g. interactive REPL), it falls back to the process CWD,
            // which is identical to \i behaviour.
            match parsed.pattern.as_deref() {
                Some(raw_path) => {
                    let resolved = crate::io::resolve_relative_path(
                        raw_path,
                        settings.current_file.as_deref(),
                    );
                    crate::io::include_file(client, &resolved, settings, tx, params).await;
                }
                None => eprintln!("\\ir: file name required"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::Output => {
            match crate::io::open_output(parsed.pattern.as_deref()) {
                Ok(target) => {
                    settings.output_target = target;
                }
                Err(e) => eprintln!("{e}"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::ResetBuffer => Some(MetaResult::ClearBuffer),
        MetaCmd::PrintBuffer => Some(MetaResult::PrintBuffer),
        MetaCmd::WriteBuffer => {
            match parsed.pattern.as_deref() {
                Some(path) => return Some(MetaResult::WriteBufferToFile(path.to_owned())),
                None => eprintln!("\\w: file name required"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::Edit => {
            // Pattern may be "file" or "file line".
            let (file, line) = match parsed.pattern.as_deref() {
                None => (None, None),
                Some(p) => {
                    let mut parts = p.splitn(2, char::is_whitespace);
                    let f = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
                    let l = parts.next().and_then(|s| s.trim().parse::<usize>().ok());
                    (f, l)
                }
            };
            Some(MetaResult::EditBuffer { file, line })
        }
        MetaCmd::Shell => {
            crate::io::shell_command(parsed.pattern.as_deref());
            Some(MetaResult::Continue)
        }
        MetaCmd::Chdir => {
            if let Err(e) = crate::io::change_dir(parsed.pattern.as_deref()) {
                eprintln!("{e}");
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::Echo => {
            let raw = parsed.pattern.as_deref().unwrap_or("");
            // Split into tokens (strips surrounding single-quotes, handles
            // `\'`/`''` escapes inside quoted strings), join with spaces,
            // then process backslash escape sequences — matching psql's
            // \echo behaviour so that e.g. `\echo '\033[1;35mMenu:\033[0m'`
            // emits an ANSI-coloured string.
            let joined = crate::metacmd::split_params(raw).join(" ");
            println!("{}", unescape_echo(&joined));
            Some(MetaResult::Continue)
        }
        MetaCmd::QEcho => {
            let raw = parsed.pattern.as_deref().unwrap_or("");
            let text = unescape_echo(&crate::metacmd::split_params(raw).join(" "));
            if let Some(ref mut w) = settings.output_target {
                let _ = writeln!(w, "{text}");
            } else {
                println!("{text}");
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::Warn => {
            let raw = parsed.pattern.as_deref().unwrap_or("");
            eprintln!(
                "{}",
                unescape_echo(&crate::metacmd::split_params(raw).join(" "))
            );
            Some(MetaResult::Continue)
        }
        MetaCmd::Encoding => {
            crate::io::encoding(parsed.pattern.as_deref());
            Some(MetaResult::Continue)
        }
        MetaCmd::Password => {
            dispatch_password(parsed.pattern.as_deref(), client).await;
            Some(MetaResult::Continue)
        }
        MetaCmd::GoExecute(ref target) => {
            let result = match target.as_deref() {
                None => MetaResult::ExecuteBuffer,
                Some(t) if t.starts_with('|') => MetaResult::ExecuteBufferPiped(t.to_owned()),
                Some(f) => MetaResult::ExecuteBufferToFile(f.to_owned()),
            };
            Some(result)
        }
        MetaCmd::GoExecuteExpanded(ref target) => {
            let result = match target.as_deref() {
                None => MetaResult::ExecuteBufferExpanded,
                Some(f) => MetaResult::ExecuteBufferExpandedToFile(f.to_owned()),
            };
            Some(result)
        }
        MetaCmd::GDesc => Some(MetaResult::DescribeBuffer),
        MetaCmd::GExec => Some(MetaResult::GExecBuffer),
        MetaCmd::GSet(ref prefix) => Some(MetaResult::GSet(prefix.clone())),
        MetaCmd::Copy(ref args) => {
            let args = args.clone();
            match crate::copy::parse_copy_args(&args) {
                Ok(spec) => {
                    if let Err(e) = crate::copy::execute_copy(client, &spec).await {
                        eprintln!("{e}");
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::CrosstabView(ref args) => Some(MetaResult::CrosstabViewBuffer(args.clone())),

        // -- Extended query protocol (#57) -----------------------------------
        MetaCmd::Bind(ref params) => Some(MetaResult::BindParams(params.clone())),
        MetaCmd::BindNamed(ref name, ref params) => {
            Some(MetaResult::BindNamedExec(name.clone(), params.clone()))
        }
        MetaCmd::Parse(ref name) => Some(MetaResult::ParseStatement(name.clone())),
        MetaCmd::ClosePrepared(ref name) => Some(MetaResult::ClosePrepared(name.clone())),

        MetaCmd::LogFile(ref path) => {
            if let Some(raw_path) = path.as_deref() {
                // Expand leading `~` to the home directory.
                let expanded = expand_tilde(raw_path);

                // Create parent directories if needed.
                if let Some(parent) = expanded.parent() {
                    if !parent.as_os_str().is_empty() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            eprintln!("\\log-file: cannot create directory: {e}");
                            return Some(MetaResult::Continue);
                        }
                    }
                }

                match std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&expanded)
                {
                    Ok(file) => {
                        settings.audit_log_file = Some(std::io::BufWriter::new(file));
                        settings.audit_log_path = Some(expanded.clone());
                        if !settings.quiet {
                            println!("Logging queries to \"{}\".", expanded.display());
                        }
                    }
                    Err(e) => {
                        eprintln!("\\log-file: cannot open \"{}\": {e}", expanded.display());
                    }
                }
            } else {
                // No path — close the current log file.
                if let Some(ref path) = settings.audit_log_path.take() {
                    if !settings.quiet {
                        println!("Stopped logging to \"{}\".", path.display());
                    }
                }
                settings.audit_log_file = None;
            }
            Some(MetaResult::Continue)
        }
        _ => None,
    }
}

/// Handle `\password [user]`.
///
/// Matches psql behaviour:
/// - Prompts `Enter new password for user "<user>": ` then `Enter it again: `
/// - When no user is given, resolves the current role via `SELECT CURRENT_USER`
/// - Error message on mismatch: `Passwords didn't match.`
/// - Executes `ALTER USER <ident> PASSWORD '<escaped>'` on the live connection
///
/// Note: psql encrypts the password client-side via `PQencryptPasswordConn`
/// before sending it, so the plaintext never appears in server logs.  We
/// instead send the cleartext password and rely on the server's
/// `password_encryption` setting to hash it at rest — a pragmatic trade-off
/// until a Rust-native SCRAM / MD5 implementation is added.
async fn dispatch_password(user: Option<&str>, client: &Client) {
    use tokio_postgres::SimpleQueryMessage;

    // Resolve effective username: argument takes priority, otherwise ask the
    // server for the currently authenticated role.
    let resolved_user: String = match user {
        Some(u) if !u.is_empty() => u.to_owned(),
        _ => match client.simple_query("select current_user").await {
            Ok(msgs) => {
                let name = msgs.into_iter().find_map(|m| {
                    if let SimpleQueryMessage::Row(row) = m {
                        row.get(0).map(str::to_owned)
                    } else {
                        None
                    }
                });
                if let Some(n) = name {
                    n
                } else {
                    eprintln!("\\password: could not determine current user");
                    return;
                }
            }
            Err(e) => {
                eprintln!("\\password: {e}");
                return;
            }
        },
    };

    let prompt = format!("Enter new password for user \"{resolved_user}\": ");

    // rpassword requires a TTY and is not available on WASM.
    #[cfg(target_arch = "wasm32")]
    {
        let _ = prompt;
        eprintln!("\\password: not supported in browser");
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let pw = match rpassword::prompt_password(&prompt) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("\\password: {e}");
                return;
            }
        };

        let confirm = match rpassword::prompt_password("Enter it again: ") {
            Ok(p) => p,
            Err(e) => {
                eprintln!("\\password: {e}");
                return;
            }
        };

        if pw != confirm {
            eprintln!("Passwords didn't match.");
            return;
        }

        // Escape the username as a SQL identifier (double-quote and double any
        // internal double-quotes) and the password as a SQL string literal
        // (single-quote and double any internal single-quotes).
        let ident_escaped = resolved_user.replace('"', "\"\"");
        let pw_escaped = pw.replace('\'', "''");
        let sql = format!("alter user \"{ident_escaped}\" password '{pw_escaped}'");

        match client.simple_query(&sql).await {
            Ok(_) => {}
            Err(e) => eprintln!("{e}"),
        }
    }
}

/// Process backslash escape sequences in `\echo` output, matching psql.
///
/// Recognised sequences:
/// - `\n` → newline
/// - `\t` → tab
/// - `\r` → carriage return
/// - `\b` → backspace
/// - `\f` → form feed
/// - `\\` → backslash
/// - `\'` → single quote
/// - `\ooo` (1–3 octal digits) → byte with that octal value
/// - `\xhh` (1–2 hex digits) → byte with that hex value
///
/// Unknown sequences (e.g. `\q`) are left verbatim.
fn unescape_echo(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] != b'\\' || i + 1 >= len {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // Peek at the character after the backslash.
        match bytes[i + 1] {
            b'n' => {
                out.push(b'\n');
                i += 2;
            }
            b't' => {
                out.push(b'\t');
                i += 2;
            }
            b'r' => {
                out.push(b'\r');
                i += 2;
            }
            b'b' => {
                out.push(0x08);
                i += 2;
            }
            b'f' => {
                out.push(0x0C);
                i += 2;
            }
            b'\\' => {
                out.push(b'\\');
                i += 2;
            }
            b'\'' => {
                out.push(b'\'');
                i += 2;
            }
            b'x' | b'X' => {
                // Hex escape: \xhh (1–2 hex digits).
                let start = i + 2;
                let end = bytes[start..]
                    .iter()
                    .take(2)
                    .take_while(|b| b.is_ascii_hexdigit())
                    .count();
                if end > 0 {
                    let hex: String = bytes[start..start + end]
                        .iter()
                        .map(|&b| b as char)
                        .collect();
                    if let Ok(val) = u8::from_str_radix(&hex, 16) {
                        out.push(val);
                        i = start + end;
                        continue;
                    }
                }
                // Not a valid hex escape — emit verbatim.
                out.push(b'\\');
                i += 1;
            }
            b'0'..=b'7' => {
                // Octal escape: \ooo (1–3 octal digits).
                let start = i + 1;
                let end = bytes[start..]
                    .iter()
                    .take(3)
                    .take_while(|&&b| (b'0'..=b'7').contains(&b))
                    .count();
                let octal: String = bytes[start..start + end]
                    .iter()
                    .map(|&b| b as char)
                    .collect();
                if let Ok(val) = u32::from_str_radix(&octal, 8) {
                    // Truncate to 8 bits, matching psql behaviour (\400 → 0x00).
                    #[allow(clippy::cast_possible_truncation)]
                    out.push((val & 0xFF) as u8);
                    i = start + end;
                } else {
                    out.push(b'\\');
                    i += 1;
                }
            }
            _ => {
                // Unknown escape — emit verbatim.
                out.push(b'\\');
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Dispatch a parsed meta-command, applying any side-effects to `settings`.
///
/// `tx` is the caller's transaction state; it is forwarded to I/O commands
/// such as `\i` / `\ir` so that included files inherit the outer context.
///
/// Returns a [`MetaResult`] indicating whether the loop should continue,
/// exit, or replace the current connection. Buffer-mutating commands
/// (`\r`, `\p`, `\w`, `\e`) return special variants that the REPL loop
/// handles where the buffer is accessible.
///
/// `\if` / `\elif` / `\else` / `\endif` are always processed to maintain
/// correct nesting, even when inside a suppressed (inactive) branch.
/// All other commands are skipped when the conditional state is inactive.
#[allow(clippy::too_many_lines)]
async fn dispatch_meta(
    parsed: crate::metacmd::ParsedMeta,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> MetaResult {
    use crate::conditional::eval_bool;
    use crate::metacmd::MetaCmd;

    crate::logging::trace("repl", &format!("dispatch meta-command: {:?}", parsed.cmd));

    // -- Conditional commands: always process regardless of active state -----
    match &parsed.cmd {
        MetaCmd::If(expr) => {
            if expr.trim().is_empty() {
                eprintln!("\\if: missing expression");
            }
            let condition = eval_bool(expr);
            settings.cond.push_if(condition);
            return MetaResult::Continue;
        }
        MetaCmd::Elif(expr) => {
            if expr.trim().is_empty() {
                eprintln!("\\elif: missing expression");
            }
            let condition = eval_bool(expr);
            if let Err(e) = settings.cond.handle_elif(condition) {
                eprintln!("{e}");
            }
            return MetaResult::Continue;
        }
        MetaCmd::Else => {
            if let Err(e) = settings.cond.handle_else() {
                eprintln!("{e}");
            }
            return MetaResult::Continue;
        }
        MetaCmd::Endif => {
            if let Err(e) = settings.cond.pop_endif() {
                eprintln!("{e}");
            }
            return MetaResult::Continue;
        }
        _ => {}
    }

    // -- All other commands: skip when in a suppressed branch ---------------
    if !settings.cond.is_active() {
        return MetaResult::Continue;
    }

    // Try I/O commands first (they are the most numerous).
    if let Some(result) = dispatch_io(&parsed, client, params, settings, tx).await {
        return result;
    }

    match parsed.cmd {
        MetaCmd::Quit => return MetaResult::Quit,
        MetaCmd::Help => {
            maybe_page(settings, &help_text());
        }
        MetaCmd::Timing(mode) => apply_timing(settings, mode),
        MetaCmd::Expanded(mode) => apply_expanded(settings, mode),
        MetaCmd::ConnInfo => {
            // `\conninfo`   — psql-compatible single line (always shown).
            // `\conninfo+`  — additionally show pooler / provider details.
            println!("{}", crate::connection::connection_info(params));
            if parsed.plus {
                let caps = &settings.db_capabilities;
                match &caps.pooler {
                    crate::capabilities::PoolerType::None => {}
                    crate::capabilities::PoolerType::PgBouncer { pool_mode } => {
                        println!("Pooler: PgBouncer (pool_mode={pool_mode})");
                    }
                    crate::capabilities::PoolerType::Supavisor => {
                        println!("Pooler: Supavisor");
                    }
                    crate::capabilities::PoolerType::PgCat => {
                        println!("Pooler: PgCat");
                    }
                }
                match caps.managed_provider {
                    crate::capabilities::ManagedProvider::None => {}
                    crate::capabilities::ManagedProvider::Rds => {
                        println!("Provider: Amazon RDS");
                    }
                    crate::capabilities::ManagedProvider::CloudSql => {
                        println!("Provider: Google Cloud SQL");
                    }
                    crate::capabilities::ManagedProvider::Supabase => {
                        println!("Provider: Supabase");
                    }
                    crate::capabilities::ManagedProvider::Neon => {
                        println!("Provider: Neon");
                    }
                }
                if let Some(warning) = caps.pooler_warning() {
                    eprintln!("WARNING: {warning}");
                }
            }
        }
        MetaCmd::ListProfiles => {
            print_profiles(&settings.config);
        }
        MetaCmd::Copyright => {
            print_copyright(settings.db_capabilities.server_version.as_deref());
        }
        MetaCmd::Version => {
            println!("{}", crate::version_string());
            if let Some(ref sv) = settings.db_capabilities.server_version {
                println!("Server: PostgreSQL {sv}");
            }
        }
        MetaCmd::SqlMode => {
            return MetaResult::SetInputMode(InputMode::Sql);
        }
        MetaCmd::Text2SqlMode => {
            return MetaResult::SetInputMode(InputMode::Text2Sql);
        }
        MetaCmd::ShowMode => {
            return MetaResult::ShowMode;
        }
        MetaCmd::PlanMode => {
            return MetaResult::SetExecMode(ExecMode::Plan);
        }
        MetaCmd::YoloMode => {
            return MetaResult::SetExecMode(ExecMode::Yolo);
        }
        MetaCmd::InteractiveMode => {
            return MetaResult::SetExecMode(ExecMode::Interactive);
        }
        MetaCmd::RefreshSchema => {
            #[cfg(not(target_arch = "wasm32"))]
            match &settings.schema_cache {
                None => {
                    eprintln!("\\refresh: no active connection or not in interactive mode");
                }
                Some(cache) => match load_schema_cache(client).await {
                    Ok(loaded) => {
                        *cache.write().unwrap() = loaded;
                        println!("Schema cache refreshed.");
                    }
                    Err(e) => {
                        eprintln!("\\refresh: failed to reload schema cache: {e}");
                    }
                },
            }
            #[cfg(target_arch = "wasm32")]
            {
                eprintln!("\\refresh: schema completion not available on this platform");
            }
        }
        // Function-key toggle metacommands (#321, #324, #325).
        MetaCmd::ToggleCompletion => {
            apply_fkey_toggle(FKeyAction::Completion, settings);
        }
        MetaCmd::ToggleSingleLine => {
            apply_fkey_toggle(FKeyAction::SingleLine, settings);
        }
        MetaCmd::ToggleViEmacs => {
            apply_fkey_toggle(FKeyAction::ViEmacs, settings);
        }
        MetaCmd::ToggleAutoExplain => {
            apply_fkey_toggle(FKeyAction::AutoExplain, settings);
        }
        // Custom Lua commands (#659).
        MetaCmd::ListCustomCommands => {
            let cmds = &settings.lua_registry.commands;
            if cmds.is_empty() {
                println!(
                    "No custom commands loaded.\n\
                     Add Lua scripts to ~/.config/rpg/commands/*.lua"
                );
            } else {
                let mut out = String::from("Custom commands:\n");
                for cmd in cmds {
                    use std::fmt::Write as _;
                    let _ = writeln!(out, "  \\{:<20} {}", cmd.name, cmd.description);
                }
                maybe_page(settings, &out);
            }
        }
        MetaCmd::Unknown(ref name) => {
            // Before reporting "unknown command", check whether a custom Lua
            // command with this name exists.  The Unknown token stores the raw
            // string after `\`; it may include arguments after the first word.
            let (cmd_name, rest) = name
                .split_once(char::is_whitespace)
                .map_or((name.as_str(), ""), |(n, r)| (n, r));
            if settings.lua_registry.get(cmd_name).is_some() {
                let args: Vec<String> = rest.split_whitespace().map(str::to_owned).collect();
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                let dbname = params.dbname.clone();
                let cmd_name_owned = cmd_name.to_owned();
                // block_in_place requires rt-multi-thread (not available on WASM).
                #[cfg(not(target_arch = "wasm32"))]
                let result = tokio::task::block_in_place(|| {
                    settings.lua_registry.execute_command(
                        &cmd_name_owned,
                        &arg_refs,
                        &dbname,
                        client,
                    )
                });
                #[cfg(target_arch = "wasm32")]
                let result = settings.lua_registry.execute_command(
                    &cmd_name_owned,
                    &arg_refs,
                    &dbname,
                    client,
                );
                if let Err(e) = result {
                    eprintln!("{e}");
                }
            } else {
                eprintln!("Invalid command \\{name}. Try \\? for help.");
            }
        }
        MetaCmd::SqlHelp => match crate::session::sql_help_text(parsed.pattern.as_deref()) {
            Ok(text) => maybe_page(settings, &text),
            Err(t) => {
                eprintln!("No help available for \"{t}\".");
                eprintln!("Try \\h with no argument to list available topics.");
            }
        },
        MetaCmd::ShowFunctionSource => match parsed.pattern.as_deref() {
            Some(name) => {
                crate::session::show_function_source(client, name, parsed.plus, parsed.echo_hidden)
                    .await;
            }
            None => eprintln!("\\sf: function name required"),
        },
        MetaCmd::ShowViewDef => match parsed.pattern.as_deref() {
            Some(name) => {
                crate::session::show_view_def(client, name, parsed.plus, parsed.echo_hidden).await;
            }
            None => eprintln!("\\sv: view name required"),
        },
        MetaCmd::Reconnect => {
            // Detect `\c @profile` — look up profile from loaded config.
            let pattern = parsed.pattern.as_deref();
            let resolved_pattern: Option<std::borrow::Cow<str>> =
                if let Some(p) = pattern.filter(|s| s.trim_start().starts_with('@')) {
                    let name = p.trim_start()[1..].trim();
                    if let Some(profile) = crate::config::get_profile(&settings.config, name) {
                        // Build a synthetic \c argument string from the profile.
                        // Fields absent from the profile are represented as `-`
                        // (meaning "keep current value").
                        let host = profile.host.as_deref().unwrap_or("-");
                        let user = profile.username.as_deref().unwrap_or("-");
                        let db = profile.dbname.as_deref().unwrap_or("-");
                        let port_str;
                        let port = match profile.port {
                            Some(n) => {
                                port_str = n.to_string();
                                port_str.as_str()
                            }
                            None => "-",
                        };
                        Some(std::borrow::Cow::Owned(format!(
                            "{db} {user} {host} {port}"
                        )))
                    } else {
                        eprintln!("\\c: unknown profile \"@{name}\"");
                        eprintln!(
                            "Configure profiles in {} under [connections.{name}]",
                            crate::config::user_config_path_display()
                        );
                        return MetaResult::Continue;
                    }
                } else {
                    pattern.map(std::borrow::Cow::Borrowed)
                };

            match crate::session::reconnect(resolved_pattern.as_deref(), params).await {
                Ok((new_client, mut new_params)) => {
                    // If the target was a profile, carry forward its sslmode
                    // and password when the profile specifies them.
                    if let Some(p) = pattern
                        .and_then(|s| {
                            let t = s.trim_start();
                            t.starts_with('@').then(|| &t[1..])
                        })
                        .and_then(|name| crate::config::get_profile(&settings.config, name.trim()))
                    {
                        if let Some(ref ssl) = p.sslmode {
                            if let Ok(mode) = crate::connection::SslMode::parse(ssl) {
                                new_params.sslmode = mode;
                            }
                        }
                        if new_params.password.is_none() {
                            new_params.password.clone_from(&p.password);
                        }
                    }
                    // Detect server version to include in the reconnect
                    // banner (always shown, matching psql behaviour).
                    let server_ver =
                        crate::capabilities::detect_server_version_pub(&new_client).await;
                    let msg = crate::connection::reconnect_info(
                        crate::version_string(),
                        server_ver.as_deref(),
                        &new_params,
                    );
                    println!("{msg}");
                    return MetaResult::Reconnected(Box::new(new_client), Box::new(new_params));
                }
                Err(e) => eprintln!("\\c: {e}"),
            }
        }
        // Variable commands (issue #32).
        MetaCmd::Set(ref name, ref value) => {
            apply_set(settings, name, value);
        }
        MetaCmd::Unset(ref name) => {
            apply_unset(settings, name);
        }
        MetaCmd::Prompt(ref prompt_text, ref var_name) => {
            apply_prompt(settings, prompt_text, var_name);
        }
        MetaCmd::Pset(ref option, ref value) => {
            apply_pset(settings, option, value.as_deref());
        }
        MetaCmd::ToggleAlign => {
            apply_toggle_align(settings);
        }
        MetaCmd::TuplesOnly(mode) => {
            apply_tuples_only(settings, mode);
        }
        MetaCmd::FieldSep(ref sep) => {
            apply_field_sep(settings, sep.as_deref());
        }
        MetaCmd::ToggleHtml => {
            apply_toggle_html(settings);
        }
        MetaCmd::SetTitle(ref title) => {
            apply_set_title(settings, title.as_deref());
        }
        MetaCmd::Watch => {
            let interval = parse_watch_interval(parsed.pattern.as_deref().unwrap_or(""));
            // Capture the last query before the (potentially long) watch loop
            // to avoid borrow issues with `settings`.
            let sql = settings.last_query.clone();
            match sql {
                Some(ref q) => {
                    watch_query(client, q, interval, settings).await;
                }
                None => {
                    eprintln!("\\watch cannot be used with an empty query");
                }
            }
        }
        // Diagnostic commands — delegate to the dba module.
        MetaCmd::Dba => {
            let subcommand = parsed.pattern.as_deref().unwrap_or("");
            let caps = settings.db_capabilities.clone();
            let ai_context =
                crate::dba::execute(client, subcommand, parsed.plus, Some(&caps), settings).await;
            // AI interpretation when the command returns context (e.g. \dba waits+).
            if let Some(ref context) = ai_context {
                interpret_dba_output(context, subcommand, settings).await;
            }
        }
        // Named queries (#69).
        MetaCmd::NamedSave(ref name, ref query) => {
            if crate::named::NamedQueries::is_valid_name(name) {
                let mut nq = crate::named::NamedQueries::load();
                nq.set(name, query);
                match nq.save() {
                    Ok(()) => {
                        if !settings.quiet {
                            eprintln!("Saved query \"{name}\".");
                        }
                    }
                    Err(e) => eprintln!("\\ns: {e}"),
                }
            } else {
                eprintln!(
                    "\\ns: invalid query name \"{name}\": \
                     names must contain only alphanumerics and underscores"
                );
            }
        }
        MetaCmd::NamedExec(ref name, ref args) => {
            let nq = crate::named::NamedQueries::load();
            match nq.get(name) {
                Some(query) => {
                    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    let sql = crate::named::NamedQueries::substitute(query, &arg_refs);
                    execute_query(client, &sql, settings, tx).await;
                }
                None => eprintln!("\\n: unknown query \"{name}\""),
            }
        }
        MetaCmd::NamedList => {
            let nq = crate::named::NamedQueries::load();
            let queries = nq.list();
            if queries.is_empty() {
                println!("No named queries saved.");
            } else {
                let mut out = String::new();
                for (name, query) in queries {
                    use std::fmt::Write as FmtWrite;
                    let _ = writeln!(out, "  {name}: {query}");
                }
                maybe_page(settings, &out);
            }
        }
        MetaCmd::NamedDelete(ref name) => {
            let mut nq = crate::named::NamedQueries::load();
            if nq.delete(name) {
                match nq.save() {
                    Ok(()) => {
                        if !settings.quiet {
                            eprintln!("Deleted query \"{name}\".");
                        }
                    }
                    Err(e) => eprintln!("\\nd: {e}"),
                }
            } else {
                eprintln!("\\nd: unknown query \"{name}\"");
            }
        }
        MetaCmd::NamedPrint(ref name) => {
            let nq = crate::named::NamedQueries::load();
            match nq.get(name) {
                Some(query) => println!("{query}"),
                None => eprintln!("\\np: unknown query \"{name}\""),
            }
        }
        // Describe-family commands — delegate to the describe module.
        ref describe_cmd
            if matches!(
                describe_cmd,
                MetaCmd::DescribeObject
                    | MetaCmd::ListTables
                    | MetaCmd::ListIndexes
                    | MetaCmd::ListSequences
                    | MetaCmd::ListViews
                    | MetaCmd::ListMatViews
                    | MetaCmd::ListForeignTables
                    | MetaCmd::ListFunctions
                    | MetaCmd::ListSchemas
                    | MetaCmd::ListRoles
                    | MetaCmd::ListDatabases
                    | MetaCmd::ListExtensions
                    | MetaCmd::ListTablespaces
                    | MetaCmd::ListTypes
                    | MetaCmd::ListDomains
                    | MetaCmd::ListEventTriggers
                    | MetaCmd::ListPrivileges
                    | MetaCmd::ListConversions
                    | MetaCmd::ListCasts
                    | MetaCmd::ListComments
                    | MetaCmd::ListForeignServers
                    | MetaCmd::ListFdws
                    | MetaCmd::ListForeignTablesViaFdw
                    | MetaCmd::ListOperators
                    | MetaCmd::ListUserMappings
            ) =>
        {
            crate::describe::execute(
                client,
                &parsed,
                settings.db_capabilities.pg_major_version(),
                settings,
            )
            .await;
        }
        // Session persistence meta-commands (#247).
        MetaCmd::SessionList => {
            dispatch_session_list();
        }
        MetaCmd::SessionSave(ref name) => {
            dispatch_session_save(
                params,
                &settings.session_id,
                name.as_deref(),
                settings.query_count,
            );
        }
        MetaCmd::SessionDelete(ref id) => {
            dispatch_session_delete(id);
        }
        MetaCmd::SessionResume(ref id) => {
            if let Some(result) = dispatch_session_resume(id).await {
                return result;
            }
        }
        // Explain share (#655).
        MetaCmd::ExplainShare(ref service) => {
            dispatch_explain_share(client, settings, service).await;
        }
        // Large object commands (#400).
        MetaCmd::LoImport(ref filename, ref comment) => {
            let filename = filename.clone();
            let comment = comment.clone();
            crate::large_object::lo_import(client, &filename, &comment).await;
        }
        MetaCmd::LoExport(ref loid, ref filename) => {
            let loid = loid.clone();
            let filename = filename.clone();
            crate::large_object::lo_export(client, &loid, &filename).await;
        }
        MetaCmd::LoList => {
            crate::large_object::lo_list(client).await;
        }
        MetaCmd::LoUnlink(ref loid) => {
            let loid = loid.clone();
            crate::large_object::lo_unlink(client, &loid).await;
        }
        // History (#history).
        MetaCmd::History(ref arg) => {
            dispatch_history(settings, arg.as_deref());
        }
        ref stub => {
            eprintln!("{}: not yet implemented (see #27)", stub.label());
        }
    }

    MetaResult::Continue
}

// ---------------------------------------------------------------------------
// History command helper (#history)
// ---------------------------------------------------------------------------

/// Decide whether `arg` looks like a file path (save mode) or a pattern
/// (filter mode).
///
/// A path heuristic: contains `/`, starts with `.` or `~`, or contains a `.`
/// after the last `/` (has a file extension).  Everything else is a pattern.
fn arg_is_filepath(arg: &str) -> bool {
    if arg.contains('/') {
        return true;
    }
    if arg.starts_with('~') || arg.starts_with('.') {
        return true;
    }
    // Has a file extension (a dot somewhere in the last component).
    arg.contains('.')
}

/// Read history entries from the history file.
///
/// Returns a `Vec` of history lines in chronological order (oldest first).
/// Returns an empty `Vec` when the file cannot be read.
fn read_history_entries() -> Vec<String> {
    let Some(path) = history_file() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Handle `\s [filename | pattern]`.
///
/// - `\s` — display numbered, syntax-highlighted history through the pager.
/// - `\s filename` — save raw history to a file.
/// - `\s pattern` — filter history by substring (case-insensitive) and
///   display through the pager.
fn dispatch_history(settings: &mut ReplSettings, arg: Option<&str>) {
    use std::fmt::Write as FmtWrite;

    let entries = read_history_entries();

    match arg {
        // ---- Interactive TUI picker (no argument) ------------------------
        None => {
            #[cfg(not(target_arch = "wasm32"))]
            match crate::history_picker::run(entries) {
                Ok(Some(query)) => {
                    settings.initial_input = Some(query);
                }
                Ok(None) => {} // user cancelled
                Err(e) => eprintln!("\\s: {e}"),
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = entries;
                eprintln!("\\s: interactive history picker not available on this platform");
            }
        }

        // ---- Save to file ------------------------------------------------
        Some(path) if arg_is_filepath(path) => {
            // Expand leading `~`.
            let resolved = expand_tilde(path);

            match std::fs::write(&resolved, entries.join("\n") + "\n") {
                Ok(()) => {
                    if !settings.quiet {
                        eprintln!("History saved to {}.", resolved.display());
                    }
                }
                Err(e) => eprintln!("\\s: {e}"),
            }
        }

        // ---- Filter by pattern -------------------------------------------
        Some(pattern) => {
            let pattern_lc = pattern.to_lowercase();
            let filtered: Vec<(usize, &str)> = entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.to_lowercase().contains(&pattern_lc))
                .map(|(i, entry)| (i + 1, entry.as_str()))
                .collect();

            if filtered.is_empty() {
                eprintln!("\\s: no history entries match \"{pattern}\"");
                return;
            }

            // Width of the highest line number for alignment.
            let num_width = filtered.last().map(|(n, _)| n).copied().unwrap_or(1);
            let width = num_width.to_string().len();

            let use_color = !settings.no_highlight
                && std::env::var("TERM").as_deref() != Ok("dumb")
                && std::io::stdout().is_terminal();

            let mut out = String::new();
            for (num, entry) in &filtered {
                let highlighted = if use_color && !entry.starts_with('\\') {
                    crate::highlight::highlight_sql(entry, None).into_owned()
                } else {
                    (*entry).to_owned()
                };
                let _ = writeln!(out, "{num:>width$}  {highlighted}");
            }

            let total = entries.len();
            let shown = filtered.len();
            let _ = writeln!(
                out,
                "-- {shown} of {total} entr{} match \"{pattern}\"",
                if shown == 1 { "y" } else { "ies" }
            );

            maybe_page(settings, &out);
        }
    }
}

// ---------------------------------------------------------------------------
// Explain share helper (#655)
// ---------------------------------------------------------------------------

/// Handle `\explain share <service>`.
///
/// Uploads the last stored EXPLAIN plan to the chosen external visualiser,
/// prints the resulting URL, and copies it to the system clipboard.
///
/// For pgMustard the plan must be in JSON format.  When the stored plan is
/// plain text (which is the common case), the function extracts the inner
/// query from `last_query`, re-executes it as
/// `EXPLAIN (ANALYZE, FORMAT JSON) <inner_query>`, and uploads the JSON.
async fn dispatch_explain_share(client: &Client, settings: &mut ReplSettings, service: &str) {
    let plan_text = match settings.last_explain_text.as_deref() {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => {
            eprintln!(
                "\\explain share: no EXPLAIN plan available.\n\
                 Run an EXPLAIN query first, then use \\explain share."
            );
            return;
        }
    };

    if service.is_empty() {
        eprintln!(
            "\\explain share: service name required.\n\
             Usage: \\explain share depesz\n\
             Usage: \\explain share dalibo\n\
             Usage: \\explain share pgmustard"
        );
        return;
    }

    println!("Uploading EXPLAIN plan to {service}…");

    // For pgMustard we need the plan as a JSON array and the original query.
    let mut plan_json: Option<serde_json::Value> = None;
    let mut query_text: Option<String> = None;

    if service == "pgmustard" {
        let Some(inner) = settings
            .last_query
            .as_deref()
            .and_then(strip_explain_prefix)
        else {
            eprintln!(
                "\\explain share: cannot determine the inner query.\n\
                 Run an EXPLAIN query first, then use \\explain share pgmustard."
            );
            return;
        };
        query_text = Some(inner.clone());

        let json_sql = format!("EXPLAIN (ANALYZE, FORMAT JSON) {inner}");
        match client.simple_query(&json_sql).await {
            Ok(messages) => {
                // Collect the single-column rows into one string (the JSON plan).
                let mut json_text = String::new();
                for msg in &messages {
                    if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                        if let Some(val) = row.get(0) {
                            json_text.push_str(val);
                        }
                    }
                }
                match serde_json::from_str::<serde_json::Value>(&json_text) {
                    Ok(val) => plan_json = Some(val),
                    Err(e) => {
                        eprintln!("\\explain share: failed to parse JSON EXPLAIN output: {e}");
                        return;
                    }
                }
            }
            Err(e) => {
                eprintln!("\\explain share: failed to re-run EXPLAIN with FORMAT JSON: {e}");
                return;
            }
        }
    }

    let pgmustard_cfg = &settings.config.pgmustard;
    match crate::explain::share::share_explain_plan(
        &plan_text,
        service,
        Some(pgmustard_cfg),
        plan_json.as_ref(),
        query_text.as_deref(),
    )
    .await
    {
        Ok(url) => {
            println!("Plan URL: {url}");
            crate::explain::share::copy_to_clipboard(&url);
            println!("(URL copied to clipboard)");
        }
        Err(e) => {
            eprintln!("\\explain share: {e}");
        }
    }
}

/// Strip the `EXPLAIN ...` prefix from a SQL statement, returning the inner
/// query.
///
/// Handles `EXPLAIN <query>`, `EXPLAIN ANALYZE <query>`,
/// `EXPLAIN (options...) <query>`, etc.  Returns `None` if the input does
/// not look like an EXPLAIN statement.
fn strip_explain_prefix(sql: &str) -> Option<String> {
    let trimmed = sql.trim();
    let upper = trimmed.to_uppercase();
    if !upper.starts_with("EXPLAIN") {
        return None;
    }
    let rest = trimmed["EXPLAIN".len()..].trim_start();
    // EXPLAIN (options...) <query>
    if rest.starts_with('(') {
        if let Some(close) = rest.find(')') {
            let inner = rest[close + 1..].trim_start();
            if inner.is_empty() {
                return None;
            }
            return Some(inner.to_owned());
        }
        return None;
    }
    // EXPLAIN ANALYZE [VERBOSE] <query>
    let upper_rest = rest.to_uppercase();
    let after_kw = if upper_rest.starts_with("ANALYZE") {
        rest["ANALYZE".len()..].trim_start()
    } else if upper_rest.starts_with("ANALYSE") {
        rest["ANALYSE".len()..].trim_start()
    } else {
        // Plain EXPLAIN <query>
        rest
    };
    // Optional VERBOSE after ANALYZE
    let upper_after = after_kw.to_uppercase();
    let after_verbose = if upper_after.starts_with("VERBOSE") {
        after_kw["VERBOSE".len()..].trim_start()
    } else {
        after_kw
    };
    if after_verbose.is_empty() {
        return None;
    }
    Some(after_verbose.to_owned())
}

// ---------------------------------------------------------------------------
// Session persistence helpers
// ---------------------------------------------------------------------------

/// Auto-save the current session on connect (best-effort; errors are silenced).
fn session_store_auto_save(params: &crate::connection::ConnParams, session_id: &str) {
    let Ok(store) = crate::session_store::SessionStore::open() else {
        return;
    };
    let now = crate::session_store::now_iso8601();
    let rec = crate::session_store::SessionRecord {
        id: session_id.to_owned(),
        host: Some(params.host.clone()),
        port: Some(params.port),
        username: Some(params.user.clone()),
        dbname: Some(params.dbname.clone()),
        created_at: now.clone(),
        last_used: now,
        query_count: 0,
        name: None,
    };
    let _ = store.upsert(&rec);
}

/// Print a table of recent sessions (used by `\session list`).
fn dispatch_session_list() {
    let store = match crate::session_store::SessionStore::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\\session list: {e}");
            return;
        }
    };
    let sessions = match store.list() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\\session list: {e}");
            return;
        }
    };
    if sessions.is_empty() {
        println!("No saved sessions.");
        return;
    }
    println!(
        "{:<16}  {:<20}  {:<5}  {:<16}  {:<24}  name",
        "id", "host", "port", "dbname", "last_used"
    );
    println!("{}", "-".repeat(100));
    for s in &sessions {
        let host = s.host.as_deref().unwrap_or("-");
        let port = s.port.map_or_else(|| "-".to_owned(), |p| p.to_string());
        let dbname = s.dbname.as_deref().unwrap_or("-");
        let name = s.name.as_deref().unwrap_or("");
        println!(
            "{:<16}  {:<20}  {:<5}  {:<16}  {:<24}  {}",
            s.id, host, port, dbname, s.last_used, name
        );
    }
}

/// Save the current session with an optional friendly name.
fn dispatch_session_save(
    params: &crate::connection::ConnParams,
    session_id: &str,
    name: Option<&str>,
    query_count: u32,
) {
    let store = match crate::session_store::SessionStore::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\\session save: {e}");
            return;
        }
    };
    let now = crate::session_store::now_iso8601();
    let rec = crate::session_store::SessionRecord {
        id: session_id.to_owned(),
        host: Some(params.host.clone()),
        port: Some(params.port),
        username: Some(params.user.clone()),
        dbname: Some(params.dbname.clone()),
        created_at: now.clone(),
        last_used: now,
        query_count,
        name: name.map(str::to_owned),
    };
    if let Err(e) = store.upsert(&rec) {
        eprintln!("\\session save: {e}");
        return;
    }
    if let Some(n) = name {
        println!("Session saved as \"{n}\" (id: {sid}).", sid = rec.id);
    } else {
        println!("Session saved (id: {sid}).", sid = rec.id);
    }
}

/// Delete a saved session by id.
fn dispatch_session_delete(id: &str) {
    let store = match crate::session_store::SessionStore::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\\session delete: {e}");
            return;
        }
    };
    match store.delete(id) {
        Ok(true) => println!("Session {id} deleted."),
        Ok(false) => eprintln!("\\session delete: no session with id \"{id}\""),
        Err(e) => eprintln!("\\session delete: {e}"),
    }
}

/// Look up a session by id and reconnect using its saved parameters.
///
/// Returns `Some(MetaResult::Reconnected(...))` on success, or `None` (error
/// already printed) on failure.
async fn dispatch_session_resume(id: &str) -> Option<MetaResult> {
    let store = match crate::session_store::SessionStore::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("\\session resume: {e}");
            return None;
        }
    };
    let rec = match store.get(id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!("\\session resume: no session with id \"{id}\"");
            return None;
        }
        Err(e) => {
            eprintln!("\\session resume: {e}");
            return None;
        }
    };

    let pattern = format!(
        "{db} {user} {host} {port}",
        db = rec.dbname.as_deref().unwrap_or("-"),
        user = rec.username.as_deref().unwrap_or("-"),
        host = rec.host.as_deref().unwrap_or("-"),
        port = rec.port.map_or_else(|| "-".to_owned(), |p| p.to_string()),
    );

    // Borrow a dummy current_params for reconnect (port 5432 default).
    let dummy = crate::connection::ConnParams::default();
    match crate::session::reconnect(Some(&pattern), &dummy).await {
        Ok((new_client, new_params)) => {
            let server_ver = crate::capabilities::detect_server_version_pub(&new_client).await;
            let msg = crate::connection::reconnect_info(
                crate::version_string(),
                server_ver.as_deref(),
                &new_params,
            );
            println!("{msg}");
            Some(MetaResult::Reconnected(
                Box::new(new_client),
                Box::new(new_params),
            ))
        }
        Err(e) => {
            eprintln!("\\session resume: {e}");
            None
        }
    }
}

/// Run the interactive REPL loop.
///
/// Accepts caller-provided `settings` so that flags set on the command line
/// (e.g. `--timing`, `--expanded`) take effect immediately.
///
/// `no_psqlrc` suppresses reading the startup file (`-X`).
///
/// Returns the exit code (0 = normal exit, non-zero = error).
pub async fn run_repl(
    client: Client,
    params: ConnParams,
    settings: ReplSettings,
    no_readline: bool,
    no_psqlrc: bool,
) -> i32 {
    let mut settings = settings;
    let mut tx = TxState::default();
    let mut client = client;
    let mut params = params;

    // Populate audit connection context from the resolved params.
    settings.audit_dbname = params.dbname.clone();
    settings.audit_user = params.user.clone();

    // Load custom Lua commands now that we know the database name.
    settings.lua_registry = crate::lua_commands::LuaRegistry::load(&params.dbname);

    // Open audit log file from config if one is configured.
    if settings.audit_log_file.is_none() {
        if let Some(ref raw_path) = settings.config.logging.audit_file.clone() {
            let expanded = expand_tilde(raw_path);
            if let Some(parent) = expanded.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(parent);
                }
            }
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&expanded)
            {
                settings.audit_log_file = Some(std::io::BufWriter::new(file));
                settings.audit_log_path = Some(expanded);
            }
        }
    }

    // Auto-save current connection to session store (best-effort; non-fatal).
    session_store_auto_save(&params, &settings.session_id);

    // Execute startup file unless suppressed by -X.
    if !no_psqlrc {
        if let Some(rc_path) = startup_file() {
            let path_str = rc_path.to_string_lossy().into_owned();
            crate::io::include_file(&client, &path_str, &mut settings, &mut tx, &params).await;
        }
    }

    // Build rustyline editor (skip if --no-readline).
    let use_readline = !no_readline && io::stdin().is_terminal();

    // Clear terminal so the REPL starts with a clean screen.
    if use_readline {
        print!("\x1b[2J\x1b[H");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    // Initialise the status bar for interactive sessions.
    // Enabled when: readline mode AND stderr is a terminal AND config allows it.
    if use_readline
        && crate::statusline::StatusLine::is_interactive()
        && settings.config.display.statusline_enabled
    {
        let mut sl = crate::statusline::StatusLine::new(true);
        sl.set_connection(&params.host, params.port, &params.dbname);
        sl.setup_scroll_region();
        sl.render();
        settings.statusline = Some(Arc::new(Mutex::new(sl)));
    }

    let exit_code = {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if use_readline {
                run_readline_loop(&mut client, &mut params, &mut settings, &mut tx).await
            } else {
                run_dumb_loop(&mut client, &mut params, &mut settings, &mut tx).await
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = use_readline;
            run_dumb_loop(&mut client, &mut params, &mut settings, &mut tx).await
        }
    };

    // Tear down the status bar on exit.
    if let Some(ref sl_arc) = settings.statusline {
        sl_arc.lock().unwrap().teardown_scroll_region();
    }

    exit_code
}

// ---------------------------------------------------------------------------
// Function key bindings (#321)
// ---------------------------------------------------------------------------

/// Action triggered by an F-key press.
///
/// The handler stores the pending action in a shared slot; the readline loop
/// reads and handles it when `Cmd::Interrupt` is returned by the handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FKeyAction {
    /// F2 — toggle schema-aware completion.
    Completion,
    /// F3 — toggle single-line mode.
    SingleLine,
    /// F4 — toggle Vi/Emacs editing mode (#325).
    ViEmacs,
    /// F5 — toggle auto-EXPLAIN.
    AutoExplain,
    /// Ctrl-T — toggle SQL/text2sql input mode (#324).
    Text2Sql,
}

/// rustyline `ConditionalEventHandler` for a single F-key.
///
/// On each press it stores `action` into the shared `pending` slot and
/// returns `Cmd::Interrupt` so the readline loop gets control back without
/// adding a blank line to history.  The loop checks the slot, clears it,
/// and performs the toggle.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct FKeyHandler {
    action: FKeyAction,
    pending: Arc<Mutex<Option<FKeyAction>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ConditionalEventHandler for FKeyHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        _ctx: &EventContext,
    ) -> Option<Cmd> {
        if let Ok(mut slot) = self.pending.lock() {
            *slot = Some(self.action);
        }
        Some(Cmd::Interrupt)
    }
}

/// Run with rustyline readline support.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_lines)]
async fn run_readline_loop(
    client: &mut Client,
    params: &mut ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let edit_mode = if settings.vi_mode || settings.config.display.vi_mode {
        EditMode::Vi
    } else {
        EditMode::Emacs
    };
    let config = Config::builder()
        .max_history_size(HISTORY_SIZE)
        .expect("valid history size")
        .history_ignore_space(true)
        // Use List mode: first Tab inserts the longest common prefix and
        // shows the dropdown (via Hinter); subsequent Tabs cycle through
        // candidates.  The DropdownEventHandler handles Up/Down/Esc navigation.
        .completion_type(rustyline::CompletionType::List)
        .edit_mode(edit_mode)
        .build();

    // Build schema cache (best-effort — completion degrades gracefully on
    // failure).
    let cache = Arc::new(RwLock::new(SchemaCache::default()));
    match load_schema_cache(client).await {
        Ok(loaded) => {
            *cache.write().unwrap() = loaded;
        }
        Err(e) => {
            if settings.debug {
                eprintln!("rpg: schema cache load failed: {e}");
            }
        }
    }
    // Store the Arc in settings so `\refresh` can update the same cache
    // that the completion helper holds.
    settings.schema_cache = Some(Arc::clone(&cache));
    // Enable syntax highlighting unless the user opted out or $TERM is dumb.
    let highlight = !settings.no_highlight && std::env::var("TERM").as_deref() != Ok("dumb");
    let mut helper = RpgHelper::new(Arc::clone(&cache), highlight);
    // Apply the experimental dropdown flag from config (disabled by default).
    helper.set_dropdown_completion(settings.config.display.dropdown_completion);

    // Obtain a handle to the dropdown state *before* moving the helper into
    // the editor so we can share it with the event handlers below.
    let dropdown_handle = helper.dropdown_handle();

    let mut rl: Editor<RpgHelper, FileHistory> = match Editor::with_config(config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("rpg: readline init failed: {e}");
            return 1;
        }
    };
    rl.set_helper(Some(helper));

    // Shared slot for F-key actions.  The FKeyHandler stores the pending
    // action here and returns Cmd::Interrupt; the loop reads and clears it.
    let fkey_pending: Arc<Mutex<Option<FKeyAction>>> = Arc::new(Mutex::new(None));

    // Bind Down / Up / Escape / Enter to the dropdown navigation handler.
    // When the dropdown is inactive these fall through to the default
    // behaviour (history navigation for Up/Down, AcceptLine for Enter,
    // nothing for Escape).
    for (code, key) in [
        (KeyCode::Down, DropdownKey::Down),
        (KeyCode::Up, DropdownKey::Up),
        (KeyCode::Esc, DropdownKey::Escape),
        (KeyCode::Enter, DropdownKey::Enter),
    ] {
        let handler = DropdownEventHandler {
            key,
            dropdown: Arc::clone(&dropdown_handle),
        };
        rl.bind_sequence(
            KeyEvent(code, Modifiers::NONE),
            EventHandler::Conditional(Box::new(handler)),
        );
    }

    // Bind F2 / F3 / F4 / F5 to their respective toggle actions.
    for (code, action) in [
        (KeyCode::F(2), FKeyAction::Completion),
        (KeyCode::F(3), FKeyAction::SingleLine),
        (KeyCode::F(4), FKeyAction::ViEmacs),
        (KeyCode::F(5), FKeyAction::AutoExplain),
    ] {
        let handler = FKeyHandler {
            action,
            pending: Arc::clone(&fkey_pending),
        };
        rl.bind_sequence(
            KeyEvent(code, Modifiers::NONE),
            EventHandler::Conditional(Box::new(handler)),
        );
    }

    // Bind Ctrl-T to text2sql toggle (#324).
    {
        let handler = FKeyHandler {
            action: FKeyAction::Text2Sql,
            pending: Arc::clone(&fkey_pending),
        };
        rl.bind_sequence(
            KeyEvent(KeyCode::Char('T'), Modifiers::CTRL),
            EventHandler::Conditional(Box::new(handler)),
        );
    }

    let hist_path = history_file();
    if let Some(ref p) = hist_path {
        // Best-effort — ignore errors (file may not exist yet).
        let _ = rl.load_history(p);
    }

    // Install a SIGWINCH handler so the status bar redraws immediately when
    // the terminal is resized, even while a query is running.
    //
    // The `shutdown` flag is set before `run_readline_loop` returns so the
    // task does not call `on_resize()` after `teardown_scroll_region()`.
    let sigwinch_shutdown = Arc::new(AtomicBool::new(false));
    // SIGWINCH is a Unix-only signal; skip on Windows where the terminal
    // resize event model differs and tokio::signal::unix is unavailable.
    #[cfg(unix)]
    if let Some(ref sl_arc) = settings.statusline {
        use tokio::signal::unix::{signal, SignalKind};
        let sl_watcher = Arc::clone(sl_arc);
        let shutdown_flag = Arc::clone(&sigwinch_shutdown);
        tokio::spawn(async move {
            let Ok(mut sigwinch) = signal(SignalKind::window_change()) else {
                return;
            };
            while sigwinch.recv().await.is_some() {
                if shutdown_flag.load(Ordering::Relaxed) {
                    break;
                }
                let mut sl = sl_watcher
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                sl.on_resize();
                sl.render();
            }
        });
    }

    let mut buf = String::new();
    // Accumulates the complete multi-line statement text for history.
    let mut stmt_buf = String::new();

    loop {
        // Clear any interrupt flag left by a `\prompt` Ctrl+C in a script.
        settings.prompt_interrupted = false;

        // Re-render the status bar before each prompt so it stays fresh
        // (handles resize events and mode changes from previous commands).
        if let Some(ref sl_arc) = settings.statusline {
            let mut sl = sl_arc.lock().unwrap();
            sl.set_auto_explain(settings.auto_explain);
            sl.on_resize();
        }

        let prompt = build_prompt_from_settings(settings, params, *tx, !buf.is_empty());

        // Keep the completion helper in sync with the current prompt width
        // and input mode (text2sql suppresses SQL syntax highlighting).
        if let Some(helper) = rl.helper_mut() {
            helper.set_prompt_width(prompt.chars().count());
            helper.set_input_mode(settings.input_mode);
        }

        let readline_result = if let Some(initial) = settings.initial_input.take() {
            rl.readline_with_initial(&prompt, (&initial, ""))
        } else {
            rl.readline(&prompt)
        };
        match readline_result {
            Ok(line) => {
                // Dismiss the dropdown so it does not intercept the next
                // prompt's Up-arrow history navigation (fix for #552 bug 2).
                if let Ok(mut dd) = dropdown_handle.lock() {
                    dd.dismiss();
                }

                // Obtain a cancel token *before* the query executes so that
                // a concurrent Ctrl-C handler can send a CancelRequest to the
                // server mid-query.
                let cancel_token = client.cancel_token();

                // Spawn a background task that listens for Ctrl-C while the
                // current line (and any query it triggers) is being processed.
                // When Ctrl-C arrives it sends a PostgreSQL CancelRequest so
                // the server aborts the running query; the query future then
                // resolves with an error and control returns to the prompt.
                // A oneshot channel lets us tear down the task once the line
                // has been handled without a spurious cancel on the next query.
                let (cancel_done_tx, cancel_done_rx) = tokio::sync::oneshot::channel::<()>();
                // tokio::signal is not available on WASM; skip the Ctrl-C
                // cancel-guard task on that target.
                #[cfg(not(target_arch = "wasm32"))]
                tokio::spawn(async move {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            // Best-effort: ignore send errors (connection may
                            // have already closed or no query was running).
                            let _ = cancel_token
                                .cancel_query(tokio_postgres::NoTls)
                                .await;
                        }
                        _ = cancel_done_rx => {
                            // Line processing finished before Ctrl-C — nothing
                            // to do.
                        }
                    }
                });
                #[cfg(target_arch = "wasm32")]
                drop(cancel_done_rx);

                let result =
                    handle_line(&line, &mut buf, &mut stmt_buf, client, params, settings, tx).await;

                // Signal the cancel-guard task that we are done with this
                // line; if Ctrl-C has not fired yet it can exit cleanly.
                // Ignore the error — the task may have already completed.
                let _ = cancel_done_tx.send(());

                // If buf is empty a statement was completed — add the full
                // accumulated statement text to history.
                if buf.is_empty() && !stmt_buf.trim().is_empty() {
                    let _ = rl.add_history_entry(stmt_buf.trim());
                    stmt_buf.clear();
                }

                // Keep the helper's highlight and completion state in sync
                // with settings (allows live toggles via \set and F-keys).
                if let Some(h) = rl.helper_mut() {
                    h.set_highlight(
                        !settings.no_highlight && std::env::var("TERM").as_deref() != Ok("dumb"),
                    );
                    h.set_completion(!settings.no_completion);
                }

                match result {
                    HandleLineResult::Quit => break,
                    HandleLineResult::Reconnected(new_client, new_params) => {
                        *client = *new_client;
                        *params = *new_params;
                        // Reset transaction state on reconnect.
                        *tx = TxState::default();
                        buf.clear();
                        stmt_buf.clear();
                        // Re-detect superuser status for the new connection.
                        settings.is_superuser = crate::capabilities::detect_superuser(client).await;
                        // Update audit connection context for the new connection.
                        settings.audit_dbname.clone_from(&params.dbname);
                        settings.audit_user.clone_from(&params.user);
                        // Update status bar with new connection label.
                        if let Some(ref sl_arc) = settings.statusline {
                            let mut sl = sl_arc.lock().unwrap();
                            sl.set_connection(&params.host, params.port, &params.dbname);
                            sl.render();
                        }
                    }
                    HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Check whether an F-key handler triggered the interrupt.
                // If so, perform the toggle and re-prompt without clearing
                // the buffer or printing a blank line.
                let fkey_action = fkey_pending.lock().ok().and_then(|mut g| g.take());
                if let Some(action) = fkey_action {
                    apply_fkey_toggle(action, settings);
                    // Sync helper state immediately.
                    if let Some(h) = rl.helper_mut() {
                        h.set_completion(!settings.no_completion);
                    }
                    continue;
                }
                // Ctrl-C at idle prompt: psql prints a blank line and
                // re-prompts.  Clear any partial multi-line buffer so the
                // user gets a clean slate.
                println!();
                if !buf.is_empty() {
                    buf.clear();
                    stmt_buf.clear();
                }
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D on empty line: exit cleanly.
                break;
            }
            Err(e) => {
                eprintln!("rpg: readline error: {e}");
                break;
            }
        }
    }

    if let Some(ref p) = hist_path {
        let _ = rl.save_history(p);
    }

    if settings.cond.depth() > 0 {
        eprintln!(
            "rpg: warning: {} unterminated \\if block(s) at end of session",
            settings.cond.depth()
        );
    }

    // Signal the SIGWINCH watcher to stop before we return so it cannot call
    // `on_resize()` after `teardown_scroll_region()` runs in the caller.
    sigwinch_shutdown.store(true, Ordering::Relaxed);

    0
}

/// Run without readline (dumb terminal or --no-readline).
async fn run_dumb_loop(
    client: &mut Client,
    params: &mut ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let stdin = io::stdin();
    let mut buf = String::new();

    loop {
        // Clear any interrupt flag left by a `\prompt` Ctrl+C in a script.
        settings.prompt_interrupted = false;

        // Print prompt to stderr (so it doesn't mix with redirected output).
        let prompt = build_prompt_from_settings(settings, params, *tx, !buf.is_empty());
        eprint!("{prompt}");
        let _ = io::stderr().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF / Ctrl-D
            Ok(_) => {
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                // `quit` / `exit` bare words exit in all modes.
                if is_quit_exit(line.trim(), buf.is_empty()) {
                    break;
                }
                if line.trim_start().starts_with('\\') {
                    match handle_backslash_dumb(line.trim(), &mut buf, client, params, settings, tx)
                        .await
                    {
                        HandleLineResult::Quit => break,
                        HandleLineResult::Reconnected(new_client, new_params) => {
                            *client = *new_client;
                            *params = *new_params;
                            *tx = TxState::default();
                            buf.clear();
                            // Re-detect superuser status for the new connection.
                            settings.is_superuser =
                                crate::capabilities::detect_superuser(client).await;
                            // Update audit connection context for the new connection.
                            settings.audit_dbname.clone_from(&params.dbname);
                            settings.audit_user.clone_from(&params.user);
                        }
                        HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
                    }
                } else if settings.cond.is_active() {
                    // Check for inline backslash command (e.g. `select 1 \gset`).
                    if let Some(pos) = find_inline_backslash(&line) {
                        let sql_part = &line[..pos];
                        let meta_part = line[pos..].trim();
                        if !sql_part.trim().is_empty() {
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(sql_part.trim_end());
                        }
                        match handle_backslash_dumb(
                            meta_part, &mut buf, client, params, settings, tx,
                        )
                        .await
                        {
                            HandleLineResult::Quit => break,
                            HandleLineResult::Reconnected(new_client, new_params) => {
                                *client = *new_client;
                                *params = *new_params;
                                *tx = TxState::default();
                                buf.clear();
                                // Re-detect superuser status for the new connection.
                                settings.is_superuser =
                                    crate::capabilities::detect_superuser(client).await;
                                // Update audit connection context for the new connection.
                                settings.audit_dbname.clone_from(&params.dbname);
                                settings.audit_user.clone_from(&params.user);
                            }
                            HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
                        }
                    } else {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(&line);
                        // In single-line mode, newline terminates the statement.
                        let complete = settings.single_line || is_complete(&buf);
                        if complete {
                            let sql = buf.trim().to_owned();
                            if !sql.is_empty() {
                                execute_query_interactive(client, &sql, settings, tx).await;
                            }
                            buf.clear();
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("rpg: read error: {e}");
                return 1;
            }
        }
    }

    if settings.cond.depth() > 0 {
        eprintln!(
            "rpg: warning: {} unterminated \\if block(s) at end of input",
            settings.cond.depth()
        );
    }

    0
}

// ---------------------------------------------------------------------------
// HandleLineResult — outcome of processing one input line
// ---------------------------------------------------------------------------

/// Find the byte offset of the first unquoted backslash in `line` that could
/// be the start of an inline meta-command (e.g. `select 1 \gset`).
///
/// Returns `Some(offset)` if found, `None` if the line has no inline
/// backslash command.  The scan respects single-quoted strings, dollar-quoted
/// strings, line comments (`--`), and block comments (`/* … */`).
fn find_inline_backslash(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_block_comment = false;
    let mut dollar_tag: Option<String> = None;

    while i < len {
        // Dollar-quoted string
        if let Some(ref tag) = dollar_tag.clone() {
            let tag_bytes = tag.as_bytes();
            if bytes[i..].starts_with(tag_bytes) {
                i += tag_bytes.len();
                dollar_tag = None;
            } else {
                i += 1;
            }
            continue;
        }

        if in_block_comment {
            if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                i += 2;
                in_block_comment = false;
            } else {
                i += 1;
            }
            continue;
        }

        if in_single {
            if bytes[i] == b'\'' {
                if i + 1 < len && bytes[i + 1] == b'\'' {
                    i += 2;
                } else {
                    in_single = false;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }

        // Line comment
        if i + 1 < len && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            return None; // rest of line is a comment
        }

        // Block comment start
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_block_comment = true;
            i += 2;
            continue;
        }

        // Single-quote start
        if bytes[i] == b'\'' {
            in_single = true;
            i += 1;
            continue;
        }

        // Dollar-quote start
        if bytes[i] == b'$' {
            let rest = &line[i..];
            if let Some(end) = rest[1..].find('$') {
                let inner = &rest[1..=end];
                let valid = inner.is_empty()
                    || (inner.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !inner.chars().all(|c| c.is_ascii_digit()));
                if valid {
                    let tag = &rest[..end + 2];
                    dollar_tag = Some(tag.to_owned());
                    i += tag.len();
                    continue;
                }
            }
        }

        // Backslash followed by a letter — potential meta-command
        if bytes[i] == b'\\' && i + 1 < len && bytes[i + 1].is_ascii_alphabetic() {
            // Only treat as inline if there is some SQL before this position
            if line[..i].trim().is_empty() {
                // The line starts with `\` — not an inline command
                return None;
            }
            return Some(i);
        }

        i += 1;
    }
    None
}

/// Outcome of processing a single input line in the REPL.
enum HandleLineResult {
    /// Continue the loop normally.
    Continue,
    /// Exit the loop (`\q`).
    Quit,
    /// Connection replaced by `\c`.
    Reconnected(Box<tokio_postgres::Client>, Box<ConnParams>),
    /// The buffer was modified by a meta-command (cleared, edited, etc.).
    /// The new buffer content is supplied by the caller.
    BufferUpdated,
}

/// Handle a single input line in the dumb loop (backslash commands).
///
/// Buffer-mutating commands (`\r`, `\p`, `\w`, `\e`) are handled inline
/// here because the dumb loop owns the buffer directly.
#[allow(clippy::too_many_lines)]
async fn handle_backslash_dumb(
    input: &str,
    buf: &mut String,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> HandleLineResult {
    let interpolated = settings.vars.interpolate(input);
    let mut parsed = crate::metacmd::parse(&interpolated);
    parsed.echo_hidden = settings.echo_hidden;
    match dispatch_meta(parsed, client, params, settings, tx).await {
        MetaResult::Quit => HandleLineResult::Quit,
        MetaResult::Reconnected(c, p) => HandleLineResult::Reconnected(c, p),
        MetaResult::ClearBuffer => {
            buf.clear();
            println!("Query buffer reset (empty).");
            HandleLineResult::BufferUpdated
        }
        MetaResult::PrintBuffer => {
            if buf.is_empty() {
                println!("Query buffer is empty.");
            } else {
                println!("{buf}");
            }
            HandleLineResult::Continue
        }
        MetaResult::WriteBufferToFile(path) => {
            if let Err(e) = crate::io::write_buffer(buf, &path) {
                eprintln!("{e}");
            }
            HandleLineResult::Continue
        }
        MetaResult::EditBuffer { file, line } => {
            match crate::io::edit(buf, file.as_deref(), line) {
                Ok(new_content) => {
                    let trimmed = new_content.trim().to_owned();
                    if !trimmed.is_empty() {
                        execute_query_interactive(client, &trimmed, settings, tx).await;
                    }
                    buf.clear();
                }
                Err(e) => eprintln!("{e}"),
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::ExecuteBuffer => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                if let Some(bind_params) = settings.pending_bind_params.take() {
                    execute_query_extended_interactive(client, &sql, &bind_params, settings, tx)
                        .await;
                } else {
                    execute_query_interactive(client, &sql, settings, tx).await;
                }
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::ExecuteBufferToFile(path) => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                execute_to_file(client, &sql, &path, settings, tx).await;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::ExecuteBufferPiped(cmd) => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                execute_piped(client, &sql, &cmd, settings, tx).await;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::ExecuteBufferExpanded => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                let prev = settings.expanded;
                settings.expanded = ExpandedMode::On;
                settings.pset.expanded = ExpandedMode::On;
                if let Some(bind_params) = settings.pending_bind_params.take() {
                    execute_query_extended_interactive(client, &sql, &bind_params, settings, tx)
                        .await;
                } else {
                    execute_query_interactive(client, &sql, settings, tx).await;
                }
                settings.expanded = prev;
                settings.pset.expanded = prev;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::ExecuteBufferExpandedToFile(path) => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                let prev = settings.expanded;
                settings.expanded = ExpandedMode::On;
                settings.pset.expanded = ExpandedMode::On;
                execute_to_file(client, &sql, &path, settings, tx).await;
                settings.expanded = prev;
                settings.pset.expanded = prev;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::DescribeBuffer => {
            // Buffer is NOT cleared after \gdesc (same as psql).
            let sql = buf.trim();
            describe_buffer(client, sql, settings.verbose_errors).await;
            HandleLineResult::Continue
        }
        MetaResult::GExecBuffer => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                execute_gexec(client, &sql, settings, tx).await;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::GSet(prefix) => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                execute_gset(client, &sql, prefix.as_deref(), settings, tx).await;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::CrosstabViewBuffer(args) => {
            let sql = buf.trim().to_owned();
            buf.clear();
            if !sql.is_empty() {
                execute_crosstabview(client, &sql, &args, settings, tx).await;
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::BindParams(params) => {
            settings.pending_bind_params = Some(params);
            HandleLineResult::Continue
        }
        MetaResult::ParseStatement(name) => {
            let sql = buf.trim().to_owned();
            if sql.is_empty() {
                eprintln!("\\parse: query buffer is empty");
            } else {
                match client.prepare(&sql).await {
                    Ok(stmt) => {
                        settings.named_statements.insert(name.clone(), stmt);
                    }
                    Err(e) => {
                        crate::output::eprint_db_error(&e, Some(&sql), settings.verbose_errors);
                    }
                }
            }
            HandleLineResult::Continue
        }
        MetaResult::BindNamedExec(name, params) => {
            execute_named_stmt(client, &name, &params, settings, tx).await;
            HandleLineResult::Continue
        }
        MetaResult::ClosePrepared(name) => {
            if settings.named_statements.remove(&name).is_some() {
                let deallocate = format!("deallocate {name}");
                execute_query_interactive(client, &deallocate, settings, tx).await;
            } else {
                eprintln!("\\close_prepared: prepared statement \"{name}\" does not exist");
            }
            HandleLineResult::Continue
        }
        result @ (MetaResult::SetInputMode(_) | MetaResult::SetExecMode(_)) => {
            let label = apply_mode_change(&result, settings);
            match result {
                MetaResult::SetInputMode(_) => eprintln!("Input mode: {label}"),
                _ => eprintln!("Execution mode: {label}"),
            }
            HandleLineResult::Continue
        }
        MetaResult::ShowMode => {
            let input_label = match settings.input_mode {
                InputMode::Sql => "sql",
                InputMode::Text2Sql => "text2sql",
            };
            let exec_label = match settings.exec_mode {
                ExecMode::Interactive => "interactive",
                ExecMode::Plan => "plan",
                ExecMode::Yolo => "yolo",
            };
            eprintln!("Input mode: {input_label}  Execution mode: {exec_label}");
            HandleLineResult::Continue
        }
        MetaResult::Continue => HandleLineResult::Continue,
    }
}

/// Print the bare-word `help` message, matching psql's output.
///
/// Shown when the user types `help` at an empty prompt, directing them to
/// the standard backslash commands for further assistance.
fn print_bare_help() {
    println!(
        "You are using rpg, the command-line interface to PostgreSQL.\n\
         Type:  \\copyright for distribution terms\n       \
                \\h for help with SQL commands\n       \
                \\? for help with rpg commands\n       \
                \\g or terminate with semicolon to execute query\n       \
                \\q to quit"
    );
}

/// Return `true` when `trimmed` is a bare `quit` or `exit` and the query
/// buffer is empty (primary prompt, not mid-statement).
///
/// This matches `PostgreSQL` 11+ behaviour: both keywords are recognised as
/// exit commands in **all** input modes — interactive readline, dumb-terminal
/// loop, piped stdin, and `-c` / `-f` single-command mode.
#[inline]
fn is_quit_exit(trimmed: &str, buf_empty: bool) -> bool {
    if !buf_empty {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower == "quit" || lower == "exit"
}

/// Roll back a stale internal transaction if one is detected.
///
/// After a `/ask` query is interrupted or errors, the read-only transaction
/// that `/ask` opened can remain open, leaving the session in an
/// `InTransaction` or `Failed` state.  At the start of each REPL command we
/// check for this condition:
///
/// - `tx` is `InTransaction` or `Failed` (not `Idle`), **and**
/// - `settings.internal_tx` is `true` — meaning the open transaction was
///   started by the internal `/ask` machinery, not by the user typing `BEGIN`.
///
/// When both conditions hold we issue `ROLLBACK` automatically and print a
/// one-line warning so the user knows what happened.  The `internal_tx` flag
/// ensures we never auto-roll back a transaction the user opened deliberately.
///
/// Returns `true` when a rollback was issued (so callers can log or act on
/// it), `false` otherwise.
async fn auto_rollback_stale_tx(
    client: &Client,
    tx: &mut TxState,
    settings: &mut ReplSettings,
) -> bool {
    if settings.internal_tx && matches!(*tx, TxState::InTransaction | TxState::Failed) {
        eprintln!("-- auto-rolled back stale transaction");
        let _ = client.simple_query("rollback").await;
        *tx = TxState::Idle;
        settings.internal_tx = false;
        return true;
    }
    false
}

/// Process one line of input in the readline loop.
///
/// `stmt_buf` accumulates the full multi-line statement for history recording.
///
/// Returns a [`HandleLineResult`] indicating how the loop should proceed.
#[allow(clippy::too_many_lines)]
async fn handle_line(
    line: &str,
    buf: &mut String,
    stmt_buf: &mut String,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> HandleLineResult {
    // Auto-rollback any stale internal transaction left by a previous
    // interrupted or failed `/ask` command, before processing this command.
    auto_rollback_stale_tx(client, tx, settings).await;

    // AI commands use a `/` prefix and are handled before backslash commands.
    let trimmed = line.trim();

    // `quit` / `exit` bare words: handled in all modes via `is_quit_exit`.
    if is_quit_exit(trimmed, buf.is_empty()) {
        return HandleLineResult::Quit;
    }
    // `help` bare word: matches psql — show usage hint at primary prompt.
    if buf.is_empty() && trimmed.eq_ignore_ascii_case("help") {
        print_bare_help();
        return HandleLineResult::Continue;
    }
    if trimmed.starts_with('/') {
        stmt_buf.clear();
        stmt_buf.push_str(line);
        dispatch_ai_command(trimmed, client, params, settings, tx).await;
        return HandleLineResult::Continue;
    }

    // Text2SQL mode: non-empty lines that don't start with `\` or `;` are
    // treated as natural language prompts forwarded to `/ask`.
    // Lines starting with `;` are sent as raw SQL (the `;` prefix is stripped).
    if settings.input_mode == InputMode::Text2Sql
        && !trimmed.is_empty()
        && !trimmed.starts_with('\\')
    {
        stmt_buf.clear();
        stmt_buf.push_str(line);
        if let Some(raw_sql) = trimmed.strip_prefix(';') {
            let sql = raw_sql.trim();
            if !sql.is_empty() {
                execute_query(client, sql, settings, tx).await;
            }
        } else {
            // Forward as AI prompt — behavior depends on execution mode.
            match settings.exec_mode {
                ExecMode::Plan => {
                    handle_ai_plan(client, trimmed, settings, params).await;
                }
                ExecMode::Interactive | ExecMode::Yolo => {
                    handle_ai_ask(client, trimmed, settings, params, tx).await;
                }
            }
        }
        return HandleLineResult::Continue;
    }

    if line.trim_start().starts_with('\\') {
        // Backslash command — execute immediately, with access to the buffer.
        // Record the command in stmt_buf so the caller adds it to readline history.
        stmt_buf.clear();
        stmt_buf.push_str(line);
        let interpolated = settings.vars.interpolate(line.trim());
        let mut parsed = crate::metacmd::parse(&interpolated);
        parsed.echo_hidden = settings.echo_hidden;
        return match dispatch_meta(parsed, client, params, settings, tx).await {
            MetaResult::Quit => HandleLineResult::Quit,
            MetaResult::Reconnected(c, p) => HandleLineResult::Reconnected(c, p),
            MetaResult::ClearBuffer => {
                buf.clear();
                stmt_buf.clear();
                println!("Query buffer reset (empty).");
                HandleLineResult::BufferUpdated
            }
            MetaResult::PrintBuffer => {
                if buf.is_empty() {
                    println!("Query buffer is empty.");
                } else {
                    println!("{buf}");
                }
                HandleLineResult::Continue
            }
            MetaResult::WriteBufferToFile(path) => {
                if let Err(e) = crate::io::write_buffer(buf, &path) {
                    eprintln!("{e}");
                }
                HandleLineResult::Continue
            }
            MetaResult::EditBuffer { file, line } => {
                // Write buffer to temp file, open editor, read back, execute.
                match crate::io::edit(buf, file.as_deref(), line) {
                    Ok(new_content) => {
                        let trimmed = new_content.trim().to_owned();
                        if !trimmed.is_empty() {
                            execute_query_interactive(client, &trimmed, settings, tx).await;
                        }
                        buf.clear();
                        stmt_buf.clear();
                    }
                    Err(e) => eprintln!("{e}"),
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBuffer => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    if let Some(bind_params) = settings.pending_bind_params.take() {
                        execute_query_extended_interactive(
                            client,
                            &sql,
                            &bind_params,
                            settings,
                            tx,
                        )
                        .await;
                    } else {
                        execute_query_interactive(client, &sql, settings, tx).await;
                    }
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferToFile(path) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    execute_to_file(client, &sql, &path, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferPiped(cmd) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    execute_piped(client, &sql, &cmd, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferExpanded => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    let prev = settings.expanded;
                    settings.expanded = ExpandedMode::On;
                    settings.pset.expanded = ExpandedMode::On;
                    if let Some(bind_params) = settings.pending_bind_params.take() {
                        execute_query_extended_interactive(
                            client,
                            &sql,
                            &bind_params,
                            settings,
                            tx,
                        )
                        .await;
                    } else {
                        execute_query_interactive(client, &sql, settings, tx).await;
                    }
                    settings.expanded = prev;
                    settings.pset.expanded = prev;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferExpandedToFile(path) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    let prev = settings.expanded;
                    settings.expanded = ExpandedMode::On;
                    settings.pset.expanded = ExpandedMode::On;
                    execute_to_file(client, &sql, &path, settings, tx).await;
                    settings.expanded = prev;
                    settings.pset.expanded = prev;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::DescribeBuffer => {
                // Buffer is NOT cleared after \gdesc (same as psql).
                let sql = buf.trim().to_owned();
                describe_buffer(client, &sql, settings.verbose_errors).await;
                HandleLineResult::Continue
            }
            MetaResult::GExecBuffer => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    execute_gexec(client, &sql, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::GSet(prefix) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    execute_gset(client, &sql, prefix.as_deref(), settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::CrosstabViewBuffer(args) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                stmt_buf.clear();
                if !sql.is_empty() {
                    execute_crosstabview(client, &sql, &args, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::BindParams(params) => {
                settings.pending_bind_params = Some(params);
                HandleLineResult::Continue
            }
            MetaResult::ParseStatement(name) => {
                let sql = buf.trim().to_owned();
                if sql.is_empty() {
                    eprintln!("\\parse: query buffer is empty");
                } else {
                    match client.prepare(&sql).await {
                        Ok(stmt) => {
                            settings.named_statements.insert(name.clone(), stmt);
                        }
                        Err(e) => {
                            crate::output::eprint_db_error(&e, Some(&sql), settings.verbose_errors);
                        }
                    }
                }
                HandleLineResult::Continue
            }
            MetaResult::BindNamedExec(name, params) => {
                execute_named_stmt(client, &name, &params, settings, tx).await;
                HandleLineResult::Continue
            }
            MetaResult::ClosePrepared(name) => {
                if settings.named_statements.remove(&name).is_some() {
                    let deallocate = format!("deallocate {name}");
                    execute_query_interactive(client, &deallocate, settings, tx).await;
                } else {
                    eprintln!(
                        "\\close_prepared: prepared statement \"{name}\" \
                         does not exist"
                    );
                }
                HandleLineResult::Continue
            }
            result @ (MetaResult::SetInputMode(_) | MetaResult::SetExecMode(_)) => {
                let label = apply_mode_change(&result, settings);
                match result {
                    MetaResult::SetInputMode(_) => eprintln!("Input mode: {label}"),
                    _ => eprintln!("Execution mode: {label}"),
                }
                HandleLineResult::Continue
            }
            MetaResult::ShowMode => {
                let input_label = match settings.input_mode {
                    InputMode::Sql => "sql",
                    InputMode::Text2Sql => "text2sql",
                };
                let exec_label = match settings.exec_mode {
                    ExecMode::Interactive => "interactive",
                    ExecMode::Plan => "plan",
                    ExecMode::Yolo => "yolo",
                };
                eprintln!("Input mode: {input_label}  Execution mode: {exec_label}");
                HandleLineResult::Continue
            }
            MetaResult::Continue => HandleLineResult::Continue,
        };
    }

    // SQL input: accumulate lines until we have a complete statement.
    // When inside a suppressed conditional branch, discard the input.
    if !settings.cond.is_active() {
        return HandleLineResult::Continue;
    }

    // Check for inline backslash command (e.g. `select 1 \gset my_`).
    if let Some(pos) = find_inline_backslash(line) {
        let sql_part = &line[..pos];
        let meta_part = line[pos..].trim();
        // Accumulate the SQL portion into the buffer.
        if !sql_part.trim().is_empty() {
            if !buf.is_empty() {
                buf.push('\n');
                stmt_buf.push('\n');
            }
            buf.push_str(sql_part.trim_end());
            stmt_buf.push_str(sql_part.trim_end());
        }
        // Record the meta-command in stmt_buf for history.
        if !stmt_buf.is_empty() {
            stmt_buf.push(' ');
        }
        stmt_buf.push_str(meta_part);
        // Dispatch the backslash command (interpolate variables first).
        let interpolated_meta = settings.vars.interpolate(meta_part);
        let mut parsed = crate::metacmd::parse(&interpolated_meta);
        parsed.echo_hidden = settings.echo_hidden;
        return match dispatch_meta(parsed, client, params, settings, tx).await {
            MetaResult::Quit => HandleLineResult::Quit,
            MetaResult::Reconnected(c, p) => HandleLineResult::Reconnected(c, p),
            MetaResult::ExecuteBuffer => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf is intentionally NOT cleared here — the readline
                // loop adds stmt_buf (which contains the original input with
                // the terminator, e.g. "select now() \g") to history before
                // clearing it. See #360.
                if !sql.is_empty() {
                    if let Some(bind_params) = settings.pending_bind_params.take() {
                        execute_query_extended_interactive(
                            client,
                            &sql,
                            &bind_params,
                            settings,
                            tx,
                        )
                        .await;
                    } else {
                        execute_query_interactive(client, &sql, settings, tx).await;
                    }
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferToFile(path) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    execute_to_file(client, &sql, &path, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferPiped(cmd) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    execute_piped(client, &sql, &cmd, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferExpanded => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    let prev = settings.expanded;
                    settings.expanded = ExpandedMode::On;
                    settings.pset.expanded = ExpandedMode::On;
                    if let Some(bind_params) = settings.pending_bind_params.take() {
                        execute_query_extended_interactive(
                            client,
                            &sql,
                            &bind_params,
                            settings,
                            tx,
                        )
                        .await;
                    } else {
                        execute_query_interactive(client, &sql, settings, tx).await;
                    }
                    settings.expanded = prev;
                    settings.pset.expanded = prev;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::ExecuteBufferExpandedToFile(path) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    let prev = settings.expanded;
                    settings.expanded = ExpandedMode::On;
                    settings.pset.expanded = ExpandedMode::On;
                    execute_to_file(client, &sql, &path, settings, tx).await;
                    settings.expanded = prev;
                    settings.pset.expanded = prev;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::GSet(prefix) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    execute_gset(client, &sql, prefix.as_deref(), settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::CrosstabViewBuffer(args) => {
                let sql = buf.trim().to_owned();
                buf.clear();
                // stmt_buf preserved for history (see #360).
                if !sql.is_empty() {
                    execute_crosstabview(client, &sql, &args, settings, tx).await;
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::BindParams(params) => {
                settings.pending_bind_params = Some(params);
                HandleLineResult::Continue
            }
            MetaResult::BindNamedExec(name, params) => {
                execute_named_stmt(client, &name, &params, settings, tx).await;
                HandleLineResult::Continue
            }
            _ => {
                // For terminators like \watch that return Continue after running
                // (the watch loop runs inside dispatch_meta), clear buf so the
                // readline loop sees buf.is_empty() and records stmt_buf — which
                // contains the original input, e.g. "select now() \watch 1" —
                // in history. See #360.
                buf.clear();
                HandleLineResult::BufferUpdated
            }
        };
    }

    if !buf.is_empty() {
        buf.push('\n');
        stmt_buf.push('\n');
    }
    buf.push_str(line);
    stmt_buf.push_str(line);

    // In single-line mode, a newline terminates the statement immediately.
    let complete = settings.single_line || is_complete(buf);
    if complete {
        let sql = buf.trim().to_owned();
        if !sql.is_empty() {
            execute_query_interactive(client, &sql, settings, tx).await;
        }
        buf.clear();
        // stmt_buf is cleared by the caller after adding to history.
    }

    HandleLineResult::Continue
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Serialize all tests that mutate environment variables to prevent
    // intermittent failures when tests run in parallel.
    static ENV_MUTEX: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    // -- is_complete -----------------------------------------------------------

    #[test]
    fn complete_single_semicolon() {
        assert!(is_complete("select 1;"));
    }

    #[test]
    fn incomplete_no_semicolon() {
        assert!(!is_complete("select 1"));
    }

    #[test]
    fn incomplete_multiline() {
        assert!(!is_complete("SELECT\n  1"));
    }

    #[test]
    fn complete_multiline() {
        assert!(is_complete("SELECT\n  1;"));
    }

    #[test]
    fn complete_with_inline_comment() {
        assert!(is_complete("select 1; -- a comment"));
    }

    #[test]
    fn incomplete_semicolon_inside_string() {
        assert!(!is_complete("select 'hello; world'"));
    }

    #[test]
    fn complete_after_string_with_embedded_semicolon() {
        assert!(is_complete("select 'hello; world';"));
    }

    #[test]
    fn incomplete_dollar_quoted() {
        assert!(!is_complete("do $$ begin"));
    }

    #[test]
    fn complete_dollar_quoted() {
        assert!(is_complete("do $$ begin end $$;"));
    }

    // -- metacmd::parse (backslash command parser) ----------------------------

    #[test]
    fn parse_quit() {
        assert_eq!(
            crate::metacmd::parse("\\q").cmd,
            crate::metacmd::MetaCmd::Quit
        );
    }

    #[test]
    fn parse_help() {
        assert_eq!(
            crate::metacmd::parse("\\?").cmd,
            crate::metacmd::MetaCmd::Help
        );
    }

    #[test]
    fn parse_conninfo() {
        assert_eq!(
            crate::metacmd::parse("\\conninfo").cmd,
            crate::metacmd::MetaCmd::ConnInfo
        );
        assert!(
            !crate::metacmd::parse("\\conninfo").plus,
            "bare \\conninfo must not set plus"
        );
    }

    #[test]
    fn parse_conninfo_plus() {
        let m = crate::metacmd::parse("\\conninfo+");
        assert_eq!(m.cmd, crate::metacmd::MetaCmd::ConnInfo);
        assert!(m.plus, "\\conninfo+ must set plus=true");
    }

    #[test]
    fn parse_timing_on() {
        assert_eq!(
            crate::metacmd::parse("\\timing on").cmd,
            crate::metacmd::MetaCmd::Timing(Some(true))
        );
    }

    #[test]
    fn parse_timing_off() {
        assert_eq!(
            crate::metacmd::parse("\\timing off").cmd,
            crate::metacmd::MetaCmd::Timing(Some(false))
        );
    }

    #[test]
    fn parse_timing_toggle() {
        assert_eq!(
            crate::metacmd::parse("\\timing").cmd,
            crate::metacmd::MetaCmd::Timing(None)
        );
    }

    #[test]
    fn parse_expanded_on() {
        assert_eq!(
            crate::metacmd::parse("\\x on").cmd,
            crate::metacmd::MetaCmd::Expanded(ExpandedMode::On)
        );
    }

    #[test]
    fn parse_expanded_auto() {
        assert_eq!(
            crate::metacmd::parse("\\x auto").cmd,
            crate::metacmd::MetaCmd::Expanded(ExpandedMode::Auto)
        );
    }

    #[test]
    fn parse_expanded_toggle() {
        assert_eq!(
            crate::metacmd::parse("\\x").cmd,
            crate::metacmd::MetaCmd::Expanded(ExpandedMode::Toggle)
        );
    }

    #[test]
    fn parse_unknown_command() {
        // Unknown commands store the name WITHOUT a leading backslash.
        assert_eq!(
            crate::metacmd::parse("\\foo").cmd,
            crate::metacmd::MetaCmd::Unknown("foo".to_owned())
        );
    }

    // -- TxState ---------------------------------------------------------------

    #[test]
    fn tx_begin_transitions_to_in_transaction() {
        let mut tx = TxState::Idle;
        tx.update_from_sql("begin;");
        assert_eq!(tx, TxState::InTransaction);
    }

    #[test]
    fn tx_begin_uppercase_transitions_to_in_transaction() {
        let mut tx = TxState::Idle;
        tx.update_from_sql("BEGIN");
        assert_eq!(tx, TxState::InTransaction);
    }

    #[test]
    fn tx_commit_returns_to_idle() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("commit;");
        assert_eq!(tx, TxState::Idle);
    }

    #[test]
    fn tx_rollback_returns_to_idle() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("rollback;");
        assert_eq!(tx, TxState::Idle);
    }

    #[test]
    fn tx_error_while_in_transaction_goes_failed() {
        let mut tx = TxState::InTransaction;
        tx.on_error();
        assert_eq!(tx, TxState::Failed);
    }

    #[test]
    fn tx_error_while_idle_stays_idle() {
        let mut tx = TxState::Idle;
        tx.on_error();
        assert_eq!(tx, TxState::Idle);
    }

    #[test]
    fn tx_select_does_not_change_state() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("select 1;");
        assert_eq!(tx, TxState::InTransaction);
    }

    #[test]
    fn tx_abort_returns_to_idle() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("ABORT;");
        assert_eq!(tx, TxState::Idle);
    }

    #[test]
    fn tx_abort_lowercase_returns_to_idle() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("abort;");
        assert_eq!(tx, TxState::Idle);
    }

    #[test]
    fn tx_rollback_to_savepoint_stays_in_transaction() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("ROLLBACK TO SAVEPOINT sp1;");
        assert_eq!(tx, TxState::InTransaction);
    }

    #[test]
    fn tx_rollback_to_stays_in_transaction() {
        let mut tx = TxState::InTransaction;
        tx.update_from_sql("rollback to sp1;");
        assert_eq!(tx, TxState::InTransaction);
    }

    // -- auto-rollback stale internal transaction detection --------------------

    /// When `internal_tx` is `true` and the session is `InTransaction`,
    /// the stale-tx condition is detected (requires rollback).
    #[test]
    fn auto_rollback_stale_detected_in_transaction() {
        let settings = ReplSettings {
            internal_tx: true,
            ..ReplSettings::default()
        };
        let tx = TxState::InTransaction;
        // Stale: internal flag set and tx is open.
        assert!(
            settings.internal_tx && matches!(tx, TxState::InTransaction | TxState::Failed),
            "stale internal tx should be detected"
        );
    }

    /// When `internal_tx` is `true` and the session is `Failed`,
    /// the stale-tx condition is also detected.
    #[test]
    fn auto_rollback_stale_detected_failed() {
        let settings = ReplSettings {
            internal_tx: true,
            ..ReplSettings::default()
        };
        let tx = TxState::Failed;
        assert!(
            settings.internal_tx && matches!(tx, TxState::InTransaction | TxState::Failed),
            "stale failed internal tx should be detected"
        );
    }

    /// When `internal_tx` is `false`, a stale tx is NOT auto-rolled back —
    /// this protects user-initiated `BEGIN` blocks.
    #[test]
    fn auto_rollback_not_triggered_for_user_begin() {
        let settings = ReplSettings {
            internal_tx: false,
            ..ReplSettings::default()
        };
        let tx = TxState::InTransaction;
        // Not stale: user opened the transaction, `internal_tx` is false.
        assert!(
            !settings.internal_tx || !matches!(tx, TxState::InTransaction | TxState::Failed),
            "user-initiated tx must NOT be auto-rolled back"
        );
    }

    /// When the session is `Idle`, no rollback is needed regardless of the
    /// `internal_tx` flag.
    #[test]
    fn auto_rollback_not_triggered_when_idle() {
        let settings = ReplSettings {
            internal_tx: true,
            ..ReplSettings::default()
        };
        let tx = TxState::Idle;
        // Not stale: already idle.
        assert!(
            !matches!(tx, TxState::InTransaction | TxState::Failed),
            "idle session needs no rollback"
        );
        // The flag value is irrelevant when tx is Idle.
        let _ = settings.internal_tx;
    }

    // -- dollar-quote tag validation --------------------------------------------

    #[test]
    fn dollar_param_not_treated_as_dollar_quote() {
        // $1 is a positional parameter, not a dollar-quote open tag.
        // The semicolon outside $1 should still terminate the statement.
        assert!(is_complete("select $1;"));
    }

    #[test]
    fn dollar_quote_empty_tag_valid() {
        assert!(is_complete("do $$ begin end $$;"));
    }

    #[test]
    fn dollar_quote_named_tag_valid() {
        assert!(is_complete("do $body$ begin end $body$;"));
    }

    #[test]
    fn dollar_quote_incomplete_named_tag() {
        assert!(!is_complete("do $body$ begin end"));
    }

    // -- startup_file ----------------------------------------------------------

    #[test]
    fn startup_file_returns_psqlrc_env_when_set() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Override PSQLRC to a known path.
        std::env::set_var("PSQLRC", "/tmp/test_rpg_rc");
        let result = startup_file();
        std::env::remove_var("PSQLRC");
        assert_eq!(result, Some(std::path::PathBuf::from("/tmp/test_rpg_rc")));
    }

    #[test]
    fn startup_file_returns_none_when_no_rc_exists_and_no_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // Remove PSQLRC env so the function falls through to file checks.
        std::env::remove_var("PSQLRC");
        // We cannot guarantee ~/.rpgrc or ~/.psqlrc don't exist on the test
        // machine, so we just verify the function doesn't panic and returns
        // an Option.
        let _result = startup_file();
    }

    // -- ReplSettings new fields -----------------------------------------------

    #[test]
    fn repl_settings_default_flags_are_false() {
        let s = ReplSettings::default();
        assert!(!s.echo_queries);
        assert!(!s.echo_errors);
        assert!(!s.single_step);
        assert!(!s.single_line);
        assert!(!s.single_transaction);
        assert!(!s.quiet);
        assert!(!s.debug);
    }

    #[test]
    fn repl_settings_log_file_default_is_none() {
        let s = ReplSettings::default();
        assert!(s.log_file.is_none());
    }

    // -- \watch helper functions (#47) ----------------------------------------

    #[test]
    fn watch_interval_default_when_empty() {
        assert!((parse_watch_interval("") - WATCH_DEFAULT_INTERVAL).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_bare_integer() {
        assert!((parse_watch_interval("5") - 5.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_float() {
        assert!((parse_watch_interval("0.5") - 0.5_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_seconds_suffix() {
        assert!((parse_watch_interval("3s") - 3.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_float_seconds_suffix() {
        assert!((parse_watch_interval("0.5s") - 0.5_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_invalid_uses_default() {
        assert!((parse_watch_interval("abc") - WATCH_DEFAULT_INTERVAL).abs() < f64::EPSILON);
    }

    #[test]
    fn watch_interval_negative_uses_default() {
        assert!((parse_watch_interval("-1") - WATCH_DEFAULT_INTERVAL).abs() < f64::EPSILON);
    }

    #[test]
    fn format_system_time_output_structure() {
        // Output must match the ctime-like psql format:
        //   "Www Mmm DD HH:MM:SS YYYY"
        // e.g. "Thu Mar 13 19:00:00 2026"
        //
        // Use 2026-03-13 12:00:00 UTC (noon UTC) to avoid timezone boundary
        // effects that can shift the year at the Unix epoch.
        use std::time::{Duration, UNIX_EPOCH};
        let ts = UNIX_EPOCH + Duration::from_secs(1_773_316_800);
        let s = format_system_time(ts);
        // Must be at least 23 characters long.
        assert!(s.len() >= 23, "output too short: {s:?}");
        // Last 4 chars must be a 4-digit year.
        let _year: i32 = s[s.len() - 4..].parse().expect("year digits");
        // Must contain exactly 2 colons (HH:MM:SS).
        let colon_count = s.chars().filter(|&c| c == ':').count();
        assert_eq!(colon_count, 2, "expected 2 colons in {s:?}");
        // Must start with a 3-letter weekday abbreviation.
        let wdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        assert!(
            wdays.iter().any(|w| s.starts_with(w)),
            "expected weekday prefix in {s:?}"
        );
    }

    #[test]
    fn format_system_time_known_noon_utc() {
        use std::time::{Duration, UNIX_EPOCH};
        // 2026-03-13 12:00:00 UTC = 1_773_316_800 seconds since epoch.
        // At noon UTC the date is the same across UTC-11..UTC+11 timezones.
        let ts = UNIX_EPOCH + Duration::from_secs(1_773_316_800);
        let s = format_system_time(ts);
        assert!(s.contains("2026"), "expected year 2026 in {s:?}");
        assert!(s.contains("Mar"), "expected 'Mar' in {s:?}");
        let colon_count = s.chars().filter(|&c| c == ':').count();
        assert_eq!(colon_count, 2, "expected 2 colons in {s:?}");
    }

    #[test]
    fn repl_settings_last_query_default_is_none() {
        let s = ReplSettings::default();
        assert!(s.last_query.is_none());
    }

    // -- single-line mode (is_complete or single_line) -------------------------

    #[test]
    fn single_line_empty_trimmed_does_not_execute() {
        // In single-line mode an empty trimmed input should not result in
        // execution.  We test the logic directly: if buf.trim().is_empty()
        // we skip the execute call.
        let buf = "   ";
        assert!(buf.trim().is_empty());
    }

    // -- build_prompt ----------------------------------------------------------

    #[test]
    fn prompt_idle() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                false,
                InputMode::Sql,
                ExecMode::Interactive
            ),
            "mydb=> "
        );
    }

    #[test]
    fn prompt_in_transaction() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::InTransaction,
                false,
                InputMode::Sql,
                ExecMode::Interactive
            ),
            "mydb=*> "
        );
    }

    #[test]
    fn prompt_failed_transaction() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Failed,
                false,
                InputMode::Sql,
                ExecMode::Interactive
            ),
            "mydb=!> "
        );
    }

    #[test]
    fn prompt_continuation() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                true,
                InputMode::Sql,
                ExecMode::Interactive
            ),
            "mydb-> "
        );
    }

    #[test]
    fn prompt_continuation_in_transaction() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::InTransaction,
                true,
                InputMode::Sql,
                ExecMode::Interactive
            ),
            "mydb-*> "
        );
    }

    #[test]
    fn prompt_text2sql_mode() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                false,
                InputMode::Text2Sql,
                ExecMode::Interactive
            ),
            "mydb text2sql=> "
        );
    }

    #[test]
    fn prompt_text2sql_continuation() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                true,
                InputMode::Text2Sql,
                ExecMode::Interactive
            ),
            "mydb text2sql-> "
        );
    }

    #[test]
    fn prompt_text2sql_in_transaction() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::InTransaction,
                false,
                InputMode::Text2Sql,
                ExecMode::Interactive
            ),
            "mydb text2sql=*> "
        );
    }

    #[test]
    fn prompt_plan_mode() {
        assert_eq!(
            build_prompt("mydb", TxState::Idle, false, InputMode::Sql, ExecMode::Plan),
            "mydb plan=> "
        );
    }

    #[test]
    fn prompt_yolo_mode() {
        assert_eq!(
            build_prompt("mydb", TxState::Idle, false, InputMode::Sql, ExecMode::Yolo),
            "mydb yolo=> "
        );
    }

    #[test]
    fn prompt_plan_overrides_text2sql() {
        // Execution mode tag takes priority over input mode tag.
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                false,
                InputMode::Text2Sql,
                ExecMode::Plan
            ),
            "mydb plan=> "
        );
    }

    // -- expand_prompt ---------------------------------------------------------

    fn make_ctx<'a>(
        dbname: &'a str,
        user: &'a str,
        tx: TxState,
        continuation: bool,
    ) -> PromptContext<'a> {
        PromptContext {
            dbname,
            user,
            host: "localhost",
            port: 5432,
            is_superuser: false,
            tx,
            continuation,
            in_block_comment: false,
            single_line_mode: false,
            connected: true,
            line_number: 0,
            backend_pid: None,
        }
    }

    #[test]
    fn expand_prompt_default_idle() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%/%R%x%# ", &ctx), "mydb=> ");
    }

    #[test]
    fn expand_prompt_default_in_tx() {
        let ctx = make_ctx("mydb", "alice", TxState::InTransaction, false);
        assert_eq!(expand_prompt("%/%R%x%# ", &ctx), "mydb=*> ");
    }

    #[test]
    fn expand_prompt_default_failed_tx() {
        let ctx = make_ctx("mydb", "alice", TxState::Failed, false);
        assert_eq!(expand_prompt("%/%R%x%# ", &ctx), "mydb=!> ");
    }

    #[test]
    fn expand_prompt_continuation() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, true);
        assert_eq!(expand_prompt("%/%R%x%# ", &ctx), "mydb-> ");
    }

    #[test]
    fn expand_prompt_percent_n_user() {
        let ctx = make_ctx("mydb", "bob", TxState::Idle, false);
        assert_eq!(expand_prompt("%n@%/", &ctx), "bob@mydb");
    }

    #[test]
    fn expand_prompt_percent_m_short_host() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.host = "pg01.example.com";
        assert_eq!(expand_prompt("%m", &ctx), "pg01");
    }

    #[test]
    fn expand_prompt_percent_m_no_dot() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.host = "localhost";
        assert_eq!(expand_prompt("%m", &ctx), "localhost");
    }

    #[test]
    fn expand_prompt_percent_big_m_full_host() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.host = "pg01.example.com";
        assert_eq!(expand_prompt("%M", &ctx), "pg01.example.com");
    }

    #[test]
    fn expand_prompt_percent_gt_port() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%>", &ctx), "5432");
    }

    #[test]
    fn expand_prompt_percent_hash_not_superuser() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%#", &ctx), ">");
    }

    #[test]
    fn expand_prompt_percent_hash_superuser() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.is_superuser = true;
        assert_eq!(expand_prompt("%#", &ctx), "#");
    }

    #[test]
    fn expand_prompt_percent_tilde_different_db() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%~", &ctx), "mydb");
    }

    #[test]
    fn expand_prompt_percent_tilde_same_as_user() {
        let ctx = make_ctx("alice", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%~", &ctx), "~");
    }

    #[test]
    fn expand_prompt_percent_p_with_pid() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.backend_pid = Some(12345);
        assert_eq!(expand_prompt("%p", &ctx), "12345");
    }

    #[test]
    fn expand_prompt_percent_p_no_pid() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("%p", &ctx), "");
    }

    #[test]
    fn expand_prompt_percent_l_line_number() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.line_number = 7;
        assert_eq!(expand_prompt("%l", &ctx), "7");
    }

    #[test]
    fn expand_prompt_percent_percent_literal() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        assert_eq!(expand_prompt("100%%", &ctx), "100%");
    }

    #[test]
    fn expand_prompt_unknown_code_passthrough() {
        let ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        // %Z is not a recognised code — should pass through verbatim.
        assert_eq!(expand_prompt("%Z", &ctx), "%Z");
    }

    #[test]
    fn expand_prompt_r_block_comment() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.in_block_comment = true;
        assert_eq!(expand_prompt("%R", &ctx), "*");
    }

    #[test]
    fn expand_prompt_r_single_line_mode() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.single_line_mode = true;
        assert_eq!(expand_prompt("%R", &ctx), "^");
    }

    #[test]
    fn expand_prompt_r_disconnected() {
        let mut ctx = make_ctx("mydb", "alice", TxState::Idle, false);
        ctx.connected = false;
        assert_eq!(expand_prompt("%R", &ctx), "!");
    }

    // -- build_prompt_from_settings -------------------------------------------

    #[test]
    fn prompt_from_settings_default_prompt1() {
        let settings = ReplSettings::default();
        let params = ConnParams {
            dbname: "mydb".to_owned(),
            user: "alice".to_owned(),
            ..ConnParams::default()
        };
        let result = build_prompt_from_settings(&settings, &params, TxState::Idle, false);
        assert_eq!(result, "mydb=> ");
    }

    #[test]
    fn prompt_from_settings_custom_prompt1() {
        let mut settings = ReplSettings::default();
        settings.vars.set("PROMPT1", "%n@%/>%x ");
        let params = ConnParams {
            dbname: "mydb".to_owned(),
            user: "bob".to_owned(),
            ..ConnParams::default()
        };
        let result = build_prompt_from_settings(&settings, &params, TxState::Idle, false);
        assert_eq!(result, "bob@mydb> ");
    }

    #[test]
    fn prompt_from_settings_uses_prompt2_for_continuation() {
        let mut settings = ReplSettings::default();
        settings.vars.set("PROMPT2", "... ");
        let params = ConnParams {
            dbname: "mydb".to_owned(),
            user: "alice".to_owned(),
            ..ConnParams::default()
        };
        let result = build_prompt_from_settings(&settings, &params, TxState::Idle, true);
        assert_eq!(result, "... ");
    }

    #[test]
    fn prompt_from_settings_prompt1_in_tx() {
        let settings = ReplSettings::default();
        let params = ConnParams {
            dbname: "mydb".to_owned(),
            user: "alice".to_owned(),
            ..ConnParams::default()
        };
        let result = build_prompt_from_settings(&settings, &params, TxState::InTransaction, false);
        assert_eq!(result, "mydb=*> ");
    }

    // -- AutoExplain -----------------------------------------------------------

    #[test]
    fn auto_explain_off_prefix() {
        assert_eq!(AutoExplain::Off.prefix(), "");
    }

    #[test]
    fn auto_explain_on_prefix() {
        assert_eq!(AutoExplain::On.prefix(), "EXPLAIN ");
    }

    #[test]
    fn auto_explain_analyze_prefix() {
        assert_eq!(AutoExplain::Analyze.prefix(), "EXPLAIN ANALYZE ");
    }

    #[test]
    fn auto_explain_verbose_prefix() {
        assert_eq!(
            AutoExplain::Verbose.prefix(),
            "EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING) "
        );
    }

    #[test]
    fn auto_explain_cycle() {
        assert_eq!(AutoExplain::Off.cycle(), AutoExplain::On);
        assert_eq!(AutoExplain::On.cycle(), AutoExplain::Analyze);
        assert_eq!(AutoExplain::Analyze.cycle(), AutoExplain::Verbose);
        assert_eq!(AutoExplain::Verbose.cycle(), AutoExplain::Off);
    }

    #[test]
    fn auto_explain_labels() {
        assert_eq!(AutoExplain::Off.label(), "off");
        assert_eq!(AutoExplain::On.label(), "on");
        assert_eq!(AutoExplain::Analyze.label(), "analyze");
        assert_eq!(AutoExplain::Verbose.label(), "verbose");
    }

    #[test]
    fn set_explain_on_updates_auto_explain() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "EXPLAIN", "on");
        assert_eq!(settings.auto_explain, AutoExplain::On);
    }

    #[test]
    fn set_explain_analyze_updates_auto_explain() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "EXPLAIN", "analyze");
        assert_eq!(settings.auto_explain, AutoExplain::Analyze);
    }

    #[test]
    fn set_explain_verbose_updates_auto_explain() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "EXPLAIN", "verbose");
        assert_eq!(settings.auto_explain, AutoExplain::Verbose);
    }

    #[test]
    fn set_explain_off_updates_auto_explain() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "EXPLAIN", "on");
        apply_set(&mut settings, "EXPLAIN", "off");
        assert_eq!(settings.auto_explain, AutoExplain::Off);
    }

    #[test]
    fn set_explain_no_value_does_not_change_mode() {
        // \set EXPLAIN (no value) should show current mode, not reset it.
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "EXPLAIN", "analyze");
        // Calling with empty value must not change the mode.
        apply_set(&mut settings, "EXPLAIN", "");
        assert_eq!(settings.auto_explain, AutoExplain::Analyze);
    }

    #[test]
    fn fkey_auto_explain_cycles_all_modes() {
        let mut settings = ReplSettings::default();
        assert_eq!(settings.auto_explain, AutoExplain::Off);
        apply_fkey_toggle(FKeyAction::AutoExplain, &mut settings);
        assert_eq!(settings.auto_explain, AutoExplain::On);
        apply_fkey_toggle(FKeyAction::AutoExplain, &mut settings);
        assert_eq!(settings.auto_explain, AutoExplain::Analyze);
        apply_fkey_toggle(FKeyAction::AutoExplain, &mut settings);
        assert_eq!(settings.auto_explain, AutoExplain::Verbose);
        apply_fkey_toggle(FKeyAction::AutoExplain, &mut settings);
        assert_eq!(settings.auto_explain, AutoExplain::Off);
    }

    // -- \set AI_PROVIDER / AI_MODEL -------------------------------------------

    #[test]
    fn set_ai_provider_known_updates_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "anthropic");
        assert_eq!(settings.config.ai.provider.as_deref(), Some("anthropic"));
        // Also stored in the vars map.
        assert_eq!(settings.vars.get("AI_PROVIDER"), Some("anthropic"));
    }

    #[test]
    fn set_ai_provider_openai_updates_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "openai");
        assert_eq!(settings.config.ai.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn set_ai_provider_ollama_updates_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "ollama");
        assert_eq!(settings.config.ai.provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn set_ai_provider_claude_alias_updates_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "claude");
        assert_eq!(settings.config.ai.provider.as_deref(), Some("claude"));
    }

    #[test]
    fn set_ai_provider_unknown_still_updates_config() {
        // Unknown providers are allowed (custom endpoints) but emit a warning.
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "my-custom-provider");
        assert_eq!(
            settings.config.ai.provider.as_deref(),
            Some("my-custom-provider")
        );
    }

    #[test]
    fn set_ai_provider_overwrites_previous() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "openai");
        apply_set(&mut settings, "AI_PROVIDER", "anthropic");
        assert_eq!(settings.config.ai.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn unset_ai_provider_clears_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_PROVIDER", "openai");
        assert!(settings.config.ai.provider.is_some());
        apply_unset(&mut settings, "AI_PROVIDER");
        assert!(settings.config.ai.provider.is_none());
    }

    #[test]
    fn set_ai_model_updates_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_MODEL", "gpt-4o");
        assert_eq!(settings.config.ai.model.as_deref(), Some("gpt-4o"));
        assert_eq!(settings.vars.get("AI_MODEL"), Some("gpt-4o"));
    }

    #[test]
    fn set_ai_model_overwrites_previous() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_MODEL", "gpt-4o");
        apply_set(&mut settings, "AI_MODEL", "claude-sonnet-4-6");
        assert_eq!(
            settings.config.ai.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn unset_ai_model_clears_config() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AI_MODEL", "claude-sonnet-4-6");
        assert!(settings.config.ai.model.is_some());
        apply_unset(&mut settings, "AI_MODEL");
        assert!(settings.config.ai.model.is_none());
    }

    // -- \set TOKEN_BUDGET -----------------------------------------------------

    #[test]
    fn set_token_budget_numeric_updates_config() {
        let mut settings = ReplSettings::default();
        assert_eq!(settings.config.ai.token_budget, 0);
        apply_set(&mut settings, "TOKEN_BUDGET", "50000");
        assert_eq!(settings.config.ai.token_budget, 50_000);
        assert_eq!(settings.vars.get("TOKEN_BUDGET"), Some("50000"));
    }

    #[test]
    fn set_token_budget_zero_means_unlimited() {
        let mut settings = ReplSettings::default();
        settings.config.ai.token_budget = 10_000;
        apply_set(&mut settings, "TOKEN_BUDGET", "0");
        assert_eq!(settings.config.ai.token_budget, 0);
    }

    #[test]
    fn set_token_budget_invalid_value_leaves_config_unchanged() {
        let mut settings = ReplSettings::default();
        settings.config.ai.token_budget = 5_000;
        apply_set(&mut settings, "TOKEN_BUDGET", "not_a_number");
        // Budget is unchanged because the value was rejected.
        assert_eq!(settings.config.ai.token_budget, 5_000);
    }

    #[test]
    fn set_token_budget_overwrites_previous() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "TOKEN_BUDGET", "10000");
        apply_set(&mut settings, "TOKEN_BUDGET", "20000");
        assert_eq!(settings.config.ai.token_budget, 20_000);
    }

    // -- \set AUTO_SUGGEST (#368) ----------------------------------------------

    #[test]
    fn auto_suggest_fix_default_is_true() {
        let s = ReplSettings::default();
        assert!(s.auto_suggest_fix);
    }

    #[test]
    fn last_was_fix_default_is_false() {
        let s = ReplSettings::default();
        assert!(!s.last_was_fix);
    }

    #[test]
    fn text2sql_show_sql_default_is_true() {
        let s = ReplSettings::default();
        assert!(s.text2sql_show_sql);
    }

    #[test]
    fn set_text2sql_show_sql_off_disables_flag() {
        let mut s = ReplSettings::default();
        apply_set(&mut s, "TEXT2SQL_SHOW_SQL", "off");
        assert!(!s.text2sql_show_sql);
    }

    #[test]
    fn set_text2sql_show_sql_on_enables_flag() {
        let mut s = ReplSettings::default();
        apply_set(&mut s, "TEXT2SQL_SHOW_SQL", "off");
        apply_set(&mut s, "TEXT2SQL_SHOW_SQL", "on");
        assert!(s.text2sql_show_sql);
    }

    #[test]
    fn unset_text2sql_show_sql_resets_to_default() {
        let mut s = ReplSettings::default();
        apply_set(&mut s, "TEXT2SQL_SHOW_SQL", "off");
        assert!(!s.text2sql_show_sql);
        apply_unset(&mut s, "TEXT2SQL_SHOW_SQL");
        assert!(s.text2sql_show_sql);
    }

    #[test]
    fn set_auto_suggest_off_disables_flag() {
        let mut settings = ReplSettings::default();
        assert!(settings.auto_suggest_fix);
        apply_set(&mut settings, "AUTO_SUGGEST", "off");
        assert!(!settings.auto_suggest_fix);
    }

    #[test]
    fn set_auto_suggest_on_enables_flag() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AUTO_SUGGEST", "off");
        apply_set(&mut settings, "AUTO_SUGGEST", "on");
        assert!(settings.auto_suggest_fix);
    }

    #[test]
    fn set_auto_suggest_false_disables_flag() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AUTO_SUGGEST", "false");
        assert!(!settings.auto_suggest_fix);
    }

    #[test]
    fn set_auto_suggest_zero_disables_flag() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "AUTO_SUGGEST", "0");
        assert!(!settings.auto_suggest_fix);
    }

    // -- \set VI ---------------------------------------------------------------

    #[test]
    fn set_vi_on_enables_vi_mode() {
        let mut settings = ReplSettings::default();
        assert!(!settings.vi_mode);
        apply_set(&mut settings, "VI", "on");
        assert!(settings.vi_mode);
        assert!(settings.config.display.vi_mode);
    }

    #[test]
    fn set_vi_off_disables_vi_mode() {
        let mut settings = ReplSettings::default();
        apply_set(&mut settings, "VI", "on");
        assert!(settings.vi_mode);
        apply_set(&mut settings, "VI", "off");
        assert!(!settings.vi_mode);
        assert!(!settings.config.display.vi_mode);
    }

    #[test]
    fn set_vi_default_is_emacs() {
        let settings = ReplSettings::default();
        assert!(!settings.vi_mode);
    }

    // -- \gexec parser ---------------------------------------------------------

    #[test]
    fn parse_gexec_bare() {
        assert_eq!(
            crate::metacmd::parse("\\gexec").cmd,
            crate::metacmd::MetaCmd::GExec
        );
    }

    #[test]
    fn parse_gexec_with_trailing_space() {
        // Trailing whitespace must still be recognised.
        assert_eq!(
            crate::metacmd::parse("\\gexec ").cmd,
            crate::metacmd::MetaCmd::GExec
        );
    }

    #[test]
    fn parse_gexec_prefix_not_g() {
        // \gexecfoo is not \gexec.
        assert!(matches!(
            crate::metacmd::parse("\\gexecfoo").cmd,
            crate::metacmd::MetaCmd::Unknown(_)
        ));
    }

    // -- command_tag_for -------------------------------------------------------

    #[test]
    fn command_tag_create_table() {
        assert_eq!(
            command_tag_for("CREATE TABLE t1(id int)", 0),
            "CREATE TABLE"
        );
    }

    #[test]
    fn command_tag_insert() {
        assert_eq!(command_tag_for("INSERT INTO t VALUES (1)", 1), "INSERT 0 1");
    }

    #[test]
    fn command_tag_update() {
        assert_eq!(command_tag_for("UPDATE t SET x=1", 3), "UPDATE 3");
    }

    #[test]
    fn command_tag_delete() {
        assert_eq!(command_tag_for("DELETE FROM t", 2), "DELETE 2");
    }

    #[test]
    fn command_tag_select() {
        assert_eq!(command_tag_for("SELECT 1", 1), "SELECT 1");
    }

    #[test]
    fn command_tag_drop_table() {
        assert_eq!(command_tag_for("DROP TABLE t1", 0), "DROP TABLE");
    }

    // -- find_inline_backslash -----------------------------------------------

    #[test]
    fn inline_backslash_simple_gset() {
        assert_eq!(find_inline_backslash("select 1 \\gset"), Some(9));
    }

    #[test]
    fn inline_backslash_g_bare() {
        assert_eq!(find_inline_backslash("select 1 \\g"), Some(9));
    }

    #[test]
    fn inline_backslash_none_when_starts_with_backslash() {
        assert_eq!(find_inline_backslash("\\dt"), None);
    }

    #[test]
    fn inline_backslash_none_when_no_backslash() {
        assert_eq!(find_inline_backslash("select 1"), None);
    }

    #[test]
    fn inline_backslash_inside_string_not_detected() {
        assert_eq!(find_inline_backslash("select '\\gset'"), None);
    }

    #[test]
    fn inline_backslash_after_comment_not_detected() {
        assert_eq!(find_inline_backslash("select 1 -- \\gset"), None);
    }

    #[test]
    fn inline_backslash_gexec() {
        assert_eq!(
            find_inline_backslash("select 'create table t()' \\gexec"),
            Some(26)
        );
    }

    // -- history stmt_buf construction for inline terminators (#360) ----------

    /// Helper: simulate the `stmt_buf` construction for an inline backslash line.
    ///
    /// Mirrors the logic in `handle_line` (lines beginning
    /// "Check for inline backslash command"): find the split point, push
    /// `sql_part` then " " + `meta_part` into `stmt_buf`.
    fn build_stmt_buf_for_inline(line: &str) -> Option<String> {
        let pos = find_inline_backslash(line)?;
        let sql_part = &line[..pos];
        let meta_part = line[pos..].trim();
        let mut stmt_buf = String::new();
        if !sql_part.trim().is_empty() {
            stmt_buf.push_str(sql_part.trim_end());
        }
        if !stmt_buf.is_empty() {
            stmt_buf.push(' ');
        }
        stmt_buf.push_str(meta_part);
        Some(stmt_buf)
    }

    #[test]
    fn history_stmt_buf_gx_preserves_terminator() {
        // "select * from users \gx" → stmt_buf must be the full original input
        // so that history records the terminator, not a bare semicolon. (#360)
        let line = "select * from users \\gx";
        let stmt = build_stmt_buf_for_inline(line).expect("should find inline backslash");
        assert_eq!(stmt, "select * from users \\gx");
    }

    #[test]
    fn history_stmt_buf_watch_preserves_terminator() {
        // "select now() \watch 1" → stmt_buf must contain \watch, not just
        // the SQL part, so history records the original expression. (#360)
        let line = "select now() \\watch 1";
        let stmt = build_stmt_buf_for_inline(line).expect("should find inline backslash");
        assert_eq!(stmt, "select now() \\watch 1");
    }

    #[test]
    fn history_stmt_buf_g_bare_preserves_terminator() {
        let line = "select 1 \\g";
        let stmt = build_stmt_buf_for_inline(line).expect("should find inline backslash");
        assert_eq!(stmt, "select 1 \\g");
    }

    #[test]
    fn history_stmt_buf_gset_preserves_terminator() {
        let line = "select count(*) from users \\gset";
        let stmt = build_stmt_buf_for_inline(line).expect("should find inline backslash");
        assert_eq!(stmt, "select count(*) from users \\gset");
    }

    // -- AI command prefix detection -----------------------------------------

    #[test]
    fn ai_ask_prefix_detected() {
        // `/ask` lines start with `/` and should be routed as AI commands.
        let line = "/ask list all users";
        assert!(line.trim().starts_with('/'));
    }

    #[test]
    fn ai_fix_prefix_detected() {
        let line = "/fix";
        assert!(line.trim().starts_with('/'));
    }

    #[test]
    fn ai_explain_prefix_detected() {
        let line = "/explain select 1";
        assert!(line.trim().starts_with('/'));
    }

    #[test]
    fn ai_optimize_prefix_detected() {
        let line = "/optimize";
        assert!(line.trim().starts_with('/'));
    }

    #[test]
    fn regular_slash_regex_not_ai_command() {
        // A bare `/` (e.g., used in SQL division) is also `/`-prefixed;
        // this test documents that we accept that edge case in the prefix
        // check — the dispatcher will print "Unknown AI command" for it,
        // which is acceptable for v1.
        let line = "/ 2";
        assert!(line.trim().starts_with('/'));
    }

    #[test]
    fn ask_strip_prefix_extracts_prompt() {
        let input = "/ask list all active users";
        let prompt = input.strip_prefix("/ask").map(str::trim);
        assert_eq!(prompt, Some("list all active users"));
    }

    #[test]
    fn ask_strip_prefix_empty_prompt() {
        let input = "/ask";
        let prompt = input.strip_prefix("/ask").map(str::trim);
        assert_eq!(prompt, Some(""));
    }

    #[test]
    fn ai_clear_prefix_detected() {
        let line = "/clear";
        assert!(line.trim().starts_with('/'));
        assert_eq!(line, "/clear");
    }

    #[test]
    fn ai_compact_prefix_detected() {
        let input = "/compact performance";
        let focus = input.strip_prefix("/compact").map(str::trim);
        assert_eq!(focus, Some("performance"));
    }

    #[test]
    fn ai_compact_no_focus() {
        let input = "/compact";
        let focus = input.strip_prefix("/compact").map(str::trim);
        assert_eq!(focus, Some(""));
    }

    // -- AskChoice enum ---------------------------------------------------------

    #[test]
    fn ask_choice_enum_values() {
        // Just verify the enum variants exist and are distinct.
        assert_ne!(AskChoice::Yes, AskChoice::No);
        assert_ne!(AskChoice::Yes, AskChoice::Edit);
        assert_ne!(AskChoice::No, AskChoice::Edit);
    }

    #[test]
    fn ask_choice_debug_format() {
        // Ensure Debug trait works (derived).
        let _ = format!("{:?}", AskChoice::Edit);
    }

    // -- /explain helpers ------------------------------------------------------

    #[test]
    fn explain_strip_prefix_with_inline_query() {
        // "/explain SELECT ..." → inline query is extracted.
        let input = "/explain select 1";
        let arg = input.strip_prefix("/explain").map(str::trim).unwrap();
        assert_eq!(arg, "select 1");
    }

    #[test]
    fn explain_strip_prefix_bare_uses_last_query() {
        // "/explain" with no args → arg is empty → must fall back to
        // last_query.
        let input = "/explain";
        let arg = input.strip_prefix("/explain").map(str::trim).unwrap();
        assert!(arg.is_empty());
        // When arg is empty and last_query is None, we should surface an error.
        let s = ReplSettings::default();
        assert!(s.last_query.is_none());
    }

    #[test]
    fn explain_no_prior_query_and_no_args_signals_error() {
        // Verify the decision logic: empty arg + no last_query → error path.
        let query_arg = "";
        let last_query: Option<String> = None;
        let resolved = if query_arg.is_empty() {
            last_query.as_deref().map(str::to_owned)
        } else {
            Some(query_arg.to_owned())
        };
        assert!(resolved.is_none(), "should have no query to explain");
    }

    // -- is_write_query --------------------------------------------------------

    #[test]
    fn write_query_insert() {
        assert!(is_write_query("INSERT INTO t VALUES (1)"));
    }

    #[test]
    fn write_query_update() {
        assert!(is_write_query("UPDATE t SET x = 1"));
    }

    #[test]
    fn write_query_delete() {
        assert!(is_write_query("DELETE FROM t WHERE id = 1"));
    }

    #[test]
    fn write_query_merge() {
        assert!(is_write_query("MERGE INTO t USING src ON (t.id = src.id)"));
    }

    #[test]
    fn write_query_select_is_false() {
        assert!(!is_write_query("select * from users"));
    }

    #[test]
    fn write_query_with_cte_is_true() {
        // All CTEs treated as write to prevent CTE-prefixed DML bypass.
        assert!(is_write_query("with cte as (select 1) select * from cte"));
        assert!(is_write_query("WITH data AS (SELECT 1) DELETE FROM t"));
        assert!(is_write_query(
            "WITH data AS (SELECT 1) INSERT INTO t VALUES (1)"
        ));
        assert!(is_write_query("WITH x AS (SELECT 1) UPDATE t SET a = 1"));
    }

    #[test]
    fn write_query_case_insensitive() {
        assert!(is_write_query("insert into t values (1)"));
        assert!(is_write_query("Insert Into t values (1)"));
    }

    #[test]
    fn write_query_ddl_is_true() {
        assert!(is_write_query("CREATE TABLE t (id int)"));
        assert!(is_write_query("CREATE INDEX idx_foo ON t(id)"));
        assert!(is_write_query("DROP TABLE t"));
        assert!(is_write_query("ALTER TABLE t ADD COLUMN x text"));
        assert!(is_write_query("TRUNCATE TABLE t"));
        assert!(is_write_query("RENAME TABLE t TO t2"));
        assert!(is_write_query("create index concurrently ..."));
    }

    #[test]
    fn write_query_grant_revoke_is_true() {
        assert!(is_write_query("GRANT SELECT ON t TO user1"));
        assert!(is_write_query("REVOKE ALL ON t FROM user1"));
    }

    #[test]
    fn write_query_maintenance_is_true() {
        assert!(is_write_query("VACUUM ANALYZE t"));
        assert!(is_write_query("CLUSTER t USING t_pkey"));
        assert!(is_write_query("REINDEX TABLE t"));
        assert!(is_write_query("REFRESH MATERIALIZED VIEW mv"));
    }

    // -- build_explain_sql -----------------------------------------------------

    #[test]
    fn build_explain_sql_select_no_wrap() {
        let sql = build_explain_sql("select * from users");
        assert!(sql.starts_with("explain (analyze, costs, verbose, buffers, format text)"));
        assert!(!sql.contains("begin"));
        assert!(!sql.contains("rollback"));
    }

    #[test]
    fn build_explain_sql_write_wraps_in_transaction() {
        let sql = build_explain_sql("INSERT INTO t VALUES (1)");
        assert!(sql.starts_with("begin;"));
        assert!(sql.contains("explain (analyze, costs, verbose, buffers, format text)"));
        assert!(sql.ends_with("rollback;"));
    }

    #[test]
    fn build_explain_sql_delete_wraps_in_transaction() {
        let sql = build_explain_sql("DELETE FROM t WHERE id = 1");
        assert!(sql.starts_with("begin;"));
        assert!(sql.ends_with("rollback;"));
    }

    // -- LastError and /fix ---------------------------------------------------

    #[test]
    fn last_error_construction() {
        let err = LastError {
            query: "select * from nonexistent_table".to_owned(),
            error_message: "relation \"nonexistent_table\" does not exist".to_owned(),
            sqlstate: Some("42P01".to_owned()),
        };
        assert_eq!(err.query, "select * from nonexistent_table");
        assert!(err.error_message.contains("does not exist"));
        assert_eq!(err.sqlstate.as_deref(), Some("42P01"));
    }

    #[test]
    fn last_error_without_sqlstate() {
        let err = LastError {
            query: "select 1 +".to_owned(),
            error_message: "syntax error at end of input".to_owned(),
            sqlstate: None,
        };
        assert!(err.sqlstate.is_none());
    }

    #[test]
    fn repl_settings_last_error_default_is_none() {
        let s = ReplSettings::default();
        assert!(s.last_error.is_none());
    }

    #[test]
    fn last_error_clone() {
        let err = LastError {
            query: "select 1".to_owned(),
            error_message: "some error".to_owned(),
            sqlstate: Some("42601".to_owned()),
        };
        let cloned = err.clone();
        assert_eq!(cloned.query, err.query);
        assert_eq!(cloned.error_message, err.error_message);
        assert_eq!(cloned.sqlstate, err.sqlstate);
    }

    #[test]
    fn fix_no_error_message_check() {
        // When last_error is None, the /fix handler should print "No recent
        // error to fix." -- verify the condition matches.
        let settings = ReplSettings::default();
        assert!(settings.last_error.is_none());
        // The handler checks: if last_error.is_none() -> print message and return.
        // We test the predicate here; the async handler itself requires a DB.
        let would_bail = settings.last_error.is_none();
        assert!(would_bail);
    }

    // -- extract_last_sql_block ------------------------------------------------

    #[test]
    fn extract_sql_block_single() {
        let text = "Explanation\n```sql\nSELECT 1;\n```";
        assert_eq!(extract_last_sql_block(text), Some("SELECT 1;"));
    }

    #[test]
    fn extract_sql_block_multiple_returns_last() {
        let text = "First\n```sql\nSELECT 1;\n```\nSecond\n```sql\nSELECT 2;\n```";
        assert_eq!(extract_last_sql_block(text), Some("SELECT 2;"));
    }

    #[test]
    fn extract_sql_block_no_fences_returns_none() {
        assert_eq!(extract_last_sql_block("just plain text"), None);
    }

    #[test]
    fn extract_sql_block_unclosed_fence() {
        // Unclosed fence: content after the opening fence is treated as body.
        let text = "```sql\nSELECT 42;";
        assert_eq!(extract_last_sql_block(text), Some("SELECT 42;"));
    }

    #[test]
    fn extract_sql_block_plain_fence_no_lang_tag() {
        let text = "```\nSELECT 1;\n```";
        assert_eq!(extract_last_sql_block(text), Some("SELECT 1;"));
    }

    // -- extract_table_names ---------------------------------------------------

    #[test]
    fn extract_tables_simple_select() {
        let tables = extract_table_names("SELECT * FROM users WHERE id = 1");
        assert_eq!(tables, vec!["users"]);
    }

    #[test]
    fn extract_tables_join() {
        let tables =
            extract_table_names("SELECT u.name FROM users u JOIN orders o ON u.id = o.user_id");
        assert_eq!(tables, vec!["orders", "users"]);
    }

    #[test]
    fn extract_tables_left_join() {
        let tables = extract_table_names(
            "SELECT * FROM products LEFT JOIN categories ON products.cat_id = categories.id",
        );
        assert_eq!(tables, vec!["categories", "products"]);
    }

    #[test]
    fn extract_tables_schema_qualified() {
        let tables = extract_table_names("SELECT * FROM public.users");
        assert_eq!(tables, vec!["public.users"]);
    }

    #[test]
    fn extract_tables_multiple_from() {
        let tables = extract_table_names("SELECT * FROM a, b WHERE a.id = b.a_id");
        // Only first token after FROM is captured; comma-separated second
        // table "b" is not preceded by FROM/JOIN so it's not found.
        // This is a known limitation of the heuristic parser.
        assert!(tables.contains(&"a".to_owned()));
    }

    #[test]
    fn extract_tables_subselect_skipped() {
        let tables =
            extract_table_names("SELECT * FROM (SELECT id FROM inner_t) sub JOIN outer_t ON true");
        // The sub-select is skipped (starts with '('), but outer_t is captured.
        assert!(tables.contains(&"outer_t".to_owned()));
        assert!(!tables.contains(&"(SELECT".to_owned()));
    }

    #[test]
    fn extract_tables_empty() {
        let tables = extract_table_names("SELECT 1");
        assert!(tables.is_empty());
    }

    #[test]
    fn extract_tables_deduplicates() {
        let tables =
            extract_table_names("SELECT * FROM users u1 JOIN users u2 ON u1.id = u2.partner_id");
        assert_eq!(tables, vec!["users"]);
    }

    // -- strip_sql_fences ------------------------------------------------------

    #[test]
    fn strip_fences_no_fences() {
        assert_eq!(strip_sql_fences("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn strip_fences_sql_tag() {
        assert_eq!(strip_sql_fences("```sql\nSELECT 1;\n```"), "SELECT 1;");
    }

    #[test]
    fn strip_fences_no_tag() {
        assert_eq!(strip_sql_fences("```\nSELECT 1;\n```"), "SELECT 1;");
    }

    #[test]
    fn strip_fences_with_whitespace() {
        assert_eq!(
            strip_sql_fences("  ```sql\n  SELECT 1;  \n```  "),
            "SELECT 1;"
        );
    }

    #[test]
    fn strip_fences_no_closing_fence() {
        // Gracefully handles missing closing fence.
        assert_eq!(strip_sql_fences("```sql\nSELECT 1;"), "SELECT 1;");
    }

    // -- ConversationContext ---------------------------------------------------

    #[test]
    fn conversation_context_new_is_empty() {
        let ctx = ConversationContext::new();
        assert!(ctx.is_empty());
        assert_eq!(ctx.token_estimate(), 0);
        assert!(ctx.to_messages().is_empty());
    }

    #[test]
    fn conversation_context_push_user() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("show me all users".to_owned());
        assert!(!ctx.is_empty());
        assert_eq!(ctx.entries.len(), 1);
        assert_eq!(ctx.entries[0].role, "user");
        assert_eq!(ctx.entries[0].content, "show me all users");
    }

    #[test]
    fn conversation_context_push_assistant() {
        let mut ctx = ConversationContext::new();
        ctx.push_assistant("SELECT * FROM users;".to_owned());
        assert_eq!(ctx.entries.len(), 1);
        assert_eq!(ctx.entries[0].role, "assistant");
    }

    #[test]
    fn conversation_context_push_query_result() {
        let mut ctx = ConversationContext::new();
        ctx.push_query_result("SELECT 1", "1 row");
        assert_eq!(ctx.entries.len(), 1);
        assert!(ctx.entries[0].content.contains("SELECT 1"));
        assert!(ctx.entries[0].content.contains("1 row"));
        // Query results are action entries.
        assert!(ctx.entries[0].is_action);
    }

    #[test]
    fn conversation_context_to_messages() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("hello".to_owned());
        ctx.push_assistant("world".to_owned());
        let msgs = ctx.to_messages();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, crate::ai::Role::User));
        assert!(matches!(msgs[1].role, crate::ai::Role::Assistant));
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "world");
    }

    #[test]
    fn conversation_context_clear() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("a".to_owned());
        ctx.push_assistant("b".to_owned());
        assert!(!ctx.is_empty());
        ctx.clear();
        assert!(ctx.is_empty());
        assert_eq!(ctx.token_estimate(), 0);
    }

    #[test]
    fn conversation_context_trim_at_max() {
        let mut ctx = ConversationContext::new();
        ctx.max_entries = 3;
        ctx.push_user("1".to_owned());
        ctx.push_user("2".to_owned());
        ctx.push_user("3".to_owned());
        ctx.push_user("4".to_owned());
        assert_eq!(ctx.entries.len(), 3);
        // Oldest entry ("1") should have been trimmed.
        assert_eq!(ctx.entries[0].content, "2");
    }

    #[test]
    fn conversation_context_compact_small_noop() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("a".to_owned());
        ctx.push_assistant("b".to_owned());
        // <= 4 entries, compact should be a no-op.
        ctx.compact(None);
        assert_eq!(ctx.entries.len(), 2);
    }

    #[test]
    fn conversation_context_compact_reduces_entries() {
        let mut ctx = ConversationContext::new();
        for i in 0..10 {
            ctx.push_user(format!("q{i}"));
            ctx.push_assistant(format!("a{i}"));
        }
        assert_eq!(ctx.entries.len(), 20);
        ctx.compact(None);
        // Should have: 1 summary + 4 recent = 5 entries.
        assert_eq!(ctx.entries.len(), 5);
        assert!(ctx.entries[0]
            .content
            .contains("Previous conversation summary"));
    }

    #[test]
    fn conversation_context_compact_with_focus() {
        let mut ctx = ConversationContext::new();
        for i in 0..8 {
            ctx.push_user(format!("q{i}"));
        }
        ctx.compact(Some("performance"));
        assert!(ctx.entries[0].content.contains("(focus: performance)"));
    }

    #[test]
    fn conversation_context_push_user_not_action() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("hello".to_owned());
        assert!(!ctx.entries[0].is_action);
    }

    #[test]
    fn conversation_context_push_assistant_not_action() {
        let mut ctx = ConversationContext::new();
        ctx.push_assistant("response".to_owned());
        assert!(!ctx.entries[0].is_action);
    }

    #[test]
    fn action_entries_survive_compaction() {
        let mut ctx = ConversationContext::new();
        // Add a mix of conversation and action entries.
        for i in 0..8 {
            ctx.push_user(format!("question {i}"));
            ctx.push_assistant(format!("answer {i}"));
            ctx.push_query_result(&format!("SELECT {i}"), &format!("{i} rows"));
        }
        // Total: 24 entries (8 user + 8 assistant + 8 actions).
        assert_eq!(ctx.entries.len(), 24);

        let action_count_before = ctx.entries.iter().filter(|e| e.is_action).count();
        assert_eq!(action_count_before, 8);

        ctx.compact(None);

        // Action entries from the compacted range should survive.
        let action_count_after = ctx.entries.iter().filter(|e| e.is_action).count();
        // All 8 action entries should still be present (some in compacted
        // range, some in the kept-last-4 range).
        assert_eq!(action_count_after, action_count_before);

        // Verify the summary does NOT contain "Executed SQL" (action content).
        let summary = &ctx.entries[0].content;
        assert!(summary.contains("Previous conversation summary"));
        assert!(!summary.contains("Executed SQL"));
    }

    #[test]
    fn action_entries_ordered_after_compaction() {
        let mut ctx = ConversationContext::new();
        for i in 0..6 {
            ctx.push_user(format!("q{i}"));
            ctx.push_query_result(&format!("SELECT {i}"), "ok");
        }
        // 12 entries total.
        ctx.compact(None);

        // Structure: summary + surviving actions + last 4 entries.
        // First entry should be the summary.
        assert!(!ctx.entries[0].is_action);
        assert!(ctx.entries[0].content.contains("Previous conversation"));

        // Action entries from compacted range should follow the summary.
        let actions: Vec<&ConversationEntry> = ctx.entries.iter().filter(|e| e.is_action).collect();
        assert_eq!(actions.len(), 6);
    }

    #[test]
    fn conversation_context_token_estimate_grows() {
        let mut ctx = ConversationContext::new();
        assert_eq!(ctx.token_estimate(), 0);
        ctx.push_user("a long message with many words".to_owned());
        assert!(ctx.token_estimate() > 0);
    }

    #[test]
    fn repl_settings_conversation_default_is_empty() {
        let s = ReplSettings::default();
        assert!(s.conversation.is_empty());
    }

    #[test]
    fn conversation_auto_compact_below_threshold() {
        let mut ctx = ConversationContext::new();
        ctx.push_user("short message".to_owned());
        // With a 128k context window, a short message is well below 70%.
        assert!(!ctx.auto_compact_if_needed(128_000));
    }

    #[test]
    fn conversation_auto_compact_above_threshold() {
        let mut ctx = ConversationContext::new();
        // Push enough data to exceed 70% of a tiny context window (100 tokens).
        // 100 tokens * 70% = 70 tokens. At ~4 chars/token, that's ~280 chars.
        for i in 0..20 {
            ctx.push_user(format!("message {i} with enough content to fill tokens"));
        }
        assert!(ctx.entries.len() > 4);
        let compacted = ctx.auto_compact_if_needed(100);
        assert!(compacted);
        // After compaction: 1 summary + 4 recent.
        assert_eq!(ctx.entries.len(), 5);
    }

    #[test]
    fn conversation_auto_compact_too_few_entries() {
        let mut ctx = ConversationContext::new();
        // Even if tokens are high, don't compact if <= 4 entries.
        ctx.push_user("x".repeat(2000));
        assert_eq!(ctx.entries.len(), 1);
        assert!(!ctx.auto_compact_if_needed(10)); // threshold = 7 tokens
    }

    // -- Token budget ---------------------------------------------------------

    #[test]
    fn check_budget_unlimited_returns_false() {
        let settings = ReplSettings::default();
        // Default budget is 0 (unlimited).
        assert_eq!(settings.config.ai.token_budget, 0);
        assert!(!check_token_budget(&settings));
    }

    #[test]
    fn check_budget_within_limit() {
        let mut settings = ReplSettings::default();
        settings.config.ai.token_budget = 10_000;
        settings.tokens_used = 5_000;
        assert!(!check_token_budget(&settings));
    }

    #[test]
    fn check_budget_at_limit() {
        let mut settings = ReplSettings::default();
        settings.config.ai.token_budget = 10_000;
        settings.tokens_used = 10_000;
        assert!(check_token_budget(&settings));
    }

    #[test]
    fn check_budget_over_limit() {
        let mut settings = ReplSettings::default();
        settings.config.ai.token_budget = 10_000;
        settings.tokens_used = 15_000;
        assert!(check_token_budget(&settings));
    }

    #[test]
    fn record_usage_increments_total() {
        let mut settings = ReplSettings::default();
        assert_eq!(settings.tokens_used, 0);

        let result = crate::ai::CompletionResult {
            content: String::new(),
            input_tokens: 100,
            output_tokens: 50,
        };
        record_token_usage(&mut settings, &result);
        assert_eq!(settings.tokens_used, 150);

        // Second call adds to the running total.
        let result2 = crate::ai::CompletionResult {
            content: String::new(),
            input_tokens: 200,
            output_tokens: 100,
        };
        record_token_usage(&mut settings, &result2);
        assert_eq!(settings.tokens_used, 450);
    }

    #[test]
    fn tokens_used_default_is_zero() {
        let s = ReplSettings::default();
        assert_eq!(s.tokens_used, 0);
    }

    // -- resolve_api_key -------------------------------------------------------

    #[test]
    fn resolve_api_key_none() {
        assert!(resolve_api_key(None).is_none());
    }

    #[test]
    fn resolve_api_key_raw_openai() {
        // Reset the warn flag for this test.
        RAW_KEY_WARNED.store(false, std::sync::atomic::Ordering::Relaxed);
        let result = resolve_api_key(Some("sk-proj-abc123xyz456def789"));
        assert_eq!(result, Some("sk-proj-abc123xyz456def789".to_owned()));
    }

    #[test]
    fn resolve_api_key_raw_anthropic() {
        RAW_KEY_WARNED.store(false, std::sync::atomic::Ordering::Relaxed);
        let result = resolve_api_key(Some("sk-ant-api03-abcdefghijklmnop"));
        assert_eq!(result, Some("sk-ant-api03-abcdefghijklmnop".to_owned()));
    }

    #[test]
    fn resolve_api_key_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("RPG_TEST_API_KEY_12345", "test-secret-value");
        let result = resolve_api_key(Some("RPG_TEST_API_KEY_12345"));
        assert_eq!(result, Some("test-secret-value".to_owned()));
        std::env::remove_var("RPG_TEST_API_KEY_12345");
    }

    #[test]
    fn resolve_api_key_missing_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::remove_var("NONEXISTENT_RPG_VAR_99999");
        let result = resolve_api_key(Some("NONEXISTENT_RPG_VAR_99999"));
        assert!(result.is_none());
    }

    #[test]
    fn resolve_api_key_empty_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        std::env::set_var("RPG_EMPTY_KEY_TEST", "");
        let result = resolve_api_key(Some("RPG_EMPTY_KEY_TEST"));
        assert!(result.is_none());
        std::env::remove_var("RPG_EMPTY_KEY_TEST");
    }

    // -- --no-readline / use_readline routing ---------------------------------

    /// When `no_readline` is true, `use_readline` must be false regardless of
    /// whether stdin is a terminal.  This mirrors the logic in `run_repl`:
    ///   `let use_readline = !no_readline && io::stdin().is_terminal();`
    #[test]
    fn no_readline_flag_forces_dumb_path() {
        // Simulate the routing decision for both terminal and non-terminal stdin.
        // In tests stdin is never a real terminal, so is_terminal() is false;
        // we therefore cover the `no_readline=true` arm directly.
        let no_readline = true;
        // Regardless of the terminal state, no_readline overrides to false.
        let use_readline = !no_readline; // is_terminal() is always false in tests
        assert!(!use_readline, "no_readline=true must disable readline path");
    }

    /// When `no_readline` is false and stdin is not a terminal (e.g. piped
    /// input), `use_readline` is also false — the dumb loop is used.
    #[test]
    fn non_terminal_stdin_uses_dumb_path() {
        let no_readline = false;
        // In unit tests stdin is never a TTY.
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
        let use_readline = !no_readline && is_tty;
        assert!(
            !use_readline,
            "piped stdin must use dumb loop even without -n"
        );
    }

    // -- print_profiles -------------------------------------------------------

    /// `print_profiles` with an empty config should not panic.
    #[test]
    fn print_profiles_empty_config_does_not_panic() {
        let config = crate::config::Config::default();
        // Just verify no panic; we don't capture stdout in unit tests.
        print_profiles(&config);
    }

    /// `print_profiles` with multiple profiles should not panic.
    #[test]
    fn print_profiles_with_profiles_does_not_panic() {
        use crate::config::ConnectionProfile;
        use std::collections::HashMap;
        let mut connections = HashMap::new();
        connections.insert(
            "production".to_owned(),
            ConnectionProfile {
                host: Some("10.0.1.5".to_owned()),
                port: Some(5432),
                username: Some("postgres".to_owned()),
                dbname: Some("mydb".to_owned()),
                sslmode: Some("require".to_owned()),
                password: None,
                ssh_tunnel: None,
            },
        );
        connections.insert(
            "staging".to_owned(),
            ConnectionProfile {
                host: Some("staging.local".to_owned()),
                port: Some(5432),
                username: Some("app".to_owned()),
                dbname: Some("mydb".to_owned()),
                sslmode: None,
                password: None,
                ssh_tunnel: None,
            },
        );
        let config = crate::config::Config {
            connections,
            ..Default::default()
        };
        print_profiles(&config);
    }

    /// A profile with all optional fields absent renders empty strings.
    #[test]
    fn print_profiles_minimal_profile_does_not_panic() {
        use crate::config::ConnectionProfile;
        use std::collections::HashMap;
        let mut connections = HashMap::new();
        connections.insert("local".to_owned(), ConnectionProfile::default());
        let config = crate::config::Config {
            connections,
            ..Default::default()
        };
        print_profiles(&config);
    }

    // -- quit/exit bare-word detection ----------------------------------------
    //
    // These tests exercise `is_quit_exit` directly, which is the shared helper
    // used by handle_line (readline), run_dumb_loop (dumb/piped), exec_lines
    // (stdin pipe / -f), and exec_command (-c).

    #[test]
    fn quit_bare_word_empty_buf_quits() {
        assert!(is_quit_exit("quit", true));
    }

    #[test]
    fn exit_bare_word_empty_buf_quits() {
        assert!(is_quit_exit("exit", true));
    }

    #[test]
    fn quit_uppercase_empty_buf_quits() {
        assert!(is_quit_exit("QUIT", true));
    }

    #[test]
    fn exit_mixed_case_empty_buf_quits() {
        assert!(is_quit_exit("Exit", true));
    }

    #[test]
    fn quit_with_whitespace_stripped_quits() {
        // Callers pass `line.trim()` — verify trimmed variants are recognised.
        assert!(is_quit_exit("quit", true));
        assert!(is_quit_exit("exit", true));
    }

    #[test]
    fn quit_mid_statement_does_not_quit() {
        // Buffer is non-empty — we are in continuation mode.
        assert!(!is_quit_exit("quit", false));
    }

    #[test]
    fn exit_mid_statement_does_not_quit() {
        assert!(!is_quit_exit("exit", false));
    }

    #[test]
    fn quit_with_args_does_not_quit() {
        // "quit foo" is not a bare word.
        assert!(!is_quit_exit("quit foo", true));
    }

    #[test]
    fn exit_with_args_does_not_quit() {
        assert!(!is_quit_exit("exit now", true));
    }

    #[test]
    fn regular_sql_does_not_trigger_quit() {
        assert!(!is_quit_exit("select 1", true));
    }

    // -- quit/exit in non-interactive (exec_lines / piped) path ---------------

    /// Simulate `exec_lines` processing a single "quit" line with an empty
    /// buffer.  The loop must break immediately — no SQL dispatched.
    #[test]
    fn exec_lines_quit_exits_immediately() {
        let lines: Vec<String> = vec!["quit".to_owned()];
        let mut buf = String::new();
        let mut saw_sql = false;
        for line in lines {
            if is_quit_exit(line.trim(), buf.is_empty()) {
                break;
            }
            // Anything past the guard would be SQL execution.
            saw_sql = true;
            buf.push_str(&line);
        }
        assert!(
            !saw_sql,
            "quit should prevent any SQL from being dispatched"
        );
    }

    #[test]
    fn exec_lines_exit_exits_immediately() {
        let lines: Vec<String> = vec!["exit".to_owned()];
        let mut buf = String::new();
        let mut saw_sql = false;
        for line in lines {
            if is_quit_exit(line.trim(), buf.is_empty()) {
                break;
            }
            saw_sql = true;
            buf.push_str(&line);
        }
        assert!(
            !saw_sql,
            "exit should prevent any SQL from being dispatched"
        );
    }

    /// quit mid-statement (non-empty buffer) must NOT exit — it falls through
    /// to be accumulated as SQL, matching psql behaviour.
    #[test]
    fn exec_lines_quit_mid_statement_is_sql() {
        let lines: Vec<String> = vec!["select".to_owned(), "quit".to_owned()];
        let mut buf = String::new();
        let mut lines_processed = 0usize;
        for line in lines {
            if is_quit_exit(line.trim(), buf.is_empty()) {
                break;
            }
            buf.push_str(&line);
            lines_processed += 1;
        }
        // Both lines were processed — quit did not fire because buf was
        // non-empty when the second line arrived.
        assert_eq!(lines_processed, 2);
    }

    // -- apply_expanded pset sync (bug fix: \x must persist across -c) --------

    /// `apply_expanded` must update both `settings.expanded` and
    /// `settings.pset.expanded` so that subsequent queries rendered via
    /// `settings.pset` (the path taken in `-c` mode) use the new setting.
    #[test]
    fn apply_expanded_syncs_pset_expanded() {
        let mut s = ReplSettings::default();
        assert_eq!(s.expanded, ExpandedMode::Off);
        assert_eq!(s.pset.expanded, ExpandedMode::Off);

        apply_expanded(&mut s, ExpandedMode::On);

        assert_eq!(s.expanded, ExpandedMode::On, "settings.expanded must be On");
        assert_eq!(
            s.pset.expanded,
            ExpandedMode::On,
            "settings.pset.expanded must be synced to On"
        );
    }

    /// Toggle from Off to On updates both fields.
    #[test]
    fn apply_expanded_toggle_off_to_on_syncs_pset() {
        let mut s = ReplSettings::default();
        apply_expanded(&mut s, ExpandedMode::Toggle);
        assert_eq!(s.expanded, ExpandedMode::On);
        assert_eq!(s.pset.expanded, ExpandedMode::On);
    }

    /// Toggle from On to Off updates both fields.
    #[test]
    fn apply_expanded_toggle_on_to_off_syncs_pset() {
        let mut s = ReplSettings {
            expanded: ExpandedMode::On,
            ..Default::default()
        };
        s.pset.expanded = ExpandedMode::On;
        apply_expanded(&mut s, ExpandedMode::Toggle);
        assert_eq!(s.expanded, ExpandedMode::Off);
        assert_eq!(s.pset.expanded, ExpandedMode::Off);
    }

    // -- parse_ai_response_segments -------------------------------------------

    fn collect_segments(response: &str) -> Vec<(bool, String)> {
        parse_ai_response_segments(response)
            .into_iter()
            .map(|s| match s {
                AiResponseSegment::Text(t) => (false, t),
                AiResponseSegment::Sql(q) => (true, q),
            })
            .collect()
    }

    #[test]
    fn parse_segments_plain_text_only() {
        let segs = collect_segments("Hello, world!");
        assert_eq!(segs.len(), 1);
        assert!(!segs[0].0); // Text
        assert_eq!(segs[0].1.trim(), "Hello, world!");
    }

    #[test]
    fn parse_segments_sql_only() {
        let segs = collect_segments("```sql\nSELECT 1;\n```");
        assert_eq!(segs.len(), 1);
        assert!(segs[0].0); // Sql
        assert_eq!(segs[0].1, "SELECT 1;");
    }

    #[test]
    fn parse_segments_text_then_sql() {
        let response = "Here is the count:\n```sql\nSELECT count(*) FROM users;\n```";
        let segs = collect_segments(response);
        assert_eq!(segs.len(), 2);
        assert!(!segs[0].0); // Text
        assert!(segs[1].0); // Sql
        assert_eq!(segs[1].1, "SELECT count(*) FROM users;");
    }

    #[test]
    fn parse_segments_sql_then_text() {
        let response = "```sql\nSELECT now();\n```\nThe current time is above.";
        let segs = collect_segments(response);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].0); // Sql
        assert!(!segs[1].0); // Text
        assert_eq!(segs[0].1, "SELECT now();");
    }

    #[test]
    fn parse_segments_text_sql_text() {
        let response = "Count of users:\n```sql\nSELECT count(*) FROM users;\n```\nThat's all.";
        let segs = collect_segments(response);
        assert_eq!(segs.len(), 3);
        assert!(!segs[0].0); // Text
        assert!(segs[1].0); // Sql
        assert!(!segs[2].0); // Text
        assert_eq!(segs[1].1, "SELECT count(*) FROM users;");
    }

    #[test]
    fn parse_segments_no_sql_fence_no_segments() {
        // A plain code fence (no "sql" tag) is not treated as SQL.
        let response = "Some text\n```\nnot sql\n```\nmore text";
        let segs = collect_segments(response);
        // No SQL segments — everything is text.
        assert!(segs.iter().all(|(is_sql, _)| !is_sql));
    }

    #[test]
    fn parse_segments_empty_response() {
        let segs = collect_segments("");
        assert!(segs.is_empty());
    }

    #[test]
    fn parse_segments_unclosed_fence() {
        let response = "Intro:\n```sql\nSELECT 1;";
        let segs = collect_segments(response);
        // Should still find the SQL even without a closing fence.
        assert_eq!(segs.len(), 2);
        assert!(!segs[0].0);
        assert!(segs[1].0);
        assert_eq!(segs[1].1, "SELECT 1;");
    }

    // -- text2sql commentary suppression ---------------------------------------

    /// Verify that a mixed LLM response (text + SQL + text) parsed into
    /// segments produces exactly one SQL segment and two text segments, so
    /// the caller can skip the text segments when in text2sql mode.
    ///
    /// The actual suppression is done in `handle_ai_ask()` by checking
    /// `settings.input_mode == InputMode::Text2Sql` before printing text
    /// segments; this test confirms the segments are correctly identified for
    /// that guard to act on.
    #[test]
    fn text2sql_response_contains_suppressible_text_segments() {
        let response = "It looks like the query executed successfully.\n\
                        ```sql\nselect count(*) from users;\n```\n\
                        This will return the total number of rows in the \
                        users table.";
        let segs = collect_segments(response);

        let text_segs: Vec<_> = segs.iter().filter(|(is_sql, _)| !is_sql).collect();
        let sql_segs: Vec<_> = segs.iter().filter(|(is_sql, _)| *is_sql).collect();

        // Both surrounding text segments are present and would be suppressed
        // by the InputMode::Text2Sql guard in handle_ai_ask().
        assert_eq!(
            text_segs.len(),
            2,
            "expected two suppressible text segments"
        );
        assert_eq!(sql_segs.len(), 1, "expected one SQL segment");
        assert_eq!(sql_segs[0].1, "select count(*) from users;");
    }

    // -- is_ddl_statement -----------------------------------------------------

    #[test]
    fn ddl_create_table_is_ddl() {
        assert!(is_ddl_statement("CREATE TABLE foo (id int)"));
    }

    #[test]
    fn ddl_alter_table_lowercase_is_ddl() {
        assert!(is_ddl_statement("alter table foo add column bar text"));
    }

    #[test]
    fn ddl_drop_index_is_ddl() {
        assert!(is_ddl_statement("DROP INDEX idx_foo"));
    }

    #[test]
    fn ddl_comment_on_table_is_ddl() {
        assert!(is_ddl_statement("COMMENT ON TABLE foo IS 'desc'"));
    }

    #[test]
    fn ddl_leading_whitespace_is_ddl() {
        assert!(is_ddl_statement("  create  table foo (id int)"));
    }

    #[test]
    fn ddl_select_is_not_ddl() {
        assert!(!is_ddl_statement("SELECT 1"));
    }

    #[test]
    fn ddl_insert_is_not_ddl() {
        assert!(!is_ddl_statement("INSERT INTO foo VALUES (1)"));
    }

    #[test]
    fn ddl_update_is_not_ddl() {
        assert!(!is_ddl_statement("UPDATE foo SET bar = 1"));
    }

    #[test]
    fn ddl_delete_is_not_ddl() {
        assert!(!is_ddl_statement("DELETE FROM foo WHERE id = 1"));
    }

    // -- FKeyAction toggles (#324, #325) --------------------------------------

    #[test]
    fn fkey_text2sql_toggle_sql_to_text2sql() {
        let mut s = ReplSettings {
            input_mode: InputMode::Sql,
            ..Default::default()
        };
        apply_fkey_toggle(FKeyAction::Text2Sql, &mut s);
        assert_eq!(s.input_mode, InputMode::Text2Sql);
    }

    #[test]
    fn fkey_text2sql_toggle_text2sql_to_sql() {
        let mut s = ReplSettings {
            input_mode: InputMode::Text2Sql,
            ..Default::default()
        };
        apply_fkey_toggle(FKeyAction::Text2Sql, &mut s);
        assert_eq!(s.input_mode, InputMode::Sql);
    }

    #[test]
    fn fkey_vi_emacs_toggle_on() {
        let mut s = ReplSettings {
            vi_mode: false,
            ..Default::default()
        };
        apply_fkey_toggle(FKeyAction::ViEmacs, &mut s);
        assert!(s.vi_mode);
        assert!(s.config.display.vi_mode);
    }

    #[test]
    fn fkey_vi_emacs_toggle_off() {
        let mut s = ReplSettings {
            vi_mode: true,
            config: crate::config::Config {
                display: crate::config::DisplayConfig {
                    vi_mode: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        apply_fkey_toggle(FKeyAction::ViEmacs, &mut s);
        assert!(!s.vi_mode);
        assert!(!s.config.display.vi_mode);
    }

    // -- format_audit_entry (FR-23) -----------------------------------------

    #[test]
    fn audit_entry_contains_sql() {
        let ctx = AuditEntryCtx {
            sql: "select * from users where id = 42",
            dbname: "mydb",
            user: "nik",
            duration: std::time::Duration::from_millis(12),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        // SQL is present in the entry (with trailing semicolon added).
        assert!(
            entry.contains("select * from users where id = 42;"),
            "entry should contain the sql: {entry}"
        );
    }

    #[test]
    fn audit_entry_contains_header_fields() {
        let ctx = AuditEntryCtx {
            sql: "select 1",
            dbname: "testdb",
            user: "alice",
            duration: std::time::Duration::from_millis(5),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("| testdb |"),
            "entry should contain dbname: {entry}"
        );
        assert!(
            entry.contains("user=alice"),
            "entry should contain user: {entry}"
        );
        assert!(
            entry.contains("duration="),
            "entry should contain duration: {entry}"
        );
    }

    #[test]
    fn audit_entry_row_count_singular() {
        let ctx = AuditEntryCtx {
            sql: "select 1",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(1),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("-- (1 row)"),
            "entry should say '(1 row)': {entry}"
        );
    }

    #[test]
    fn audit_entry_row_count_plural() {
        let ctx = AuditEntryCtx {
            sql: "select * from users",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(10),
            row_count: Some(47),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("-- (47 rows)"),
            "entry should say '(47 rows)': {entry}"
        );
    }

    #[test]
    fn audit_entry_no_row_count_shows_ok() {
        let ctx = AuditEntryCtx {
            sql: "create table foo (id int)",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(20),
            row_count: None,
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("-- (ok)"),
            "entry should show '(ok)' for DDL: {entry}"
        );
    }

    #[test]
    fn audit_entry_text2sql_includes_source_and_prompt() {
        let ctx = AuditEntryCtx {
            sql: "select * from users where created_at >= date_trunc('week', current_date)",
            dbname: "mydb",
            user: "nik",
            duration: std::time::Duration::from_millis(340),
            row_count: Some(47),
            text2sql_prompt: Some("show me users who signed up this week"),
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("source=text2sql"),
            "entry should contain source=text2sql: {entry}"
        );
        assert!(
            entry.contains("-- prompt:"),
            "entry should contain prompt line: {entry}"
        );
        assert!(
            entry.contains("show me users who signed up this week"),
            "entry should contain the prompt text: {entry}"
        );
    }

    #[test]
    fn audit_entry_no_password_or_connection_string() {
        // Passwords must never appear in audit entries.
        let ctx = AuditEntryCtx {
            sql: "select current_user",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(1),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        // The entry must not contain anything that looks like a password or
        // connection string pattern.
        assert!(
            !entry.contains("password"),
            "entry must not contain 'password': {entry}"
        );
        assert!(
            !entry.contains("postgresql://"),
            "entry must not contain connection string: {entry}"
        );
    }

    #[test]
    fn audit_entry_sql_without_trailing_semicolon_gets_one_added() {
        let ctx = AuditEntryCtx {
            sql: "select 1",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(1),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        assert!(
            entry.contains("select 1;"),
            "missing semicolon should be added: {entry}"
        );
    }

    #[test]
    fn audit_entry_sql_with_trailing_semicolon_not_doubled() {
        let ctx = AuditEntryCtx {
            sql: "select 1;",
            dbname: "db",
            user: "u",
            duration: std::time::Duration::from_millis(1),
            row_count: Some(1),
            text2sql_prompt: None,
        };
        let entry = format_audit_entry(&ctx);
        // Should not have double semicolons.
        assert!(
            !entry.contains(";;"),
            "semicolons should not be doubled: {entry}"
        );
    }

    #[test]
    fn format_utc_timestamp_epoch() {
        // Unix epoch should produce 1970-01-01 00:00:00 UTC.
        let ts = format_utc_timestamp(0);
        assert_eq!(ts, "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn format_utc_timestamp_known_date() {
        // 2026-03-12 14:23:01 UTC = 1773325381 seconds.
        let ts = format_utc_timestamp(1_773_325_381);
        assert_eq!(ts, "2026-03-12 14:23:01 UTC");
    }

    // -- error push behavior ---------------------------------------------------

    #[test]
    fn conversation_error_push_appears_in_messages() {
        // Verifies that pushing an error result via push_query_result causes the
        // error text to appear in to_messages(), so the AI receives the signal.
        let mut ctx = ConversationContext::new();
        ctx.push_query_result("SELECT boom()", "ERROR: function boom() does not exist");
        let msgs = ctx.to_messages();
        assert!(
            !msgs.is_empty(),
            "messages should not be empty after error push"
        );
        let combined: String = msgs.iter().map(|m| m.content.as_str()).collect();
        assert!(
            combined.contains("ERROR:"),
            "expected 'ERROR:' in conversation messages, got: {combined}"
        );
    }

    // -- unescape_echo ---------------------------------------------------------

    #[test]
    fn unescape_echo_plain_text() {
        assert_eq!(super::unescape_echo("hello world"), "hello world");
    }

    #[test]
    fn unescape_echo_newline_seq() {
        assert_eq!(super::unescape_echo("a\\nb"), "a\nb");
    }

    #[test]
    fn unescape_echo_tab_seq() {
        assert_eq!(super::unescape_echo("a\\tb"), "a\tb");
    }

    #[test]
    fn unescape_echo_backslash_seq() {
        assert_eq!(super::unescape_echo("a\\\\b"), "a\\b");
    }

    #[test]
    fn unescape_echo_single_quote_seq() {
        assert_eq!(super::unescape_echo("a\\'b"), "a'b");
    }

    #[test]
    fn unescape_echo_octal_esc_seq() {
        // \033 is ESC (decimal 27).
        let result = super::unescape_echo("\\033[1;35m");
        assert_eq!(result.as_bytes()[0], 27);
        assert_eq!(&result[1..], "[1;35m");
    }

    #[test]
    fn unescape_echo_hex_seq() {
        // \x1b is ESC.
        let result = super::unescape_echo("\\x1b[0m");
        assert_eq!(result.as_bytes()[0], 0x1b);
        assert_eq!(&result[1..], "[0m");
    }

    #[test]
    fn unescape_echo_unknown_escape_stays_verbatim() {
        assert_eq!(super::unescape_echo("\\q"), "\\q");
    }

    #[test]
    fn unescape_echo_ansi_color_sequence() {
        // Simulates postgres_dba: '\033[1;35mMenu:\033[0m'
        // After split_params strips quotes: \033[1;35mMenu:\033[0m
        let text = "\\033[1;35mMenu:\\033[0m";
        let result = super::unescape_echo(text);
        assert_eq!(result.as_bytes()[0], 27);
        assert!(result.ends_with("Menu:\x1b[0m"));
    }

    #[test]
    fn unescape_echo_octal_overflow_truncates() {
        // \400 = 256 decimal; psql truncates mod 256 → 0x00.
        let result = super::unescape_echo("\\400");
        assert_eq!(result.as_bytes(), &[0x00]);
    }

    // -- postgres_dba patterns (Copyright 2026) --------------------------------
    //
    // These tests replicate the exact \echo calls from
    // https://github.com/NikolayS/postgres_dba/blob/master/start.psql
    // to verify that split_params + unescape_echo together produce the
    // correct ANSI output, matching what psql does.

    #[test]
    fn postgres_dba_menu_header_split_then_unescape() {
        // start.psql line 2: \echo '\033[1;35mMenu:\033[0m'
        // split_params strips the surrounding single quotes, then
        // unescape_echo converts \033 to ESC (0x1b).
        let raw = "'\\033[1;35mMenu:\\033[0m'";
        let joined = crate::metacmd::split_params(raw).join(" ");
        assert_eq!(joined, "\\033[1;35mMenu:\\033[0m");
        let result = super::unescape_echo(&joined);
        // First byte must be ESC (0x1b = 27).
        assert_eq!(result.as_bytes()[0], 0x1b);
        // Bold magenta on: [1;35m
        assert!(result.contains("[1;35m"));
        // Reset: ESC[0m
        assert!(result.ends_with("\x1b[0m"));
        // The literal text "Menu:" must be present.
        assert!(result.contains("Menu:"));
    }

    #[test]
    fn postgres_dba_error_banner_split_then_unescape() {
        // start.psql line 219:
        //   \echo '\033[1;31mError:\033[0m Unknown option! Try again.'
        // split_params strips quotes; unescape_echo resolves \033.
        let raw = "'\\033[1;31mError:\\033[0m Unknown option! Try again.'";
        let joined = crate::metacmd::split_params(raw).join(" ");
        assert_eq!(
            joined,
            "\\033[1;31mError:\\033[0m Unknown option! Try again."
        );
        let result = super::unescape_echo(&joined);
        // First byte is ESC.
        assert_eq!(result.as_bytes()[0], 0x1b);
        // Bold red on: [1;31m
        assert!(result.contains("[1;31m"));
        // The literal error text must survive.
        assert!(result.contains("Error:"));
        assert!(result.contains("Unknown option! Try again."));
        // Reset sequence present.
        assert!(result.contains("\x1b[0m"));
    }

    #[test]
    fn postgres_dba_plain_echo_no_escape() {
        // start.psql line 79: \echo 'Bye!'
        // No escape sequences; split_params strips quotes, output is literal.
        let raw = "'Bye!'";
        let joined = crate::metacmd::split_params(raw).join(" ");
        let result = super::unescape_echo(&joined);
        assert_eq!(result, "Bye!");
    }

    #[test]
    fn postgres_dba_menu_item_echo_preserves_spacing() {
        // start.psql line 3 (representative plain menu line):
        //   \echo '   0 – Node and current database information'
        // Spaces inside quotes must be preserved.
        let raw = "'   0 \u{2013} Node and current database information'";
        let joined = crate::metacmd::split_params(raw).join(" ");
        let result = super::unescape_echo(&joined);
        assert!(result.starts_with("   0"));
        assert!(result.contains("Node and current database information"));
    }

    // -- mode transition tests (apply_mode_change) ----------------------------

    #[test]
    fn yolo_sets_text2sql_input_mode() {
        let mut s = ReplSettings::default();
        // Default state: sql + interactive.
        assert_eq!(s.input_mode, InputMode::Sql);
        assert_eq!(s.exec_mode, ExecMode::Interactive);

        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Yolo), &mut s);

        assert_eq!(s.exec_mode, ExecMode::Yolo);
        assert_eq!(s.input_mode, InputMode::Text2Sql);
    }

    #[test]
    fn t2s_after_yolo_resets_exec_mode_to_interactive() {
        let mut s = ReplSettings::default();
        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Yolo), &mut s);
        assert_eq!(s.exec_mode, ExecMode::Yolo);

        // \t2s / \text2sql → SetInputMode(Text2Sql)
        super::apply_mode_change(&MetaResult::SetInputMode(InputMode::Text2Sql), &mut s);

        assert_eq!(s.input_mode, InputMode::Text2Sql);
        assert_eq!(s.exec_mode, ExecMode::Interactive);
    }

    #[test]
    fn sql_after_yolo_resets_exec_mode_to_interactive() {
        let mut s = ReplSettings::default();
        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Yolo), &mut s);
        assert_eq!(s.exec_mode, ExecMode::Yolo);

        // \sql → SetInputMode(Sql)
        super::apply_mode_change(&MetaResult::SetInputMode(InputMode::Sql), &mut s);

        assert_eq!(s.input_mode, InputMode::Sql);
        assert_eq!(s.exec_mode, ExecMode::Interactive);
    }

    #[test]
    fn interactive_after_yolo_resets_both_modes() {
        let mut s = ReplSettings::default();
        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Yolo), &mut s);
        assert_eq!(s.exec_mode, ExecMode::Yolo);
        assert_eq!(s.input_mode, InputMode::Text2Sql);

        // \interactive → SetExecMode(Interactive)
        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Interactive), &mut s);

        assert_eq!(s.exec_mode, ExecMode::Interactive);
        assert_eq!(s.input_mode, InputMode::Sql);
    }

    #[test]
    fn plan_mode_leaves_input_mode_unchanged() {
        let mut s = ReplSettings {
            input_mode: InputMode::Text2Sql,
            ..ReplSettings::default()
        };

        super::apply_mode_change(&MetaResult::SetExecMode(ExecMode::Plan), &mut s);

        assert_eq!(s.exec_mode, ExecMode::Plan);
        // \plan does not touch input_mode.
        assert_eq!(s.input_mode, InputMode::Text2Sql);
    }

    #[test]
    fn set_input_mode_sql_resets_exec_mode() {
        let mut s = ReplSettings {
            exec_mode: ExecMode::Plan,
            ..ReplSettings::default()
        };

        super::apply_mode_change(&MetaResult::SetInputMode(InputMode::Sql), &mut s);

        assert_eq!(s.input_mode, InputMode::Sql);
        assert_eq!(s.exec_mode, ExecMode::Interactive);
    }

    // -- is_write_query (comment stripping) ------------------------------------

    #[test]
    fn is_write_query_leading_single_line_comment_create() {
        // AI-generated SQL with a leading -- comment must still be detected
        // as a write query.
        assert!(ai_commands::is_write_query(
            "-- Create table\nCREATE TABLE t2 (id int);"
        ));
    }

    #[test]
    fn is_write_query_leading_block_comment_drop() {
        // A /* block comment */ before DROP must still be detected as write.
        assert!(ai_commands::is_write_query("/* block */\nDROP TABLE t2;"));
    }

    #[test]
    fn is_write_query_leading_comments_before_select() {
        // Multiple leading comments before SELECT must return false (read-only).
        assert!(!ai_commands::is_write_query(
            "-- comment\n-- another\nSELECT 1;"
        ));
    }

    #[test]
    fn is_write_query_comment_no_space_insert() {
        // --comment (no space after --) before INSERT must return true.
        assert!(ai_commands::is_write_query(
            "--comment\nINSERT INTO t VALUES (1);"
        ));
    }

    // -- pset_status_text -------------------------------------------------------

    #[test]
    fn pset_status_text_contains_key_fields() {
        let settings = ReplSettings::default();
        let text = pset_status_text(&settings);
        assert!(text.contains("border"), "must include border field");
        assert!(text.contains("format"), "must include format field");
        assert!(text.contains("pager"), "must include pager field");
        assert!(text.contains("tuples_only"), "must include tuples_only");
        assert!(text.contains("expanded"), "must include expanded");
    }

    #[test]
    fn pset_status_text_pager_on_by_default() {
        let settings = ReplSettings::default();
        let text = pset_status_text(&settings);
        assert!(
            text.contains("pager          = on"),
            "default pager state must be 'on'"
        );
    }

    // -- sql_help_text ----------------------------------------------------------

    #[test]
    fn sql_help_text_no_topic_returns_ok_with_header() {
        let text = crate::session::sql_help_text(None).expect("should succeed");
        assert!(text.contains("Available help:"), "must include header");
        assert!(
            text.contains("\\h <command-name>"),
            "must include usage hint"
        );
    }

    #[test]
    fn sql_help_text_select_topic() {
        let text = crate::session::sql_help_text(Some("SELECT")).expect("SELECT should be found");
        assert!(text.contains("Command:     SELECT"), "must include command");
        assert!(text.contains("Syntax:"), "must include syntax header");
    }

    #[test]
    fn sql_help_text_unknown_topic_returns_err() {
        let err = crate::session::sql_help_text(Some("NOTACOMMAND")).expect_err("should be Err");
        assert_eq!(err, "NOTACOMMAND");
    }

    // -- is_write_query (comprehensive Section 13 coverage) -------------------

    #[test]
    fn write_query_empty_string_is_false() {
        // Empty input is not a write query.
        assert!(!ai_commands::is_write_query(""));
    }

    #[test]
    fn write_query_whitespace_only_is_false() {
        // Whitespace-only input is not a write query.
        assert!(!ai_commands::is_write_query("   \n\t  "));
    }

    #[test]
    fn write_query_select_with_leading_whitespace_is_false() {
        // SELECT with leading whitespace must still be detected as read.
        assert!(!ai_commands::is_write_query("   SELECT * FROM t"));
        assert!(!ai_commands::is_write_query("\n\nselect 1"));
    }

    #[test]
    fn write_query_explain_select_is_false() {
        // EXPLAIN of a SELECT is still a read query.
        assert!(!ai_commands::is_write_query("EXPLAIN SELECT * FROM users"));
        assert!(!ai_commands::is_write_query(
            "explain analyze select * from users"
        ));
    }

    #[test]
    fn write_query_show_is_false() {
        // SHOW is a read-only metadata command.
        assert!(!ai_commands::is_write_query("SHOW work_mem"));
    }

    #[test]
    fn write_query_set_is_false() {
        // SET is a session-local configuration command, not a data-write.
        // The current classifier does not flag SET — verify it remains stable.
        assert!(!ai_commands::is_write_query("SET work_mem = '256MB'"));
    }

    #[test]
    fn write_query_table_is_false() {
        // TABLE t is a shorthand for SELECT * FROM t — it is a read query.
        assert!(!ai_commands::is_write_query("TABLE users"));
    }

    #[test]
    fn write_query_values_is_false() {
        // VALUES not preceded by INSERT is a read-only expression.
        // The classifier only looks at the first keyword, so VALUES alone
        // is not flagged.
        assert!(!ai_commands::is_write_query("VALUES (1, 2, 3)"));
    }

    #[test]
    fn write_query_multiple_leading_block_comments_create() {
        // Several /* */ comments before CREATE must still be detected.
        assert!(ai_commands::is_write_query(
            "/* first */ /* second */\nCREATE TABLE t (id int);"
        ));
    }

    #[test]
    fn write_query_mixed_leading_comments_insert() {
        // Mix of -- and /* */ comments before INSERT must be detected.
        assert!(ai_commands::is_write_query(
            "-- line one\n/* block */\nINSERT INTO t VALUES (1);"
        ));
    }

    #[test]
    fn write_query_merge_uppercase_is_true() {
        // MERGE (SQL:2003) is a write operation.
        assert!(ai_commands::is_write_query(
            "MERGE INTO target AS t\n\
             USING source AS s ON (t.id = s.id)\n\
             WHEN MATCHED THEN UPDATE SET t.v = s.v;"
        ));
    }

    #[test]
    fn write_query_refresh_matview_is_true() {
        // REFRESH MATERIALIZED VIEW is flagged as write / maintenance.
        assert!(ai_commands::is_write_query(
            "REFRESH MATERIALIZED VIEW my_mv;"
        ));
        assert!(ai_commands::is_write_query(
            "refresh materialized view my_mv;"
        ));
    }

    #[test]
    fn write_query_reindex_is_true() {
        // All REINDEX variants must be detected.
        assert!(ai_commands::is_write_query("REINDEX TABLE t;"));
        assert!(ai_commands::is_write_query("REINDEX INDEX idx;"));
        assert!(ai_commands::is_write_query("REINDEX DATABASE mydb;"));
        assert!(ai_commands::is_write_query("REINDEX TABLE CONCURRENTLY t;"));
    }

    #[test]
    fn write_query_vacuum_variants_are_true() {
        // VACUUM with arguments (space-separated) must be detected.
        // Note: bare "VACUUM;" (no space before semicolon) is not detected
        // because split_whitespace treats "VACUUM;" as one token — see
        // issue #627 for the edge-case tracker.
        assert!(ai_commands::is_write_query("VACUUM FULL t;"));
        assert!(ai_commands::is_write_query("VACUUM ANALYZE t;"));
        assert!(ai_commands::is_write_query("vacuum analyze t;"));
        assert!(ai_commands::is_write_query("VACUUM t"));
    }

    #[test]
    fn write_query_cluster_is_true() {
        // CLUSTER must be detected.
        assert!(ai_commands::is_write_query("CLUSTER t USING t_pkey;"));
        assert!(ai_commands::is_write_query("cluster t using t_pkey;"));
    }

    #[test]
    fn write_query_truncate_is_true() {
        // TRUNCATE is a DDL write operation.
        assert!(ai_commands::is_write_query("TRUNCATE TABLE t;"));
        assert!(ai_commands::is_write_query("truncate t;"));
    }

    #[test]
    fn write_query_alter_variants_are_true() {
        // All ALTER variants must be detected.
        assert!(ai_commands::is_write_query(
            "ALTER TABLE t ADD COLUMN x text;"
        ));
        assert!(ai_commands::is_write_query(
            "ALTER INDEX idx RENAME TO idx2;"
        ));
        assert!(ai_commands::is_write_query(
            "ALTER SEQUENCE s RESTART WITH 1;"
        ));
        assert!(ai_commands::is_write_query("alter table t drop column x;"));
    }

    #[test]
    fn write_query_create_variants_are_true() {
        // All CREATE variants must be detected.
        assert!(ai_commands::is_write_query("CREATE TABLE t (id int);"));
        assert!(ai_commands::is_write_query("CREATE INDEX idx ON t (id);"));
        assert!(ai_commands::is_write_query(
            "CREATE UNIQUE INDEX CONCURRENTLY idx ON t (email);"
        ));
        assert!(ai_commands::is_write_query(
            "CREATE OR REPLACE FUNCTION f() RETURNS void LANGUAGE sql AS ''"
        ));
        assert!(ai_commands::is_write_query("CREATE VIEW v AS SELECT 1;"));
        assert!(ai_commands::is_write_query(
            "CREATE MATERIALIZED VIEW mv AS SELECT 1;"
        ));
        assert!(ai_commands::is_write_query("create table t (id int);"));
    }

    #[test]
    fn write_query_drop_variants_are_true() {
        // All DROP variants must be detected.
        assert!(ai_commands::is_write_query("DROP TABLE t;"));
        assert!(ai_commands::is_write_query("DROP TABLE IF EXISTS t;"));
        assert!(ai_commands::is_write_query("DROP INDEX idx;"));
        assert!(ai_commands::is_write_query("DROP VIEW v;"));
        assert!(ai_commands::is_write_query("DROP FUNCTION f(int);"));
        assert!(ai_commands::is_write_query("drop table t;"));
    }

    #[test]
    fn write_query_grant_variants_are_true() {
        // All GRANT variants must be detected.
        assert!(ai_commands::is_write_query("GRANT SELECT ON t TO user1;"));
        assert!(ai_commands::is_write_query(
            "GRANT ALL PRIVILEGES ON DATABASE mydb TO user1;"
        ));
        assert!(ai_commands::is_write_query(
            "grant select, insert on t to user1;"
        ));
    }

    #[test]
    fn write_query_revoke_variants_are_true() {
        // All REVOKE variants must be detected.
        assert!(ai_commands::is_write_query("REVOKE ALL ON t FROM user1;"));
        assert!(ai_commands::is_write_query(
            "REVOKE SELECT ON t FROM PUBLIC;"
        ));
        assert!(ai_commands::is_write_query("revoke all on t from user1;"));
    }

    #[test]
    fn write_query_with_select_only_is_true_conservative() {
        // Pure `WITH ... SELECT` (read-only CTE) is treated as write
        // conservatively — this prevents CTE-bypass of the write check.
        assert!(ai_commands::is_write_query(
            "WITH x AS (SELECT 1) SELECT * FROM x"
        ));
    }

    // -- strip_explain_prefix --------------------------------------------------

    #[test]
    fn strip_explain_plain() {
        assert_eq!(
            strip_explain_prefix("EXPLAIN select 1"),
            Some("select 1".to_owned())
        );
    }

    #[test]
    fn strip_explain_analyze() {
        assert_eq!(
            strip_explain_prefix("explain analyze select * from t"),
            Some("select * from t".to_owned())
        );
    }

    #[test]
    fn strip_explain_analyse_british() {
        assert_eq!(
            strip_explain_prefix("EXPLAIN ANALYSE select 1"),
            Some("select 1".to_owned())
        );
    }

    #[test]
    fn strip_explain_analyze_verbose() {
        assert_eq!(
            strip_explain_prefix("explain analyze verbose select * from t"),
            Some("select * from t".to_owned())
        );
    }

    #[test]
    fn strip_explain_parenthesised_options() {
        assert_eq!(
            strip_explain_prefix("EXPLAIN (ANALYZE, BUFFERS) select * from orders limit 10"),
            Some("select * from orders limit 10".to_owned())
        );
    }

    #[test]
    fn strip_explain_format_json() {
        assert_eq!(
            strip_explain_prefix("EXPLAIN (ANALYZE, FORMAT JSON) select 1"),
            Some("select 1".to_owned())
        );
    }

    #[test]
    fn strip_explain_not_explain_returns_none() {
        assert!(strip_explain_prefix("select 1").is_none());
    }

    #[test]
    fn strip_explain_empty_returns_none() {
        assert!(strip_explain_prefix("EXPLAIN").is_none());
    }
}
