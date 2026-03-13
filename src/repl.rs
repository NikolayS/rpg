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

// ExpandedMode is defined in output.rs and re-exported here for backward
// compatibility with code that imports from repl.
pub use crate::output::ExpandedMode;

// ---------------------------------------------------------------------------
// REPL settings (mutable at runtime via backslash commands)
// ---------------------------------------------------------------------------

/// Runtime-adjustable display settings.
#[derive(Default)]
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
            .finish()
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
// Query execution (stub — #19 will provide the proper implementation)
// ---------------------------------------------------------------------------

/// Print a single result set using the active [`PsetConfig`].
///
/// `col_names` and `rows` describe the result set. `had_rows` indicates
/// whether any `Row` messages were received (distinguishes an empty SELECT
/// from a DML command). `rows_affected` carries the `CommandComplete` count.
/// `is_first` is `false` when this is a subsequent result set in a
/// multi-statement query, in which case a blank separator line is printed
/// before the table (matching psql behaviour).
/// `writer` is the output destination (stdout or a redirected file).
fn print_result_set_pset(
    writer: &mut dyn io::Write,
    col_names: &[String],
    rows: &[Vec<String>],
    had_rows: bool,
    rows_affected: u64,
    is_first: bool,
    pset: &crate::output::PsetConfig,
) {
    use crate::output::format_rowset_pset;
    use crate::query::{ColumnMeta, RowSet};

    if had_rows {
        if !col_names.is_empty() {
            if !is_first {
                let _ = writeln!(writer);
            }

            // Build a RowSet from the raw string data.
            let columns: Vec<ColumnMeta> = col_names
                .iter()
                .map(|n| ColumnMeta {
                    name: n.clone(),
                    is_numeric: false,
                })
                .collect();
            // simple_query returns NULL as empty string; we wrap every cell
            // in Some to distinguish "empty string" from "NULL" at the pset
            // formatting layer (which uses null_display).  The distinction
            // is lost at this protocol level; a future migration to the
            // extended query protocol (issue #21) will fix this.
            let row_data: Vec<Vec<Option<String>>> = rows
                .iter()
                .map(|r| r.iter().map(|v| Some(v.clone())).collect())
                .collect();

            let rs = RowSet {
                columns,
                rows: row_data,
            };

            let mut out = String::new();
            format_rowset_pset(&mut out, &rs, pset);
            let _ = writer.write_all(out.as_bytes());
        }
    } else {
        // Non-SELECT statement: show rows affected if > 0.
        if rows_affected > 0 {
            if !is_first {
                let _ = writeln!(writer);
            }
            let _ = writeln!(writer, "{rows_affected}");
        }
    }
}

/// Execute a SQL string using `simple_query` and print results.
///
/// Interpolates variables from `settings.vars` before sending to the server,
/// then renders output using `settings.pset`.
///
/// Returns `true` on success, `false` if the query produced a SQL error.
pub async fn execute_query(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Interpolate variables before sending.
    let interpolated = settings.vars.interpolate(sql);
    let sql_to_send = interpolated.as_str();

    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    let success = match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut had_rows = false;
            let mut result_set_index: usize = 0;

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
                        // Flush the current result set, then reset for next
                        // statement in a multi-statement query.
                        if let Some(ref mut w) = settings.output_target {
                            print_result_set_pset(
                                w.as_mut(),
                                &col_names,
                                &rows,
                                had_rows,
                                n,
                                result_set_index == 0,
                                &settings.pset,
                            );
                        } else {
                            print_result_set_pset(
                                &mut io::stdout(),
                                &col_names,
                                &rows,
                                had_rows,
                                n,
                                result_set_index == 0,
                                &settings.pset,
                            );
                        }
                        result_set_index += 1;
                        col_names.clear();
                        rows.clear();
                        had_rows = false;
                    }
                    _ => {}
                }
            }

            // Update transaction state based on what SQL was sent.
            tx.update_from_sql(sql_to_send);

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
        // Timing always goes to stdout regardless of output redirection.
        println!("Time: {:.3} ms", elapsed.as_secs_f64() * 1000.0);
    }

    success
}

// ---------------------------------------------------------------------------
// Non-interactive (piped / -c / -f) execution
// ---------------------------------------------------------------------------

