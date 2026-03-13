//! Interactive REPL loop for Samo.
//!
//! Provides readline-based line editing with persistent history, multi-line
//! SQL accumulation, backslash command handling, transaction-state prompts,
//! and signal-aware Ctrl-C / Ctrl-D behaviour.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::{Config, Editor};
use tokio_postgres::Client;

use crate::connection::ConnParams;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default history file path (relative to home directory).
const DEFAULT_HISTORY_FILE: &str = ".samo_history";

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

/// Build the main prompt string from a database name and transaction state.
///
/// Format: `dbname=>` (idle), `dbname=*>` (in-tx), `dbname=!>` (failed).
/// Continuation uses `-` instead of `=` as the first separator.
pub fn build_prompt(dbname: &str, tx: TxState, continuation: bool) -> String {
    let infix = tx.infix();
    if continuation {
        format!("{dbname}-{infix}> ")
    } else {
        format!("{dbname}={infix}> ")
    }
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

/// Parsed backslash command.
#[derive(Debug, PartialEq, Eq)]
pub enum BackslashCmd {
    Quit,
    Timing(Option<bool>),
    Expanded(ExpandedMode),
    ConnInfo,
    Help,
    Unknown(String),
}

/// Expanded display mode argument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExpandedMode {
    On,
    #[default]
    Off,
    Auto,
    Toggle,
}

/// Parse a backslash command string (without leading `\`).
pub fn parse_backslash(input: &str) -> BackslashCmd {
    let input = input.trim_start_matches('\\');
    let mut parts = input.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("").trim();
    let arg = parts.next().map_or("", str::trim).trim();

    match cmd {
        "q" | "quit" => BackslashCmd::Quit,
        "?" => BackslashCmd::Help,
        "conninfo" => BackslashCmd::ConnInfo,
        "timing" => {
            let mode = match arg.to_lowercase().as_str() {
                "on" => Some(true),
                "off" => Some(false),
                _ => None, // empty or unknown → toggle
            };
            BackslashCmd::Timing(mode)
        }
        "x" => {
            let mode = match arg.to_lowercase().as_str() {
                "on" => ExpandedMode::On,
                "off" => ExpandedMode::Off,
                "auto" => ExpandedMode::Auto,
                _ => ExpandedMode::Toggle, // empty or unknown → toggle
            };
            BackslashCmd::Expanded(mode)
        }
        other => BackslashCmd::Unknown(format!("\\{other}")),
    }
}

// ---------------------------------------------------------------------------
// REPL settings (mutable at runtime via backslash commands)
// ---------------------------------------------------------------------------

/// Runtime-adjustable display settings.
#[derive(Debug, Default)]
pub struct ReplSettings {
    /// Whether to print query timing after each query.
    pub timing: bool,
    /// Expanded display mode.
    pub expanded: ExpandedMode,
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
// Query execution (stub — #19 will provide the proper implementation)
// ---------------------------------------------------------------------------

/// Execute a SQL string using `simple_query` and print a basic result.
///
/// This is a stub implementation. Issue #19 will replace this with proper
/// column-aligned output formatting.
///
/// Returns `true` on success, `false` if the query produced a SQL error.
pub async fn execute_query(
    client: &Client,
    sql: &str,
    settings: &ReplSettings,
    tx: &mut TxState,
) -> bool {
    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    let success = match client.simple_query(sql).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut rows_affected: Option<u64> = None;
            let mut had_rows = false;

            for msg in messages {
                match msg {
                    SimpleQueryMessage::Row(row) => {
                        had_rows = true;
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
                        rows_affected = Some(n);
                    }
                    _ => {}
                }
            }

            // Update transaction state based on what SQL was sent.
            tx.update_from_sql(sql);

            // Print result table.
            if had_rows {
                if !col_names.is_empty() {
                    // Compute column widths.
                    let mut widths: Vec<usize> = col_names.iter().map(String::len).collect();
                    for row in &rows {
                        for (i, val) in row.iter().enumerate() {
                            if i < widths.len() {
                                widths[i] = widths[i].max(val.len());
                            }
                        }
                    }

                    // Header row
                    let header: Vec<String> = col_names
                        .iter()
                        .enumerate()
                        .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                        .collect();
                    println!(" {} ", header.join(" | "));

                    // Separator
                    let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
                    println!("-{}-", sep.join("-+-"));

                    // Data rows
                    for row in &rows {
                        let cells: Vec<String> = row
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                format!("{:<width$}", v, width = *widths.get(i).unwrap_or(&v.len()))
                            })
                            .collect();
                        println!(" {} ", cells.join(" | "));
                    }

                    let nrows = rows.len();
                    let row_word = if nrows == 1 { "row" } else { "rows" };
                    println!("({nrows} {row_word})");
                }
            } else if let Some(n) = rows_affected {
                // Non-SELECT statement: show rows affected if > 0.
                if n > 0 {
                    println!("{n}");
                }
            }
            true
        }
        Err(e) => {
            eprintln!("ERROR:  {e}");
            tx.on_error();
            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        println!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }

    success
}

