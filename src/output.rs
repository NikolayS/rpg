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
        // No columns: just print the row count footer (suppressed in tuples-only).
        if !cfg.tuples_only {
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
fn column_widths(
    cols: &[ColumnMeta],
    rows: &[Vec<Option<String>>],
    cfg: &OutputConfig,
) -> Vec<usize> {
    let mut widths: Vec<usize> = cols.iter().map(|c| display_width(&c.name)).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let cell_str = cell.as_deref().unwrap_or(&cfg.null_string);
            let w = display_width(cell_str);
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    widths
}

/// Write one row of the aligned table (header or data).
///
/// `value_fn` maps `(column_meta, column_index) → String`.
/// `is_header` – when true, text columns are center-aligned (matching psql).
/// Numeric columns are always right-aligned (both header and data rows).
fn write_aligned_row<F>(
    out: &mut String,
    cols: &[ColumnMeta],
    widths: &[usize],
    value_fn: F,
    is_header: bool,
) where
    F: Fn(&ColumnMeta, usize) -> String,
{
    for (i, col) in cols.iter().enumerate() {
        let val = value_fn(col, i);
        let w = widths[i];
        let val_width = display_width(&val);
        let padding = w.saturating_sub(val_width);

        if i == 0 {
            out.push(' ');
        } else {
            out.push_str(" | ");
        }

        if col.is_numeric {
            // Right-align numeric columns (both headers and data).
            for _ in 0..padding {
                out.push(' ');
            }
            out.push_str(&val);
        } else if is_header {
            // Center-align text headers (psql behaviour).
            let left_pad = padding / 2;
            let right_pad = padding - left_pad;
            for _ in 0..left_pad {
                out.push(' ');
            }
            out.push_str(&val);
            for _ in 0..right_pad {
                out.push(' ');
            }
        } else {
            // Left-align text data.
            out.push_str(&val);
            for _ in 0..padding {
                out.push(' ');
            }
        }
    }
    // psql pads every cell including the last column with a trailing space.
    out.push(' ');
    out.push('\n');
}

/// Write the `----+--------` separator line.
fn write_separator(out: &mut String, widths: &[usize]) {
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
        // Severity line.
        let _ = writeln!(out, "{}:  {}", db_err.severity(), db_err.message());

        // Position marker.
        if let Some(pos) = db_err.position() {
            if let Some(sql) = original_sql {
                write_error_position(&mut out, sql, pos);
            }
        }

        // DETAIL line.
        if let Some(detail) = db_err.detail() {
            let _ = writeln!(out, "DETAIL:  {detail}");
        }

        // HINT line.
        if let Some(hint) = db_err.hint() {
            let _ = writeln!(out, "HINT:  {hint}");
        }

        // SQLSTATE: only shown in verbose mode (psql default: hidden).
        if cfg.verbose_errors {
            let _ = writeln!(out, "SQLSTATE:  {}", db_err.code().code());
        }
    } else {
        // Non-server error (I/O, protocol, …).
        let _ = writeln!(out, "ERROR:  {err}");
    }

    out
}

/// Print a `tokio_postgres::Error` to stderr in psql style.
///
/// Convenience wrapper around [`format_pg_error`] for call sites that do
/// not need the string representation.  `sql` is the original query text
/// (used to render the position marker); pass `None` when unavailable.
/// `verbose` enables SQLSTATE output (mirrors `\set VERBOSITY verbose`).
pub fn eprint_db_error(err: &tokio_postgres::Error, sql: Option<&str>, verbose: bool) {
    let cfg = OutputConfig {
        verbose_errors: verbose,
        ..OutputConfig::default()
    };
    let msg = format_pg_error(err, sql, &cfg);
    // format_pg_error always ends with a newline; use eprint! to avoid double.
    eprint!("{msg}");
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

/// Returns the terminal display width of a string, handling multi-byte and
/// double-width Unicode characters (CJK, emoji, …).
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

// ---------------------------------------------------------------------------
// Aligned table with PsetConfig (handles tuples_only + footer)
// ---------------------------------------------------------------------------

/// Aligned table formatter that honours `PsetConfig` for tuples-only and
/// footer suppression.  Delegates column-width calculation to the shared
/// [`column_widths`] helper.
fn format_aligned_pset(out: &mut String, rs: &RowSet, ocfg: &OutputConfig, pcfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    if cols.is_empty() {
        if !pcfg.tuples_only && pcfg.footer {
            write_row_count(out, rows.len());
        }
        return;
    }

    let widths = column_widths(cols, rows, ocfg);

    // Header (suppressed in tuples-only mode).
    // psql center-aligns text headers and right-aligns numeric ones.
    if !pcfg.tuples_only {
        write_aligned_row(out, cols, &widths, |col, _| col.name.clone(), true);
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
                    .unwrap_or_else(|| ocfg.null_string.clone())
            },
            false,
        );
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
        assert!(
            out.starts_with("ERROR:  "),
            "non-db error should start with ERROR:  — got: {out:?}"
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
}