/// Execute a single SQL command string (from `-c`) and exit.
pub async fn exec_command(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    params: &crate::connection::ConnParams,
) -> i32 {
    if sql.trim_start().starts_with('\\') {
        // Backslash meta-command in -c mode: parse and execute.
        let mut parsed = crate::metacmd::parse(sql.trim());
        parsed.echo_hidden = settings.echo_hidden;
        let mut dummy_settings = ReplSettings {
            echo_hidden: settings.echo_hidden,
            ..Default::default()
        };
        let mut tx = TxState::default();
        dispatch_meta(parsed, client, params, &mut dummy_settings, &mut tx).await;
        return 0;
    }
    let mut tx = TxState::default();
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
pub async fn exec_file(client: &Client, path: &str, settings: &mut ReplSettings) -> i32 {
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
pub async fn exec_stdin(client: &Client, settings: &mut ReplSettings) -> i32 {
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
  \q              quit samo
  \timing [on|off]      toggle/set query timing display
  \x [on|off|auto]      toggle/set expanded display
  \conninfo       show connection information
  \?              show this help

Session commands:
  \c [db [user [host [port]]]]  reconnect to database
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
  \deu [pattern]    list user mappings"
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
    // Mirror ECHO_HIDDEN into the settings flag.
    if name == "ECHO_HIDDEN" {
        settings.echo_hidden = value == "on";
    }
}

/// Apply an `\unset` command.
fn apply_unset(settings: &mut ReplSettings, name: &str) {
    if settings.vars.unset(name) {
        // Mirror ECHO_HIDDEN.
        if name == "ECHO_HIDDEN" {
            settings.echo_hidden = false;
        }
    } else {
        eprintln!("\\unset: variable {name} was not set");
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
            settings.pset.tuples_only = bool_value(value, settings.pset.tuples_only);
            let state = if settings.pset.tuples_only {
                "on"
            } else {
                "off"
            };
            println!("Tuples only is {state}.");
        }
        "footer" => {
            settings.pset.footer = bool_value(value, settings.pset.footer);
            let state = if settings.pset.footer { "on" } else { "off" };
            println!("Default footer is {state}.");
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
}

/// Dispatch I/O and utility meta-commands (the `#33` family).
///
/// Returns `Some(MetaResult)` if the command was handled, `None` if the
/// command is not an I/O command (and the caller should continue matching).
async fn dispatch_io(
    parsed: &crate::metacmd::ParsedMeta,
    client: &Client,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> Option<MetaResult> {
    use crate::metacmd::MetaCmd;

    match parsed.cmd {
        MetaCmd::Include => {
            match parsed.pattern.as_deref() {
                Some(path) => {
                    crate::io::include_file(client, path, settings, tx).await;
                }
                None => eprintln!("\\i: file name required"),
            }
            Some(MetaResult::Continue)
        }
        MetaCmd::IncludeRelative => {
            // In the interactive REPL \ir and \i behave identically; the
            // distinction matters only when we already track a "current file"
            // (future work).
            match parsed.pattern.as_deref() {
                Some(path) => {
                    crate::io::include_file(client, path, settings, tx).await;
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
            dispatch_password(parsed.pattern.as_deref());
            Some(MetaResult::Continue)
        }
        _ => None,
    }
}

/// Handle `\password [user]`.
fn dispatch_password(user: Option<&str>) {
    let u = user.unwrap_or("");
    let prompt = if u.is_empty() {
        "Enter new password: ".to_owned()
    } else {
        format!("Enter new password for user \"{u}\": ")
    };
    match rpassword::prompt_password(&prompt) {
        Ok(pw) => {
            let confirm = rpassword::prompt_password("Confirm new password: ").unwrap_or_default();
            if pw == confirm {
                println!("\\password: password change is not yet wired to the server");
            } else {
                eprintln!("\\password: passwords do not match");
            }
        }
        Err(e) => eprintln!("\\password: {e}"),
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
#[allow(clippy::too_many_lines)]
async fn dispatch_meta(
    parsed: crate::metacmd::ParsedMeta,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> MetaResult {
    use crate::metacmd::MetaCmd;

    // Try I/O commands first (they are the most numerous).
    if let Some(result) = dispatch_io(&parsed, client, settings, tx).await {
        return result;
    }

    match parsed.cmd {
        MetaCmd::Quit => return MetaResult::Quit,
        MetaCmd::Help => print_help(),
        MetaCmd::Timing(mode) => apply_timing(settings, mode),
        MetaCmd::Expanded(mode) => apply_expanded(settings, mode),
        MetaCmd::ConnInfo => {
            println!("{}", crate::connection::connection_info(params));
        }
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
            match crate::session::reconnect(parsed.pattern.as_deref(), params).await {
                Ok((new_client, new_params)) => {
                    println!("{}", crate::connection::connection_info(&new_params));
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
                    | MetaCmd::ListPrivileges
                    | MetaCmd::ListConversions
                    | MetaCmd::ListCasts
                    | MetaCmd::ListComments
                    | MetaCmd::ListForeignServers
                    | MetaCmd::ListFdws
                    | MetaCmd::ListForeignTablesViaFdw
                    | MetaCmd::ListUserMappings
            ) =>
        {
            crate::describe::execute(client, &parsed).await;
        }
        ref stub => {
            eprintln!("{}: not yet implemented (see #27)", stub.label());
        }
    }

    MetaResult::Continue
}

/// Run the interactive REPL loop.
///
/// Accepts caller-provided `settings` so that flags set on the command line
/// (e.g. `--timing`, `--expanded`) take effect immediately.
///
/// Returns the exit code (0 = normal exit, non-zero = error).
pub async fn run_repl(
    client: Client,
    params: ConnParams,
    settings: ReplSettings,
    no_readline: bool,
) -> i32 {
    let mut settings = settings;
    let mut tx = TxState::default();
    let mut client = client;
    let mut params = params;

    // Build rustyline editor (skip if --no-readline).
    let use_readline = !no_readline && io::stdin().is_terminal();

    if use_readline {
        run_readline_loop(&mut client, &mut params, &mut settings, &mut tx).await
    } else {
        run_dumb_loop(&mut client, &mut params, &mut settings, &mut tx).await
    }
}

/// Run with rustyline readline support.
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
        let prompt = build_prompt(&params.dbname, *tx, !buf.is_empty());

        match rl.readline(&prompt) {
            Ok(line) => {
                // Ctrl-C on empty line: stay at prompt (readline already
                // handles Ctrl-C during input by returning Interrupted).
                let result =
                    handle_line(&line, &mut buf, &mut stmt_buf, client, params, settings, tx).await;

                // If buf is empty a statement was completed — add the full
                // accumulated statement text to history.
                if buf.is_empty() && !stmt_buf.trim().is_empty() {
                    let _ = rl.add_history_entry(stmt_buf.trim());
                    stmt_buf.clear();
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
                    }
                    HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
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
    client: &mut Client,
    params: &mut ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> i32 {
    let stdin = io::stdin();
    let mut buf = String::new();

    loop {
        // Print prompt to stderr (so it doesn't mix with redirected output).
        let prompt = build_prompt(&params.dbname, *tx, !buf.is_empty());
        eprint!("{prompt}");
        let _ = io::stderr().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF / Ctrl-D
            Ok(_) => {
                let line = line.trim_end_matches(['\r', '\n']).to_owned();
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
                        }
                        HandleLineResult::BufferUpdated | HandleLineResult::Continue => {}
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

// ---------------------------------------------------------------------------
// HandleLineResult — outcome of processing one input line
// ---------------------------------------------------------------------------

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
async fn handle_backslash_dumb(
    input: &str,
    buf: &mut String,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> HandleLineResult {
    let mut parsed = crate::metacmd::parse(input);
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
                        execute_query(client, &trimmed, settings, tx).await;
                    }
                    buf.clear();
                }
                Err(e) => eprintln!("{e}"),
            }
            HandleLineResult::BufferUpdated
        }
        MetaResult::Continue => HandleLineResult::Continue,
    }
}

/// Process one line of input in the readline loop.
///
/// `stmt_buf` accumulates the full multi-line statement for history recording.
///
/// Returns a [`HandleLineResult`] indicating how the loop should proceed.
async fn handle_line(
    line: &str,
    buf: &mut String,
    stmt_buf: &mut String,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> HandleLineResult {
    if line.trim_start().starts_with('\\') {
        // Backslash command — execute immediately, with access to the buffer.
        let mut parsed = crate::metacmd::parse(line.trim());
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
                            execute_query(client, &trimmed, settings, tx).await;
                        }
                        buf.clear();
                        stmt_buf.clear();
                    }
                    Err(e) => eprintln!("{e}"),
                }
                HandleLineResult::BufferUpdated
            }
            MetaResult::Continue => HandleLineResult::Continue,
        };
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

    HandleLineResult::Continue
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
