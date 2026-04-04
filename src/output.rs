//! Output formatting for query results.
//!
//! Produces psql-compatible output:
//! - Aligned table (default)
//! - Expanded (`\x`) output
//! - Unaligned, CSV, JSON, HTML
//! - Error display with position marker
//! - Timing footer (`Time: X.XXX ms`)

use std::fmt::Write as FmtWrite;
use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use crate::query::{ColumnMeta, CommandTag, QueryOutcome, RowSet, StatementResult};

// ---------------------------------------------------------------------------
// ExpandedMode (shared between output, repl, and metacmd)
// ---------------------------------------------------------------------------

/// Expanded display mode (`\x`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExpandedMode {
    /// Always use expanded format.
    On,
    /// Always use normal (table) format.
    #[default]
    Off,
    /// Automatically switch to expanded when table doesn't fit.
    Auto,
    /// Toggle between `On` and `Off`.
    Toggle,
}

// ---------------------------------------------------------------------------
// Output configuration
// ---------------------------------------------------------------------------

/// Controls how query results are rendered.
///
/// Not yet wired to the REPL output path (issue #21); used by the
/// `format_outcome` / `format_aligned` pipeline that is in progress.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
#[allow(dead_code)]
pub struct OutputConfig {
    /// String to display for SQL NULL values (psql default: empty string).
    pub null_string: String,
    /// Whether to show `Time: X.XXX ms` after each result set.
    pub timing: bool,
    /// Whether to use expanded (`\x`) output instead of aligned table.
    pub expanded: bool,
    /// Unaligned output mode (-A).  When `true`, cells are separated by
    /// `field_separator` rather than being padded to column widths.
    /// Used by [`format_outcome`] to dispatch to unaligned rendering.
    pub no_align: bool,
    /// Tuples-only mode (-t).  Suppresses column headers and row-count footer.
    pub tuples_only: bool,
    /// Show verbose error detail including SQLSTATE.
    /// psql does not show SQLSTATE by default; set this for `\set VERBOSITY verbose`.
    pub verbose_errors: bool,
    /// Suppress DETAIL/HINT lines in errors.
    /// Set when `\set VERBOSITY terse` is active.
    pub terse_errors: bool,
    /// Show only the SQLSTATE code as the error message.
    /// Set when `\set VERBOSITY sqlstate` is active.
    pub sqlstate_errors: bool,
}

// ---------------------------------------------------------------------------
// Output format enum
// ---------------------------------------------------------------------------

/// The rendering format for query result sets (mirrors psql `\pset format`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Column-aligned table (psql default).
    #[default]
    Aligned,
    /// Unaligned: fields separated by `field_sep`, no padding.
    Unaligned,
    /// RFC 4180 comma-separated values.
    Csv,
    /// JSON array of objects.
    Json,
    /// HTML `<table>` element.
    Html,
    /// Like aligned but wraps long values (same as aligned for now).
    Wrapped,
    /// GitHub-flavored Markdown table.
    Markdown,
}

// ---------------------------------------------------------------------------
// PsetConfig — \pset and CLI-driven print configuration
// ---------------------------------------------------------------------------

/// Print settings controlled by `\pset`, `\a`, `\t`, `\f`, `\H`, `\C`, etc.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct PsetConfig {
    /// Output format.
    pub format: OutputFormat,
    /// Border style: 0 = no border, 1 = inner borders, 2 = full box.
    pub border: u8,
    /// String shown for NULL values (default: `""`).
    pub null_display: String,
    /// Field separator for unaligned output (default `|`).
    pub field_sep: String,
    /// Record separator for unaligned output (default `\n`).
    pub record_sep: String,
    /// Suppress headers and footers.
    pub tuples_only: bool,
    /// Show row-count footer (default `true`).
    pub footer: bool,
    /// Optional table title (printed above the table).
    pub title: Option<String>,
    /// Expanded display mode.
    pub expanded: ExpandedMode,
    /// When `true`, suppress ANSI colour codes in output (mirrors `\set HIGHLIGHT off`).
    pub no_highlight: bool,
}

impl Default for PsetConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Aligned,
            border: 1,
            null_display: String::new(),
            field_sep: "|".to_owned(),
            record_sep: "\n".to_owned(),
            tuples_only: false,
            footer: true,
            title: None,
            expanded: ExpandedMode::Off,
            no_highlight: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level pset-aware formatter
// ---------------------------------------------------------------------------

