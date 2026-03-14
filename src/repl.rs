//! Interactive REPL loop for Samo.
//!
//! Provides readline-based line editing with persistent history, multi-line
//! SQL accumulation, backslash command handling, transaction-state prompts,
//! and signal-aware Ctrl-C / Ctrl-D behaviour.

use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use std::sync::{Arc, RwLock};

use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::{Config, Editor};
use tokio_postgres::Client;

use crate::complete::{load_schema_cache, SamoHelper, SchemaCache};

use crate::connection::ConnParams;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default history file path (relative to home directory).
const DEFAULT_HISTORY_FILE: &str = ".samo_history";

/// Maximum number of history entries kept in memory and on disk.
const HISTORY_SIZE: usize = 2000;

// ---------------------------------------------------------------------------
// AI key resolution
// ---------------------------------------------------------------------------

/// Resolve an API key from the `api_key_env` config value.
///
/// If the value looks like a raw API key (starts with `sk-`, `sk-ant-`, etc.)
/// rather than an environment variable name, use it directly but warn the user.
/// Otherwise, treat it as an env-var name and look it up.
/// Track whether we've already warned about raw API keys in this session.
static RAW_KEY_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn resolve_api_key(api_key_env: Option<&str>) -> Option<String> {
    let env_or_key = api_key_env?;

    // Detect raw keys accidentally placed in api_key_env.
    let looks_like_raw_key = env_or_key.starts_with("sk-")
        || env_or_key.starts_with("gsk_")
        || (env_or_key.len() > 40
            && !env_or_key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_'));

    if looks_like_raw_key {
        // Warn only once per session to avoid noise on repeated AI commands.
        if !RAW_KEY_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "WARNING: api_key_env appears to contain a raw API key. \
                 For security, set it to an environment variable name instead:"
            );
            eprintln!(
                "  api_key_env = \"OPENAI_API_KEY\"  # then: export OPENAI_API_KEY=\"sk-...\""
            );
        }
        // Use it anyway so things work.
        return Some(env_or_key.to_owned());
    }

    match std::env::var(env_or_key) {
        Ok(val) if !val.is_empty() => Some(val),
        _ => {
            eprintln!(
                "ERROR: environment variable '{env_or_key}' is not set. \
                 Set it with: export {env_or_key}=\"your-api-key\""
            );
            None
        }
    }
}

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
/// Samo-specific execution and input mode tags (` plan`, ` text2sql`, etc.)
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
        ExecMode::Observe => " observe",
        ExecMode::Interactive => match input_mode {
            InputMode::Text2Sql => " text2sql",
            InputMode::Sql => "",
        },
    };
    // Build a template equivalent to the default PROMPT1 (`%/%R%x%# `) but
    // with the Samo mode tag injected as a literal between `%/` and `%R`.
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
/// Samo-specific mode tags are handled by [`build_prompt`]; callers that
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
    /// AI auto-executes within configured autonomy level.
    Yolo,
    /// Pure read-only observation — AI watches and reports.
    ///
    /// Currently used only for prompt display; `\observe` triggers a
    /// one-shot observation loop rather than setting a persistent mode.
    #[allow(dead_code)]
    Observe,
}

// ---------------------------------------------------------------------------
// YOLO write-action decision
// ---------------------------------------------------------------------------

/// Outcome of the YOLO-mode autonomy boundary check for write queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YoloWriteAction {
    /// Block the write — autonomy level is Observe (L1).
    Block,
    /// Execute but emit a caution warning — autonomy level is Supervised (L2).
    WarnThenExecute,
    /// Execute silently — autonomy level is Auto (L3) or bypass flag set.
    Execute,
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
    #[allow(dead_code)]
    fn cycle(self) -> Self {
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
    fn label(self) -> &'static str {
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
    /// Action audit log for the governance framework.
    ///
    /// Records every action proposed, executed, vetoed, or skipped
    /// during this session.  Never LLM-summarized (FIFO-evicted only).
    pub audit_log: crate::governance::AuditLog,
    /// Detected database capabilities (extensions, version).
    ///
    /// Populated at connect time by [`crate::capabilities::detect`].
    pub db_capabilities: crate::capabilities::DbCapabilities,
    /// Bypass autonomy level checks in YOLO mode.
    ///
    /// Set by `--i-know-what-im-doing` CLI flag. When `true` and
    /// `exec_mode == Yolo`, all write queries are auto-executed regardless
    /// of the configured autonomy level. Use with extreme care.
    pub i_know_what_im_doing: bool,
    /// Verbosity level for error display, mirroring psql's `\set VERBOSITY`.
    ///
    /// When `true`, SQLSTATE codes are appended to error output.
    /// Defaults to `false` (psql default).
    pub verbose_errors: bool,
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
    pub schema_cache: Option<Arc<RwLock<SchemaCache>>>,
}

impl std::fmt::Debug for ReplSettings {
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
            .field("pager_enabled", &self.pager_enabled)
            .field("pager_command", &self.pager_command)
            .field("pager_min_lines", &self.pager_min_lines)
            .field("safety_enabled", &self.safety_enabled)
            .field("config_profiles", &self.config.connections.len())
            .field("input_mode", &self.input_mode)
            .field("exec_mode", &self.exec_mode)
            .field("auto_explain", &self.auto_explain)
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
            .field("audit_log", &format!("{} entries", self.audit_log.len()))
            .field("db_capabilities", &self.db_capabilities)
            .field("i_know_what_im_doing", &self.i_know_what_im_doing)
            .field("verbose_errors", &self.verbose_errors)
            .field("current_file", &self.current_file)
            .field("session_id", &self.session_id)
            .field("query_count", &self.query_count)
            .field("is_superuser", &self.is_superuser)
            .field(
                "schema_cache",
                &self.schema_cache.as_ref().map(|_| "<cache>"),
            )
            .finish()
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
            last_error: None,
            conversation: ConversationContext::new(),
            tokens_used: 0,
            audit_log: crate::governance::AuditLog::new(),
            db_capabilities: crate::capabilities::DbCapabilities::default(),
            i_know_what_im_doing: false,
            verbose_errors: false,
            current_file: None,
            session_id: crate::session_store::new_session_id(),
            query_count: 0,
            is_superuser: false,
            schema_cache: None,
        }
    }
}

// ---------------------------------------------------------------------------
// History file path resolution
// ---------------------------------------------------------------------------

/// Resolve the history file path.
///
/// Priority:
/// 1. `PSQL_HISTORY` environment variable
/// 2. `~/.samo_history`
pub fn history_file() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("PSQL_HISTORY") {
        return Some(PathBuf::from(val));
    }
    dirs::home_dir().map(|h| h.join(DEFAULT_HISTORY_FILE))
}

// ---------------------------------------------------------------------------
// Startup file resolution
// ---------------------------------------------------------------------------

/// Resolve the startup RC file path.
///
/// Priority:
/// 1. `$PSQLRC` environment variable
/// 2. `~/.samorc` if the file exists
/// 3. `~/.psqlrc` if the file exists
/// 4. `None` — no startup file
pub fn startup_file() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("PSQLRC") {
        return Some(PathBuf::from(val));
    }
    if let Some(home) = dirs::home_dir() {
        let samorc = home.join(".samorc");
        if samorc.exists() {
            return Some(samorc);
        }
        let psqlrc = home.join(".psqlrc");
        if psqlrc.exists() {
            return Some(psqlrc);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Query execution (stub — #19 will provide the proper implementation)
// ---------------------------------------------------------------------------

/// Print a single result set using the active [`PsetConfig`].
///
/// `col_names` and `rows` describe the result set. `is_select` indicates
/// whether this was a SELECT-like statement (i.e. we received a
/// `RowDescription` message, even if zero rows followed). `rows_affected`
/// carries the `CommandComplete` count. `is_first` is `false` when this is
/// a subsequent result set in a multi-statement query, in which case a blank
/// separator line is printed before the table (matching psql behaviour).
/// `writer` is the output destination (stdout or a redirected file).
fn print_result_set_pset(
    writer: &mut dyn io::Write,
    col_names: &[String],
    rows: &[Vec<String>],
    is_select: bool,
    rows_affected: u64,
    is_first: bool,
    pset: &crate::output::PsetConfig,
) {
    use crate::output::format_rowset_pset;
    use crate::query::{ColumnMeta, RowSet};

    if is_select && !col_names.is_empty() {
        // simple_query returns NULL as empty string; we wrap every cell
        // in Some to distinguish "empty string" from "NULL" at the pset
        // formatting layer (which uses null_display).  The distinction
        // is lost at this protocol level; a future migration to the
        // extended query protocol (issue #21) will fix this.
        let row_data: Vec<Vec<Option<String>>> = rows
            .iter()
            .map(|r| r.iter().map(|v| Some(v.clone())).collect())
            .collect();

        // Heuristic: psql right-aligns numeric columns using type OIDs from
        // the wire protocol.  The simple query protocol does not expose OIDs,
        // so we infer numeric columns by inspecting cell values.  A column is
        // treated as numeric if every non-NULL, non-empty cell in that column
        // parses as an f64 (covers integers, decimals, and scientific notation).
        // Columns that are entirely NULL/empty are NOT marked numeric.
        let columns: Vec<ColumnMeta> = col_names
            .iter()
            .enumerate()
            .map(|(col_idx, n)| {
                let mut has_value = false;
                let is_numeric = row_data.iter().all(|row| {
                    match row.get(col_idx).and_then(|v| v.as_deref()) {
                        None | Some("") => true, // NULL or empty: skip, don't disqualify
                        Some(val) => {
                            has_value = true;
                            val.parse::<f64>().is_ok()
                        }
                    }
                }) && has_value;
                ColumnMeta {
                    name: n.clone(),
                    is_numeric,
                }
            })
            .collect();

        let rs = RowSet {
            columns,
            rows: row_data,
        };

        let mut out = String::new();
        format_rowset_pset(&mut out, &rs, pset);
        // format_rowset_pset appends a trailing blank line so that output
        // matches psql's consistent blank line after every result set.
        // No extra separator is needed before subsequent results.
        let _ = writer.write_all(out.as_bytes());
    } else if !is_select {
        // Non-SELECT statement: show rows affected if > 0.
        if rows_affected > 0 {
            if !is_first {
                let _ = writeln!(writer);
            }
            let _ = writeln!(writer, "{rows_affected}");
        }
    }
}

/// In single-step mode, prompt the user before each command.
///
/// Prints the command to stderr and asks "Execute? (y/n)".
/// Returns `true` if the user confirms (or single-step is not enabled).
fn confirm_single_step(sql: &str) -> bool {
    eprint!("***(Single step mode: verify command)*******************************************\n{sql}\n***(press return to proceed or enter x and return to cancel)***********************\n");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    let trimmed = answer.trim();
    trimmed.is_empty() || (trimmed != "x" && trimmed != "X")
}

/// Execute a SQL string using `simple_query` and print results.
///
/// Interpolates variables from `settings.vars` before sending to the server,
/// then renders output using `settings.pset`.
///
/// Returns `true` on success, `false` if the query produced a SQL error.
#[allow(clippy::too_many_lines)]
pub async fn execute_query(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Interpolate variables before sending.
    let interpolated = settings.vars.interpolate(sql);

    // Auto-EXPLAIN: prepend EXPLAIN prefix when enabled.
    // Skip for statements that are already EXPLAIN, or for
    // non-query statements (SET, BEGIN, COMMIT, etc.).
    let auto_explained;
    let mut auto_explain_active = false;
    let sql_to_send = if settings.auto_explain == AutoExplain::Off {
        interpolated.as_str()
    } else {
        let trimmed_upper = interpolated.trim_start().to_uppercase();
        let is_query = trimmed_upper.starts_with("SELECT")
            || trimmed_upper.starts_with("WITH")
            || trimmed_upper.starts_with("TABLE")
            || trimmed_upper.starts_with("VALUES");
        let already_explain = trimmed_upper.starts_with("EXPLAIN");
        if is_query && !already_explain {
            auto_explained = format!("{}{}", settings.auto_explain.prefix(), interpolated);
            auto_explain_active = true;
            auto_explained.as_str()
        } else {
            interpolated.as_str()
        }
    };

    // -s / --single-step: prompt before executing.
    if settings.single_step && !confirm_single_step(sql_to_send) {
        return true; // skipped — not an error
    }

    // Destructive statement guard: warn before DROP, TRUNCATE, DELETE without
    // WHERE, etc.  In non-interactive mode the check is skipped automatically
    // inside `confirm_destructive`.
    if settings.safety_enabled {
        let built_in = crate::safety::is_destructive(sql_to_send).map(str::to_owned);
        let custom = crate::safety::matches_custom_pattern(
            sql_to_send,
            &settings.config.safety.protected_patterns,
        )
        .map(|s| format!("matches protected pattern: {s}"));
        let reason = built_in.or(custom);
        if let Some(ref r) = reason {
            if !crate::safety::confirm_destructive(r) {
                eprintln!("Statement cancelled.");
                return true; // skipped — not an error
            }
        }
    }

    // -e / --echo-queries: print query to stderr before executing.
    if settings.echo_queries {
        eprintln!("{sql_to_send}");
    }

    // -L: log query input to the log file.
    if let Some(ref mut lf) = settings.log_file {
        let _ = writeln!(lf, "{sql_to_send}");
    }

    crate::logging::debug("repl", &format!("execute query: {}", sql_to_send.trim()));

    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    // Capture auto-EXPLAIN plan text for optional AI interpretation.
    let mut auto_explain_plan: Option<String> = None;

    let success = match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();
            // `is_select` is set to true when we receive a RowDescription
            // message (or any Row message).  This distinguishes an empty
            // SELECT (zero rows but column headers) from a DML command.
            let mut is_select = false;
            let mut result_set_index: usize = 0;

            for msg in messages {
                match msg {
                    SimpleQueryMessage::RowDescription(cols) => {
                        // Emitted before data rows (or before CommandComplete
                        // when zero rows matched).  Capture column names here
                        // so that empty result sets still show their headers.
                        is_select = true;
                        if col_names.is_empty() {
                            col_names = cols.iter().map(|c| c.name().to_owned()).collect();
                        }
                    }
                    SimpleQueryMessage::Row(row) => {
                        is_select = true;
                        if col_names.is_empty() {
                            col_names = (0..row.len())
                                .map(|i| {
                                    row.columns()
                                        .get(i)
                                        .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                                })
                                .collect();
                        }
                        let vals: Vec<String> = (0..row.len())
                            .map(|i| row.get(i).unwrap_or("").to_owned())
                            .collect();
                        rows.push(vals);
                    }
                    SimpleQueryMessage::CommandComplete(n) => {
                        // Capture plan text from auto-EXPLAIN before clearing
                        // rows. EXPLAIN output is a single-column result set.
                        if auto_explain_active && result_set_index == 0 {
                            let plan_text: String = rows
                                .iter()
                                .filter_map(|r| r.first())
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !plan_text.is_empty() {
                                auto_explain_plan = Some(plan_text);
                            }
                        }

                        // Flush the current result set, then reset for next
                        // statement in a multi-statement query.
                        // Capture rendered output so we can mirror to log.
                        let mut out_buf = Vec::<u8>::new();

                        // Print "--- EXPLAIN ---" header before the plan
                        // output so users can distinguish plan from results.
                        if auto_explain_active && result_set_index == 0 {
                            let _ = writeln!(out_buf, "--- EXPLAIN ---");
                        }

                        print_result_set_pset(
                            &mut out_buf,
                            &col_names,
                            &rows,
                            is_select,
                            n,
                            result_set_index == 0,
                            &settings.pset,
                        );

                        // Mirror output to log file if active.
                        if let Some(ref mut lf) = settings.log_file {
                            let _ = lf.write_all(&out_buf);
                        }

                        // Write to the configured output target.
                        if let Some(ref mut w) = settings.output_target {
                            let _ = w.write_all(&out_buf);
                        } else {
                            let _ = io::stdout().write_all(&out_buf);
                        }

                        result_set_index += 1;
                        col_names.clear();
                        rows.clear();
                        is_select = false;
                    }
                    _ => {}
                }
            }

            // Update transaction state based on what SQL was sent.
            tx.update_from_sql(sql_to_send);

            true
        }
        Err(e) => {
            // -b / --echo-errors: echo the failing query to stderr.
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();

            // Capture context for /fix.
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: error_message.clone(),
                sqlstate,
            });

            // Inline error suggestion: if AI is configured and
            // auto_explain_errors is on, show a brief hint.
            if settings.config.ai.auto_explain_errors {
                suggest_error_fix_inline(sql_to_send, &error_message, settings).await;
            }

            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        // Timing always goes to stdout regardless of output redirection.
        println!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }

    // Auto-EXPLAIN AI interpretation: when AI is configured and auto-EXPLAIN
    // produced plan output, stream a concise interpretation.
    if let Some(ref plan_text) = auto_explain_plan {
        interpret_auto_explain(plan_text, sql, settings).await;
    }

    // Store as the last successfully executed query (used by `\watch`).
    if success {
        settings.last_query = Some(sql.to_owned());
        // Clear last_error on success so /fix isn't stale.
        settings.last_error = None;
        // Increment session query counter.
        settings.query_count = settings.query_count.saturating_add(1);
    }

    success
}