// ---------------------------------------------------------------------------
// Non-interactive (piped / -c / -f) execution
// ---------------------------------------------------------------------------

/// Execute a single SQL command string (from `-c`) and exit.
pub async fn exec_command(client: &Client, sql: &str, settings: &ReplSettings) -> i32 {
    let mut tx = TxState::default();
    if sql.trim_start().starts_with('\\') {
        // Backslash command in -c mode: run and exit
        eprintln!("samo: backslash commands not supported in -c mode");
        return 1;
    }
    i32::from(!execute_query(client, sql, settings, &mut tx).await)
}

/// Execute all SQL statements from a file and exit.
///
/// The file content is split at statement boundaries (`;` outside quotes and
/// comments) and each statement is executed individually, matching the
/// behaviour of `exec_stdin`.
///
/// # Errors
/// Returns 1 if the file cannot be read or any statement produces a SQL error.
pub async fn exec_file(client: &Client, path: &str, settings: &ReplSettings) -> i32 {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("samo: could not read file \"{path}\": {e}");
            return 1;
        }
    };
    let mut tx = TxState::default();
    let mut buf = String::new();
    let mut exit_code = 0i32;

    for line in content.lines() {
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);

        if is_complete(&buf) {
            let sql = buf.trim().to_owned();
            if !execute_query(client, &sql, settings, &mut tx).await {
                exit_code = 1;
            }
            buf.clear();
        }
    }

    // Execute any trailing input without a semicolon.
    if !buf.trim().is_empty() && !execute_query(client, buf.trim(), settings, &mut tx).await {
        exit_code = 1;
    }

    exit_code
}

/// Execute SQL lines from stdin (non-interactive piped input).
pub async fn exec_stdin(client: &Client, settings: &ReplSettings) -> i32 {
    let stdin = io::stdin();
    let mut buf = String::new();
    let mut tx = TxState::default();
    let mut exit_code = 0i32;

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("samo: read error: {e}");
                return 1;
            }
        };

        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&line);

        if is_complete(&buf) {
            if !execute_query(client, buf.trim(), settings, &mut tx).await {
                exit_code = 1;
            }
            buf.clear();
        }
    }

    // Execute any trailing input without a semicolon.
    if !buf.trim().is_empty() && !execute_query(client, buf.trim(), settings, &mut tx).await {
        exit_code = 1;
    }

    exit_code
}

// ---------------------------------------------------------------------------
// Interactive REPL
// ---------------------------------------------------------------------------