/// Format a single [`RowSet`] using the active [`PsetConfig`].
pub fn format_rowset_pset(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    // Title line: printed as plain text for non-HTML formats.
    // HTML format emits the title itself as <caption> inside the table element.
    if cfg.format != OutputFormat::Html {
        if let Some(ref title) = cfg.title {
            let _ = writeln!(out, "{title}");
        }
    }

    match &cfg.format {
        OutputFormat::Aligned | OutputFormat::Wrapped => {
            if cfg.expanded == ExpandedMode::On {
                let ocfg = OutputConfig {
                    null_string: cfg.null_display.clone(),
                    expanded: true,
                    tuples_only: cfg.tuples_only,
                    ..Default::default()
                };
                format_expanded(out, rs, &ocfg);
            } else {
                let ocfg = OutputConfig {
                    null_string: cfg.null_display.clone(),
                    tuples_only: cfg.tuples_only,
                    ..Default::default()
                };
                format_aligned_pset(out, rs, &ocfg, cfg);
            }
        }
        OutputFormat::Unaligned => format_unaligned(out, rs, cfg),
        OutputFormat::Csv => format_csv(out, rs, cfg),
        OutputFormat::Json => format_json(out, rs, cfg),
        OutputFormat::Html => format_html(out, rs, cfg),
        OutputFormat::Markdown => format_markdown(out, rs, cfg),
    }

    // psql prints a blank line after each result set (the trailing newline
    // after `(N rows)` plus one more).  In tuples-only mode there is no row
    // count footer, and psql omits the trailing blank line entirely.
    if !cfg.tuples_only {
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Top-level formatter
// ---------------------------------------------------------------------------

/// Format all results from a [`QueryOutcome`] into a single `String`.
///
/// Each statement result is separated by a blank line (matching psql).
/// Not yet called from the REPL dispatch path (issue #21).
#[allow(dead_code)]
pub fn format_outcome(outcome: &QueryOutcome, cfg: &OutputConfig) -> String {
    let mut out = String::new();
    let n = outcome.results.len();

    for (idx, result) in outcome.results.iter().enumerate() {
        match result {
            StatementResult::Rows(rs) => {
                if cfg.no_align {
                    // Unaligned mode: build a minimal PsetConfig and delegate.
                    let pcfg = PsetConfig {
                        format: OutputFormat::Unaligned,
                        tuples_only: cfg.tuples_only,
                        ..PsetConfig::default()
                    };
                    format_unaligned(&mut out, rs, &pcfg);
                } else if cfg.expanded {
                    format_expanded(&mut out, rs, cfg);
                } else {
                    format_aligned(&mut out, rs, cfg);
                }
            }
            StatementResult::CommandTag(ct) => {
                format_command_tag(&mut out, ct);
            }
            StatementResult::Empty => {
                // Nothing to print for DDL/SET/etc.
            }
        }

        // Print timing after each statement.
        if cfg.timing {
            let _ = writeln!(out, "Time: {}", format_duration(outcome.duration));
        }

        // Blank line between multiple results (skip after the last one).
        if idx + 1 < n {
            out.push('\n');
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Aligned (default) table formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] as a psql-style aligned table.
///
/// ```text
///  id | name  | email
/// ----+-------+------------------
///   1 | Alice | alice@example.com
/// (1 row)
/// ```
#[allow(dead_code)]
pub fn format_aligned(out: &mut String, rs: &RowSet, cfg: &OutputConfig) -> usize {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if cols.is_empty() {
        // Zero-column SELECT (e.g. `SELECT FROM t`): psql renders a bare
        // `--` separator line in the header position followed by the row-count
        // footer.  Tuples-only mode suppresses both.
        if !cfg.tuples_only {
            out.push_str("--\n");
            write_row_count(out, rows.len());
        }
        return rows.len();
    }

    // Calculate column widths: max(header width, max data width).
    let widths = column_widths(cols, rows, cfg);

    // Header row — suppressed in tuples-only mode.
    if !cfg.tuples_only {
        // psql center-aligns text headers and right-aligns numeric ones.
        write_aligned_row(out, cols, &widths, |col, _| col.name.clone(), true);
        // Separator.
        write_separator(out, &widths);
    }

    // Data rows.
    for row in rows {
        write_aligned_row(
            out,
            cols,
            &widths,
            |_col, cell_idx| {
                row.get(cell_idx)
                    .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                    .unwrap_or_else(|| cfg.null_string.clone())
            },
            false,
        );
    }

    // Footer — suppressed in tuples-only mode.
    if !cfg.tuples_only {
        write_row_count(out, rows.len());
    }

    rows.len()
}

/// Calculate per-column display widths (in terminal columns, accounting for
/// Unicode multi-byte / wide characters).
///
/// `null_str` is the display string for NULL values (used to compute widths).
fn column_widths_with_null(
    cols: &[ColumnMeta],
    rows: &[Vec<Option<String>>],
    null_str: &str,
) -> Vec<usize> {
    let mut widths: Vec<usize> = cols.iter().map(|c| display_width(&c.name)).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let raw = cell.as_deref().unwrap_or(null_str);
            let escaped = psql_escape_cell(raw);
            let w = cell_display_width(&escaped);
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    widths
}

/// Calculate per-column display widths (in terminal columns, accounting for
/// Unicode multi-byte / wide characters).
fn column_widths(
    cols: &[ColumnMeta],
    rows: &[Vec<Option<String>>],
    cfg: &OutputConfig,
) -> Vec<usize> {
    column_widths_with_null(cols, rows, &cfg.null_string)
}

/// Write one row of the aligned table (header or data) with a given border
/// style.
///
/// - `border 0`: columns separated by two spaces, no leading/trailing margin.
/// - `border 1` (default): ` col1 | col2 ` — leading space, ` | ` between
///   columns, trailing space.
/// - `border 2`: `| col1 | col2 |` — `| ` prefix, ` | ` between columns,
///   ` |` suffix.
///
/// `value_fn` maps `(column_meta, column_index) → String`.
/// `is_header` – when true, all headers are center-aligned (psql centers
/// numeric headers too; only data rows are right-aligned for numeric columns).
fn write_aligned_row_border<F>(
    out: &mut String,
    cols: &[ColumnMeta],
    widths: &[usize],
    value_fn: F,
    is_header: bool,
    border: u8,
) where
    F: Fn(&ColumnMeta, usize) -> String,
{
    if is_header {
        // Headers are always single-line: center-align, no escaping needed.
        for (i, col) in cols.iter().enumerate() {
            let val = value_fn(col, i);
            let w = widths[i];
            let val_width = display_width(&val);
            let padding = w.saturating_sub(val_width);

            match border {
                0 => {
                    if i > 0 {
                        out.push_str("  ");
                    }
                }
                2 => {
                    if i == 0 {
                        out.push_str("| ");
                    } else {
                        out.push_str(" | ");
                    }
                }
                _ => {
                    if i == 0 {
                        out.push(' ');
                    } else {
                        out.push_str(" | ");
                    }
                }
            }

            // Center-align all column headers (psql behaviour).
            let left_pad = padding / 2;
            let right_pad = padding - left_pad;
            for _ in 0..left_pad {
                out.push(' ');
            }
            out.push_str(&val);
            for _ in 0..right_pad {
                out.push(' ');
            }
        }
        match border {
            0 => {}
            2 => out.push_str(" |"),
            _ => out.push(' '),
        }
        out.push('\n');
        return;
    }

    // Data rows: escape control characters and handle multi-line cells.
    //
    // psql converts non-printable control characters (0x01–0x1F except LF/TAB,
    // plus 0x7F) to visible escape sequences like `\x01`.  It also renders
    // cells that contain embedded LF characters as multiple physical lines,
    // appending a `+` continuation marker after each non-final physical line.
    let escaped: Vec<String> = (0..cols.len())
        .map(|i| psql_escape_cell(&value_fn(&cols[i], i)))
        .collect();

    // Split each cell into physical lines and determine how many physical rows
    // this logical row requires.
    let split_lines: Vec<Vec<&str>> = escaped.iter().map(|v| v.split('\n').collect()).collect();
    let max_physical_lines = split_lines.iter().map(|v| v.len()).max().unwrap_or(1);

    for phys_row in 0..max_physical_lines {
        let is_last_phys = phys_row == max_physical_lines - 1;

        // Per-column continuation flag: true when this column has more content
        // on the next physical row.  Used to place the `+` marker correctly.
        let col_continues: Vec<bool> = (0..cols.len())
            .map(|i| phys_row < split_lines[i].len().saturating_sub(1))
            .collect();

        for (i, col) in cols.iter().enumerate() {
            let line = split_lines[i].get(phys_row).copied().unwrap_or("");
            let w = widths[i];

            // Column prefix separator.  In border-1, if the PREVIOUS column
            // has continuation on this physical row, its trailing-space slot
            // becomes `+` — so use `+| ` instead of the normal ` | `.
            match border {
                0 => {
                    if i > 0 {
                        out.push_str("  ");
                    }
                }
                2 => {
                    if i == 0 {
                        out.push_str("| ");
                    } else {
                        out.push_str(" | ");
                    }
                }
                _ => {
                    if i == 0 {
                        out.push(' ');
                    } else if !is_last_phys && col_continues[i - 1] {
                        // Previous column has continuation: `+| ` instead of ` | `.
                        out.push_str("+| ");
                    } else {
                        out.push_str(" | ");
                    }
                }
            }

            // Expand tabs in cell content before rendering (psql expands tabs
            // from position 0 of each cell line, independent of the cell's
            // column position in the output line).
            let expanded_line = expand_cell_tabs(line);
            let line_width = display_width(&expanded_line);
            let padding = w.saturating_sub(line_width);

            if col.is_numeric {
                // Right-align numeric data.
                for _ in 0..padding {
                    out.push(' ');
                }
                out.push_str(&expanded_line);
            } else {
                // Left-align text data.
                out.push_str(&expanded_line);
                for _ in 0..padding {
                    out.push(' ');
                }
            }
        }

        // Row suffix.  For border-1, if the LAST column has continuation on
        // this physical row the trailing space becomes `+`.
        match border {
            0 => {}
            2 => out.push_str(" |"),
            _ => {
                let last_continues = col_continues.last().copied().unwrap_or(false);
                if !is_last_phys && last_continues {
                    out.push('+');
                } else {
                    out.push(' ');
                }
            }
        }
        out.push('\n');
    }
}

/// Write one row of the aligned table (header or data).
///
/// `value_fn` maps `(column_meta, column_index) → String`.
/// `is_header` – when true, all column headers are center-aligned (matching
/// psql: numeric headers are centered, not right-aligned; only data rows are
/// right-aligned).
fn write_aligned_row<F>(
    out: &mut String,
    cols: &[ColumnMeta],
    widths: &[usize],
    value_fn: F,
    is_header: bool,
) where
    F: Fn(&ColumnMeta, usize) -> String,
{
    write_aligned_row_border(out, cols, widths, value_fn, is_header, 1);
}

/// Write the separator line between the header and data rows.
///
/// - `border 0`: `-- ------` (dashes per column, two spaces between).
/// - `border 1` (default): `----+-------` (dashes, `-+-` between columns,
///   leading/trailing dash for margin).
/// - `border 2`: `+----+-------+` (full box, `+` at both ends and between
///   columns).
fn write_separator_border(out: &mut String, widths: &[usize], border: u8) {
    match border {
        0 => {
            // border 0: each column is `w` dashes, separated by two spaces.
            for (i, &w) in widths.iter().enumerate() {
                if i > 0 {
                    out.push_str("  ");
                }
                for _ in 0..w {
                    out.push('-');
                }
            }
            out.push('\n');
        }
        2 => {
            // border 2: `+---+------+` full box.
            for &w in widths {
                out.push('+');
                // One dash of padding on each side plus `w` dashes for content.
                for _ in 0..w + 2 {
                    out.push('-');
                }
            }
            out.push_str("+\n");
        }
        _ => {
            // border 1: `----+-------`
            for (i, &w) in widths.iter().enumerate() {
                if i == 0 {
                    for _ in 0..=w {
                        out.push('-');
                    }
                } else {
                    out.push_str("-+-");
                    for _ in 0..w {
                        out.push('-');
                    }
                }
            }
            // Trailing dash to close the last column.
            if !widths.is_empty() {
                out.push('-');
            }
            out.push('\n');
        }
    }
}

/// Write the `----+--------` separator line (border 1).
fn write_separator(out: &mut String, widths: &[usize]) {
    write_separator_border(out, widths, 1);
}

/// Write `(N rows)` / `(1 row)` / `(0 rows)`.
fn write_row_count(out: &mut String, n: usize) {
    if n == 1 {
        out.push_str("(1 row)\n");
    } else {
        let _ = writeln!(out, "({n} rows)");
    }
}

// ---------------------------------------------------------------------------
// Expanded output formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] in psql `\x` expanded format.
///
/// ```text
/// -[ RECORD 1 ]------
/// id               | 1
/// name             | Alice
/// email            | alice@example.com
/// ```
pub fn format_expanded(out: &mut String, rs: &RowSet, cfg: &OutputConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if rows.is_empty() {
        // In tuples-only mode psql omits the "(0 rows)" footer.
        if !cfg.tuples_only {
            out.push_str("(0 rows)\n");
        }
        return;
    }

    // Widest column name (for alignment of the `| value` part).
    let max_name_width = cols
        .iter()
        .map(|c| display_width(&c.name))
        .max()
        .unwrap_or(0);

    // Widest data row: `key_padded + " | " + value` = max_name_width + 3 + value_width.
    // The expanded header must be padded to this width to match psql behaviour.
    let max_data_width = rows
        .iter()
        .flat_map(|row| {
            cols.iter().enumerate().map(move |(i, _col)| {
                let val_len = row
                    .get(i)
                    .and_then(|v| v.as_deref())
                    .map_or(0, display_width);
                max_name_width + 3 + val_len
            })
        })
        .max()
        .unwrap_or(max_name_width + 3);

    for (rec_idx, row) in rows.iter().enumerate() {
        // Record header: `-[ RECORD N ]---` — suppressed in tuples-only mode.
        if !cfg.tuples_only {
            write_expanded_header(out, rec_idx + 1, max_data_width);
        }

        for (i, col) in cols.iter().enumerate() {
            let val = row
                .get(i)
                .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                .unwrap_or_else(|| cfg.null_string.clone());

            let name_width = display_width(&col.name);
            let padding = max_name_width.saturating_sub(name_width);
            let _ = write!(out, "{}", col.name);
            for _ in 0..padding {
                out.push(' ');
            }
            let _ = writeln!(out, " | {val}");
        }
    }
}

/// Write the `-[ RECORD N ]---` header line for expanded output.
///
/// `max_data_width` is the width of the widest data row
/// (`key_padded + " | " + value`). The header is padded with `-` to match
/// that width, replicating psql behaviour.
fn write_expanded_header(out: &mut String, record_num: usize, max_data_width: usize) {
    let prefix = format!("-[ RECORD {record_num} ]");
    let dashes_needed = max_data_width.saturating_sub(prefix.len());
    let _ = write!(out, "{prefix}");
    for _ in 0..dashes_needed {
        out.push('-');
    }
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Command tag formatter
// ---------------------------------------------------------------------------

/// Render the result of a non-SELECT statement.
///
/// For DML commands the format is the raw command tag from Postgres:
/// ```text
/// INSERT 0 3
/// UPDATE 2
/// DELETE 1
/// ```
#[allow(dead_code)]
pub fn format_command_tag(out: &mut String, ct: &CommandTag) {
    let _ = writeln!(out, "{}", ct.tag);
    // `ct.rows_affected` is available for callers that need the numeric count
    // (e.g., the REPL in issue #20). We touch it here to confirm it is correct.
    let _ = ct.rows_affected;
}

// ---------------------------------------------------------------------------
// Error formatter
// ---------------------------------------------------------------------------

// ANSI escape constants used for severity prefix coloring.
const ANSI_RESET: &str = "\x1b[0m";
/// Bold red — ERROR, FATAL, PANIC
const ANSI_BOLD_RED: &str = "\x1b[1;31m";
/// Yellow — WARNING
const ANSI_YELLOW: &str = "\x1b[33m";
/// Cyan — NOTICE
const ANSI_CYAN: &str = "\x1b[36m";
/// Dim/gray — INFO, DEBUG, LOG
const ANSI_DIM: &str = "\x1b[2m";

/// Return the colored form of a `PostgreSQL` severity prefix, e.g. `"ERROR"`.
///
/// The returned string has the ANSI color applied and ends with the reset code
/// so that only the keyword itself is colored, not the message that follows.
/// Stdout/stderr coloring is unconditional here; callers that write to a file
/// or non-TTY should strip colors before writing (future work).
fn color_severity(severity: &str) -> String {
    let color = match severity {
        "ERROR" | "FATAL" | "PANIC" => ANSI_BOLD_RED,
        "WARNING" => ANSI_YELLOW,
        "NOTICE" => ANSI_CYAN,
        "INFO" | "DEBUG" | "LOG" => ANSI_DIM,
        _ => "",
    };
    if color.is_empty() {
        severity.to_owned()
    } else {
        format!("{color}{severity}{ANSI_RESET}")
    }
}

/// Format a `tokio_postgres::Error` in psql style.
///
/// ```text
/// ERROR:  column "foo" does not exist
/// LINE 1: select foo from bar;
///                ^
/// HINT:  Perhaps you meant ...
/// ```
///
/// SQLSTATE is omitted unless `cfg.verbose_errors` is `true` (matching psql's
/// default behaviour; psql only shows SQLSTATE with `\set VERBOSITY verbose`).
pub fn format_pg_error(
    err: &tokio_postgres::Error,
    original_sql: Option<&str>,
    cfg: &OutputConfig,
) -> String {
    let mut out = String::new();

    if let Some(db_err) = err.as_db_error() {
        let colored = color_severity(db_err.severity());

        if cfg.sqlstate_errors {
            // sqlstate mode: show only the SQLSTATE code as the message.
            let _ = writeln!(out, "{}:  {}", colored, db_err.code().code());
        } else {
            // Severity line — color the severity keyword.
            let _ = writeln!(out, "{}:  {}", colored, db_err.message());

            // Original position marker (shown right after severity in psql).
            if let Some(pos) = db_err.position() {
                if let tokio_postgres::error::ErrorPosition::Original(_) = pos {
                    if let Some(sql) = original_sql {
                        write_error_position(&mut out, sql, pos);
                    }
                }
            }

            // DETAIL and HINT are suppressed in terse mode.
            if !cfg.terse_errors {
                if let Some(detail) = db_err.detail() {
                    let _ = writeln!(out, "DETAIL:  {detail}");
                }
                if let Some(hint) = db_err.hint() {
                    let _ = writeln!(out, "HINT:  {hint}");
                }
            }

            // CONTEXT line (e.g. PL/pgSQL call stack).
            if let Some(ctx) = db_err.where_() {
                let _ = writeln!(out, "CONTEXT:  {ctx}");
            }

            // Internal query + position (shown after CONTEXT in psql).
            if let Some(pos) = db_err.position() {
                if let tokio_postgres::error::ErrorPosition::Internal { query, .. } = pos {
                    let _ = writeln!(out, "QUERY:  {query}");
                    write_error_position(&mut out, query, pos);
                }
            }

            // SQLSTATE: only shown in verbose mode (psql default: hidden).
            if cfg.verbose_errors {
                let _ = writeln!(out, "SQLSTATE:  {}", db_err.code().code());
            }
        }
    } else {
        // Non-server error (I/O, protocol, …).
        let colored = color_severity("ERROR");
        let _ = writeln!(out, "{colored}:  {err}");
    }

    out
}

/// Print a `tokio_postgres::Error` to stderr in psql style.
///
/// Convenience wrapper around [`format_pg_error`] for call sites that do
/// not need the string representation.  `sql` is the original query text
/// (used to render the position marker); pass `None` when unavailable.
/// `verbose` enables SQLSTATE output (mirrors `\set VERBOSITY verbose`).
pub fn eprint_db_error(err: &tokio_postgres::Error, sql: Option<&str>, verbose: bool, terse: bool, sqlstate: bool) {
    let cfg = OutputConfig {
        verbose_errors: verbose,
        terse_errors: terse,
        sqlstate_errors: sqlstate,
        ..OutputConfig::default()
    };
    let msg = format_pg_error(err, sql, &cfg);
    // format_pg_error always ends with a newline; use eprint! to avoid double.
    eprint!("{msg}");
}

/// Format a `PostgreSQL` notice (from `tokio_postgres::error::DbError`) in psql
/// style, with a colored severity prefix.
///
/// Used to display `NOTICE`, `WARNING`, `INFO`, etc. messages that `PostgreSQL`
/// sends during query execution (delivered as `AsyncMessage::Notice`).
pub fn format_pg_notice(notice: &tokio_postgres::error::DbError) -> String {
    let colored = color_severity(notice.severity());
    let mut out = format!("{colored}:  {}\n", notice.message());
    if let Some(detail) = notice.detail() {
        let _ = writeln!(out, "DETAIL:  {detail}");
    }
    if let Some(hint) = notice.hint() {
        let _ = writeln!(out, "HINT:  {hint}");
    }
    out
}

/// Print a `PostgreSQL` notice to stderr with a colored severity prefix.
///
/// Convenience wrapper around [`format_pg_notice`].
pub fn eprint_pg_notice(notice: &tokio_postgres::error::DbError) {
    eprint!("{}", format_pg_notice(notice));
}

/// Write the `LINE N: …` context and the `^` position marker.
fn write_error_position(out: &mut String, sql: &str, pos: &tokio_postgres::error::ErrorPosition) {
    // Postgres reports `position` as a 1-based byte offset into the query.
    let byte_offset = match pos {
        tokio_postgres::error::ErrorPosition::Original(n) => (*n as usize).saturating_sub(1),
        tokio_postgres::error::ErrorPosition::Internal { position, .. } => {
            (*position as usize).saturating_sub(1)
        }
    };

    // Find which line the offset falls on and the column within that line.
    let before = sql.get(..byte_offset.min(sql.len())).unwrap_or(sql);
    let line_num = before.chars().filter(|&c| c == '\n').count() + 1;

    let line_start = before.rfind('\n').map_or(0, |p| p + 1);
    let col_offset = before.len() - line_start;

    // The line text (stop at the next newline).
    let line_text = sql[line_start..].lines().next().unwrap_or("");

    let _ = writeln!(out, "LINE {line_num}: {line_text}");
    // Caret: `LINE N: ` prefix is 8 + digits in line_num.
    let prefix_len = "LINE : ".len() + line_num.to_string().len() + col_offset;
    for _ in 0..prefix_len {
        out.push(' ');
    }
    out.push_str("^\n");
}

// ---------------------------------------------------------------------------
// Timing helper
// ---------------------------------------------------------------------------

/// Format a [`Duration`] as `X.XXX ms`.
#[allow(dead_code)]
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    format!("{ms:.3} ms")
}

// ---------------------------------------------------------------------------
// Unicode-aware display width
// ---------------------------------------------------------------------------

/// Escape non-printable characters in a cell value the same way psql does.
///
/// psql converts non-printable ASCII control characters to visible escape
/// sequences when rendering table cells:
/// - LF (0x0A): kept as `\n` (creates a multi-line cell)
/// - TAB (0x09): kept as `\t` (expand converts to spaces)
/// - CR (0x0D): converted to literal `\r`
/// - DEL (0x7F) and other control chars (0x01–0x08, 0x0B–0x0C, 0x0E–0x1F):
///   converted to `\xNN` (two uppercase hex digits)
///
/// Printable ASCII, non-ASCII Unicode, and NULL-replacement strings pass
/// through unchanged.
fn psql_escape_cell(s: &str) -> String {
    // Fast path: if no control chars are present return the string as-is.
    // ESC (0x1B) is excluded here; it is handled specially below (ANSI codes
    // must pass through intact).
    let needs_escape = s.chars().any(|c| {
        matches!(c, '\x01'..='\x08' | '\x0b'..='\x0c' | '\x0e'..='\x1a' | '\x1c'..='\x1f' | '\x0d' | '\x7f')
    });
    if !needs_escape {
        return s.to_owned();
    }

    let mut out = String::with_capacity(s.len() + 16);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // ANSI CSI escape sequence: ESC [ ... final-byte — pass through
            // intact so that null-highlighting codes are not corrupted.
            '\x1b' if chars.peek() == Some(&'[') => {
                out.push('\x1b');
                out.push('[');
                chars.next(); // consume '['
                for inner in chars.by_ref() {
                    out.push(inner);
                    if ('\x40'..='\x7e').contains(&inner) {
                        break; // CSI final byte consumed
                    }
                }
            }
            '\n' | '\t' => out.push(c),
            '\r' => out.push_str("\\r"),
            '\x7f' => out.push_str("\\x7F"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\x{:02X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Returns the max per-line display width of a (possibly multi-line)
/// already-escaped cell string.
///
/// For cells that span multiple physical lines (containing embedded LF), the
/// column width is the widest individual line — matching psql which renders
/// each line separately and appends a `+` continuation marker.
fn cell_display_width(escaped: &str) -> usize {
    escaped
        .split('\n')
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0)
}

/// Expand tab characters in a cell content line to spaces, using 8-space
/// tab stops measured from the start of the cell content (column 0).
///
/// This matches psql's behaviour: tabs in cell values are expanded before
/// adding the leading space, so the expansion is independent of the cell's
/// position in the output line.
fn expand_cell_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut col: usize = 0;
    for ch in s.chars() {
        if ch == '\t' {
            let next_stop = (col / 8 + 1) * 8;
            for _ in col..next_stop {
                out.push(' ');
            }
            col = next_stop;
        } else {
            out.push(ch);
            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    out
}

/// Like `display_width`, but treats the string as starting at `start_col`
/// for the purpose of tab-stop expansion.  Returns the number of display
/// columns consumed by the string (not counting `start_col` itself).
fn display_width_at_col(s: &str, start_col: usize) -> usize {
    let mut col = start_col;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' if chars.peek() == Some(&'[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            '\t' => {
                col = (col / 8 + 1) * 8;
            }
            c => {
                col += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
    }
    col - start_col
}

/// Returns the terminal display width of a string, handling multi-byte and
/// double-width Unicode characters (CJK, emoji, …).
///
/// Tab characters (`\t`) are expanded to the next 8-space tab stop, matching
/// psql's column-width calculation behaviour.
pub fn display_width(s: &str) -> usize {
    // Walk through characters, skipping ANSI CSI sequences and expanding tabs.
    let mut width: usize = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' if chars.peek() == Some(&'[') => {
                chars.next(); // consume '['
                // Consume until the CSI final byte (0x40–0x7E).
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            '\t' => {
                // Advance to the next 8-space tab stop.
                width = (width / 8 + 1) * 8;
            }
            c => {
                width += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            }
        }
    }
    width
}

// ---------------------------------------------------------------------------
// Aligned table with PsetConfig (handles tuples_only + footer)
// ---------------------------------------------------------------------------

/// Aligned table formatter that honours `PsetConfig` for border style,
/// tuples-only mode, footer suppression, and null display string.
fn format_aligned_pset(out: &mut String, rs: &RowSet, _ocfg: &OutputConfig, pcfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let border = pcfg.border;
    let null_str = &pcfg.null_display;

    if cols.is_empty() {
        // Zero-column SELECT (e.g. `SELECT FROM t`): psql renders a bare
        // `--` separator line in the header position followed by the row-count
        // footer.  Tuples-only mode suppresses both header and footer.
        if !pcfg.tuples_only {
            out.push_str("--\n");
            if pcfg.footer {
                write_row_count(out, rows.len());
            }
        }
        return;
    }

    let widths = column_widths_with_null(cols, rows, null_str);

    // border 2: top border line `+----+------+` before the header.
    if border == 2 && !pcfg.tuples_only {
        write_separator_border(out, &widths, border);
    }

    // Header (suppressed in tuples-only mode).
    // psql center-aligns text headers and right-aligns numeric ones.
    if !pcfg.tuples_only {
        write_aligned_row_border(out, cols, &widths, |col, _| col.name.clone(), true, border);
        write_separator_border(out, &widths, border);
    }

    // Data rows.
    // When highlighting is on and null_display is non-empty, render NULL cells
    // with ANSI dim so they are visually distinct from empty-string cells.
    let null_rendered = if !pcfg.no_highlight && !null_str.is_empty() {
        format!("\x1b[2m{null_str}\x1b[0m")
    } else {
        null_str.to_owned()
    };
    for row in rows {
        let null = null_rendered.clone();
        write_aligned_row_border(
            out,
            cols,
            &widths,
            |_col, cell_idx| {
                row.get(cell_idx)
                    .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                    .unwrap_or_else(|| null.clone())
            },
            false,
            border,
        );
    }

    // border 2: bottom border line after the last data row.
    if border == 2 {
        write_separator_border(out, &widths, border);
    }

    // Footer.
    if !pcfg.tuples_only && pcfg.footer {
        write_row_count(out, rows.len());
    }
}

// ---------------------------------------------------------------------------
// Unaligned formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] in unaligned mode: fields separated by `cfg.field_sep`.
///
/// The output matches psql `-A`: header line (unless tuples-only), then one
/// data row per line with `field_sep` between fields.
pub fn format_unaligned(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if !cfg.tuples_only {
        // Header.
        let header: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
        out.push_str(&header.join(&cfg.field_sep));
        out.push_str(&cfg.record_sep);
    }

    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str(&cfg.record_sep);
        }
        let cells: Vec<String> = row
            .iter()
            .map(|v| v.as_deref().unwrap_or(&cfg.null_display).to_owned())
            .collect();
        out.push_str(&cells.join(&cfg.field_sep));
    }
    if !rows.is_empty() {
        out.push('\n');
    }

    if !cfg.tuples_only && cfg.footer {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "({n} {word})");
    }
}

// ---------------------------------------------------------------------------
// CSV formatter  (RFC 4180)
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] as RFC 4180 CSV.
///
/// Fields that contain a comma, double-quote, or newline are wrapped in
/// double-quotes with any embedded double-quotes doubled.
/// Header row is always emitted (psql behaviour with `\pset format csv`).
pub fn format_csv(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if !cfg.tuples_only {
        let header: Vec<String> = cols.iter().map(|c| csv_field(&c.name)).collect();
        out.push_str(&header.join(","));
        out.push('\n');
    }

    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|v| csv_field(v.as_deref().unwrap_or(&cfg.null_display)))
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
}

/// RFC 4180: wrap in double-quotes if the value contains `,`, `"`, `\n`, or `\r`.
fn csv_field(val: &str) -> String {
    if val.contains(',') || val.contains('"') || val.contains('\n') || val.contains('\r') {
        let escaped = val.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        val.to_owned()
    }
}

// ---------------------------------------------------------------------------
// JSON formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] as a JSON array of objects.
///
/// Each row becomes `{"col1": "val1", "col2": "val2"}`.
/// NULL values are rendered as JSON `null`.
/// String values are JSON-escaped.
///
/// `tuples_only` is intentionally ignored: JSON output always includes column
/// keys because removing them would produce invalid/ambiguous data (an array of
/// bare values with no key context).  This matches psql behaviour.
pub fn format_json(out: &mut String, rs: &RowSet, _cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    out.push('[');

    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            out.push(',');
        }
        out.push('{');
        for (col_idx, col) in cols.iter().enumerate() {
            if col_idx > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&json_escape(&col.name));
            out.push_str("\":");
            match row.get(col_idx).and_then(|v| v.as_deref()) {
                Some(val) => {
                    out.push('"');
                    out.push_str(&json_escape(val));
                    out.push('"');
                }
                None => {
                    // NULL → JSON null (ignore cfg.null_display for JSON).
                    out.push_str("null");
                }
            }
        }
        out.push('}');
    }

    out.push(']');
    out.push('\n');
}