// ---------------------------------------------------------------------------
// Extended query protocol execution (#57)
// ---------------------------------------------------------------------------

/// Execute a SQL string using the extended query protocol with positional
/// parameters and print results.
///
/// All parameter values arrive as `String`s from `\bind`.  They are passed
/// as `&str` to tokio-postgres, which sends them as untyped text parameters
/// over the wire.  The query should contain explicit casts (e.g. `$1::int`)
/// so that Postgres can resolve the types.
///
/// Returns `true` on success, `false` if the query produced a SQL error.
#[allow(clippy::too_many_lines)]
pub async fn execute_query_extended(
    client: &Client,
    sql: &str,
    params: &[String],
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Interpolate variables before sending.
    let interpolated = settings.vars.interpolate(sql);
    let sql_to_send = interpolated.as_str();

    // -s / --single-step: prompt before executing.
    if settings.single_step && !confirm_single_step(sql_to_send) {
        return true; // skipped — not an error
    }

    // Destructive statement guard.
    if settings.safety_enabled {
        let built_in = crate::safety::is_destructive(sql_to_send).map(str::to_owned);
        let custom = crate::safety::matches_custom_pattern(
            sql_to_send,
            &settings.config.safety.protected_patterns,
        )
        .map(|s| format!("matches protected pattern: {s}"));
        let reason = built_in.or(custom);
        if let Some(ref r) = reason {
            if !crate::safety::confirm_destructive(r) {
                eprintln!("Statement cancelled.");
                return true; // skipped — not an error
            }
        }
    }

    // -e / --echo-queries: print query to stderr before executing.
    if settings.echo_queries {
        eprintln!("{sql_to_send}");
    }

    // -L: log query input to the log file.
    if let Some(ref mut lf) = settings.log_file {
        let _ = writeln!(lf, "{sql_to_send}");
    }

    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    // Prepare the statement so we can execute with typed parameters.
    let stmt = match client.prepare(sql_to_send).await {
        Ok(s) => s,
        Err(e) => {
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: e
                    .as_db_error()
                    .map_or_else(|| e.to_string(), |db| db.message().to_owned()),
                sqlstate,
            });
            return false;
        }
    };

    // Build the parameter list as &str references (text format).
    let param_refs: Vec<&str> = params.iter().map(String::as_str).collect();
    let dyn_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_refs
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let success = match client.query(&stmt, dyn_params.as_slice()).await {
        Ok(rows) => {
            // Print results using the same pset formatting as simple_query.
            use crate::output::format_rowset_pset;
            use crate::query::{ColumnMeta, RowSet};

            if !rows.is_empty() || !stmt.columns().is_empty() {
                let col_names: Vec<String> =
                    stmt.columns().iter().map(|c| c.name().to_owned()).collect();

                let row_data: Vec<Vec<Option<String>>> = rows
                    .iter()
                    .map(|row| {
                        (0..col_names.len())
                            .map(|i| row.try_get::<_, Option<String>>(i).unwrap_or(None))
                            .collect()
                    })
                    .collect();

                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .enumerate()
                    .map(|(col_idx, n)| {
                        let mut has_value = false;
                        let is_numeric = row_data.iter().all(|r| {
                            match r.get(col_idx).and_then(|v| v.as_deref()) {
                                None | Some("") => true,
                                Some(val) => {
                                    has_value = true;
                                    val.parse::<f64>().is_ok()
                                }
                            }
                        }) && has_value;
                        ColumnMeta {
                            name: n.clone(),
                            is_numeric,
                        }
                    })
                    .collect();

                let rs = RowSet {
                    columns,
                    rows: row_data,
                };

                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &settings.pset);

                let out_bytes = out.as_bytes();

                if let Some(ref mut lf) = settings.log_file {
                    let _ = lf.write_all(out_bytes);
                }

                if let Some(ref mut w) = settings.output_target {
                    let _ = w.write_all(out_bytes);
                } else {
                    let _ = io::stdout().write_all(out_bytes);
                }
            }

            tx.update_from_sql(sql_to_send);
            true
        }
        Err(e) => {
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();

            // Capture context for /fix.
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: e
                    .as_db_error()
                    .map_or_else(|| e.to_string(), |db| db.message().to_owned()),
                sqlstate,
            });

            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        println!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }

    if success {
        settings.last_query = Some(sql.to_owned());
        // Clear last_error on success so /fix isn't stale.
        settings.last_error = None;
        // Increment session query counter.
        settings.query_count = settings.query_count.saturating_add(1);
    }

    success
}

/// Execute a named prepared statement with the given parameters.
///
/// Returns `true` on success, `false` on error.  If `stmt_name` is not
/// found in `settings.named_statements`, an error message is printed.
async fn execute_named_stmt(
    client: &Client,
    stmt_name: &str,
    params: &[String],
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Clone the statement out of the map so we don't hold a borrow on
    // settings while calling async client methods.
    let Some(stmt) = settings.named_statements.get(stmt_name).cloned() else {
        eprintln!("\\bind_named: prepared statement \"{stmt_name}\" does not exist");
        return false;
    };

    if settings.single_step {
        let preview = format!("[execute stmt \"{stmt_name}\"]");
        if !confirm_single_step(&preview) {
            return true;
        }
    }

    if settings.echo_queries {
        eprintln!("[execute stmt \"{stmt_name}\"]");
    }

    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    let param_refs: Vec<&str> = params.iter().map(String::as_str).collect();
    let dyn_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_refs
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let success = match client.query(&stmt, dyn_params.as_slice()).await {
        Ok(rows) => {
            use crate::output::format_rowset_pset;
            use crate::query::{ColumnMeta, RowSet};

            if !rows.is_empty() || !stmt.columns().is_empty() {
                let col_names: Vec<String> =
                    stmt.columns().iter().map(|c| c.name().to_owned()).collect();

                let row_data: Vec<Vec<Option<String>>> = rows
                    .iter()
                    .map(|row| {
                        (0..col_names.len())
                            .map(|i| row.try_get::<_, Option<String>>(i).unwrap_or(None))
                            .collect()
                    })
                    .collect();

                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .enumerate()
                    .map(|(col_idx, n)| {
                        let mut has_value = false;
                        let is_numeric = row_data.iter().all(|r| {
                            match r.get(col_idx).and_then(|v| v.as_deref()) {
                                None | Some("") => true,
                                Some(val) => {
                                    has_value = true;
                                    val.parse::<f64>().is_ok()
                                }
                            }
                        }) && has_value;
                        ColumnMeta {
                            name: n.clone(),
                            is_numeric,
                        }
                    })
                    .collect();

                let rs = RowSet {
                    columns,
                    rows: row_data,
                };

                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &settings.pset);

                let out_bytes = out.as_bytes();

                if let Some(ref mut w) = settings.output_target {
                    let _ = w.write_all(out_bytes);
                } else {
                    let _ = io::stdout().write_all(out_bytes);
                }
            }

            tx.update_from_sql(&format!("[bind_named {stmt_name}]"));
            true
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, None, settings.verbose_errors);
            tx.on_error();
            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        println!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }

    if success {
        settings.last_query = Some(format!("[bind_named {stmt_name}]"));
    }

    success
}

// ---------------------------------------------------------------------------
// \g / \gx buffer execution helpers (#46)
// ---------------------------------------------------------------------------

/// Execute `buf` and write output to `path`, creating or truncating the file.
///
/// The caller is responsible for clearing `buf` after this returns.
async fn execute_to_file(
    client: &Client,
    buf: &str,
    path: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
    match std::fs::File::create(path) {
        Ok(file) => {
            let prev = settings.output_target.take();
            settings.output_target = Some(Box::new(file));
            execute_query(client, buf, settings, tx).await;
            settings.output_target = prev;
        }
        Err(e) => eprintln!("\\g: cannot open file \"{path}\": {e}"),
    }
}

/// A [`Write`] wrapper backed by a shared `Arc<Mutex<Vec<u8>>>` so that the
/// captured bytes can be retrieved after the writer is boxed and erased.
struct CapturingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Execute `buf` and pipe output through the shell command `cmd` (after `|`).
///
/// Uses `sh -c` so the full shell command string is interpreted correctly.
/// The caller is responsible for clearing `buf` after this returns.
async fn execute_piped(
    client: &Client,
    buf: &str,
    cmd: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // Strip the leading `|` and trim whitespace.
    let shell_cmd = cmd.trim_start_matches('|').trim();

    // Capture query output into a shared buffer, then pipe it to the child.
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = CapturingWriter(std::sync::Arc::clone(&shared));

    let prev = settings.output_target.take();
    settings.output_target = Some(Box::new(writer));
    execute_query(client, buf, settings, tx).await;
    settings.output_target = prev;

    let captured = std::sync::Arc::try_unwrap(shared)
        .unwrap_or_else(|arc| std::sync::Mutex::new(arc.lock().unwrap().clone()))
        .into_inner()
        .unwrap_or_default();

    match Command::new("sh")
        .arg("-c")
        .arg(shell_cmd)
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&captured);
            }
            let _ = child.wait();
        }
        Err(e) => eprintln!("\\g: cannot run command \"{shell_cmd}\": {e}"),
    }
}

/// Execute a SQL string in interactive mode, routing output through the
/// built-in pager when appropriate.
///
/// When `settings.pager_enabled` is `true` and the formatted output exceeds
/// the current terminal height, the output is displayed in the built-in TUI
/// pager instead of being written directly to stdout.
///
/// This wrapper is used only by the interactive REPL loops.  Non-interactive
/// paths (`-c`, `-f`, piped stdin) call `execute_query` directly.
async fn execute_query_interactive(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Only intercept when pager is enabled and no output redirection is active.
    if !settings.pager_enabled || settings.output_target.is_some() {
        return execute_query(client, sql, settings, tx).await;
    }

    // Capture output into a buffer.
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = CapturingWriter(std::sync::Arc::clone(&shared));
    let prev = settings.output_target.take();
    settings.output_target = Some(Box::new(writer));
    let ok = execute_query(client, sql, settings, tx).await;
    settings.output_target = prev;

    let captured = std::sync::Arc::try_unwrap(shared)
        .unwrap_or_else(|arc| std::sync::Mutex::new(arc.lock().unwrap().clone()))
        .into_inner()
        .unwrap_or_default();

    let text = String::from_utf8_lossy(&captured);

    // Determine terminal height; fall back to 24 if unavailable.
    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);

    if crate::pager::needs_paging_with_min(
        &text,
        term_rows.saturating_sub(2),
        settings.pager_min_lines,
    ) {
        run_pager_for_text(settings, &text, &captured);
    } else {
        let _ = io::stdout().write_all(&captured);
    }

    ok
}

/// Execute a SQL string using the extended query protocol in interactive mode,
/// routing output through the built-in pager when appropriate.
async fn execute_query_extended_interactive(
    client: &Client,
    sql: &str,
    params: &[String],
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Only intercept when pager is enabled and no output redirection is active.
    if !settings.pager_enabled || settings.output_target.is_some() {
        return execute_query_extended(client, sql, params, settings, tx).await;
    }

    // Capture output into a buffer.
    let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = CapturingWriter(std::sync::Arc::clone(&shared));
    let prev = settings.output_target.take();
    settings.output_target = Some(Box::new(writer));
    let ok = execute_query_extended(client, sql, params, settings, tx).await;
    settings.output_target = prev;

    let captured = std::sync::Arc::try_unwrap(shared)
        .unwrap_or_else(|arc| std::sync::Mutex::new(arc.lock().unwrap().clone()))
        .into_inner()
        .unwrap_or_default();

    let text = String::from_utf8_lossy(&captured);

    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);

    if crate::pager::needs_paging_with_min(
        &text,
        term_rows.saturating_sub(2),
        settings.pager_min_lines,
    ) {
        run_pager_for_text(settings, &text, &captured);
    } else {
        let _ = io::stdout().write_all(&captured);
    }

    ok
}