/// Print the backslash command help text.
fn print_help() {
    println!(
        r"Backslash commands:
  \q          quit samo
  \timing [on|off]  toggle/set query timing display
  \x [on|off|auto]  toggle/set expanded display
  \conninfo   show connection information
  \?          show this help"
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
    println!(
        "Expanded display is {}.",
        expanded_mode_str(settings.expanded)
    );
}

/// Run the interactive REPL loop.
///
/// Accepts caller-provided `settings` so that flags set on the command line
/// (e.g. `--timing`, `--expanded`) take effect immediately.
///
/// Returns the exit code (0 = normal exit, non-zero = error).
pub async fn run_repl(
    client: &Client,
    params: &ConnParams,
    settings: ReplSettings,
    no_readline: bool,
) -> i32 {
    let mut settings = settings;
    let mut tx = TxState::default();
    let dbname = params.dbname.clone();

    // Build rustyline editor (skip if --no-readline).
    let use_readline = !no_readline && io::stdin().is_terminal();

    if use_readline {
        run_readline_loop(&dbname, client, params, &mut settings, &mut tx).await
    } else {
        run_dumb_loop(&dbname, client, params, &mut settings, &mut tx).await
    }
}

/// Run with rustyline readline support.
async fn run_readline_loop(
    dbname: &str,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let config = Config::builder()
        .max_history_size(HISTORY_SIZE)
        .expect("valid history size")
        .history_ignore_space(true)
        .build();

    let mut rl: Editor<(), FileHistory> = match Editor::with_config(config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("samo: readline init failed: {e}");
            return 1;
        }
    };

    let hist_path = history_file();
    if let Some(ref p) = hist_path {
        // Best-effort — ignore errors (file may not exist yet).
        let _ = rl.load_history(p);
    }

    let mut buf = String::new();
    // Accumulates the complete multi-line statement text for history.
    let mut stmt_buf = String::new();

    loop {
        let prompt = build_prompt(dbname, *tx, !buf.is_empty());

        match rl.readline(&prompt) {
            Ok(line) => {
                // Ctrl-C on empty line: stay at prompt (readline already
                // handles Ctrl-C during input by returning Interrupted).
                let should_exit =
                    handle_line(&line, &mut buf, &mut stmt_buf, client, params, settings, tx).await;

                // If buf is empty a statement was completed — add the full
                // accumulated statement text to history.
                if buf.is_empty() && !stmt_buf.trim().is_empty() {
                    let _ = rl.add_history_entry(stmt_buf.trim());
                    stmt_buf.clear();
                }

                if should_exit {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C: clear current buffer, back to prompt.
                if !buf.is_empty() {
                    buf.clear();
                    stmt_buf.clear();
                }
                // On empty line Ctrl-C does nothing (just re-prompt).
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

    0
}

/// Run without readline (dumb terminal or --no-readline).
async fn run_dumb_loop(
    dbname: &str,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let stdin = io::stdin();
    let mut buf = String::new();

    loop {
        // Print prompt to stderr (so it doesn't mix with redirected output).
        let prompt = build_prompt(dbname, *tx, !buf.is_empty());
        eprint!("{prompt}");
        let _ = io::stderr().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF / Ctrl-D
            Ok(_) => {
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
                if line.trim_start().starts_with('\\') {
                    if handle_backslash_dumb(line.trim(), params, settings) {
                        break;
                    }
                } else {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(&line);
                    if is_complete(&buf) {
                        let sql = buf.trim().to_owned();
                        execute_query(client, &sql, settings, tx).await;
                        buf.clear();
                    }
                }
            }
            Err(e) => {
                eprintln!("samo: read error: {e}");
                return 1;
            }
        }
    }

    0
}

/// Handle a single input line in the dumb loop (backslash commands).
///
/// Returns `true` if the loop should exit (i.e. `\q` was issued).
fn handle_backslash_dumb(input: &str, params: &ConnParams, settings: &mut ReplSettings) -> bool {
    let cmd = parse_backslash(input);
    match cmd {
        BackslashCmd::Quit => {
            return true;
        }
        BackslashCmd::Help => {
            print_help();
        }
        BackslashCmd::Timing(mode) => apply_timing(settings, mode),
        BackslashCmd::Expanded(mode) => apply_expanded(settings, mode),
        BackslashCmd::ConnInfo => {
            println!("{}", crate::connection::connection_info(params));
        }
        BackslashCmd::Unknown(name) => {
            eprintln!("Invalid command {name}. Try \\? for help.");
        }
    }
    false
}

/// Process one line of input in the readline loop.
///
/// `stmt_buf` accumulates the full multi-line statement for history recording.
///
/// Returns `true` if the REPL should exit (i.e. `\q` was entered).
async fn handle_line(
    line: &str,
    buf: &mut String,
    stmt_buf: &mut String,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    if line.trim_start().starts_with('\\') {
        // Backslash command — execute immediately.
        let cmd = parse_backslash(line.trim());
        match cmd {
            BackslashCmd::Quit => {
                // Signal the caller to break the loop so history is saved.
                return true;
            }
            BackslashCmd::Help => {
                print_help();
            }
            BackslashCmd::Timing(mode) => apply_timing(settings, mode),
            BackslashCmd::Expanded(mode) => apply_expanded(settings, mode),
            BackslashCmd::ConnInfo => {
                println!("{}", crate::connection::connection_info(params));
            }
            BackslashCmd::Unknown(name) => {
                eprintln!("Invalid command {name}. Try \\? for help.");
            }
        }
        return false;
    }

    // SQL input: accumulate lines until we have a complete statement.
    if !buf.is_empty() {
        buf.push('\n');
        stmt_buf.push('\n');
    }
    buf.push_str(line);
    stmt_buf.push_str(line);

    if is_complete(buf) {
        let sql = buf.trim().to_owned();
        execute_query(client, &sql, settings, tx).await;
        buf.clear();
        // stmt_buf is cleared by the caller after adding to history.
    }

    false
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

    // -- parse_backslash -------------------------------------------------------

    #[test]
    fn parse_quit() {
        assert_eq!(parse_backslash("\\q"), BackslashCmd::Quit);
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse_backslash("\\?"), BackslashCmd::Help);
    }

    #[test]
    fn parse_conninfo() {
        assert_eq!(parse_backslash("\\conninfo"), BackslashCmd::ConnInfo);
    }

    #[test]
    fn parse_timing_on() {
        assert_eq!(
            parse_backslash("\\timing on"),
            BackslashCmd::Timing(Some(true))
        );
    }

    #[test]
    fn parse_timing_off() {
        assert_eq!(
            parse_backslash("\\timing off"),
            BackslashCmd::Timing(Some(false))
        );
    }

    #[test]
    fn parse_timing_toggle() {
        assert_eq!(parse_backslash("\\timing"), BackslashCmd::Timing(None));
    }

    #[test]
    fn parse_expanded_on() {
        assert_eq!(
            parse_backslash("\\x on"),
            BackslashCmd::Expanded(ExpandedMode::On)
        );
    }

    #[test]
    fn parse_expanded_auto() {
        assert_eq!(
            parse_backslash("\\x auto"),
            BackslashCmd::Expanded(ExpandedMode::Auto)
        );
    }

    #[test]
    fn parse_expanded_toggle() {
        assert_eq!(
            parse_backslash("\\x"),
            BackslashCmd::Expanded(ExpandedMode::Toggle)
        );
    }

    #[test]
    fn parse_unknown_command() {
        assert_eq!(
            parse_backslash("\\foo"),
            BackslashCmd::Unknown("\\foo".to_owned())
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

    // -- build_prompt ----------------------------------------------------------

    #[test]
    fn prompt_idle() {
        assert_eq!(build_prompt("mydb", TxState::Idle, false), "mydb=> ");
    }

    #[test]
    fn prompt_in_transaction() {
        assert_eq!(
            build_prompt("mydb", TxState::InTransaction, false),
            "mydb=*> "
        );
    }

    #[test]
    fn prompt_failed_transaction() {
        assert_eq!(build_prompt("mydb", TxState::Failed, false), "mydb=!> ");
    }

    #[test]
    fn prompt_continuation() {
        assert_eq!(build_prompt("mydb", TxState::Idle, true), "mydb-> ");
    }

    #[test]
    fn prompt_continuation_in_transaction() {
        assert_eq!(
            build_prompt("mydb", TxState::InTransaction, true),
            "mydb-*> "
        );
    }
}