/// JSON-escape a string: escape `"`, `\`, and control characters.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTML formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] as an HTML `<table>` element.
///
/// Produces a minimal but valid table: `<thead>` with `<th>` cells and
/// `<tbody>` with `<td>` cells.  Values are HTML-escaped.
pub fn format_html(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if let Some(ref title) = cfg.title {
        let _ = writeln!(out, "<caption>{}</caption>", html_escape(title));
    }

    out.push_str("<table>\n");

    if !cfg.tuples_only {
        out.push_str("<thead><tr>");
        for col in cols {
            out.push_str("<th>");
            out.push_str(&html_escape(&col.name));
            out.push_str("</th>");
        }
        out.push_str("</tr></thead>\n");
    }

    out.push_str("<tbody>\n");
    for row in rows {
        out.push_str("<tr>");
        for (col_idx, _col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(&cfg.null_display);
            out.push_str("<td>");
            out.push_str(&html_escape(val));
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n");
    out.push_str("</table>\n");
}

/// HTML-escape: replace `<`, `>`, `&`, `"`, `'` with entities.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Markdown formatter
// ---------------------------------------------------------------------------

/// Render a [`RowSet`] as a GitHub-flavored Markdown table.
///
/// ```text
/// | id | name       | plan    |
/// |----|------------|---------|
/// | 1  | Sam Martin | starter |
/// ```
///
/// Column widths are padded to the maximum content width per column.
/// NULL values use the configured null display string.
/// Footer `(N rows)` is printed after the table when not in tuples-only mode.
pub fn format_markdown(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    if cols.is_empty() {
        if !cfg.tuples_only && cfg.footer {
            write_row_count(out, rows.len());
        }
        return;
    }

    // Compute per-column widths: max(header width, max data cell width).
    let widths = column_widths_with_null(cols, rows, null_str);

    if !cfg.tuples_only {
        // Header row.
        out.push('|');
        for (i, col) in cols.iter().enumerate() {
            let w = widths[i];
            let val_w = display_width(&col.name);
            let padding = w.saturating_sub(val_w);
            out.push(' ');
            out.push_str(&col.name);
            for _ in 0..padding {
                out.push(' ');
            }
            out.push_str(" |");
        }
        out.push('\n');

        // Separator row: `|----|------------|`
        out.push('|');
        for &w in &widths {
            // Each cell: `-` repeated for width + 2 spaces of padding.
            for _ in 0..w + 2 {
                out.push('-');
            }
            out.push('|');
        }
        out.push('\n');
    }

    // Data rows.
    for row in rows {
        out.push('|');
        for (i, _col) in cols.iter().enumerate() {
            let val = row
                .get(i)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let w = widths[i];
            let val_w = display_width(val);
            let padding = w.saturating_sub(val_w);
            out.push(' ');
            out.push_str(val);
            for _ in 0..padding {
                out.push(' ');
            }
            out.push_str(" |");
        }
        out.push('\n');
    }

    // Footer: `(N rows)` — outside the table, on its own line.
    if !cfg.tuples_only && cfg.footer {
        write_row_count(out, rows.len());
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{ColumnMeta, RowSet};

    fn mk_col(name: &str, numeric: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            is_numeric: numeric,
        }
    }

    fn mk_row(vals: &[Option<&str>]) -> Vec<Option<String>> {
        vals.iter().map(|v| v.map(ToOwned::to_owned)).collect()
    }

    // -----------------------------------------------------------------------
    // display_width
    // -----------------------------------------------------------------------

    #[test]
    fn test_display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn test_display_width_empty() {
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn test_display_width_cjk() {
        // CJK characters are double-width.
        assert_eq!(display_width("中文"), 4);
    }

    #[test]
    fn test_display_width_mixed() {
        // ASCII (1) + CJK (2) + ASCII (3) = 6
        assert_eq!(display_width("a中bc"), 5);
    }

    #[test]
    fn test_display_width_ansi_stripped() {
        // ANSI dim codes must not inflate the measured width.
        assert_eq!(display_width("\x1b[2mNULL\x1b[0m"), 4);
        assert_eq!(display_width("\x1b[33mhello\x1b[39m"), 5);
        assert_eq!(display_width("\x1b[2m\x1b[0m"), 0);
    }

    // -----------------------------------------------------------------------
    // format_duration
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(Duration::ZERO), "0.000 ms");
    }

    #[test]
    fn test_format_duration_one_ms() {
        assert_eq!(format_duration(Duration::from_millis(1)), "1.000 ms");
    }

    #[test]
    fn test_format_duration_fractional() {
        // 1.5 ms
        assert_eq!(format_duration(Duration::from_micros(1500)), "1.500 ms");
    }

    // -----------------------------------------------------------------------
    // Aligned table output
    // -----------------------------------------------------------------------

    #[test]
    fn test_aligned_empty_rows() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        // Should have header, separator, and `(0 rows)`.
        assert!(out.contains("id"), "missing header 'id'");
        assert!(out.contains("name"), "missing header 'name'");
        assert!(out.contains("(0 rows)"), "missing row count");
    }

    #[test]
    fn test_aligned_one_row() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Alice")])],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        assert!(out.contains("(1 row)"), "missing '(1 row)' footer");
        assert!(out.contains("Alice"));
        assert!(out.contains("id"));
    }

    #[test]
    fn test_aligned_two_rows() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![
                mk_row(&[Some("1"), Some("Alice")]),
                mk_row(&[Some("2"), Some("Bob")]),
            ],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        assert!(out.contains("(2 rows)"));
        assert!(out.contains("Alice"));
        assert!(out.contains("Bob"));
    }

    #[test]
    fn test_aligned_separator_format() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Alice")])],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        // Separator must contain `-+-`
        assert!(out.contains("-+-"), "separator missing '-+-': {out}");
    }

    #[test]
    fn test_aligned_null_display() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[None])],
        };
        let mut out = String::new();
        let cfg = OutputConfig {
            null_string: "(null)".to_owned(),
            ..Default::default()
        };
        format_aligned(&mut out, &rs, &cfg);
        assert!(out.contains("(null)"), "null not rendered: {out}");
    }

    #[test]
    fn test_aligned_column_width_wider_than_header() {
        // Data wider than header: column should be padded to data width.
        let rs = RowSet {
            columns: vec![mk_col("x", false)],
            rows: vec![mk_row(&[Some("hello world")])],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        // "hello world" must appear intact (not truncated).
        assert!(out.contains("hello world"));
    }

    #[test]
    fn test_aligned_unicode_column_width() {
        // CJK header + ASCII data: widths should account for double-width chars.
        let rs = RowSet {
            columns: vec![mk_col("中文", false)],
            rows: vec![mk_row(&[Some("ab")])],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        // Both header and data should be present.
        assert!(out.contains("中文"));
        assert!(out.contains("ab"));
    }

    // -----------------------------------------------------------------------
    // Expanded output
    // -----------------------------------------------------------------------

    #[test]
    fn test_expanded_basic() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Alice")])],
        };
        let mut out = String::new();
        format_expanded(&mut out, &rs, &OutputConfig::default());
        assert!(out.contains("-[ RECORD 1 ]"), "missing record header");
        assert!(out.contains("id"), "missing id column");
        assert!(out.contains("Alice"), "missing value");
    }

    #[test]
    fn test_expanded_empty_rows() {
        let rs = RowSet {
            columns: vec![mk_col("id", true)],
            rows: vec![],
        };
        let mut out = String::new();
        format_expanded(&mut out, &rs, &OutputConfig::default());
        assert_eq!(out, "(0 rows)\n");
    }

    #[test]
    fn test_expanded_multiple_records() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![
                mk_row(&[Some("1"), Some("Alice")]),
                mk_row(&[Some("2"), Some("Bob")]),
            ],
        };
        let mut out = String::new();
        format_expanded(&mut out, &rs, &OutputConfig::default());
        assert!(out.contains("-[ RECORD 1 ]"));
        assert!(out.contains("-[ RECORD 2 ]"));
        assert!(out.contains("Alice"));
        assert!(out.contains("Bob"));
    }

    #[test]
    fn test_expanded_header_width_matches_widest_row() {
        // Regression test for GitHub issue #225.
        //
        // Data:
        //   num      | 1
        //   greeting | hello
        //
        // max_name_width = len("greeting") = 8
        // widest row = "greeting | hello" = 8 + 3 + 5 = 16
        // header base = "-[ RECORD 1 ]" = 13 chars
        // expected header = "-[ RECORD 1 ]---" (13 + 3 dashes = 16 chars)
        let rs = RowSet {
            columns: vec![mk_col("num", false), mk_col("greeting", false)],
            rows: vec![mk_row(&[Some("1"), Some("hello")])],
        };
        let mut out = String::new();
        format_expanded(&mut out, &rs, &OutputConfig::default());

        let first_line = out.lines().next().expect("output must not be empty");
        // Header must be exactly 16 chars wide.
        assert_eq!(
            first_line.len(),
            16,
            "header line should be 16 chars wide, got: {first_line:?}"
        );
        assert_eq!(first_line, "-[ RECORD 1 ]---");
    }

    // -----------------------------------------------------------------------
    // format_aligned tuples_only
    // -----------------------------------------------------------------------

    #[test]
    fn test_aligned_tuples_only_suppresses_header_and_footer() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![
                mk_row(&[Some("1"), Some("Alice")]),
                mk_row(&[Some("2"), Some("Bob")]),
            ],
        };
        let mut out = String::new();
        let cfg = OutputConfig {
            tuples_only: true,
            ..Default::default()
        };
        format_aligned(&mut out, &rs, &cfg);
        // Data rows must be present.
        assert!(out.contains("Alice"), "data row missing: {out}");
        assert!(out.contains("Bob"), "data row missing: {out}");
        // Header, separator, and row-count footer must be absent.
        assert!(!out.contains("id"), "header should be suppressed: {out}");
        assert!(
            !out.contains("-+-"),
            "separator should be suppressed: {out}"
        );
        assert!(!out.contains("rows)"), "footer should be suppressed: {out}");
    }

    #[test]
    fn test_aligned_tuples_only_empty_rows_no_footer() {
        let rs = RowSet {
            columns: vec![mk_col("id", true)],
            rows: vec![],
        };
        let mut out = String::new();
        let cfg = OutputConfig {
            tuples_only: true,
            ..Default::default()
        };
        format_aligned(&mut out, &rs, &cfg);
        assert!(
            out.is_empty(),
            "tuples-only with no rows should produce no output: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // format_expanded tuples_only
    // -----------------------------------------------------------------------

    #[test]
    fn test_expanded_tuples_only_suppresses_record_header() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Alice")])],
        };
        let mut out = String::new();
        let cfg = OutputConfig {
            tuples_only: true,
            ..Default::default()
        };
        format_expanded(&mut out, &rs, &cfg);
        // Data values must be present.
        assert!(out.contains("Alice"), "value missing: {out}");
        // Record header must be suppressed.
        assert!(
            !out.contains("-[ RECORD"),
            "record header should be suppressed: {out}"
        );
    }

    #[test]
    fn test_expanded_tuples_only_empty_no_footer() {
        let rs = RowSet {
            columns: vec![mk_col("id", true)],
            rows: vec![],
        };
        let mut out = String::new();
        let cfg = OutputConfig {
            tuples_only: true,
            ..Default::default()
        };
        format_expanded(&mut out, &rs, &cfg);
        assert!(
            out.is_empty(),
            "tuples-only with empty rows should produce no output: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // format_outcome no_align dispatch
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_outcome_no_align_uses_unaligned_format() {
        use crate::query::{QueryOutcome, RowSet, StatementResult};
        let rs = RowSet {
            columns: vec![mk_col("a", false), mk_col("b", false)],
            rows: vec![mk_row(&[Some("1"), Some("2")])],
        };
        let outcome = QueryOutcome {
            results: vec![StatementResult::Rows(rs)],
            duration: Duration::ZERO,
        };
        let cfg = OutputConfig {
            no_align: true,
            ..Default::default()
        };
        let out = format_outcome(&outcome, &cfg);
        // Unaligned: header + data row separated by `|`, no padding.
        assert!(out.contains("a|b"), "expected unaligned header: {out}");
        assert!(out.contains("1|2"), "expected unaligned data: {out}");
    }

    // -----------------------------------------------------------------------
    // Command tag
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_command_tag() {
        use crate::query::CommandTag;
        let ct = CommandTag {
            tag: "INSERT 0 3".to_owned(),
            rows_affected: 3,
        };
        let mut out = String::new();
        format_command_tag(&mut out, &ct);
        assert_eq!(out, "INSERT 0 3\n");
    }

    // -----------------------------------------------------------------------
    // Boolean formatting (comes through as "t"/"f" from query.rs)
    // -----------------------------------------------------------------------

    #[test]
    fn test_boolean_display_in_table() {
        // Simulate what query.rs would produce for booleans.
        let rs = RowSet {
            columns: vec![mk_col("active", false)],
            rows: vec![mk_row(&[Some("t")]), mk_row(&[Some("f")])],
        };
        let mut out = String::new();
        format_aligned(&mut out, &rs, &OutputConfig::default());
        assert!(
            out.contains(" t ") || out.contains(" t\n") || out.contains("| t"),
            "missing 't': {out}"
        );
        assert!(
            out.contains(" f ") || out.contains(" f\n") || out.contains("| f"),
            "missing 'f': {out}"
        );
    }

    // -----------------------------------------------------------------------
    // format_outcome integration
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_outcome_empty_result() {
        use crate::query::{QueryOutcome, StatementResult};
        let outcome = QueryOutcome {
            results: vec![StatementResult::Empty],
            duration: Duration::ZERO,
        };
        let out = format_outcome(&outcome, &OutputConfig::default());
        assert_eq!(out, "");
    }

    #[test]
    fn test_format_outcome_timing() {
        use crate::query::{QueryOutcome, StatementResult};
        let outcome = QueryOutcome {
            results: vec![StatementResult::Empty],
            duration: Duration::from_millis(42),
        };
        let cfg = OutputConfig {
            timing: true,
            ..Default::default()
        };
        let out = format_outcome(&outcome, &cfg);
        assert!(out.contains("Time:"), "missing timing: {out}");
        assert!(out.contains("ms"), "missing 'ms': {out}");
    }

    // -----------------------------------------------------------------------
    // CSV format
    // -----------------------------------------------------------------------

    fn mk_rowset_ab() -> RowSet {
        RowSet {
            columns: vec![mk_col("a", false), mk_col("b", false)],
            rows: vec![
                mk_row(&[Some("1"), Some("2")]),
                mk_row(&[Some("3"), Some("4")]),
            ],
        }
    }

    #[test]
    fn test_csv_basic() {
        let rs = mk_rowset_ab();
        let mut out = String::new();
        format_csv(&mut out, &rs, &PsetConfig::default());
        assert_eq!(out, "a,b\n1,2\n3,4\n");
    }

    #[test]
    fn test_csv_field_with_comma() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[Some("a,b")])],
        };
        let mut out = String::new();
        format_csv(&mut out, &rs, &PsetConfig::default());
        // Field containing comma must be double-quoted.
        assert!(out.contains("\"a,b\""), "expected quoted field: {out}");
    }

    #[test]
    fn test_csv_field_with_double_quote() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[Some("say \"hi\"")])],
        };
        let mut out = String::new();
        format_csv(&mut out, &rs, &PsetConfig::default());
        // Embedded double-quotes must be doubled.
        assert!(
            out.contains("\"say \"\"hi\"\"\""),
            "expected RFC 4180 escaping: {out}"
        );
    }

    #[test]
    fn test_csv_tuples_only_suppresses_header() {
        let rs = mk_rowset_ab();
        let cfg = PsetConfig {
            tuples_only: true,
            ..Default::default()
        };
        let mut out = String::new();
        format_csv(&mut out, &rs, &cfg);
        assert!(!out.starts_with("a,"), "header must be suppressed: {out}");
        assert!(out.contains("1,2"), "data must be present: {out}");
    }

    // -----------------------------------------------------------------------
    // JSON format
    // -----------------------------------------------------------------------

    #[test]
    fn test_json_basic() {
        let rs = mk_rowset_ab();
        let mut out = String::new();
        format_json(&mut out, &rs, &PsetConfig::default());
        // Must be parseable JSON (structural check).
        assert!(out.starts_with('['), "must start with [: {out}");
        assert!(out.trim_end().ends_with(']'), "must end with ]: {out}");
        assert!(out.contains("\"a\""), "must contain key 'a': {out}");
        assert!(out.contains("\"1\""), "must contain value '1': {out}");
    }

    #[test]
    fn test_json_null_becomes_json_null() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[None])],
        };
        let mut out = String::new();
        format_json(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains(":null"), "NULL should be JSON null: {out}");
    }

    #[test]
    fn test_json_escapes_special_chars() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[Some("say \"hi\"\nnewline")])],
        };
        let mut out = String::new();
        format_json(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains("\\\""), "must escape double-quote: {out}");
        assert!(out.contains("\\n"), "must escape newline: {out}");
    }

    #[test]
    fn test_json_empty_rows() {
        let rs = RowSet {
            columns: vec![mk_col("a", false)],
            rows: vec![],
        };
        let mut out = String::new();
        format_json(&mut out, &rs, &PsetConfig::default());
        assert_eq!(out.trim(), "[]");
    }

    // -----------------------------------------------------------------------
    // HTML format
    // -----------------------------------------------------------------------

    #[test]
    fn test_html_basic() {
        let rs = mk_rowset_ab();
        let mut out = String::new();
        format_html(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains("<table>"), "missing <table>: {out}");
        assert!(out.contains("<th>a</th>"), "missing <th>a</th>: {out}");
        assert!(out.contains("<td>1</td>"), "missing <td>1</td>: {out}");
        assert!(out.contains("</table>"), "missing </table>: {out}");
    }

    #[test]
    fn test_html_escapes_special_chars() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[Some("<b>bold</b> & \"quoted\"")])],
        };
        let mut out = String::new();
        format_html(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains("&lt;b&gt;"), "must escape <: {out}");
        assert!(out.contains("&amp;"), "must escape &: {out}");
        assert!(out.contains("&quot;"), "must escape \": {out}");
    }

    #[test]
    fn test_html_tuples_only_suppresses_header() {
        let rs = mk_rowset_ab();
        let cfg = PsetConfig {
            tuples_only: true,
            ..Default::default()
        };
        let mut out = String::new();
        format_html(&mut out, &rs, &cfg);
        assert!(!out.contains("<thead>"), "thead must be suppressed: {out}");
        assert!(out.contains("<td>"), "data must be present: {out}");
    }

    // -----------------------------------------------------------------------
    // Unaligned format
    // -----------------------------------------------------------------------

    #[test]
    fn test_unaligned_basic() {
        let rs = mk_rowset_ab();
        let mut out = String::new();
        format_unaligned(&mut out, &rs, &PsetConfig::default());
        // Default field separator is `|`.
        assert!(out.contains("a|b"), "header with | separator: {out}");
        assert!(out.contains("1|2"), "data with | separator: {out}");
    }

    #[test]
    fn test_unaligned_custom_separator() {
        let rs = mk_rowset_ab();
        let cfg = PsetConfig {
            field_sep: ",".to_owned(),
            ..Default::default()
        };
        let mut out = String::new();
        format_unaligned(&mut out, &rs, &cfg);
        assert!(out.contains("a,b"), "custom sep in header: {out}");
        assert!(out.contains("1,2"), "custom sep in data: {out}");
    }

    #[test]
    fn test_unaligned_null_display() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[None])],
        };
        let cfg = PsetConfig {
            null_display: "[NULL]".to_owned(),
            ..Default::default()
        };
        let mut out = String::new();
        format_unaligned(&mut out, &rs, &cfg);
        assert!(out.contains("[NULL]"), "null display: {out}");
    }

    /// Verify that a custom record separator is used between rows but not
    /// appended after the last row — matching psql `-A -R '|' -t` behaviour.
    #[test]
    fn test_unaligned_no_trailing_record_sep() {
        let rs = RowSet {
            columns: vec![mk_col("n", false)],
            rows: vec![
                mk_row(&[Some("1")]),
                mk_row(&[Some("2")]),
                mk_row(&[Some("3")]),
            ],
        };
        let cfg = PsetConfig {
            record_sep: "|".to_owned(),
            tuples_only: true,
            ..Default::default()
        };
        let mut out = String::new();
        format_unaligned(&mut out, &rs, &cfg);
        // Rows separated by `|`, final row ends with `\n` only (no trailing `|`).
        assert_eq!(out, "1|2|3\n", "no trailing record sep: {out:?}");
    }

    // -----------------------------------------------------------------------
    // format_pg_error — non-db-error path
    // -----------------------------------------------------------------------

    /// Strip ANSI escape sequences for assertion helpers.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip everything up to and including the 'm' terminator.
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Construct a `tokio_postgres::Error` from an I/O error so we can test
    /// the non-`DbError` branch of `format_pg_error` without a live database.
    fn make_io_pg_error() -> tokio_postgres::Error {
        // tokio_postgres::Error::from(io::Error) gives a non-db error.
        tokio_postgres::Error::__private_api_timeout()
    }

    #[test]
    fn test_format_pg_error_non_db_shows_error_prefix() {
        let e = make_io_pg_error();
        let cfg = OutputConfig::default();
        let out = format_pg_error(&e, None, &cfg);
        // Strip ANSI color codes before checking the prefix, since the
        // severity keyword is now colored.
        let plain = strip_ansi(&out);
        assert!(
            plain.starts_with("ERROR:  "),
            "non-db error should start with ERROR:  — got: {out:?}"
        );
    }

    #[test]
    fn test_format_pg_error_severity_colored() {
        // The raw output must contain the bold-red ANSI code for ERROR.
        let e = make_io_pg_error();
        let cfg = OutputConfig::default();
        let out = format_pg_error(&e, None, &cfg);
        assert!(
            out.contains("\x1b[1;31m"),
            "ERROR prefix should be bold-red: {out:?}"
        );
        assert!(
            out.contains("\x1b[0m"),
            "output should contain ANSI reset after severity: {out:?}"
        );
    }

    #[test]
    fn test_format_pg_error_ends_with_newline() {
        let e = make_io_pg_error();
        let cfg = OutputConfig::default();
        let out = format_pg_error(&e, None, &cfg);
        assert!(
            out.ends_with('\n'),
            "output should end with newline: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Zero-column SELECT rendering (issue #643)
    // -----------------------------------------------------------------------

    /// `SELECT FROM t WHERE i = 10` returns rows with zero columns.
    /// psql renders `--\n(1 row)\n` — we must match that.
    #[test]
    fn test_aligned_zero_columns_one_row() {
        let rs = RowSet {
            columns: vec![],
            // One row with no cells — matches `SELECT FROM t WHERE i = 10`
            // when exactly one row is found.
            rows: vec![vec![]],
        };
        let mut out = String::new();
        format_aligned_pset(
            &mut out,
            &rs,
            &OutputConfig::default(),
            &PsetConfig::default(),
        );
        assert!(
            out.contains("--"),
            "zero-col header separator missing: {out:?}"
        );
        assert!(out.contains("(1 row)"), "row-count footer missing: {out:?}");
    }

    #[test]
    fn test_aligned_zero_columns_zero_rows() {
        let rs = RowSet {
            columns: vec![],
            rows: vec![],
        };
        let mut out = String::new();
        format_aligned_pset(
            &mut out,
            &rs,
            &OutputConfig::default(),
            &PsetConfig::default(),
        );
        assert!(
            out.contains("--"),
            "zero-col header separator missing: {out:?}"
        );
        assert!(
            out.contains("(0 rows)"),
            "row-count footer missing: {out:?}"
        );
    }

    #[test]
    fn test_aligned_zero_columns_many_rows() {
        let rs = RowSet {
            columns: vec![],
            rows: vec![vec![]; 10],
        };
        let mut out = String::new();
        format_aligned_pset(
            &mut out,
            &rs,
            &OutputConfig::default(),
            &PsetConfig::default(),
        );
        assert!(
            out.contains("--"),
            "zero-col header separator missing: {out:?}"
        );
        assert!(
            out.contains("(10 rows)"),
            "row-count footer missing: {out:?}"
        );
    }

    #[test]
    fn test_aligned_zero_columns_tuples_only_suppresses_all() {
        let rs = RowSet {
            columns: vec![],
            rows: vec![vec![]; 3],
        };
        let cfg = PsetConfig {
            tuples_only: true,
            ..Default::default()
        };
        let mut out = String::new();
        format_aligned_pset(&mut out, &rs, &OutputConfig::default(), &cfg);
        // tuples-only suppresses both the `--` header and the row-count footer.
        assert!(
            out.is_empty(),
            "tuples-only must produce no output: {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Markdown format
    // -----------------------------------------------------------------------

    #[test]
    fn test_markdown_basic() {
        let rs = RowSet {
            columns: vec![mk_col("id", true), mk_col("name", false)],
            rows: vec![
                mk_row(&[Some("1"), Some("Alice")]),
                mk_row(&[Some("2"), Some("Bob")]),
            ],
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &PsetConfig::default());
        // Header row must contain column names delimited by `|`.
        assert!(out.contains("| id |"), "header missing 'id': {out}");
        assert!(
            out.contains("| name |") || out.contains("name"),
            "header missing 'name': {out}"
        );
        // Separator row: dashes between pipes.
        assert!(out.contains("|----"), "separator missing: {out}");
        // Data rows present.
        assert!(out.contains("Alice"), "missing Alice: {out}");
        assert!(out.contains("Bob"), "missing Bob: {out}");
        // Row count footer.
        assert!(out.contains("(2 rows)"), "missing footer: {out}");
    }

    #[test]
    fn test_markdown_structure() {
        // Verify exact output structure for a known input.
        let rs = RowSet {
            columns: vec![mk_col("id", false), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Sam Martin")])],
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &PsetConfig::default());
        let lines: Vec<&str> = out.lines().collect();
        // Line 0: header
        assert!(
            lines[0].starts_with('|') && lines[0].ends_with('|'),
            "header must start and end with '|': {out}"
        );
        // Line 1: separator (all dashes and pipes)
        assert!(
            lines[1].chars().all(|c| c == '-' || c == '|'),
            "separator must only contain '-' and '|': {:?}",
            lines[1]
        );
        // Line 2: data row
        assert!(
            lines[2].starts_with('|') && lines[2].ends_with('|'),
            "data row must start and end with '|': {out}"
        );
        // Line 3: row count footer
        assert_eq!(lines[3], "(1 row)", "footer mismatch: {out}");
    }

    #[test]
    fn test_markdown_null_display() {
        let rs = RowSet {
            columns: vec![mk_col("val", false)],
            rows: vec![mk_row(&[None])],
        };
        let cfg = PsetConfig {
            null_display: "(null)".to_owned(),
            ..Default::default()
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &cfg);
        assert!(out.contains("(null)"), "null display missing: {out}");
    }

    #[test]
    fn test_markdown_empty_rows() {
        let rs = RowSet {
            columns: vec![mk_col("id", false)],
            rows: vec![],
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains("| id |"), "header missing: {out}");
        assert!(out.contains("|----"), "separator missing: {out}");
        assert!(out.contains("(0 rows)"), "footer missing: {out}");
    }

    #[test]
    fn test_markdown_tuples_only_suppresses_header_and_footer() {
        let rs = RowSet {
            columns: vec![mk_col("id", false), mk_col("name", false)],
            rows: vec![mk_row(&[Some("1"), Some("Alice")])],
        };
        let cfg = PsetConfig {
            tuples_only: true,
            ..Default::default()
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &cfg);
        // Data must be present.
        assert!(out.contains("Alice"), "data row missing: {out}");
        // Header and footer must be absent.
        assert!(!out.contains("| id |"), "header must be suppressed: {out}");
        assert!(!out.contains("(1 row)"), "footer must be suppressed: {out}");
    }

    #[test]
    fn test_markdown_column_width_wider_than_header() {
        // Data wider than header: separator dashes must match data width.
        let rs = RowSet {
            columns: vec![mk_col("x", false)],
            rows: vec![mk_row(&[Some("hello world")])],
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &PsetConfig::default());
        assert!(out.contains("hello world"), "data not truncated: {out}");
        // Separator should have at least 11 dashes (len of "hello world").
        let sep_line = out.lines().nth(1).expect("separator line must exist");
        let dash_count = sep_line.chars().filter(|&c| c == '-').count();
        assert!(
            dash_count >= 11,
            "separator must cover data width (11): got {dash_count} dashes in {sep_line:?}"
        );
    }

    #[test]
    fn test_markdown_footer_suppressed_when_footer_off() {
        let rs = RowSet {
            columns: vec![mk_col("id", false)],
            rows: vec![mk_row(&[Some("1")])],
        };
        let cfg = PsetConfig {
            footer: false,
            ..Default::default()
        };
        let mut out = String::new();
        format_markdown(&mut out, &rs, &cfg);
        // Column name "id" is 2 chars; value "1" is padded to 2 chars.
        assert!(out.contains("| 1"), "data missing: {out}");
        assert!(!out.contains("(1 row)"), "footer must be suppressed: {out}");
    }
}