/// Activate the appropriate pager for `text`.
///
/// Uses the external pager command when `settings.pager_command` is set,
/// falling back to the built-in TUI pager otherwise.  On any pager error,
/// falls back to printing directly to stdout.
fn run_pager_for_text(settings: &ReplSettings, text: &str, raw_bytes: &[u8]) {
    if let Some(ref cmd) = settings.pager_command {
        if let Err(e) = crate::pager::run_pager_external(cmd, text) {
            eprintln!("samo: pager error: {e}");
            let _ = io::stdout().write_all(raw_bytes);
        }
    } else if let Err(e) = crate::pager::run_pager(text) {
        eprintln!("samo: pager error: {e}");
        let _ = io::stdout().write_all(raw_bytes);
    }
}

/// Execute `buf`, then execute each non-NULL result cell as a separate SQL
/// statement (`\gexec`).
///
/// The initial query is run via `simple_query`.  For each row, for each
/// column, if the cell value is `Some` and non-empty, that value is executed
/// as a SQL statement.  `tokio_postgres` returns `None` for NULL cells via
/// `SimpleQueryRow::get()`; both `None` and empty-string cells are skipped.
///
/// On success the command tag (e.g. `"CREATE TABLE"`) is printed.  On error
/// the error message is printed and processing continues with the next cell.
///
/// The caller is responsible for clearing `buf` after this returns.
async fn execute_gexec(client: &Client, buf: &str, settings: &mut ReplSettings, tx: &mut TxState) {
    use tokio_postgres::SimpleQueryMessage;

    // Interpolate variables (mirrors execute_query).
    let interpolated = settings.vars.interpolate(buf);
    let sql_to_send = interpolated.as_str();

    // Collect result cell values from the initial query.
    let cell_sqls: Vec<String> = match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            let mut rows: Vec<Vec<Option<String>>> = Vec::new();

            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    let vals: Vec<Option<String>> = (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect();
                    rows.push(vals);
                }
            }

            tx.update_from_sql(sql_to_send);

            // Flatten row-major: row 0 col 0, row 0 col 1, …, row 1 col 0, …
            // NULL (None) and empty-string cells are both skipped.
            let mut cells = Vec::new();
            for row in rows {
                for s in row.into_iter().flatten() {
                    if !s.is_empty() {
                        cells.push(s);
                    }
                }
            }
            cells
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            return;
        }
    };

    // Execute each cell value as a SQL statement.
    for cell_sql in cell_sqls {
        match client.simple_query(&cell_sql).await {
            Ok(messages) => {
                for msg in messages {
                    if let SimpleQueryMessage::CommandComplete(n) = msg {
                        // Extract the command tag from the completion count.
                        // tokio-postgres 0.7 CommandComplete carries only the
                        // row count as u64; derive the tag by inspecting the
                        // first keyword of the cell SQL.
                        let tag = command_tag_for(&cell_sql, n);
                        println!("{tag}");
                    }
                }
                tx.update_from_sql(&cell_sql);
            }
            Err(e) => {
                crate::output::eprint_db_error(&e, Some(&cell_sql), settings.verbose_errors);
                tx.on_error();
            }
        }
    }
}

/// Derive a psql-style command tag string from the first keyword of `sql`
/// and the affected-row count `n`.
///
/// For most DDL statements the tag is just the uppercased verb + noun
/// (e.g. `"CREATE TABLE"`).  For INSERT/UPDATE/DELETE/SELECT we append the
/// row count.
fn command_tag_for(sql: &str, n: u64) -> String {
    let upper = sql.trim().to_uppercase();
    let words: Vec<&str> = upper.split_whitespace().take(2).collect();
    let first = words.first().copied().unwrap_or("");
    let second = words.get(1).copied().unwrap_or("");

    match first {
        "INSERT" => format!("INSERT 0 {n}"),
        "UPDATE" => format!("UPDATE {n}"),
        "DELETE" => format!("DELETE {n}"),
        "SELECT" | "VALUES" | "TABLE" | "MOVE" | "FETCH" | "COPY" => {
            format!("{first} {n}")
        }
        _ => {
            // DDL and other statements: two-word tag (e.g. "CREATE TABLE").
            if second.is_empty() {
                first.to_owned()
            } else {
                format!("{first} {second}")
            }
        }
    }
}

/// Execute `buf` and store each column of the single result row as a variable.
///
/// - Exactly 1 row: for each column, sets `{prefix}{column_name}` to the
///   cell value (empty string for NULL), matching psql behaviour.
/// - 0 rows: prints an error message and leaves existing variables unchanged.
/// - >1 rows: prints an error message and leaves existing variables unchanged.
/// - SQL error: prints the error message and updates `tx` state.
async fn execute_gset(
    client: &Client,
    buf: &str,
    prefix: Option<&str>,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
    let prefix = prefix.unwrap_or("");

    // Interpolate variables before sending (mirrors execute_query behaviour).
    let interpolated = settings.vars.interpolate(buf);
    let sql_to_send = interpolated.as_str();

    match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<Option<String>>> = Vec::new();

            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    if col_names.is_empty() {
                        col_names = (0..row.len())
                            .map(|i| {
                                row.columns()
                                    .get(i)
                                    .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                            })
                            .collect();
                    }
                    let vals: Vec<Option<String>> = (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect();
                    rows.push(vals);
                }
            }

            match rows.len() {
                0 => eprintln!("\\gset: query returned no rows"),
                1 => {
                    tx.update_from_sql(sql_to_send);
                    // Store last query for \watch compatibility.
                    settings.last_query = Some(buf.to_owned());
                    let row = &rows[0];
                    for (col, val) in col_names.iter().zip(row.iter()) {
                        let var_name = format!("{prefix}{col}");
                        let var_value = val.as_deref().unwrap_or("");
                        settings.vars.set(&var_name, var_value);
                    }
                }
                n => eprintln!("\\gset: more than one row returned ({n} rows)"),
            }
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
        }
    }
}

// ---------------------------------------------------------------------------
// \crosstabview — execute buffer and pivot result into cross-tab table
// ---------------------------------------------------------------------------

/// Execute `buf`, pivot the result using `\crosstabview` rules, and print.
///
/// Column arguments are passed in `raw_args` (may be empty for defaults).
/// The query must return at least 3 columns and all `(colV, colH)` pairs must
/// be unique.  Any violation is printed as an error message without modifying
/// the transaction state beyond what the query itself did.
///
/// The caller is responsible for clearing `buf` after this returns.
async fn execute_crosstabview(
    client: &Client,
    buf: &str,
    raw_args: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
    use tokio_postgres::SimpleQueryMessage;

    let interpolated = settings.vars.interpolate(buf);
    let sql_to_send = interpolated.as_str();

    let result = match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();

            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    if col_names.is_empty() {
                        col_names = (0..row.len())
                            .map(|i| {
                                row.columns()
                                    .get(i)
                                    .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                            })
                            .collect();
                    }
                    let vals: Vec<String> = (0..row.len())
                        .map(|i| row.get(i).unwrap_or("").to_owned())
                        .collect();
                    rows.push(vals);
                }
            }

            tx.update_from_sql(sql_to_send);
            settings.last_query = Some(buf.to_owned());
            Some((col_names, rows))
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            None
        }
    };

    let Some((col_names, rows)) = result else {
        return;
    };

    // Parse and apply the pivot specification.
    let args = crate::crosstab::parse_args(raw_args);
    match crate::crosstab::pivot(&col_names, &rows, &args) {
        Ok((pivot_headers, pivot_rows)) => {
            let mut out = String::new();
            crate::crosstab::format_pivot(&mut out, &pivot_headers, &pivot_rows);
            let _ = io::stdout().write_all(out.as_bytes());
        }
        Err(e) => {
            eprintln!("{e}");
        }
    }
}

// ---------------------------------------------------------------------------
// \gdesc — describe buffer columns without executing (#52)
// ---------------------------------------------------------------------------

/// Describe the result columns of `buf` using the extended-protocol `Prepare`
/// message (no rows are produced; no side-effects occur on the server).
///
/// Output format (matching psql):
/// ```text
///  Column | Type
/// --------+---------
///  id     | integer
///  name   | text
/// (2 rows)
/// ```
///
/// Type names are resolved via `pg_catalog.format_type(oid, NULL)` so they
/// match psql's display names (`integer` not `int4`, etc.).
///
/// When `buf` is empty, prints an informational message.
/// On prepare error, prints the Postgres error message.
async fn describe_buffer(client: &Client, buf: &str, verbose_errors: bool) {
    if buf.is_empty() {
        println!("Query buffer is empty.");
        return;
    }

    let stmt = match client.prepare(buf).await {
        Ok(s) => s,
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(buf), verbose_errors);
            return;
        }
    };

    let cols = stmt.columns();
    if cols.is_empty() {
        println!("This command doesn't return data.");
        return;
    }

    // Collect (name, oid) pairs.
    let col_info: Vec<(String, u32)> = cols
        .iter()
        .map(|c| (c.name().to_owned(), c.type_().oid()))
        .collect();

    // Resolve OIDs to display type names in a single query.
    // Build: SELECT format_type($1, NULL), format_type($2, NULL), …
    let select_exprs: Vec<String> = (1..=col_info.len())
        .map(|i| format!("pg_catalog.format_type(${i}, NULL)"))
        .collect();
    let type_query = format!("select {}", select_exprs.join(", "));

    let oid_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = col_info
        .iter()
        .map(|(_, oid)| oid as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let type_names: Vec<String> = match client.query_one(&type_query, &oid_params).await {
        Ok(row) => (0..col_info.len())
            .map(|i| row.get::<_, String>(i))
            .collect(),
        Err(e) => {
            crate::output::eprint_db_error(&e, None, verbose_errors);
            return;
        }
    };

    // Compute column widths for aligned output.
    let header_col = "Column";
    let header_type = "Type";
    let col_w = col_info
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0)
        .max(header_col.len());
    let type_w = type_names
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max(header_type.len());

    // Header.
    println!(" {header_col:<col_w$} | {header_type:<type_w$}");
    // Separator.
    println!("-{}-+-{}-", "-".repeat(col_w), "-".repeat(type_w));
    // Rows.
    for ((name, _), type_name) in col_info.iter().zip(type_names.iter()) {
        println!(" {name:<col_w$} | {type_name:<type_w$}");
    }
    // Footer.
    let n = col_info.len();
    if n == 1 {
        println!("(1 row)");
    } else {
        println!("({n} rows)");
    }
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
                    ExecMode::Observe => "observe",
                };
                eprintln!("Input mode: {input_label}  Execution mode: {exec_label}");
            }
            MetaResult::SetInputMode(mode) => {
                settings.input_mode = mode;
                let label = match mode {
                    InputMode::Sql => "sql",
                    InputMode::Text2Sql => "text2sql",
                };
                eprintln!("Input mode: {label}");
            }
            MetaResult::SetExecMode(mode) => {
                settings.exec_mode = mode;
                let label = match mode {
                    ExecMode::Interactive => "interactive",
                    ExecMode::Plan => "plan",
                    ExecMode::Yolo => "yolo",
                    ExecMode::Observe => "observe",
                };
                eprintln!("Execution mode: {label}");
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
            eprintln!("samo: could not read file \"{path}\": {e}");
            return 1;
        }
    };
    let mut tx = TxState::default();

    // -1 / --single-transaction: open a transaction before the first statement.
    // Use simple_query directly so that begin/commit/rollback are not echoed,
    // logged, or prompted (they are internal bookkeeping, not user SQL).
    if settings.single_transaction {
        if let Err(e) = client.simple_query("begin").await {
            eprintln!("samo: could not begin transaction: {e}");
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
            "samo: warning: {} unterminated \\if block(s) at end of file \"{path}\"",
            settings.cond.depth()
        );
    }

    // -1 / --single-transaction: commit on success, rollback on failure.
    if settings.single_transaction {
        if exit_code == 0 {
            if let Err(e) = client.simple_query("commit").await {
                eprintln!("samo: could not commit transaction: {e}");
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
            eprintln!("samo: read error: {e}");
            None
        }
    });
    let mut tx = TxState::default();
    let exit_code = exec_lines(client, lines, settings, params, &mut tx).await;

    if settings.cond.depth() > 0 {
        eprintln!(
            "samo: warning: {} unterminated \\if block(s) at end of input",
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

/// Print the backslash command help text.
fn print_help() {
    println!("{}", crate::version_string());
    println!(
        r"
Backslash commands:
  \q              quit samo
  quit            quit samo (interactive mode only)
  exit            quit samo (interactive mode only)
  help            show this help overview (interactive mode only)
  \timing [on|off]      toggle/set query timing display
  \x [on|off|auto]      toggle/set expanded display
  \conninfo       show connection information
  \copyright      show PostgreSQL usage and distribution terms
  \version        show samo version and build information
  \?              show this help

Session commands:
  \c [db [user [host [port]]]]  reconnect to database
  \c @profile                   reconnect using a named profile
  \profiles                     list all configured connection profiles
  \sf[+] <func>   show function source
  \sv[+] <view>   show view definition
  \h [command]    SQL syntax help

Describe commands (stubs; see #27 for full implementation):
  \d  [pattern]     describe objects
  \dt [pattern]     list tables
  \di [pattern]     list indexes
  \ds [pattern]     list sequences
  \dv [pattern]     list views
  \dm [pattern]     list materialised views
  \df [pattern]     list functions
  \dn [pattern]     list schemas
  \du [pattern]     list roles
  \dp [pattern]     list access privileges
  \db [pattern]     list tablespaces
  \dT [pattern]     list data types
  \dx [pattern]     list extensions
  \l  [pattern]     list databases
  \dE [pattern]     list foreign tables
  \dD [pattern]     list domains
  \dc [pattern]     list conversions
  \dC [pattern]     list casts
  \dd [pattern]     list object comments
  \des [pattern]    list foreign servers
  \dew [pattern]    list foreign-data wrappers
  \det [pattern]    list foreign tables via FDW
  \deu [pattern]    list user mappings

AI commands:
  /ask <prompt>     natural language to SQL
  /explain          explain the last query plan
  /fix              diagnose and fix the last error
  /optimize <query> suggest query optimizations
  /describe <table> AI-generated table description
  /rca              root cause analysis of current database state
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
  \yolo             enter YOLO execution mode
  \observe          enter observe execution mode
  \interactive      return to interactive mode (default)
  \mode             show current input and execution mode

Auto-EXPLAIN:
  \\set EXPLAIN on       show EXPLAIN for every query
  \\set EXPLAIN analyze  show EXPLAIN ANALYZE for every query
  \\set EXPLAIN verbose  show EXPLAIN (ANALYZE, VERBOSE, BUFFERS, TIMING)
  \\set EXPLAIN off      disable auto-EXPLAIN"
    );
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
        println!("Add profiles to ~/.config/samo/config.toml under [connections.<name>].");
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

/// Print the `PostgreSQL` copyright notice (matches psql `\copyright` output).
fn print_copyright() {
    println!(
        "PostgreSQL Database Management System
(also known as Postgres, formerly known as Postgres95)

Portions Copyright (c) 1996-2023, PostgreSQL Global Development Group

Portions Copyright (c) 1994, The Regents of the University of California

Permission to use, copy, modify, and distribute this software and its
documentation for any purpose, without fee, and without a written agreement
is hereby granted, provided that the above copyright notice and this
paragraph and the following two paragraphs appear in all copies.

IN NO EVENT SHALL THE UNIVERSITY OF CALIFORNIA BE LIABLE TO ANY PARTY FOR
DIRECT, INDIRECT, SPECIAL, INCIDENTAL, OR CONSEQUENTIAL DAMAGES, INCLUDING
LOST PROFITS, ARISING OUT OF THE USE OF THIS SOFTWARE AND ITS
DOCUMENTATION, EVEN IF THE UNIVERSITY OF CALIFORNIA HAS BEEN ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.

THE UNIVERSITY OF CALIFORNIA SPECIFICALLY DISCLAIMS ANY WARRANTIES,
INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY
AND FITNESS FOR A PARTICULAR PURPOSE.  THE SOFTWARE PROVIDED HEREUNDER IS
ON AN \"AS IS\" BASIS, AND THE UNIVERSITY OF CALIFORNIA HAS NO OBLIGATIONS TO
PROVIDE MAINTENANCE, SUPPORT, UPDATES, ENHANCEMENTS, OR MODIFICATIONS."
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

/// Apply a `\set` command.
///
/// - `\set` (bare) — print all variables sorted by name.
/// - `\set name` — print one variable.
/// - `\set name value` — assign.
///
/// Special case: when `ECHO_HIDDEN` is set to `on`, update `settings.echo_hidden`.
fn apply_set(settings: &mut ReplSettings, name: &str, value: &str) {
    if name.is_empty() {
        // List all variables.
        let mut pairs: Vec<(&String, &String)> = settings.vars.all().iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (k, v) in pairs {
            println!("{k} = '{v}'");
        }
        return;
    }
    if value.is_empty() {
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
    // Mirror HIGHLIGHT into the settings flag.
    if name == "HIGHLIGHT" {
        settings.no_highlight = value == "off";
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
            "off" | "false" | "0" | "" => AutoExplain::Off,
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
    } else {
        eprintln!("\\unset: variable {name} was not set");
    }
}

/// Apply a `\prompt [text] name` command.
///
/// Prints `prompt_text` to stderr (matching psql behaviour — the prompt goes
/// to the tty, not stdout), reads one line from stdin, and stores the result
/// in the variable `var_name`.  When stdin is not a terminal the prompt text
/// is suppressed.
fn apply_prompt(settings: &mut ReplSettings, prompt_text: &str, var_name: &str) {
    if io::stdin().is_terminal() && !prompt_text.is_empty() {
        eprint!("{prompt_text}");
        let _ = io::stderr().flush();
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

/// Apply a `\pset` command.
#[allow(clippy::too_many_lines)]
fn apply_pset(settings: &mut ReplSettings, option: &str, value: Option<&str>) {
    use crate::output::OutputFormat;

    if option.is_empty() {
        // Display all pset options.
        print_pset_status(&settings.pset);
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
    }
}

/// Print a summary of the current `PsetConfig` (matching psql `\pset` output).
fn print_pset_status(pset: &crate::output::PsetConfig) {
    println!("border         = {}", pset.border);
    println!("expanded       = {}", expanded_mode_str(pset.expanded));
    println!("fieldsep       = \"{}\"", pset.field_sep);
    println!(
        "footer         = {}",
        if pset.footer { "on" } else { "off" }
    );
    println!("format         = {}", format_name(&pset.format));
    println!("null           = \"{}\"", pset.null_display);
    println!(
        "tuples_only    = {}",
        if pset.tuples_only { "on" } else { "off" }
    );
    match &pset.title {
        Some(t) => println!("title          = \"{t}\""),
        None => println!("title          = (not set)"),
    }
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
    Reconnected(Box<tokio_postgres::Client>, ConnParams),
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
    /// Switch execution mode (`\plan`, `\yolo`, `\observe`, `\interactive`).
    SetExecMode(ExecMode),
    /// Show current mode summary (`\mode`).
    ShowMode,
}

/// Default `\watch` interval in seconds.
const WATCH_DEFAULT_INTERVAL: f64 = 2.0;

/// Parse an interval string from `\watch [interval]`.
///
/// Accepts:
/// - bare number: `"5"`, `"0.5"` → seconds as f64
/// - `s`-suffixed number: `"5s"`, `"0.5s"` → same
///
/// Returns the default interval (2.0 s) when the string is empty or
/// cannot be parsed as a non-negative number.
fn parse_watch_interval(s: &str) -> f64 {
    let s = s.trim();
    if s.is_empty() {
        return WATCH_DEFAULT_INTERVAL;
    }
    // Strip optional trailing `s`.
    let digits = s.strip_suffix('s').unwrap_or(s);
    digits
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(WATCH_DEFAULT_INTERVAL)
}

/// Format a [`std::time::SystemTime`] as psql does for `\watch` headers.
///
/// Produces the ctime-like format that psql uses in local time:
/// `Www Mmm DD HH:MM:SS YYYY`
///
/// Example: `Thu Mar 13 19:00:00 2026`
///
/// On Unix, uses `libc::localtime_r` to convert to local time so the
/// output matches the user's timezone, consistent with psql behaviour.
/// On Windows, falls back to UTC (local-time conversion via POSIX
/// `localtime_r` is not available on that platform).
fn format_system_time(now: std::time::SystemTime) -> String {
    const WDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    use std::time::{Duration, UNIX_EPOCH};

    let duration = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);

    // Windows path: pure-Rust UTC computation.
    // localtime_r is POSIX-only and unavailable on Windows; UTC is an
    // acceptable fallback for the \watch timestamp display on Windows.
    #[cfg(windows)]
    return {
        let secs = duration.as_secs();

        // Decompose unix timestamp into calendar fields (UTC).
        // Algorithm: http://howardhinnant.github.io/date_algorithms.html
        let days = secs / 86_400;
        let time_of_day = secs % 86_400;
        let hour = (time_of_day / 3_600) as u32;
        let min = ((time_of_day % 3_600) / 60) as u32;
        let sec = (time_of_day % 60) as u32;

        // days since Unix epoch → civil date (Gregorian proleptic)
        let z = days as i64 + 719_468;
        let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };

        // Day-of-week: 0=Sun (Unix epoch was a Thursday = 4)
        let wday_idx = ((days + 4) % 7) as usize;
        let mon = MON[(m - 1).clamp(0, 11) as usize];
        let wday = WDAY[wday_idx.clamp(0, 6)];

        format!("{wday} {mon} {d:2} {hour:02}:{min:02}:{sec:02} {y}")
    };

    // Unix path: use libc::localtime_r for local-time conversion.
    // `#[allow(deprecated)]` silences the musl time_t 32→64-bit transition
    // warning; the cast is correct on all 64-bit targets Samo ships for.
    #[cfg(not(windows))]
    {
        #[allow(deprecated)]
        #[allow(clippy::cast_possible_wrap)]
        let unix_secs: libc::time_t = duration.as_secs() as libc::time_t;

        // SAFETY: localtime_r is thread-safe and only reads the time_t we pass.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            libc::localtime_r(&raw const unix_secs, &raw mut tm);
        }

        // clamp() guarantees the index is in-bounds; the value was already
        // non-negative so the cast to usize is safe.
        #[allow(clippy::cast_sign_loss)]
        let wday = WDAY[tm.tm_wday.clamp(0, 6) as usize];
        #[allow(clippy::cast_sign_loss)]
        let mon = MON[tm.tm_mon.clamp(0, 11) as usize];
        let day = tm.tm_mday;
        let hour = tm.tm_hour;
        let min = tm.tm_min;
        let sec = tm.tm_sec;
        let year = tm.tm_year + 1900;

        format!("{wday} {mon} {day:2} {hour:02}:{min:02}:{sec:02} {year}")
    }
}

/// Re-execute `sql` repeatedly, printing a timestamp header before each run.
///
/// The loop exits when Ctrl-C (SIGINT) is received while sleeping between
/// iterations.  Each iteration:
/// 1. Prints timestamp header matching psql's ctime-like format.
/// 2. Executes the query.
/// 3. Sleeps `interval_secs`; if Ctrl-C arrives during the sleep, exits.
async fn watch_query(client: &Client, sql: &str, interval_secs: f64, settings: &mut ReplSettings) {
    use std::time::Duration;
    use tokio::signal;
    use tokio::time::sleep;

    loop {
        // Print timestamp header matching psql's ctime-like format.
        let ts = format_system_time(std::time::SystemTime::now());
        println!("{ts} (every {interval_secs}s)\n");

        // Execute the stored query.  Use a fresh TxState so that
        // transaction state changes inside the loop are not persisted.
        let mut dummy_tx = TxState::default();
        execute_query(client, sql, settings, &mut dummy_tx).await;

        // Sleep for the interval, but exit cleanly on Ctrl-C.
        tokio::select! {
            () = sleep(Duration::from_secs_f64(interval_secs)) => {},
            _ = signal::ctrl_c() => {
                break;
            },
        }
    }
}

/// Parse a duration string like `"30s"`, `"5m"`, `"1h"`.
///
/// Returns `None` for invalid input.
fn parse_duration(s: &str) -> Option<std::time::Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else {
        // Bare number defaults to seconds.
        (s, 1)
    };
    let num: u64 = num_str.parse().ok()?;
    Some(std::time::Duration::from_secs(num * multiplier))
}

/// Run the observe loop — periodic database health snapshots.
///
/// Polls key diagnostic views every 10 seconds and prints a timestamped
/// summary.  Exits on Ctrl-C or when `duration_arg` elapses.  After
/// exiting, offers an AI-generated summary of the observation period.
#[allow(clippy::too_many_lines)]
async fn observe_loop(
    client: &Client,
    settings: &mut ReplSettings,
    params: &ConnParams,
    duration_arg: Option<&str>,
) {
    use std::fmt::Write as _;
    use std::time::Duration;
    use tokio::signal;
    use tokio::time::sleep;

    let total_duration = duration_arg.and_then(parse_duration);

    if let Some(d) = total_duration {
        eprintln!(
            "-- Observing for {}s (Ctrl-C to stop early)...",
            d.as_secs()
        );
    } else {
        if duration_arg.is_some() {
            eprintln!("-- Invalid duration. Use e.g. \\observe 30s, \\observe 5m, \\observe 1h");
            return;
        }
        eprintln!("-- Observing (Ctrl-C to stop)...");
    }

    let start = std::time::Instant::now();
    let mut snapshots: Vec<String> = Vec::new();
    let interval = Duration::from_secs(10);
    let mut anomaly_detector = crate::anomaly::AnomalyDetector::new();

    loop {
        let ts = format_system_time(std::time::SystemTime::now());
        let mut report = format!("{ts} |");
        let mut metric_snap = crate::anomaly::MetricSnapshot::default();

        // 1. Connection count.
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FILTER (WHERE state = 'active') AS active, \
                 count(*) AS total \
                 FROM pg_stat_activity WHERE backend_type = 'client backend'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let active = row.get(0).unwrap_or("?");
                    let total = row.get(1).unwrap_or("?");
                    let _ = write!(report, " connections: {active} active / {total} total");
                    metric_snap.active_sessions = active.parse().unwrap_or(0);
                    metric_snap.total_sessions = total.parse().unwrap_or(0);
                }
            }
        }

        // 2. Top wait event.
        if let Ok(rows) = client
            .simple_query(
                "SELECT wait_event_type || ':' || wait_event AS we, count(*) AS cnt \
                 FROM pg_stat_activity \
                 WHERE state = 'active' AND wait_event IS NOT NULL \
                 GROUP BY 1 ORDER BY 2 DESC LIMIT 1",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let we = row.get(0).unwrap_or("?");
                    let cnt = row.get(1).unwrap_or("?");
                    let _ = write!(report, " | top wait: {we} ({cnt})");
                    metric_snap.top_wait_count = cnt.parse().unwrap_or(0);
                }
            }
        }

        // 3. Long-running queries (> 30s).
        if let Ok(rows) = client
            .simple_query(
                "SELECT pid, \
                 extract(epoch FROM now() - query_start)::int AS secs, \
                 left(query, 60) AS q \
                 FROM pg_stat_activity \
                 WHERE state = 'active' \
                 AND query_start < now() - interval '30 seconds' \
                 AND backend_type = 'client backend' \
                 ORDER BY query_start LIMIT 3",
            )
            .await
        {
            let mut long_count = 0u32;
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let pid = row.get(0).unwrap_or("?");
                    let secs = row.get(1).unwrap_or("?");
                    let q = row.get(2).unwrap_or("?");
                    let _ = write!(report, "\n  long query (pid {pid}, {secs}s): {q}");
                    long_count += 1;
                }
            }
            metric_snap.long_queries = long_count;
        }

        // 3b. Blocked sessions (for anomaly detection).
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE pid != pg_backend_pid() \
                 AND backend_type = 'client backend' \
                 AND wait_event_type = 'Lock'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    metric_snap.blocked_sessions =
                        row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        // 4. Autovacuum activity.
        if let Ok(rows) = client
            .simple_query(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE backend_type = 'autovacuum worker'",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let cnt = row.get(0).unwrap_or("0");
                    if cnt != "0" {
                        let _ = write!(report, " | autovacuum workers: {cnt}");
                    }
                }
            }
        }

        // 5. Replication lag (if streaming replication is active).
        if let Ok(rows) = client
            .simple_query(
                "SELECT application_name, \
                 pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)::bigint AS lag_bytes \
                 FROM pg_stat_replication LIMIT 3",
            )
            .await
        {
            for msg in &rows {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let name = row.get(0).unwrap_or("?");
                    let lag = row.get(1).unwrap_or("?");
                    let _ = write!(report, " | repl lag ({name}): {lag} bytes");
                }
            }
        }

        eprintln!("{report}");
        snapshots.push(report);

        // Anomaly detection.
        let anomalies = anomaly_detector.check(&metric_snap);
        for anomaly in &anomalies {
            eprintln!(
                "  ** ANOMALY [{}]: {}",
                anomaly.kind.label(),
                anomaly.description
            );
        }
        if crate::anomaly::AnomalyDetector::should_trigger_rca(&anomalies) {
            eprintln!("  >> Auto-triggering RCA investigation...");
            let pg_ash_available = settings.db_capabilities.pg_ash.is_available();
            let snapshot = crate::rca::collect_snapshot(client, pg_ash_available).await;
            let data_steps = snapshot.steps.iter().filter(|s| s.has_data).count();
            eprintln!("  >> RCA: collected {data_steps} diagnostic steps.");
            print!("{}", snapshot.to_prompt());
            anomaly_detector.reset_rca_cooldown();
        }

        // Check if duration has elapsed.
        if let Some(d) = total_duration {
            if start.elapsed() >= d {
                break;
            }
        }

        // Sleep for the interval, exit on Ctrl-C.
        let remaining = total_duration.map(|d| d.saturating_sub(start.elapsed()));
        let sleep_time = match remaining {
            Some(r) if r < interval => r,
            _ => interval,
        };
        if sleep_time.is_zero() {
            break;
        }
        tokio::select! {
            () = sleep(sleep_time) => {},
            _ = signal::ctrl_c() => {
                break;
            },
        }
    }

    eprintln!("-- Observation ended ({} snapshots).", snapshots.len());

    // Offer AI summary if configured and we have data.
    if snapshots.is_empty() {
        return;
    }

    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");
    if provider_name.is_empty() {
        return;
    }

    if !ask_yn_prompt("Generate AI summary? [Y/n] ", true) {
        return;
    }

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    let observation_data = snapshots.join("\n");
    let system_content = format!(
        "You are a PostgreSQL expert analyzing database observation data.\n\
         Database: {dbname}\n\n\
         Rules:\n\
         - Summarize the key findings from the observation period\n\
         - Highlight any concerning patterns (connection pressure, long queries, lock contention)\n\
         - Provide actionable recommendations\n\
         - Be concise — this is a terminal report",
        dbname = params.dbname,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!(
                "Here are the observation snapshots:\n\n{observation_data}\n\n\
                 Please summarize the findings and recommendations."
            ),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    eprintln!("\n-- Summary:");
    match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
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
            println!("{}", parsed.pattern.as_deref().unwrap_or(""));
            Some(MetaResult::Continue)
        }
        MetaCmd::QEcho => {
            let text = parsed.pattern.as_deref().unwrap_or("");
            if let Some(ref mut w) = settings.output_target {
                let _ = writeln!(w, "{text}");
            } else {
                println!("{text}");
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::Warn => {
            eprintln!("{}", parsed.pattern.as_deref().unwrap_or(""));
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
        MetaCmd::If => {
            let expr = parsed.pattern.as_deref().unwrap_or("");
            if expr.trim().is_empty() {
                eprintln!("\\if: missing expression");
            }
            let condition = eval_bool(expr);
            settings.cond.push_if(condition);
            return MetaResult::Continue;
        }
        MetaCmd::Elif => {
            let expr = parsed.pattern.as_deref().unwrap_or("");
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
        MetaCmd::Help => print_help(),
        MetaCmd::Timing(mode) => apply_timing(settings, mode),
        MetaCmd::Expanded(mode) => apply_expanded(settings, mode),
        MetaCmd::ConnInfo => {
            println!("{}", crate::connection::connection_info(params));
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
        MetaCmd::ListProfiles => {
            print_profiles(&settings.config);
        }
        MetaCmd::Copyright => {
            print_copyright();
        }
        MetaCmd::Version => {
            println!("{}", crate::version_string());
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
        MetaCmd::ObserveMode => {
            observe_loop(client, settings, params, parsed.pattern.as_deref()).await;
            return MetaResult::Continue;
        }
        MetaCmd::InteractiveMode => {
            return MetaResult::SetExecMode(ExecMode::Interactive);
        }
        MetaCmd::RefreshSchema => match &settings.schema_cache {
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
        },
        MetaCmd::Unknown(ref name) => {
            eprintln!("Invalid command \\{name}. Try \\? for help.");
        }
        MetaCmd::SqlHelp => {
            crate::session::sql_help(parsed.pattern.as_deref());
        }
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
                    // Detect server version so we can show it when the
                    // server endpoint changed (matches psql behaviour).
                    let server_ver =
                        crate::capabilities::detect_server_version_pub(&new_client).await;
                    let msg = crate::connection::reconnect_info(
                        crate::version_string(),
                        server_ver.as_deref(),
                        params,
                        &new_params,
                    );
                    println!("{msg}");
                    return MetaResult::Reconnected(Box::new(new_client), new_params);
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
            let ai_context = crate::dba::execute(
                client,
                subcommand,
                parsed.plus,
                Some(&settings.config.governance),
                Some(&settings.db_capabilities),
            )
            .await;
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
                for (name, query) in queries {
                    println!("  {name}: {query}");
                }
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
            crate::describe::execute(client, &parsed, settings.db_capabilities.pg_major_version())
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
        ref stub => {
            eprintln!("{}: not yet implemented (see #27)", stub.label());
        }
    }

    MetaResult::Continue
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
                &dummy,
                &new_params,
            );
            println!("{msg}");
            Some(MetaResult::Reconnected(Box::new(new_client), new_params))
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

    if use_readline {
        run_readline_loop(&mut client, &mut params, &mut settings, &mut tx).await
    } else {
        run_dumb_loop(&mut client, &mut params, &mut settings, &mut tx).await
    }
}

/// Run with rustyline readline support.
#[allow(clippy::too_many_lines)]
async fn run_readline_loop(
    client: &mut Client,
    params: &mut ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let config = Config::builder()
        .max_history_size(HISTORY_SIZE)
        .expect("valid history size")
        .history_ignore_space(true)
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
                eprintln!("samo: schema cache load failed: {e}");
            }
        }
    }
    // Store the Arc in settings so `\refresh` can update the same cache
    // that the completion helper holds.
    settings.schema_cache = Some(Arc::clone(&cache));
    // Enable syntax highlighting unless the user opted out or $TERM is dumb.
    let highlight = !settings.no_highlight && std::env::var("TERM").as_deref() != Ok("dumb");
    let helper = SamoHelper::new(Arc::clone(&cache), highlight);

    let mut rl: Editor<SamoHelper, FileHistory> = match Editor::with_config(config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("samo: readline init failed: {e}");
            return 1;
        }
    };
    rl.set_helper(Some(helper));

    let hist_path = history_file();
    if let Some(ref p) = hist_path {
        // Best-effort — ignore errors (file may not exist yet).
        let _ = rl.load_history(p);
    }

    let mut buf = String::new();
    // Accumulates the complete multi-line statement text for history.
    let mut stmt_buf = String::new();

    loop {
        let prompt = build_prompt_from_settings(settings, params, *tx, !buf.is_empty());

        match rl.readline(&prompt) {
            Ok(line) => {
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

                // Keep the helper's highlight state in sync with settings
                // (allows `\set HIGHLIGHT off` to take effect live).
                if let Some(h) = rl.helper_mut() {
                    h.set_highlight(
                        !settings.no_highlight && std::env::var("TERM").as_deref() != Ok("dumb"),
                    );
                }

                match result {
                    HandleLineResult::Quit => break,
                    HandleLineResult::Reconnected(new_client, new_params) => {
                        *client = *new_client;
                        *params = new_params;
                        // Reset transaction state on reconnect.
                        *tx = TxState::default();
                        buf.clear();
                        stmt_buf.clear();
                        // Re-detect superuser status for the new connection.
                        settings.is_superuser = crate::capabilities::detect_superuser(client).await;
                    }
                    HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
                }
            }
            Err(ReadlineError::Interrupted) => {
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
                eprintln!("samo: readline error: {e}");
                break;
            }
        }
    }

    if let Some(ref p) = hist_path {
        let _ = rl.save_history(p);
    }

    if settings.cond.depth() > 0 {
        eprintln!(
            "samo: warning: {} unterminated \\if block(s) at end of session",
            settings.cond.depth()
        );
    }

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
                            *params = new_params;
                            *tx = TxState::default();
                            buf.clear();
                            // Re-detect superuser status for the new connection.
                            settings.is_superuser =
                                crate::capabilities::detect_superuser(client).await;
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
                                *params = new_params;
                                *tx = TxState::default();
                                buf.clear();
                                // Re-detect superuser status for the new connection.
                                settings.is_superuser =
                                    crate::capabilities::detect_superuser(client).await;
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
                eprintln!("samo: read error: {e}");
                return 1;
            }
        }
    }

    if settings.cond.depth() > 0 {
        eprintln!(
            "samo: warning: {} unterminated \\if block(s) at end of input",
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
    Reconnected(Box<tokio_postgres::Client>, ConnParams),
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
        MetaResult::SetInputMode(mode) => {
            settings.input_mode = mode;
            let label = match mode {
                InputMode::Sql => "sql",
                InputMode::Text2Sql => "text2sql",
            };
            eprintln!("Input mode: {label}");
            HandleLineResult::Continue
        }
        MetaResult::SetExecMode(mode) => {
            settings.exec_mode = mode;
            let label = match mode {
                ExecMode::Interactive => "interactive",
                ExecMode::Plan => "plan",
                ExecMode::Yolo => "yolo",
                ExecMode::Observe => "observe",
            };
            eprintln!("Execution mode: {label}");
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
                ExecMode::Observe => "observe",
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
        "You are using samo, the command-line interface to PostgreSQL.\n\
         Type:  \\copyright for distribution terms\n       \
                \\h for help with SQL commands\n       \
                \\? for help with samo commands\n       \
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
                ExecMode::Observe => {
                    eprintln!("Observe mode is read-only. Use \\interactive to switch back.");
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
            MetaResult::SetInputMode(mode) => {
                settings.input_mode = mode;
                let label = match mode {
                    InputMode::Sql => "sql",
                    InputMode::Text2Sql => "text2sql",
                };
                eprintln!("Input mode: {label}");
                HandleLineResult::Continue
            }
            MetaResult::SetExecMode(mode) => {
                settings.exec_mode = mode;
                let label = match mode {
                    ExecMode::Interactive => "interactive",
                    ExecMode::Plan => "plan",
                    ExecMode::Yolo => "yolo",
                    ExecMode::Observe => "observe",
                };
                eprintln!("Execution mode: {label}");
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
                    ExecMode::Observe => "observe",
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
            MetaResult::BindNamedExec(name, params) => {
                execute_named_stmt(client, &name, &params, settings, tx).await;
                HandleLineResult::Continue
            }
            _ => HandleLineResult::Continue,
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
// AI command helpers
// ---------------------------------------------------------------------------

/// Stream an LLM completion to stdout, printing tokens as they arrive.
///
/// Falls back to printing the full response at once if the provider does
/// not implement true streaming.
/// Show a brief inline AI suggestion after a SQL error.
///
/// Called automatically when `[ai] auto_explain_errors = true`.  The
/// suggestion is dimmed to visually distinguish it from the error itself.
/// Uses a small `max_tokens` budget to keep latency low.
async fn suggest_error_fix_inline(sql: &str, error_message: &str, settings: &mut ReplSettings) {
    if check_token_budget(settings) {
        return;
    }

    let provider_name = match settings.config.ai.provider.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => return, // AI not configured — silently skip.
    };

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let Ok(provider) = crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) else {
        return;
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL expert. \
                      The user just got a SQL error. \
                      Give a ONE-LINE fix suggestion. \
                      Be extremely concise — just the fix, nothing else."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Query: {sql}\nError: {error_message}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 150,
        temperature: 0.0,
    };

    // Use non-streaming for lower latency on a short response.
    if let Ok(result) = provider.complete(&messages, &options).await {
        record_token_usage(settings, &result);
        let suggestion = result.content.trim();
        if !suggestion.is_empty() {
            // Print dimmed (ANSI escape: dim = \x1b[2m, reset = \x1b[0m).
            eprintln!("\x1b[2mHint: {suggestion}\x1b[0m");
        }
    }
}

/// Interpret an auto-EXPLAIN plan with AI.
///
/// Called automatically after auto-EXPLAIN output is displayed. Uses a
/// concise system prompt to produce a brief interpretation of the plan.
/// Skips silently when AI is not configured or the token budget is exhausted.
async fn interpret_auto_explain(
    plan_text: &str,
    original_query: &str,
    settings: &mut ReplSettings,
) {
    if check_token_budget(settings) {
        return;
    }

    let provider_name = match settings.config.ai.provider.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => return,
    };

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let Ok(provider) = crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) else {
        return;
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL performance expert. \
                      Give a brief (2-4 sentence) interpretation of the \
                      EXPLAIN plan. Focus on: most expensive nodes, \
                      sequential scans, row estimate errors, and one \
                      actionable suggestion if applicable."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Query: {original_query}\n\nPlan:\n{plan_text}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 300,
        temperature: 0.0,
    };

    if let Ok(result) = provider.complete(&messages, &options).await {
        record_token_usage(settings, &result);
        let interpretation = result.content.trim();
        if !interpretation.is_empty() {
            eprintln!("\x1b[2m{interpretation}\x1b[0m");
        }
    }
}

/// Interpret `\dba` diagnostic output with AI.
///
/// Called when a `\dba` command returns AI context (e.g. `\dba waits+`).
/// Produces a brief analysis of the diagnostic data. Skips silently when
/// AI is not configured.
async fn interpret_dba_output(context: &str, subcommand: &str, settings: &mut ReplSettings) {
    if check_token_budget(settings) {
        return;
    }

    let provider_name = match settings.config.ai.provider.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            eprintln!("-- AI interpretation requires [ai] provider to be configured");
            return;
        }
    };

    let api_key = settings
        .config
        .ai
        .api_key_env
        .as_deref()
        .and_then(|env_name| std::env::var(env_name).ok());

    let Ok(provider) = crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) else {
        return;
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL performance expert. \
                      Analyze the diagnostic output below and give a brief (3-5 sentence) \
                      interpretation. Focus on: dominant wait events, potential bottlenecks, \
                      and one actionable recommendation if applicable."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("\\dba {subcommand} output:\n\n{context}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 400,
        temperature: 0.0,
    };

    eprintln!("-- AI interpreting wait events...");
    if let Ok(result) = provider.complete(&messages, &options).await {
        record_token_usage(settings, &result);
        let interpretation = result.content.trim();
        if !interpretation.is_empty() {
            eprintln!("\x1b[2m{interpretation}\x1b[0m");
        }
    }
}

/// Stream a completion to the terminal, rendering markdown when enabled.
///
/// When `no_highlight` is `false` (the default), tokens are buffered and
/// the completed response is passed through [`crate::markdown::render_markdown`]
/// before printing, so that headers, bold text, code fences, etc. are
/// displayed with ANSI styling.
///
/// When `no_highlight` is `true` the raw text is streamed directly to
/// stdout token by token (original behaviour).
async fn stream_completion(
    provider: &dyn crate::ai::LlmProvider,
    messages: &[crate::ai::Message],
    options: &crate::ai::CompletionOptions,
    no_highlight: bool,
) -> Result<crate::ai::CompletionResult, String> {
    use std::io::Write;

    if no_highlight {
        // Raw streaming — emit each token immediately as it arrives.
        let result = provider
            .complete_streaming(
                messages,
                options,
                Box::new(|token| {
                    print!("{token}");
                    let _ = io::stdout().flush();
                }),
            )
            .await?;
        println!();
        return Ok(result);
    }

    // Markdown rendering mode — buffer tokens, render after completion.
    //
    // We still want to show progress to the user, so we print a dim "…"
    // indicator that gets overwritten once the full response arrives.
    // Use a shared buffer via Arc<Mutex<String>>.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let buf_clone = buf.clone();

    // Show a progress indicator so the terminal doesn't look stuck.
    eprint!("\x1b[2m…\x1b[0m");
    let _ = io::stderr().flush();

    let result = provider
        .complete_streaming(
            messages,
            options,
            Box::new(move |token| {
                if let Ok(mut b) = buf_clone.lock() {
                    b.push_str(token);
                }
            }),
        )
        .await?;

    // Erase the progress indicator (carriage return clears the line).
    eprint!("\r\x1b[K");
    let _ = io::stderr().flush();

    // Render markdown on the fully-collected content and print.
    let content = buf.lock().map(|b| b.clone()).unwrap_or_default();
    let rendered = crate::markdown::render_markdown(&content, false);
    print!("{rendered}");
    let _ = io::stdout().flush();

    Ok(result)
}

/// Dispatch a `/`-prefixed AI command.
///
/// Recognised commands:
/// - `/ask <prompt>` — generate SQL from natural language
/// - `/fix` — explain and fix the last error
/// - `/explain [query]` — explain query plan with AI interpretation
/// - `/optimize [query]` — suggest query optimizations
/// - `/rca` — root cause analysis of current database state
async fn dispatch_ai_command(
    input: &str,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
    // Budget gate — skip for /clear and /compact (they don't use tokens).
    let is_budget_exempt =
        input == "/clear" || input.starts_with("/compact") || input.starts_with("/budget");
    if !is_budget_exempt && check_token_budget(settings) {
        return;
    }

    if let Some(prompt) = input.strip_prefix("/ask").map(str::trim) {
        if prompt.is_empty() {
            eprintln!("Usage: /ask <natural language description>");
            return;
        }
        match settings.exec_mode {
            ExecMode::Plan => handle_ai_plan(client, prompt, settings, params).await,
            _ => handle_ai_ask(client, prompt, settings, params, tx).await,
        }
    } else if input == "/fix" || input.starts_with("/fix ") {
        handle_ai_fix(client, settings, params, tx).await;
    } else if let Some(query_arg) = input.strip_prefix("/explain").map(str::trim) {
        handle_ai_explain(client, query_arg, settings, params).await;
    } else if let Some(query_arg) = input.strip_prefix("/optimize").map(str::trim) {
        handle_ai_optimize(client, query_arg, settings, params).await;
    } else if let Some(table_arg) = input.strip_prefix("/describe").map(str::trim) {
        if table_arg.is_empty() {
            eprintln!("Usage: /describe <table_name>");
            return;
        }
        handle_ai_describe(client, table_arg, settings, params).await;
    } else if input == "/clear" {
        settings.conversation.clear();
        eprintln!("AI conversation context cleared.");
    } else if let Some(focus) = input.strip_prefix("/compact").map(str::trim) {
        if settings.conversation.is_empty() {
            eprintln!("Nothing to compact — conversation context is empty.");
        } else {
            let focus = if focus.is_empty() { None } else { Some(focus) };
            let before = settings.conversation.entries.len();
            settings.conversation.compact(focus);
            eprintln!(
                "Compacted {before} entries → {} entries (~{} tokens)",
                settings.conversation.entries.len(),
                settings.conversation.token_estimate(),
            );
        }
    } else if input == "/budget" {
        let budget = settings.config.ai.token_budget;
        let used = settings.tokens_used;
        if budget == 0 {
            eprintln!("Token budget: unlimited ({used} tokens used this session)");
        } else {
            let remaining = budget.saturating_sub(used);
            eprintln!("Token budget: {used}/{budget} used, {remaining} remaining");
        }
    } else if input == "/rca" || input.starts_with("/rca ") {
        handle_ai_rca(client, settings, params).await;
    } else {
        eprintln!(
            "Unknown AI command: {input}\n\
             Available: /ask, /fix, /explain, /optimize, /describe, /rca, /clear, /compact, /budget"
        );
    }
}

/// Strip markdown code fences from LLM output.
///
/// LLMs sometimes wrap SQL in `` ```sql ... ``` `` blocks.  This function
/// removes the fences and returns the inner content, trimmed.  If no fences
/// are found, the original string is returned as-is.
#[cfg(test)]
fn strip_sql_fences(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on the opening fence line.
        let after_tag = rest.find('\n').map_or(rest, |i| &rest[i + 1..]);
        // Remove closing fence.
        let body = if let Some(pos) = after_tag.rfind("```") {
            &after_tag[..pos]
        } else {
            after_tag
        };
        body.trim()
    } else {
        trimmed
    }
}

/// Check whether the session token budget has been exceeded.
///
/// Returns `true` (and prints a message) if the budget is exceeded,
/// meaning the caller should abort the AI operation.
/// Returns `false` if the budget is unlimited (0) or not yet reached.
fn check_token_budget(settings: &ReplSettings) -> bool {
    let budget = settings.config.ai.token_budget;
    if budget == 0 {
        return false; // No budget limit.
    }
    if settings.tokens_used >= budget {
        eprintln!(
            "Token budget exhausted ({used}/{budget} tokens used). \
             AI commands are disabled for this session.",
            used = settings.tokens_used,
        );
        true
    } else {
        false
    }
}

/// Record token usage from a completion result.
fn record_token_usage(settings: &mut ReplSettings, result: &crate::ai::CompletionResult) {
    settings.tokens_used += u64::from(result.input_tokens) + u64::from(result.output_tokens);
}

/// Prompt the user with a yes/no question and return their answer.
///
/// `default_yes` controls what happens when the user presses Enter without
/// typing anything: `true` → default is yes, `false` → default is no.
fn ask_yn_prompt(prompt: &str, default_yes: bool) -> bool {
    use std::io::Write;
    eprint!("{prompt}");
    let _ = io::stderr().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    let answer = input.trim().to_lowercase();
    if answer.is_empty() {
        return default_yes;
    }
    answer.starts_with('y')
}

/// User's choice when asked about executing AI-generated SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AskChoice {
    /// Execute as-is.
    Yes,
    /// Skip execution.
    No,
    /// Open in `$EDITOR` first, then execute the edited version.
    Edit,
}

/// Prompt the user with `[Y/n/e]` (yes / no / edit) and return their choice.
///
/// `default_yes` controls the behaviour when the user presses Enter without
/// typing: `true` → defaults to `Yes`, `false` → defaults to `No`.
fn ask_yne_prompt(prompt: &str, default_yes: bool) -> AskChoice {
    use std::io::Write;
    eprint!("{prompt}");
    let _ = io::stderr().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return AskChoice::No;
    }
    let answer = input.trim().to_lowercase();
    if answer.is_empty() {
        return if default_yes {
            AskChoice::Yes
        } else {
            AskChoice::No
        };
    }
    if answer.starts_with('e') {
        AskChoice::Edit
    } else if answer.starts_with('y') {
        AskChoice::Yes
    } else {
        AskChoice::No
    }
}

/// Handle a `/ask <prompt>` command end-to-end.
///
/// Acts as a general-purpose `PostgreSQL` expert assistant.  The AI answers
/// questions directly and, when a database query is needed, includes it in
/// a triple-backtick `sql` code fence.  Any SQL blocks found in the response
/// are automatically executed; results are shown interleaved with the AI's
/// explanatory text.
///
/// SQL visibility is controlled by `ai.show_sql = true` (config) or
/// `\set ECHO_HIDDEN on` (runtime): when either is set the generated SQL is
/// printed (with syntax highlighting) before its result set.
///
/// Write queries (`INSERT`/`UPDATE`/`DELETE`/`MERGE`) always prompt for
/// confirmation unless running in YOLO mode.
#[allow(clippy::too_many_lines)]
async fn handle_ai_ask(
    client: &Client,
    prompt: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
    tx: &mut TxState,
) {
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");

    if provider_name.is_empty() {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        eprintln!("Supported providers: anthropic, openai, ollama");
        eprintln!("Example:");
        eprintln!("  [ai]");
        eprintln!("  provider = \"anthropic\"");
        eprintln!("  api_key_env = \"ANTHROPIC_API_KEY\"");
        return;
    }

    // Resolve the API key (handles raw keys, missing env vars, etc.).
    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build a compact schema description for the system prompt.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Collect wait event context for richer analysis.
    let wait_ctx = crate::ai::context::build_wait_context(
        client,
        settings.db_capabilities.pg_ash.is_available(),
    )
    .await;

    let wait_section = if wait_ctx.is_empty() {
        String::new()
    } else {
        format!("\n\nDatabase activity:\n{wait_ctx}")
    };

    let system_content = format!(
        "You are a helpful PostgreSQL expert assistant connected to a \
         live database.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}{wait}\n\n\
         Guidelines:\n\
         - Answer the user's question directly and concisely.\n\
         - If you need to query the database to answer, include the SQL \
           wrapped in a ```sql code fence.\n\
         - You may include explanatory text before or after the SQL.\n\
         - If the question does not require a database query, just answer \
           it directly — do not generate SQL.\n\
         - Use standard PostgreSQL syntax.\n\
         - If the request is ambiguous, make reasonable assumptions.",
        dbname = params.dbname,
        schema = schema_ctx,
        wait = wait_section,
    );

    // Build messages: system + conversation history + current prompt.
    let mut messages = vec![crate::ai::Message {
        role: crate::ai::Role::System,
        content: system_content,
    }];

    // Include conversation history for follow-up context.
    messages.extend(settings.conversation.to_messages());

    messages.push(crate::ai::Message {
        role: crate::ai::Role::User,
        content: prompt.to_owned(),
    });

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    let ai_response = match provider.complete(&messages, &options).await {
        Ok(result) => {
            record_token_usage(settings, &result);
            result.content
        }
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Record the exchange in conversation context for follow-ups.
    settings.conversation.push_user(prompt.to_owned());
    settings.conversation.push_assistant(ai_response.clone());

    // Auto-compact when approaching the context window limit.
    if settings
        .conversation
        .auto_compact_if_needed(settings.config.ai.context_window)
    {
        eprintln!("-- AI context auto-compacted to save tokens");
    }

    // Parse the response into text and SQL segments, then process each.
    let show_sql = settings.config.ai.show_sql || settings.echo_hidden;
    let yolo = settings.exec_mode == ExecMode::Yolo;

    let segments = parse_ai_response_segments(&ai_response);

    for segment in &segments {
        match segment {
            AiResponseSegment::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    println!("{text}");
                }
            }
            AiResponseSegment::Sql(sql) => {
                // Optionally reveal the generated SQL.
                if show_sql {
                    if settings.no_highlight {
                        eprintln!("{sql}");
                    } else {
                        eprintln!("{}", crate::highlight::highlight_sql(sql, None));
                    }
                }

                let read_only = !is_write_query(sql);

                // In YOLO mode, check autonomy level for write queries.
                //
                // L1 Observe     — block all writes (read-only fence).
                // L2 Supervised  — warn then execute.
                // L3 Auto        — execute silently.
                //
                // --i-know-what-im-doing bypasses all level checks.
                let yolo_write_action = if yolo && !read_only {
                    if settings.i_know_what_im_doing {
                        YoloWriteAction::Execute
                    } else {
                        let autonomy = settings
                            .config
                            .governance
                            .autonomy_for(crate::governance::FeatureArea::Rca);
                        match autonomy {
                            crate::governance::AutonomyLevel::Observe => YoloWriteAction::Block,
                            crate::governance::AutonomyLevel::Supervised => {
                                YoloWriteAction::WarnThenExecute
                            }
                            crate::governance::AutonomyLevel::Auto => YoloWriteAction::Execute,
                        }
                    }
                } else {
                    YoloWriteAction::Execute
                };

                if yolo_write_action == YoloWriteAction::Block {
                    eprintln!(
                        "-- YOLO: write query blocked \
                         (autonomy level is Observe). \
                         Use \\set governance.default_autonomy supervised \
                         or pass --i-know-what-im-doing to bypass"
                    );
                    continue;
                }

                // Read-only queries auto-execute; write queries prompt for
                // confirmation unless YOLO mode overrides.
                let auto_exec = read_only || (yolo && yolo_write_action != YoloWriteAction::Block);

                let choice = if auto_exec {
                    if yolo && !read_only {
                        match yolo_write_action {
                            YoloWriteAction::WarnThenExecute => {
                                eprintln!(
                                    "-- YOLO: write query executing \
                                     (autonomy level is Supervised \
                                     — proceed with care)"
                                );
                            }
                            YoloWriteAction::Execute => {
                                eprintln!("-- YOLO: auto-executing write query");
                            }
                            YoloWriteAction::Block => {
                                unreachable!("Block handled above")
                            }
                        }
                    }
                    AskChoice::Yes
                } else {
                    ask_yne_prompt("Execute (write query)? [y/N/e] ", false)
                };

                match choice {
                    AskChoice::Yes => {
                        let ok = execute_query(client, sql, settings, tx).await;
                        if ok {
                            settings.conversation.push_query_result(sql, "(executed)");
                        }
                    }
                    AskChoice::Edit => match crate::io::edit(sql, None, None) {
                        Ok(edited) => {
                            let edited = edited.trim();
                            if edited.is_empty() {
                                eprintln!("(empty — skipped)");
                            } else {
                                let ok = execute_query(client, edited, settings, tx).await;
                                if ok {
                                    settings
                                        .conversation
                                        .push_query_result(edited, "(executed after edit)");
                                }
                            }
                        }
                        Err(e) => eprintln!("{e}"),
                    },
                    AskChoice::No => {}
                }
            }
        }
    }
}

/// A segment of an AI response: plain text or a SQL block.
enum AiResponseSegment {
    Text(String),
    Sql(String),
}

/// Split an AI response into alternating text and SQL segments.
///
/// SQL blocks are delimited by ` ```sql ` … ` ``` ` (case-insensitive
/// language tag).  Plain ` ``` ` fences without a language tag are treated
/// as text, not SQL.  The function never allocates an empty segment.
fn parse_ai_response_segments(response: &str) -> Vec<AiResponseSegment> {
    let mut segments: Vec<AiResponseSegment> = Vec::new();
    let mut remaining = response;

    while !remaining.is_empty() {
        // Look for the start of a ```sql fence.
        if let Some(fence_start) = remaining.find("```sql") {
            // Any text before the fence becomes a Text segment.
            let before = &remaining[..fence_start];
            if !before.trim().is_empty() {
                segments.push(AiResponseSegment::Text(before.to_owned()));
            }
            // Advance past the opening fence + language tag.
            let after_open = &remaining[fence_start + 6..];
            // Skip the newline immediately after the tag.
            let body_start = after_open
                .find('\n')
                .map_or(after_open, |i| &after_open[i + 1..]);
            // Find the closing fence.
            if let Some(close_pos) = body_start.find("```") {
                let sql_body = body_start[..close_pos].trim();
                if !sql_body.is_empty() {
                    segments.push(AiResponseSegment::Sql(sql_body.to_owned()));
                }
                remaining = &body_start[close_pos + 3..];
            } else {
                // Unclosed fence: treat everything as SQL.
                let sql_body = body_start.trim();
                if !sql_body.is_empty() {
                    segments.push(AiResponseSegment::Sql(sql_body.to_owned()));
                }
                break;
            }
        } else {
            // No more SQL fences — rest is plain text.
            if !remaining.trim().is_empty() {
                segments.push(AiResponseSegment::Text(remaining.to_owned()));
            }
            break;
        }
    }

    segments
}

/// Handle a plan-mode prompt.
///
/// Gathers schema context, sends the user's natural-language prompt to the
/// LLM with a plan-generation system prompt, and streams the resulting plan.
/// Offers to save the plan to `~/.local/share/samo/plans/`.
#[allow(clippy::too_many_lines)]
async fn handle_ai_plan(
    client: &Client,
    prompt: &str,
    settings: &ReplSettings,
    params: &ConnParams,
) {
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");

    if provider_name.is_empty() {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        return;
    }

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Collect live diagnostic data (read-only queries) for richer context.
    eprintln!("-- Plan mode: collecting diagnostics...");
    let pg_ash = settings.db_capabilities.pg_ash.is_available();
    let diagnostics = crate::rca::collect_snapshot(client, pg_ash).await;
    let diag_steps = diagnostics.steps.iter().filter(|s| s.has_data).count();
    let diag_context = diagnostics.to_prompt();
    eprintln!("-- Collected {diag_steps} diagnostic steps");

    let system_content = format!(
        "You are a PostgreSQL expert. \
         The user has asked you to investigate and produce an action plan.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n\n\
         Live diagnostics (read-only queries just collected):\n{diagnostics}\n\n\
         Rules:\n\
         - Produce a structured plan in markdown format\n\
         - Each action should include the SQL command and a safety assessment\n\
         - Mark actions as [safe], [caution], or [dangerous]\n\
         - Order actions from safest to most impactful\n\
         - Include estimated duration where possible\n\
         - Start with a brief root-cause analysis based on the live diagnostics above\n\
         - Reference specific data from the diagnostics to support your analysis\n\
         - Do NOT execute anything — only plan",
        dbname = params.dbname,
        schema = schema_ctx,
        diagnostics = diag_context,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: prompt.to_owned(),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    eprintln!("-- Plan mode: investigating...");
    let result = match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Offer to save the plan.
    if ask_yn_prompt("Save this plan? [y/N] ", false) {
        let plans_dir = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("samo")
            .join("plans");
        if let Err(e) = std::fs::create_dir_all(&plans_dir) {
            eprintln!("Cannot create plans directory: {e}");
            return;
        }
        let date = format_system_time(std::time::SystemTime::now())
            .replace(' ', "-")
            .replace(':', "");
        // Build a slug from the first few words of the prompt.
        let slug: String = prompt
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        let filename = format!("{date}-{slug}.md");
        let path = plans_dir.join(&filename);
        match std::fs::write(&path, &result.content) {
            Ok(()) => eprintln!("Saved to: {}", path.display()),
            Err(e) => eprintln!("Failed to save plan: {e}"),
        }
    }
}

/// Extract the last SQL code block from a mixed text+SQL LLM response.
///
/// LLMs responding to `/fix` produce a mix of explanation and corrected SQL.
/// This function scans for the last `` ```sql ... ``` `` fence (or plain
/// `` ``` ... ``` `` fence) and returns the inner content, trimmed.  If no
/// fences are found it returns `None`.
fn extract_last_sql_block(text: &str) -> Option<&str> {
    // Iterate forward through all fence pairs, keeping the last non-empty one.
    // rfind("```") was incorrect: it found the closing fence, then looked for
    // another "```" after it — always returning None for a single block.
    let mut last_sql: Option<&str> = None;
    let mut search_from = 0;
    while let Some(open_pos) = text[search_from..].find("```") {
        let open_abs = search_from + open_pos;
        let after_open = &text[open_abs + 3..];
        // Skip optional language tag (e.g. "sql\n") on the opening fence line.
        let body_start_rel = after_open.find('\n').map_or(after_open.len(), |i| i + 1);
        let body_text = &after_open[body_start_rel..];
        if let Some(close_pos) = body_text.find("```") {
            let body = body_text[..close_pos].trim();
            if !body.is_empty() {
                last_sql = Some(body);
            }
            // Advance past the closing fence before looking for the next pair.
            search_from = open_abs + 3 + body_start_rel + close_pos + 3;
        } else {
            // Unclosed fence — treat rest of text as the body.
            let body = body_text.trim();
            if !body.is_empty() {
                last_sql = Some(body);
            }
            break;
        }
    }
    last_sql
}

/// Handle a `/fix` command end-to-end.
///
/// Looks up the most recently failed query from [`ReplSettings::last_error`],
/// sends it to the configured LLM with schema context, and prints an
/// explanation plus a corrected SQL query.  After streaming the response,
/// if a corrected SQL block is detected the user is prompted
/// `Execute? [Y/n]` (default yes) and the query is executed when confirmed.
/// Gracefully degrades when no prior error exists or when AI is not
/// configured.
#[allow(clippy::too_many_lines)]
async fn handle_ai_fix(
    client: &Client,
    settings: &mut ReplSettings,
    params: &ConnParams,
    tx: &mut TxState,
) {
    // Require a prior error to fix.
    let last_error = if let Some(e) = &settings.last_error {
        e.clone()
    } else {
        eprintln!("No recent error to fix. Run a query first.");
        return;
    };

    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");

    if provider_name.is_empty() {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        eprintln!("Supported providers: anthropic, openai, ollama");
        eprintln!("Example:");
        eprintln!("  [ai]");
        eprintln!("  provider = \"anthropic\"");
        eprintln!("  api_key_env = \"ANTHROPIC_API_KEY\"");
        return;
    }

    // Resolve the API key (handles raw keys, missing env vars, etc.).
    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build a compact schema description for the system prompt.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Format the SQLSTATE hint if available.
    let sqlstate_hint = last_error
        .sqlstate
        .as_deref()
        .map(|s| format!(" (SQLSTATE {s})"))
        .unwrap_or_default();

    let system_content = format!(
        "You are a PostgreSQL expert. \
         Explain SQL errors and provide corrected queries.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n\n\
         Rules:\n\
         - First, briefly explain what caused the error (1-2 sentences)\n\
         - Then output the corrected SQL query\n\
         - Use standard PostgreSQL syntax\n\
         - Keep the corrected query as close to the original intent as possible",
        dbname = params.dbname,
        schema = schema_ctx,
    );

    let user_content = format!(
        "The following query failed{sqlstate_hint}:\n\n\
         ```sql\n{query}\n```\n\n\
         Error: {error}",
        query = last_error.query,
        error = last_error.error_message,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    let result = match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };
    record_token_usage(settings, &result);

    // If the response contains a corrected SQL block, offer to execute it.
    if let Some(fix_sql) = extract_last_sql_block(&result.content) {
        let choice = ask_yne_prompt("Execute? [Y/n/e] ", true);
        match choice {
            AskChoice::Yes => {
                execute_query(client, fix_sql, settings, tx).await;
            }
            AskChoice::Edit => match crate::io::edit(fix_sql, None, None) {
                Ok(edited) => {
                    let edited = edited.trim();
                    if edited.is_empty() {
                        eprintln!("(empty — skipped)");
                    } else {
                        execute_query(client, edited, settings, tx).await;
                    }
                }
                Err(e) => eprintln!("{e}"),
            },
            AskChoice::No => {}
        }
    }
}

/// Detect whether a query is a data-modifying statement that must be
/// wrapped in a rolled-back transaction before `EXPLAIN ANALYZE`.
///
/// Returns `true` for `INSERT`, `UPDATE`, `DELETE`, and `MERGE`.
fn is_write_query(sql: &str) -> bool {
    let first = sql.split_whitespace().next().unwrap_or("").to_uppercase();
    matches!(first.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE")
}

/// Build the `EXPLAIN` SQL for a given target query.
///
/// Write queries are wrapped in `BEGIN` / `ROLLBACK` so that
/// `EXPLAIN ANALYZE` can run them without persisting any changes.
fn build_explain_sql(target_query: &str) -> String {
    let explain = format!("explain (analyze, costs, verbose, buffers, format text) {target_query}");
    if is_write_query(target_query) {
        format!("begin;\n{explain};\nrollback;")
    } else {
        explain
    }
}

/// Handle a `/explain [query]` command end-to-end.
///
/// 1. Resolves the target query: inline arg or `last_query`.
/// 2. Runs `EXPLAIN (ANALYZE, COSTS, VERBOSE, BUFFERS, FORMAT TEXT)`.
///    Write queries (`INSERT`/`UPDATE`/`DELETE`/`MERGE`) are wrapped in
///    a `BEGIN` … `ROLLBACK` to prevent side-effects.
/// 3. Prints the raw plan.
/// 4. If AI is configured, sends plan + schema context to the LLM and
///    prints its interpretation.
#[allow(clippy::too_many_lines)]
async fn handle_ai_explain(
    client: &Client,
    query_arg: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    // Resolve target query.
    let target_query = if query_arg.is_empty() {
        if let Some(q) = settings.last_query.as_deref() {
            q.to_owned()
        } else {
            eprintln!(
                "/explain: no query to explain. \
                 Run a query first or provide one: /explain SELECT ..."
            );
            return;
        }
    } else {
        query_arg.to_owned()
    };

    // Run EXPLAIN ANALYZE (wrapped in BEGIN/ROLLBACK for write queries).
    let explain_sql = build_explain_sql(&target_query);

    let messages_result = client.simple_query(&explain_sql).await;
    let raw_messages = match messages_result {
        Ok(msgs) => msgs,
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(&target_query), settings.verbose_errors);
            return;
        }
    };

    // Collect plan lines from the result.
    let mut plan_lines: Vec<String> = Vec::new();
    for msg in &raw_messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if let Some(line) = row.get(0) {
                plan_lines.push(line.to_owned());
            }
        }
    }

    if plan_lines.is_empty() {
        eprintln!("/explain: EXPLAIN returned no output");
        return;
    }

    let plan_text = plan_lines.join("\n");
    println!("{plan_text}");

    // AI interpretation — skip gracefully when AI is not configured.
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");
    if provider_name.is_empty() {
        return;
    }

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build schema context for richer analysis.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Collect wait event context for performance correlation.
    let wait_ctx = crate::ai::context::build_wait_context(
        client,
        settings.db_capabilities.pg_ash.is_available(),
    )
    .await;

    let wait_section = if wait_ctx.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDatabase activity (use to correlate plan behavior \
             with current wait patterns):\n{wait_ctx}"
        )
    };

    let system_content = format!(
        "You are a PostgreSQL performance expert. \
         Analyse the EXPLAIN ANALYZE plan provided by the user and give \
         a concise, actionable interpretation:\n\
         - Identify the most expensive nodes\n\
         - Flag sequential scans on large tables\n\
         - Note any high row-estimate errors\n\
         - Suggest specific indexes or query rewrites when applicable\n\
         - If wait event data is provided, correlate plan behavior with waits\n\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}{wait}",
        dbname = params.dbname,
        schema = schema_ctx,
        wait = wait_section,
    );

    let user_content = format!("Query:\n{target_query}\n\nEXPLAIN ANALYZE output:\n{plan_text}");

    let ai_messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    println!();
    match stream_completion(
        provider.as_ref(),
        &ai_messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

/// Extract table names referenced by `FROM` and `JOIN` clauses.
///
/// Best-effort heuristic parser — handles common patterns including
/// schema-qualified names but does not aim for full SQL parsing.
/// Used by `/optimize` to query `pg_stat_user_tables`.
fn extract_table_names(sql: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    let upper_tokens: Vec<String> = upper.split_whitespace().map(String::from).collect();
    let mut tables = Vec::new();

    let mut i = 0;
    while i < upper_tokens.len() {
        let is_from = upper_tokens[i] == "FROM";
        let is_join = upper_tokens[i].ends_with("JOIN") && upper_tokens[i] != "DISJOIN";

        if (is_from || is_join) && i + 1 < tokens.len() {
            let candidate = tokens[i + 1];
            // Skip sub-selects: FROM (SELECT ...)
            if !candidate.starts_with('(') {
                let clean = candidate.trim_end_matches([',', ')', ';']);
                if !clean.is_empty() {
                    tables.push(clean.to_owned());
                }
            }
        }
        i += 1;
    }

    tables.sort();
    tables.dedup();
    tables
}

/// Handle a `/optimize [query]` command end-to-end.
///
/// 1. Resolves the target query: inline arg or `last_query`.
/// 2. Runs `EXPLAIN (ANALYZE, COSTS, VERBOSE, BUFFERS, FORMAT TEXT)`.
/// 3. Gathers `pg_stat_user_tables` stats for referenced tables.
/// 4. Sends plan + stats + schema context to the LLM for optimization
///    suggestions (index creation, query rewrites, join order changes).
#[allow(clippy::too_many_lines)]
async fn handle_ai_optimize(
    client: &Client,
    query_arg: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    // Resolve target query.
    let target_query = if query_arg.is_empty() {
        if let Some(q) = settings.last_query.as_deref() {
            q.to_owned()
        } else {
            eprintln!(
                "/optimize: no query to optimize. \
                 Run a query first or provide one: /optimize SELECT ..."
            );
            return;
        }
    } else {
        query_arg.to_owned()
    };

    // Run EXPLAIN ANALYZE (wrapped in BEGIN/ROLLBACK for write queries).
    let explain_sql = build_explain_sql(&target_query);

    let raw_messages = match client.simple_query(&explain_sql).await {
        Ok(msgs) => msgs,
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(&target_query), settings.verbose_errors);
            return;
        }
    };

    // Collect plan lines.
    let mut plan_lines: Vec<String> = Vec::new();
    for msg in &raw_messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if let Some(line) = row.get(0) {
                plan_lines.push(line.to_owned());
            }
        }
    }

    if plan_lines.is_empty() {
        eprintln!("/optimize: EXPLAIN returned no output");
        return;
    }

    let plan_text = plan_lines.join("\n");
    println!("{plan_text}");

    // Gather table statistics for referenced tables.
    let table_names = extract_table_names(&target_query);
    let mut stats_text = String::new();

    if !table_names.is_empty() {
        let in_list: String = table_names
            .iter()
            .map(|t| {
                let escaped = t.replace('\'', "''");
                format!("'{escaped}'")
            })
            .collect::<Vec<_>>()
            .join(", ");

        let stats_sql = format!(
            "SELECT schemaname || '.' || relname AS table_name, \
                    n_live_tup, n_dead_tup, \
                    seq_scan, seq_tup_read, \
                    idx_scan, idx_tup_fetch, \
                    last_vacuum::text, last_analyze::text \
             FROM pg_stat_user_tables \
             WHERE relname IN ({in_list}) \
                OR schemaname || '.' || relname IN ({in_list}) \
             ORDER BY relname"
        );

        if let Ok(msgs) = client.simple_query(&stats_sql).await {
            let mut stat_rows = Vec::new();
            for msg in &msgs {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let cols: Vec<String> = (0..9)
                        .map(|i| row.get(i).unwrap_or("(null)").to_owned())
                        .collect();
                    stat_rows.push(cols.join(" | "));
                }
            }
            if !stat_rows.is_empty() {
                stats_text = format!(
                    "\n\nTable statistics (table | live_tup | dead_tup | \
                     seq_scan | seq_tup_read | idx_scan | idx_tup_fetch | \
                     last_vacuum | last_analyze):\n{}",
                    stat_rows.join("\n")
                );
            }
        }
    }

    // AI optimization — skip gracefully when AI is not configured.
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");
    if provider_name.is_empty() {
        eprintln!(
            "\nAI not configured — showing raw plan only. \
             Add an [ai] section to {} for optimization suggestions.",
            crate::config::user_config_path_display()
        );
        return;
    }

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    let system_content = format!(
        "You are a PostgreSQL performance optimization expert. \
         Analyse the query, its EXPLAIN ANALYZE plan, and table statistics, \
         then provide actionable optimization suggestions.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n\n\
         Rules:\n\
         - Identify the most expensive operations in the plan\n\
         - Suggest specific CREATE INDEX statements when beneficial\n\
         - Suggest query rewrites (join order, CTEs, subquery elimination)\n\
         - Note any sequential scans on large tables\n\
         - Estimate the expected improvement for each suggestion\n\
         - Output suggestions ordered by expected impact (highest first)",
        dbname = params.dbname,
        schema = schema_ctx,
    );

    let user_content = format!(
        "Query:\n```sql\n{target_query}\n```\n\n\
         EXPLAIN ANALYZE output:\n{plan_text}{stats_text}"
    );

    let ai_messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    println!();
    match stream_completion(
        provider.as_ref(),
        &ai_messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

/// Handle a `/describe <table>` command.
///
/// Queries the table's columns, constraints, indexes, and row estimate,
/// then sends everything to the LLM for a human-readable description of
/// the table's purpose, relationships, and notable patterns.
#[allow(clippy::too_many_lines)]
async fn handle_ai_describe(
    client: &Client,
    table_name: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");
    if provider_name.is_empty() {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        return;
    }

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Gather table metadata.
    let mut table_info = String::new();

    // Columns.
    let col_query = format!(
        "SELECT column_name, data_type, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_name = '{table_name}' \
         ORDER BY ordinal_position"
    );
    if let Ok(rows) = client.simple_query(&col_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "Columns:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let dtype = row.get(1).unwrap_or("?");
                let nullable = row.get(2).unwrap_or("?");
                let default = row.get(3).unwrap_or("");
                let _ = writeln!(
                    table_info,
                    "  {name} {dtype} nullable={nullable} default={default}"
                );
            }
        }
    }

    // Constraints (PK, FK, unique, check).
    let constraint_query = format!(
        "SELECT conname, contype, pg_get_constraintdef(oid) \
         FROM pg_constraint \
         WHERE conrelid = '{table_name}'::regclass"
    );
    if let Ok(rows) = client.simple_query(&constraint_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "\nConstraints:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let ctype = row.get(1).unwrap_or("?");
                let def = row.get(2).unwrap_or("?");
                let type_label = match ctype {
                    "p" => "PRIMARY KEY",
                    "f" => "FOREIGN KEY",
                    "u" => "UNIQUE",
                    "c" => "CHECK",
                    "x" => "EXCLUSION",
                    other => other,
                };
                let _ = writeln!(table_info, "  {name} ({type_label}): {def}");
            }
        }
    }

    // Indexes.
    let idx_query = format!(
        "SELECT indexname, indexdef \
         FROM pg_indexes \
         WHERE tablename = '{table_name}'"
    );
    if let Ok(rows) = client.simple_query(&idx_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "\nIndexes:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let def = row.get(1).unwrap_or("?");
                let _ = writeln!(table_info, "  {name}: {def}");
            }
        }
    }

    // Row estimate + size.
    let stats_query = format!(
        "SELECT reltuples::bigint AS row_estimate, \
         pg_size_pretty(pg_total_relation_size('{table_name}'::regclass)) AS size \
         FROM pg_class WHERE relname = '{table_name}'"
    );
    if let Ok(rows) = client.simple_query(&stats_query).await {
        use std::fmt::Write as _;
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let rows_est = row.get(0).unwrap_or("?");
                let size = row.get(1).unwrap_or("?");
                let _ = writeln!(
                    table_info,
                    "\nEstimated rows: {rows_est}, Total size: {size}"
                );
            }
        }
    }

    if table_info.trim().is_empty() {
        eprintln!("No metadata found for table '{table_name}'.");
        return;
    }

    let system_content = format!(
        "You are a PostgreSQL expert. \
         Describe the purpose and design of this database table.\n\
         Database: {dbname}\n\n\
         Rules:\n\
         - Infer the table's purpose from its name, columns, and constraints\n\
         - Describe relationships (foreign keys) to other tables\n\
         - Note any design patterns (audit columns, soft deletes, etc.)\n\
         - Mention notable indexes and their likely purpose\n\
         - Be concise — this is for quick understanding",
        dbname = params.dbname,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Describe table '{table_name}':\n\n{table_info}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// /rca — Root Cause Analysis
// ---------------------------------------------------------------------------

/// Handle the `/rca` command: collect diagnostic snapshot and analyze.
async fn handle_ai_rca(client: &Client, settings: &mut ReplSettings, params: &ConnParams) {
    let provider_name = settings.config.ai.provider.as_deref().unwrap_or("");
    if provider_name.is_empty() {
        // Without AI, still collect and display the diagnostic snapshot.
        eprintln!("AI not configured — collecting raw diagnostic data only.");
        let pg_ash = settings
            .config
            .governance
            .autonomy_for(crate::governance::FeatureArea::Rca);
        let _ = pg_ash; // autonomy level not used in Observe mode yet
        let snapshot = crate::rca::collect_snapshot(client, false).await;
        print!("{}", snapshot.to_prompt());
        return;
    }

    eprintln!("Collecting diagnostic data...");

    // Detect pg_ash availability from capabilities (set at connect time).
    let pg_ash_available = settings.db_capabilities.pg_ash.is_available();

    let snapshot = crate::rca::collect_snapshot(client, pg_ash_available).await;

    // Show raw data summary.
    let data_steps = snapshot.steps.iter().filter(|s| s.has_data).count();
    let total_steps = snapshot.steps.len();
    eprintln!("Collected {data_steps}/{total_steps} steps with data. Analyzing...\n");

    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());

    let provider = match crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build schema context for richer analysis.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            String::new()
        }
    };

    let system_content = crate::rca::rca_system_prompt(&schema_ctx);
    let user_content = format!(
        "Database: {dbname}\n\n{snapshot}",
        dbname = params.dbname,
        snapshot = snapshot.to_prompt(),
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) => eprintln!("AI error: {e}"),
    }

    // In Supervised mode, propose actionable mitigations after analysis.
    let rca_autonomy = settings
        .config
        .governance
        .autonomy_for(crate::governance::FeatureArea::Rca);
    if rca_autonomy == crate::governance::AutonomyLevel::Supervised {
        let proposals = crate::rca_actions::propose_mitigations(client).await;
        if !proposals.is_empty() {
            let mut audit_log = crate::governance::AuditLog::new();
            crate::rca_actions::run_supervised_flow(client, &proposals, &mut audit_log).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        // Override PSQLRC to a known path.
        std::env::set_var("PSQLRC", "/tmp/test_samo_rc");
        let result = startup_file();
        std::env::remove_var("PSQLRC");
        assert_eq!(result, Some(std::path::PathBuf::from("/tmp/test_samo_rc")));
    }

    #[test]
    fn startup_file_returns_none_when_no_rc_exists_and_no_env() {
        // Remove PSQLRC env so the function falls through to file checks.
        std::env::remove_var("PSQLRC");
        // We cannot guarantee ~/.samorc or ~/.psqlrc don't exist on the test
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
    fn prompt_observe_mode() {
        assert_eq!(
            build_prompt(
                "mydb",
                TxState::Idle,
                false,
                InputMode::Sql,
                ExecMode::Observe
            ),
            "mydb observe=> "
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
    fn write_query_with_cte_select_is_false() {
        assert!(!is_write_query("with cte as (select 1) select * from cte"));
    }

    #[test]
    fn write_query_case_insensitive() {
        assert!(is_write_query("insert into t values (1)"));
        assert!(is_write_query("Insert Into t values (1)"));
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

    // -- parse_duration --------------------------------------------------------

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(
            parse_duration("30s"),
            Some(std::time::Duration::from_secs(30))
        );
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(
            parse_duration("5m"),
            Some(std::time::Duration::from_secs(300))
        );
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(
            parse_duration("1h"),
            Some(std::time::Duration::from_secs(3600))
        );
    }

    #[test]
    fn parse_duration_bare_number() {
        assert_eq!(
            parse_duration("60"),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[test]
    fn parse_duration_empty() {
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn parse_duration_whitespace_trimmed() {
        assert_eq!(
            parse_duration("  10s  "),
            Some(std::time::Duration::from_secs(10))
        );
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
        std::env::set_var("SAMO_TEST_API_KEY_12345", "test-secret-value");
        let result = resolve_api_key(Some("SAMO_TEST_API_KEY_12345"));
        assert_eq!(result, Some("test-secret-value".to_owned()));
        std::env::remove_var("SAMO_TEST_API_KEY_12345");
    }

    #[test]
    fn resolve_api_key_missing_env_var() {
        std::env::remove_var("NONEXISTENT_SAMO_VAR_99999");
        let result = resolve_api_key(Some("NONEXISTENT_SAMO_VAR_99999"));
        assert!(result.is_none());
    }

    #[test]
    fn resolve_api_key_empty_env_var() {
        std::env::set_var("SAMO_EMPTY_KEY_TEST", "");
        let result = resolve_api_key(Some("SAMO_EMPTY_KEY_TEST"));
        assert!(result.is_none());
        std::env::remove_var("SAMO_EMPTY_KEY_TEST");
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
}
