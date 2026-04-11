//! Output formatting for query results.
//!
//! Produces psql-compatible output:
//! - Aligned table (default)
//! - Expanded (`\x`) output
//! - Unaligned, CSV, JSON, HTML
//! - Error display with position marker
//! - Timing footer (`Time: X.XXX ms`)

use std::fmt::Write as FmtWrite;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::query::{ColumnMeta, CommandTag, QueryOutcome, RowSet, StatementResult};

/// Global terse-errors flag, mirroring `\set VERBOSITY terse`.
/// Set by the REPL when VERBOSITY changes; read by the notice handler
/// in the connection task (which has no access to `Settings`).
static TERSE_NOTICES: AtomicBool = AtomicBool::new(false);

/// Update the global terse-notice flag.  Call this whenever
/// `settings.terse_errors` changes.
pub fn set_terse_notices(terse: bool) {
    TERSE_NOTICES.store(terse, Ordering::Relaxed);
}

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// LaTeX tabular format.
    Latex,
    /// LaTeX longtable format.
    LatexLongtable,
    /// Troff-ms table format.
    TroffMs,
    /// `AsciiDoc` table format.
    Asciidoc,
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
    /// Field separator for CSV output (default `,`).
    pub csv_field_sep: String,
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
    /// Line style: "ascii", "old-ascii", "unicode" (default: "ascii").
    pub linestyle: String,
    /// Target column width for wrapped format (0 = auto/unset).
    pub columns: usize,
    /// Use locale-aware numeric formatting (not fully implemented, stored for psql compat).
    pub numericlocale: bool,
    /// HTML table attributes (e.g. "border=1").
    pub tableattr: Option<String>,
    /// Unicode border line style: "single", "double" (default: "single").
    pub unicode_border_linestyle: String,
    /// Unicode column line style: "single", "double" (default: "single").
    pub unicode_column_linestyle: String,
    /// Unicode header line style: "single", "double" (default: "single").
    pub unicode_header_linestyle: String,
    /// Use zero byte as field separator for unaligned output.
    pub fieldsep_zero: bool,
    /// Use zero byte as record separator for unaligned output.
    pub recordsep_zero: bool,
    /// `xheader_width`: "full", "column", or N (default: "full").
    pub xheader_width: String,
}

impl Default for PsetConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Aligned,
            border: 1,
            null_display: String::new(),
            field_sep: "|".to_owned(),
            csv_field_sep: ",".to_owned(),
            record_sep: "\n".to_owned(),
            tuples_only: false,
            footer: true,
            title: None,
            expanded: ExpandedMode::Off,
            no_highlight: false,
            linestyle: "ascii".to_owned(),
            columns: 0,
            numericlocale: false,
            tableattr: None,
            unicode_border_linestyle: "single".to_owned(),
            unicode_column_linestyle: "single".to_owned(),
            unicode_header_linestyle: "single".to_owned(),
            fieldsep_zero: false,
            recordsep_zero: false,
            xheader_width: "full".to_owned(),
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
    // Suppressed in tuples_only mode and CSV format (matching psql behaviour).
    let show_title =
        !cfg.tuples_only && cfg.format != OutputFormat::Html && cfg.format != OutputFormat::Csv;
    if show_title {
        if let Some(ref title) = cfg.title {
            let _ = writeln!(out, "{title}");
        }
    }

    match &cfg.format {
        OutputFormat::Aligned => {
            if cfg.expanded == ExpandedMode::On {
                format_expanded_pset(out, rs, cfg);
            } else {
                let ocfg = OutputConfig {
                    null_string: cfg.null_display.clone(),
                    tuples_only: cfg.tuples_only,
                    ..Default::default()
                };
                format_aligned_pset(out, rs, &ocfg, cfg);
            }
        }
        OutputFormat::Wrapped => {
            if cfg.expanded == ExpandedMode::On {
                format_expanded_pset(out, rs, cfg);
            } else {
                format_wrapped_pset(out, rs, cfg);
            }
        }
        OutputFormat::Unaligned => {
            if cfg.expanded == ExpandedMode::On {
                format_expanded_unaligned(out, rs, cfg);
            } else {
                format_unaligned(out, rs, cfg);
            }
        }
        OutputFormat::Csv => {
            if cfg.expanded == ExpandedMode::On {
                format_expanded_csv(out, rs, cfg);
            } else {
                format_csv(out, rs, cfg);
            }
        }
        OutputFormat::Json => format_json(out, rs, cfg),
        OutputFormat::Html => format_html(out, rs, cfg),
        OutputFormat::Markdown => format_markdown(out, rs, cfg),
        OutputFormat::Latex => format_latex(out, rs, cfg),
        OutputFormat::LatexLongtable => format_latex_longtable(out, rs, cfg),
        OutputFormat::TroffMs => format_troff_ms(out, rs, cfg),
        OutputFormat::Asciidoc => format_asciidoc(out, rs, cfg),
    }

    // psql prints a blank line after aligned result sets (trailing newline
    // after `(N rows)` plus one more blank line).  Unaligned and CSV modes
    // omit this extra blank line entirely.
    let is_unaligned = matches!(cfg.format, OutputFormat::Unaligned | OutputFormat::Csv);
    if !is_unaligned {
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
    let mut widths: Vec<usize> = cols.iter().map(|c| cell_display_width(&c.name)).collect();

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
#[allow(clippy::too_many_lines)]
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
        // Split each column name into physical lines (handles embedded '\n').
        let ncols = cols.len();
        let header_vals: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, col)| value_fn(col, i))
            .collect();
        let header_name_lines: Vec<Vec<&str>> = header_vals
            .iter()
            .map(|v| v.split('\n').collect())
            .collect();
        let max_header_lines = header_name_lines
            .iter()
            .map(std::vec::Vec::len)
            .max()
            .unwrap_or(1);

        // Format:
        //   border=0: col0_centered marker col1_centered marker ... lastcol_centered ['+' if more]
        //     where marker = '+' if this col has more lines, else ' ' (1-space gap)
        //   border=1: ' ' col0_centered marker '|' ' ' col1_centered marker '|' ... lastcol_centered ['+' if more]
        //   border=2: '|' ' ' col0_centered marker '|' ' ' col1_centered marker '|' ... lastcol_centered marker '|'
        for line_idx in 0..max_header_lines {
            // Leading border prefix (before first column).
            match border {
                0 => {}
                2 => out.push_str("| "),
                _ => out.push(' '),
            }

            for (i, _col) in cols.iter().enumerate() {
                let w = widths[i];
                let col_lines = &header_name_lines[i];
                let text = col_lines.get(line_idx).copied().unwrap_or("");
                let has_more = line_idx + 1 < col_lines.len();
                let marker = if has_more { '+' } else { ' ' };

                let text_width = display_width(text);
                let padding = w.saturating_sub(text_width);
                let left_pad = padding / 2;
                let right_pad = padding - left_pad;

                // Inter-column space (printed before col j>0 content, after previous
                // col's marker+pipe were already emitted as that col's suffix).
                if i > 0 {
                    out.push(' '); // space between pipe and content
                }

                // Center-aligned content.
                for _ in 0..left_pad {
                    out.push(' ');
                }
                out.push_str(text);
                for _ in 0..right_pad {
                    out.push(' ');
                }

                // Column suffix: marker [+ '|'] depending on border and position.
                //   border=0: marker (gap to next col, or '+' if last-with-more)
                //   border=1: marker + '|' (non-last); '+' or nothing (last)
                //   border=2: marker + '|' (all columns, including last = closing '|')
                let is_last = i == ncols - 1;
                match border {
                    0 => {
                        if !is_last {
                            out.push(marker); // '+' or ' ' as gap
                        } else if has_more {
                            out.push('+'); // '+' on last col if more lines
                        }
                    }
                    2 => {
                        out.push(marker);
                        out.push('|');
                    }
                    _ => {
                        if !is_last {
                            out.push(marker);
                            out.push('|');
                        } else if has_more {
                            out.push('+');
                        } else {
                            out.push(' '); // trailing space on last header col (psql compat)
                        }
                    }
                }
            }
            out.push('\n');
        }
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
    let max_physical_lines = split_lines
        .iter()
        .map(std::vec::Vec::len)
        .max()
        .unwrap_or(1);

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
                        // border 0: one-space gap, replaced by '+' when previous
                        // column has more physical lines.
                        if !is_last_phys && col_continues[i - 1] {
                            out.push('+');
                        } else {
                            out.push(' ');
                        }
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

        // Row suffix.  For border-0/1, if the LAST column has continuation on
        // this physical row, append `+` (border-0) or replace trailing space
        // with `+` (border-1).
        match border {
            0 => {
                let last_continues = col_continues.last().copied().unwrap_or(false);
                if !is_last_phys && last_continues {
                    out.push('+');
                }
            }
            2 => out.push_str(" |"),
            _ => {
                let last_continues = col_continues.last().copied().unwrap_or(false);
                if !is_last_phys && last_continues {
                    out.push('+');
                }
                // No trailing space on data rows (psql compat).
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
            // border 0: each column is `w` dashes, separated by one space.
            for (i, &w) in widths.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
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

/// Format expanded output using full `PsetConfig` (supports border=0/1/2 and wrapping).
#[allow(clippy::too_many_lines)]
fn format_expanded_pset(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let border = cfg.border;
    let is_wrapped = cfg.format == OutputFormat::Wrapped;
    let columns = cfg.columns;
    let null_str = &cfg.null_display;
    let tuples_only = cfg.tuples_only;
    let is_old_ascii = cfg.linestyle == "old-ascii";

    if rows.is_empty() {
        if !tuples_only {
            out.push_str("(0 rows)\n");
        }
        return;
    }

    // Widest single line of any column name.
    let max_name_width = cols
        .iter()
        .map(|c| c.name.split('\n').map(display_width).max().unwrap_or(0))
        .max()
        .unwrap_or(0);

    // Widest single line of any data value.
    let max_value_width = rows
        .iter()
        .flat_map(|row| {
            cols.iter().enumerate().map(move |(i, _col)| {
                row.get(i)
                    .and_then(|v| v.as_deref())
                    .map_or(0, |v| v.split('\n').map(display_width).max().unwrap_or(0))
            })
        })
        .max()
        .unwrap_or(0);

    // wrap_w = max chars of value content per physical line (not counting the end marker).
    // 0 = no wrapping.
    //
    // ASCII line format widths:
    //   border=0: MARKER(1) + name(N) + SEP(1) + val(wrap_w) + END_MARKER(1) = columns
    //             => wrap_w = columns - N - 3
    //   border=1: name(N) + MARKER(1) + |(1) + SP(1) + val(wrap_w) + END_MARKER(1) = columns
    //             => wrap_w = columns - N - 4
    //   border=2: "| "(2) + name(N) + MARKER(1) + "| "(2) + val(wrap_w) + END_MARKER(1) + |(1) = cols
    //             => wrap_w = columns - N - 7
    //
    // Old-ASCII has no END_MARKER at line end (continuation shown on next line):
    //   border=0: MARKER(1) + name(N) + SEP(1) + val(wrap_w) = columns
    //             => wrap_w = columns - N - 2
    //   border=1: MARKER(1) + name(N) + SP(1) + SEP(1) + SP(1) + val(wrap_w) = columns
    //             => wrap_w = columns - N - 4
    //   border=2: |(1) + MARKER(1) + name(N) + SP(1) + SEP(1) + SP(1) + val(wrap_w) + SP(1) + |(1) = cols
    //             => wrap_w = columns - N - 6
    let wrap_w: usize = if is_wrapped && columns > 0 {
        let overhead: usize = if is_old_ascii {
            match border {
                0 => max_name_width + 2,
                1 => max_name_width + 4,
                _ => max_name_width + 6,
            }
        } else {
            match border {
                0 => max_name_width + 3,
                1 => max_name_width + 4,
                _ => max_name_width + 7,
            }
        };
        columns.saturating_sub(overhead).max(3)
    } else {
        0
    };

    // For border=2: val display area width (chars between central '|' and closing '|').
    // ASCII wrapped:     wrap_w + 1  (val text + end_marker slot)
    // ASCII aligned:     max_value_width + 1  (val text + trailing space)
    // Old-ascii wrapped: wrap_w      (no end-marker slot; val text + trailing space)
    // Old-ascii aligned: max_value_width + 1  (val text + trailing space)
    let val_display_w = if border == 2 {
        if wrap_w > 0 && is_old_ascii {
            wrap_w
        } else if wrap_w > 0 {
            wrap_w + 1
        } else {
            max_value_width + 1
        }
    } else {
        0
    };

    // Right inner dashes for border=2 separator lines (+----+-------+).
    // = val_display_w + 1  (right outer '+' boundary counts as 1 extra)
    // ASCII wrapped:     wrap_w + 2
    // ASCII aligned:     max_value_width + 2
    // Old-ascii wrapped: wrap_w + 1
    // Old-ascii aligned: max_value_width + 2  (same as ASCII aligned)
    let b2_right_dashes = if wrap_w > 0 {
        if is_old_ascii {
            wrap_w + 1
        } else {
            wrap_w + 2
        }
    } else {
        max_value_width + 2
    };

    // Write a border=2 separator line.
    // For column separator (label=""):  +----+------+
    // For record header (label="-[ RECORD N ]"): +-[ RECORD N ]---+  (spans full width)
    let write_b2_sep = |out: &mut String, label: &str| {
        if label.is_empty() {
            // Column separator: split at name/val boundary
            let left_inner = max_name_width + 2;
            out.push('+');
            for _ in 0..left_inner {
                out.push('-');
            }
            out.push('+');
            for _ in 0..b2_right_dashes {
                out.push('-');
            }
            out.push('+');
        } else {
            // Record header: single separator spanning full width
            // total_width = 1(+) + left_inner(max_name_width+2) + 1(+) + b2_right_dashes + 1(+)
            let total_width = max_name_width + b2_right_dashes + 5;
            let fill = total_width.saturating_sub(label.len() + 2);
            out.push('+');
            out.push_str(label);
            for _ in 0..fill {
                out.push('-');
            }
            out.push('+');
        }
        out.push('\n');
    };

    // --- Wrapped val line info ---
    // For each physical output line we need: text, is_physical_cont, is_last_of_segment.
    // is_physical_cont: true if this line continues a previous physical wrap (use |. separator).
    // is_last_of_segment: true if no more physical lines follow in the same segment.
    struct WrapLine {
        text: String,
        is_physical_cont: bool,
        is_last_of_segment: bool,
    }

    for (rec_idx, row) in rows.iter().enumerate() {
        // --- Record separator / header ---
        match border {
            0 => {
                if !tuples_only {
                    let _ = writeln!(out, "* Record {}", rec_idx + 1);
                }
            }
            1 => {
                if !tuples_only {
                    let label = format!("-[ RECORD {} ]", rec_idx + 1);
                    let label_len = label.len();
                    // The pipe position in data rows: max_name_width + 1 (name + ASCII marker).
                    // For old-ascii: leading marker before name, but pipe at same position.
                    let pipe_pos = max_name_width + 1;
                    let val_part = if wrap_w > 0 { wrap_w } else { max_value_width };
                    out.push_str(&label);
                    if label_len <= pipe_pos {
                        // label fits in name column: use '+' separator
                        let fill_left = pipe_pos - label_len;
                        for _ in 0..fill_left {
                            out.push('-');
                        }
                        out.push('+');
                        // Right side fill:
                        // ASCII:     name + marker + '|' + ' ' + val + end_marker = N+1+1+1+W+1
                        //            => right = val_part + 1
                        // Old-ascii: MARKER + name + ' ' + sep + ' ' + val = 1+N+1+1+1+W
                        //            => right = val_part + 2
                        let right_fill = if is_old_ascii {
                            val_part + 2
                        } else {
                            val_part + 1
                        };
                        for _ in 0..right_fill {
                            out.push('-');
                        }
                    } else {
                        // label overflows into value column, no '+' separator
                        // Total header width:
                        //   ASCII:     max_name_width + val_part + 3  (= columns - 1)
                        //   Old-ascii: max_name_width + val_part + 4  (= columns)
                        let total_header = if is_old_ascii {
                            max_name_width + val_part + 4
                        } else {
                            max_name_width + val_part + 3
                        };
                        let fill = total_header.saturating_sub(label_len);
                        for _ in 0..fill {
                            out.push('-');
                        }
                    }
                    out.push('\n');
                }
            }
            _ => {
                let label = format!("-[ RECORD {} ]", rec_idx + 1);
                write_b2_sep(out, &label);
            }
        }

        // --- Field rows ---
        for (ci, col) in cols.iter().enumerate() {
            let val_raw = row
                .get(ci)
                .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                .unwrap_or_else(|| null_str.clone());

            let name_lines: Vec<&str> = col.name.split('\n').collect();
            let val_lines: Vec<&str> = val_raw.split('\n').collect();

            // Build wrapped_val with physical-line tracking.
            let wrapped_val: Vec<WrapLine> = if wrap_w > 0 {
                let mut wl = Vec::new();
                for vl in &val_lines {
                    let chars: Vec<char> = vl.chars().collect();
                    let total = chars.len();
                    if total == 0 {
                        wl.push(WrapLine {
                            text: String::new(),
                            is_physical_cont: false,
                            is_last_of_segment: true,
                        });
                    } else {
                        let mut start = 0;
                        let mut first_chunk = true;
                        while start < total {
                            let end = (start + wrap_w).min(total);
                            let is_last_chunk = end == total;
                            wl.push(WrapLine {
                                text: chars[start..end].iter().collect(),
                                is_physical_cont: !first_chunk,
                                is_last_of_segment: is_last_chunk,
                            });
                            start = end;
                            first_chunk = false;
                            // For old-ascii: if segment is too long, we still wrap,
                            // but use `;` continuation marker on next line.
                            // For ascii: `.` at end of truncated line (handled in output).
                            if is_last_chunk {
                                break;
                            }
                        }
                        // is_last_of_segment is set via is_last_chunk above.
                    }
                }
                wl
            } else {
                val_lines
                    .iter()
                    .map(|l| WrapLine {
                        text: l.to_string(),
                        is_physical_cont: false,
                        is_last_of_segment: true,
                    })
                    .collect()
            };

            let n_name_lines = name_lines.len();
            let n_val_lines = wrapped_val.len();
            // Total output rows: enough for all name lines AND all val lines.
            let total_rows = n_name_lines.max(n_val_lines);

            for li in 0..total_rows {
                let name_line = if li < n_name_lines {
                    name_lines[li]
                } else {
                    ""
                };
                let wl = if li < n_val_lines {
                    Some(&wrapped_val[li])
                } else {
                    None
                };
                let val_text = wl.map_or("", |w| w.text.as_str());
                let is_physical_cont = wl.is_some_and(|w| w.is_physical_cont);
                let is_last_of_segment = wl.is_none_or(|w| w.is_last_of_segment);
                let is_last_val = li + 1 == n_val_lines;

                let name_w = display_width(name_line);
                let name_pad = max_name_width.saturating_sub(name_w);

                // ASCII name cont marker: '+' for all non-last name lines (li+1 < n_name_lines),
                // ' ' for the last name line and all pure val-cont lines.
                // (li+1 < n_name_lines means "there is another name line after this one")
                let ascii_name_marker = if li + 1 < n_name_lines { '+' } else { ' ' };

                // Old-ascii leading marker: '+' for all non-first name lines (li > 0 && li < n_name_lines),
                // ' ' for li=0 (first name line) and pure val-cont lines.
                let oa_leading = if is_old_ascii && li > 0 && li < n_name_lines {
                    '+'
                } else {
                    ' '
                };

                // Val text width for padding calculations
                let vt_w = display_width(val_text);

                // End-of-line marker for ASCII (appended after val text):
                // - If segment physically wraps to next line: '.' (not last of segment)
                // - Elif more val lines follow: '+' (last of segment but more segs)
                // - Else: nothing
                // For old-ascii: no end-of-line marker (continuation shown at start of next line).
                let ascii_eol_marker: Option<char> = if is_old_ascii || wrap_w == 0 {
                    None
                } else if !is_last_of_segment {
                    Some('.')
                } else if !is_last_val {
                    Some('+')
                } else {
                    None
                };

                // For aligned (wrap_w=0) ASCII: '+' with padding if more val segments
                let aligned_eol_plus = !is_old_ascii && wrap_w == 0 && !is_last_val;

                match border {
                    0 => {
                        if is_old_ascii {
                            // old-ascii border=0:
                            // Format: LEADING_MARKER + name_padded + SEP_SPACE + val
                            out.push(oa_leading);
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(' '); // separator space
                            out.push_str(val_text);
                            // old-ascii: no end-of-line marker; continuation shows on next line
                        } else {
                            // ASCII border=0:
                            // Format: name_padded + MARKER + SEP_SPACE + val + END_MARKER
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(ascii_name_marker);
                            out.push(' '); // separator space
                            out.push_str(val_text);
                            if let Some(m) = ascii_eol_marker {
                                let pad = wrap_w.saturating_sub(vt_w);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push(m);
                            } else if aligned_eol_plus {
                                let pad = max_value_width.saturating_sub(vt_w);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push('+');
                            }
                        }
                        out.push('\n');
                    }
                    1 => {
                        if is_old_ascii {
                            // old-ascii border=1:
                            // Format: LEADING_MARKER + name_padded + SP + SEPARATOR + SP + val
                            // SEPARATOR: '|' for li=0 with non-empty val, ':' for subsequent
                            //            non-empty val, ';' for empty val or physical cont
                            out.push(oa_leading);
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(' ');
                            let sep = if is_physical_cont || val_text.is_empty() {
                                ';'
                            } else if li == 0 {
                                '|'
                            } else {
                                ':'
                            };
                            out.push(sep);
                            if !val_text.is_empty() || is_physical_cont {
                                out.push(' ');
                                out.push_str(val_text);
                            }
                        } else {
                            // ASCII border=1:
                            // Format: name_padded + MARKER + SEP + SP + val + END_MARKER
                            // SEP: '|.' for physical cont, '|' otherwise (space after '|')
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(ascii_name_marker);
                            if is_physical_cont {
                                out.push_str("|.");
                            } else {
                                out.push_str("| ");
                            }
                            out.push_str(val_text);
                            if let Some(m) = ascii_eol_marker {
                                let pad = wrap_w.saturating_sub(vt_w);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push(m);
                            } else if aligned_eol_plus {
                                let pad = max_value_width.saturating_sub(vt_w);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push('+');
                            }
                        }
                        out.push('\n');
                    }
                    _ => {
                        if is_old_ascii {
                            // old-ascii border=2:
                            // Format: | + LEADING_MARKER + name_padded + SP + SEP + SP + val + SP + |
                            out.push('|');
                            out.push(oa_leading);
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(' ');
                            let sep =
                                if is_physical_cont || (val_text.is_empty() && li < n_name_lines) {
                                    ';'
                                } else if li == 0 {
                                    '|'
                                } else {
                                    ':'
                                };
                            out.push(sep);
                            out.push(' ');
                            out.push_str(val_text);
                            let pad = val_display_w.saturating_sub(vt_w);
                            for _ in 0..pad {
                                out.push(' ');
                            }
                            out.push('|');
                        } else {
                            // ASCII border=2:
                            // Format: "| " + name_padded + MARKER + SEP + val_area + "|"
                            // SEP: '|.' for physical cont, '| ' otherwise
                            out.push_str("| ");
                            out.push_str(name_line);
                            for _ in 0..name_pad {
                                out.push(' ');
                            }
                            out.push(ascii_name_marker);
                            if is_physical_cont {
                                out.push_str("|.");
                            } else {
                                out.push_str("| ");
                            }
                            // val_area = val_display_w chars before closing '|'
                            out.push_str(val_text);
                            if let Some(m) = ascii_eol_marker {
                                let pad = val_display_w.saturating_sub(vt_w + 1);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push(m);
                            } else if aligned_eol_plus {
                                let pad = val_display_w.saturating_sub(vt_w + 1);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                                out.push('+');
                            } else {
                                let pad = val_display_w.saturating_sub(vt_w);
                                for _ in 0..pad {
                                    out.push(' ');
                                }
                            }
                            out.push('|');
                        }
                        out.push('\n');
                    }
                }
            }
        }
    }

    // border=2: final closing separator after all records
    if border == 2 {
        write_b2_sep(out, "");
    }
}

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
    // For multiline values, use the widest line.
    // The expanded header must be padded to this width to match psql behaviour.
    let max_data_width = rows
        .iter()
        .flat_map(|row| {
            cols.iter().enumerate().map(move |(i, _col)| {
                let val_len = row
                    .get(i)
                    .and_then(|v| v.as_deref())
                    .map_or(0, |v| v.lines().map(display_width).max().unwrap_or(0));
                max_name_width + 3 + val_len
            })
        })
        .max()
        .unwrap_or(max_name_width + 3);

    for (rec_idx, row) in rows.iter().enumerate() {
        // Record header: `-[ RECORD N ]--+---` — suppressed in tuples-only mode.
        if !cfg.tuples_only {
            write_expanded_header(out, rec_idx + 1, max_data_width, max_name_width);
        }

        for (i, col) in cols.iter().enumerate() {
            let val = row
                .get(i)
                .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                .unwrap_or_else(|| cfg.null_string.clone());

            let name_width = display_width(&col.name);
            let padding = max_name_width.saturating_sub(name_width);

            // Handle multiline values: psql prints each embedded newline as
            // a `+` continuation marker followed by a new ` key_pad | rest` line.
            let value_lines: Vec<&str> = val.split('\n').collect();
            let line_count = value_lines.len();
            for (li, line) in value_lines.iter().enumerate() {
                if li == 0 {
                    let _ = write!(out, "{}", col.name);
                    for _ in 0..padding {
                        out.push(' ');
                    }
                } else {
                    // Continuation: align with the key column (spaces) then ` | `
                    for _ in 0..max_name_width {
                        out.push(' ');
                    }
                }
                let is_last_line = li == line_count - 1;
                // If this is not the last segment, pad to max_data_width and
                // mark with `+` for psql compat.
                if is_last_line {
                    let _ = writeln!(out, " | {line}");
                } else {
                    let prefix_w = max_name_width + 3; // "name | " width
                    let line_w = display_width(line);
                    let used = prefix_w + line_w;
                    let pad = max_data_width.saturating_sub(used);
                    let _ = write!(out, " | {line}");
                    for _ in 0..pad {
                        out.push(' ');
                    }
                    out.push('+');
                    out.push('\n');
                }
            }
        }
    }
}

/// Write the `-[ RECORD N ]-...-` header line for expanded output.
///
/// `max_data_width` is the width of the widest data row
/// (`key_padded + " | " + value`). `max_name_width` is the widest field name.
///
/// psql places a `+` at the `|` position when the field-name column is wide
/// enough that the `|` position falls at or after the end of the prefix.
/// When the prefix is wider than the `|` position, only dashes are used.
fn write_expanded_header(
    out: &mut String,
    record_num: usize,
    max_data_width: usize,
    max_name_width: usize,
) {
    let prefix = format!("-[ RECORD {record_num} ]");
    let prefix_len = prefix.len();

    // The `|` in data rows is at column position max_name_width + 1 (0-indexed).
    // Only place `+` there if the prefix doesn't already extend past it.
    let pipe_pos = max_name_width + 1;
    let _ = write!(out, "{prefix}");

    if pipe_pos >= prefix_len {
        // Fill dashes up to the pipe position, then `+`, then remaining dashes.
        let dashes_before = pipe_pos - prefix_len;
        let dashes_after = max_data_width.saturating_sub(prefix_len + dashes_before + 1);
        for _ in 0..dashes_before {
            out.push('-');
        }
        out.push('+');
        for _ in 0..dashes_after {
            out.push('-');
        }
    } else {
        // Prefix already extends past the field-name column: just pad with dashes.
        let dashes_needed = max_data_width.saturating_sub(prefix_len);
        for _ in 0..dashes_needed {
            out.push('-');
        }
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
                    // psql suppresses "N objects in database X" summary lines
                    // that PostgreSQL appends to DROP ROLE DETAIL messages.
                    let filtered: Vec<&str> = detail
                        .lines()
                        .filter(|line| {
                            let t = line.trim();
                            // Keep line unless it looks like "N object(s) in database NAME"
                            !matches!(
                                t.split_once(' '),
                                Some((num, rest))
                                    if num.chars().all(|c| c.is_ascii_digit())
                                        && (rest.starts_with("object in database ")
                                            || rest.starts_with("objects in database "))
                            )
                        })
                        .collect();
                    if !filtered.is_empty() {
                        let _ = writeln!(out, "DETAIL:  {}", filtered.join("\n"));
                    }
                }
                if let Some(hint) = db_err.hint() {
                    let _ = writeln!(out, "HINT:  {hint}");
                }
            }

            // CONTEXT line (e.g. PL/pgSQL call stack).
            // psql shows this in default and verbose mode, but not in terse.
            if !cfg.terse_errors {
                if let Some(ctx) = db_err.where_() {
                    let _ = writeln!(out, "CONTEXT:  {ctx}");
                }
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
pub fn eprint_db_error(
    err: &tokio_postgres::Error,
    sql: Option<&str>,
    verbose: bool,
    terse: bool,
    sqlstate: bool,
) {
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
    let terse = TERSE_NOTICES.load(Ordering::Relaxed);
    if !terse {
        if let Some(detail) = notice.detail() {
            let _ = writeln!(out, "DETAIL:  {detail}");
        }
        if let Some(hint) = notice.hint() {
            let _ = writeln!(out, "HINT:  {hint}");
        }
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
    // Postgres reports `position` as a 1-based *character* offset into the
    // query string, not a byte offset.
    let char_offset = match pos {
        tokio_postgres::error::ErrorPosition::Original(n) => (*n as usize).saturating_sub(1),
        tokio_postgres::error::ErrorPosition::Internal { position, .. } => {
            (*position as usize).saturating_sub(1)
        }
    };

    // Convert character offset to byte offset for slicing.
    let byte_offset = sql
        .char_indices()
        .nth(char_offset)
        .map_or(sql.len(), |(idx, _)| idx);

    // Find which line the offset falls on and the column within that line.
    let before = &sql[..byte_offset];
    let line_num = before.chars().filter(|&c| c == '\n').count() + 1;

    let line_start = before.rfind('\n').map_or(0, |p| p + 1);
    let col_offset = before[line_start..].chars().count();

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
                use std::fmt::Write as _;
                let _ = write!(out, "\\x{:02X}", c as u32);
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
    escaped.split('\n').map(display_width).max().unwrap_or(0)
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
#[allow(dead_code)]
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

/// Render a [`RowSet`] in expanded unaligned mode (psql expanded+unaligned).
///
/// Each record is printed as `colname|value` pairs, separated by blank lines.
/// No header, no row count footer.
fn format_expanded_unaligned(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;
    let sep = &cfg.field_sep;

    for (rec_idx, row) in rows.iter().enumerate() {
        if rec_idx > 0 {
            out.push('\n');
        }
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            out.push_str(&col.name);
            out.push_str(sep);
            out.push_str(val);
            out.push('\n');
        }
    }
}

/// Render a [`RowSet`] in expanded CSV mode (psql expanded+csv).
///
/// Each record: colname,value pairs. No blank line between records.
/// No header, no row count footer.
fn format_expanded_csv(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;
    let sep = &cfg.csv_field_sep;

    for row in rows {
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            out.push_str(&csv_field_sep(&col.name, sep));
            out.push_str(sep);
            out.push_str(&csv_field_sep(val, sep));
            out.push('\n');
        }
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
    let sep = &cfg.csv_field_sep;

    if !cfg.tuples_only {
        let header: Vec<String> = cols.iter().map(|c| csv_field_sep(&c.name, sep)).collect();
        out.push_str(&header.join(sep.as_str()));
        out.push('\n');
    }

    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|v| csv_field_sep(v.as_deref().unwrap_or(&cfg.null_display), sep))
            .collect();
        out.push_str(&cells.join(sep.as_str()));
        out.push('\n');
    }
}

/// RFC 4180: wrap in double-quotes if the value contains the separator, `"`, `\n`, or `\r`.
fn csv_field_sep(val: &str, sep: &str) -> String {
    if val.contains(sep) || val.contains('"') || val.contains('\n') || val.contains('\r') {
        let escaped = val.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        val.to_owned()
    }
}

/// RFC 4180: wrap in double-quotes if the value contains `,`, `"`, `\n`, or `\r`.
#[allow(dead_code)]
fn csv_field(val: &str) -> String {
    csv_field_sep(val, ",")
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

    if cfg.expanded == ExpandedMode::On {
        format_html_expanded(out, rs, cfg);
        return;
    }

    // Use border pset value; append tableattr if set.
    let table_attrs = match &cfg.tableattr {
        Some(a) if !a.is_empty() => format!(" border=\"{}\" {a}", cfg.border),
        _ => format!(" border=\"{}\"", cfg.border),
    };
    let _ = writeln!(out, "<table{table_attrs}>");

    if let Some(ref title) = cfg.title {
        let _ = writeln!(out, "  <caption>{}</caption>", html_escape_attr(title));
    }

    if !cfg.tuples_only {
        out.push_str("  <tr>\n");
        for col in cols {
            out.push_str("    <th align=\"center\">");
            out.push_str(&html_escape_attr(&col.name));
            out.push_str("</th>\n");
        }
        out.push_str("  </tr>\n");
    }

    for row in rows {
        out.push_str("  <tr valign=\"top\">\n");
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(&cfg.null_display);
            let align = if col.is_numeric { "right" } else { "left" };
            out.push_str("    <td align=\"");
            out.push_str(align);
            out.push_str("\">");
            out.push_str(&html_escape(val));
            out.push_str("</td>\n");
        }
        out.push_str("  </tr>\n");
    }
    out.push_str("</table>\n");

    if !cfg.tuples_only && cfg.footer {
        let n = rows.len();
        let label = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "<p>({n} {label})<br />\n</p>");
    }
}

/// HTML output for expanded mode (key-value pairs).
fn format_html_expanded(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;

    let table_attrs = match &cfg.tableattr {
        Some(a) if !a.is_empty() => format!(" border=\"{}\" {a}", cfg.border),
        _ => format!(" border=\"{}\"", cfg.border),
    };
    let _ = writeln!(out, "<table{table_attrs}>");

    if let Some(ref title) = cfg.title {
        let _ = writeln!(out, "  <caption>{}</caption>", html_escape_attr(title));
    }

    for (rec_idx, row) in rows.iter().enumerate() {
        if rec_idx > 0 {
            // Empty separator row between records.
            out.push_str("  <tr><td colspan=\"2\">&nbsp;</td></tr>\n");
        }
        let record_num = rec_idx + 1;
        let _ = writeln!(
            out,
            "  <tr><td colspan=\"2\" align=\"center\">Record {record_num}</td></tr>"
        );
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(&cfg.null_display);
            let align = if col.is_numeric { "right" } else { "left" };
            out.push_str("  <tr valign=\"top\">\n");
            out.push_str("    <th>");
            out.push_str(&html_escape_attr(&col.name));
            out.push_str("</th>\n");
            out.push_str("    <td align=\"");
            out.push_str(align);
            out.push_str("\">");
            out.push_str(&html_escape(val));
            out.push_str("</td>\n");
            out.push_str("  </tr>\n");
        }
    }
    out.push_str("</table>\n");
}

/// HTML-escape a cell value: escape special chars, convert leading spaces to
/// `&nbsp;`, and convert newlines to `<br />\n` (matching psql behaviour).
///
/// Special case: if the string is entirely whitespace, psql outputs `&nbsp; `
/// (one nbsp + one space) regardless of how many spaces there are.
fn html_escape(s: &str) -> String {
    // If the entire cell is whitespace, use psql's fixed representation.
    if !s.is_empty() && s.chars().all(|c| c == ' ') {
        return "&nbsp; ".to_owned();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut leading = true; // still in leading-whitespace region
    for ch in s.chars() {
        match ch {
            ' ' if leading => out.push_str("&nbsp;"),
            '\n' => {
                out.push_str("<br />\n");
                leading = true; // reset leading after newline
            }
            '&' => {
                leading = false;
                out.push_str("&amp;");
            }
            '<' => {
                leading = false;
                out.push_str("&lt;");
            }
            '>' => {
                leading = false;
                out.push_str("&gt;");
            }
            '"' => {
                leading = false;
                out.push_str("&quot;");
            }
            c => {
                leading = false;
                out.push(c);
            }
        }
    }
    out
}

/// HTML-escape a non-cell string (title, caption): only escape special chars,
/// no leading-space or newline conversion.
fn html_escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// LaTeX / troff-ms / asciidoc formatters
// ---------------------------------------------------------------------------

/// Escape a string for LaTeX output.
fn latex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '$' => out.push_str("\\$"),
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '^' => out.push_str("\\^{}"),
            '~' => out.push_str("\\~{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            '|' => out.push_str("\\textbar{}"),
            c => out.push(c),
        }
    }
    out
}

/// Build the LaTeX column spec string (e.g. `r | l | l`).
fn latex_col_spec(cols: &[ColumnMeta], border: u8) -> String {
    let aligns: Vec<&str> = cols
        .iter()
        .map(|c| if c.is_numeric { "r" } else { "l" })
        .collect();
    match border {
        0 => aligns.join(""),
        2 => format!("| {} |", aligns.join(" | ")),
        _ => aligns.join(" | "),
    }
}

/// Render a [`RowSet`] in LaTeX tabular format.
pub fn format_latex(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    if cfg.expanded == ExpandedMode::On {
        format_latex_expanded(out, rs, cfg);
        return;
    }

    if let Some(ref title) = cfg.title {
        let _ = writeln!(out, "\\begin{{center}}\n{title}\n\\end{{center}}\n");
    }

    let col_spec = latex_col_spec(cols, cfg.border);
    let _ = writeln!(out, "\\begin{{tabular}}{{{col_spec}}}");

    if cfg.border == 2 {
        out.push_str("\\hline\n");
    }

    if !cfg.tuples_only {
        let header: Vec<String> = cols
            .iter()
            .map(|c| format!("\\textit{{{}}}", latex_escape(&c.name)))
            .collect();
        let _ = writeln!(out, "{} \\\\", header.join(" & "));
        out.push_str("\\hline\n");
    }

    for row in rows {
        let cells: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let val = row
                    .get(i)
                    .and_then(|v| v.as_deref())
                    .unwrap_or(null_str.as_str());
                latex_escape(val)
            })
            .collect();
        let _ = writeln!(out, "{} \\\\", cells.join(" & "));
        if cfg.border == 2 {
            out.push_str("\\hline\n");
        }
    }

    out.push_str("\\end{tabular}\n");

    if cfg.tuples_only {
        out.push_str("\n\\noindent\n");
    } else {
        let n = rows.len();
        let label = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "\n\\noindent ({n} {label}) \\\\");
    }
}

/// Render a [`RowSet`] in LaTeX tabular format with expanded mode.
fn format_latex_expanded(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    for (rec_idx, row) in rows.iter().enumerate() {
        if rec_idx > 0 {
            out.push('\n');
        }
        let record_num = rec_idx + 1;
        out.push_str("\\begin{tabular}{c|l}\n");
        let _ = writeln!(
            out,
            "\\multicolumn{{2}}{{c}}{{\\textit{{Record {record_num}}}}} \\\\"
        );
        out.push_str("\\hline\n");
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let _ = writeln!(
                out,
                "{} & {} \\\\",
                latex_escape(&col.name),
                latex_escape(val)
            );
        }
        out.push_str("\\end{tabular}\n");
    }

    out.push_str("\n\\noindent\n");
}

/// Render a [`RowSet`] in LaTeX longtable format.
pub fn format_latex_longtable(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    if cfg.expanded == ExpandedMode::On {
        // Fall back to regular latex expanded
        format_latex_expanded(out, rs, cfg);
        return;
    }

    let col_spec = latex_col_spec(cols, cfg.border);
    let _ = writeln!(out, "\\begin{{longtable}}{{{col_spec}}}");

    if !cfg.tuples_only {
        let header: Vec<String> = cols
            .iter()
            .map(|c| format!("\\small\\textbf{{\\textit{{{}}}}}", latex_escape(&c.name)))
            .collect();
        let _ = writeln!(out, "{} \\\\", header.join(" & "));
        out.push_str("\\midrule\n\\endfirsthead\n");
        let header2: Vec<String> = cols
            .iter()
            .map(|c| format!("\\small\\textbf{{\\textit{{{}}}}}", latex_escape(&c.name)))
            .collect();
        let _ = writeln!(out, "{} \\\\", header2.join(" & "));
        out.push_str("\\midrule\n\\endhead\n");
    }

    for row in rows {
        for (col_idx, col) in cols.iter().enumerate() {
            if col_idx > 0 {
                out.push_str("\n&\n");
            }
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let align = if col.is_numeric {
                "\\raggedleft{"
            } else {
                "\\raggedright{"
            };
            let _ = write!(out, "{align}{}", latex_escape(val));
            out.push('}');
        }
        out.push_str(" \\tabularnewline\n");
    }

    out.push_str("\\end{longtable}\n");
}

/// Render a [`RowSet`] in troff-ms table format.
pub fn format_troff_ms(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    if cfg.expanded == ExpandedMode::On {
        format_troff_ms_expanded(out, rs, cfg);
        return;
    }

    out.push_str(".LP\n.TS\ncenter;\n");
    // Column alignment spec
    let aligns: Vec<&str> = cols
        .iter()
        .map(|c| if c.is_numeric { "r" } else { "l" })
        .collect();
    let sep = match cfg.border {
        0 => " ".to_owned(),
        _ => " | ".to_owned(),
    };
    let _ = writeln!(out, "{}.", aligns.join(&sep));

    if !cfg.tuples_only {
        let header: Vec<String> = cols.iter().map(|c| format!("\\fI{}\\fP", c.name)).collect();
        out.push_str(&header.join("\t"));
        out.push('\n');
        out.push('_');
        out.push('\n');
    }

    for row in rows {
        let cells: Vec<&str> = cols
            .iter()
            .enumerate()
            .map(|(i, _)| {
                row.get(i)
                    .and_then(|v| v.as_deref())
                    .unwrap_or(null_str.as_str())
            })
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }

    out.push_str(".TE\n");

    if !cfg.tuples_only && cfg.footer {
        let n = rows.len();
        let label = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, ".DS L\n({n} {label})\n.DE");
    }
}

/// Render a [`RowSet`] in troff-ms expanded format.
fn format_troff_ms_expanded(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    for (rec_idx, row) in rows.iter().enumerate() {
        if rec_idx > 0 {
            out.push('\n');
        }
        let record_num = rec_idx + 1;
        out.push_str(".LP\n.TS\ncenter;\nl | l.\n");
        let _ = writeln!(out, "\\fBRecord {record_num}\\fP\t");
        out.push('_');
        out.push('\n');
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let _ = writeln!(out, "{}\t{}", col.name, val);
        }
        out.push_str(".TE\n");
    }
}

/// Escape a string for `AsciiDoc` table output.
fn asciidoc_escape(s: &str) -> String {
    s.replace('|', "\\|")
}

/// Render a [`RowSet`] in `AsciiDoc` table format.
pub fn format_asciidoc(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    if cfg.expanded == ExpandedMode::On {
        format_asciidoc_expanded(out, rs, cfg);
        return;
    }

    // Column spec: `h` for header column, `l` for left, `r` for right.
    let frame = match cfg.border {
        0 => "none",
        1 => "none",
        _ => "all",
    };
    let grid = match cfg.border {
        0 => "none",
        _ => "rows",
    };
    let col_spec: Vec<String> = cols
        .iter()
        .map(|c| {
            let align = if c.is_numeric { ">l" } else { "<l" };
            align.to_string()
        })
        .collect();
    let _ = writeln!(
        out,
        "[cols=\"{}\",frame=\"{frame}\",grid=\"{grid}\"]",
        col_spec.join(",")
    );
    out.push_str("|====\n");

    if !cfg.tuples_only {
        for col in cols {
            let _ = write!(out, "^l|{}", asciidoc_escape(&col.name));
            out.push(' ');
        }
        out.push('\n');
    }

    for row in rows {
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let align = if col.is_numeric { ">l" } else { "<l" };
            let _ = write!(out, "{align}|{} ", asciidoc_escape(val));
        }
        out.push('\n');
    }

    out.push_str("|====\n");

    if !cfg.tuples_only && cfg.footer {
        let n = rows.len();
        let label = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "\n....\n({n} {label})\n....");
    }
}

/// Render a [`RowSet`] in `AsciiDoc` expanded format.
fn format_asciidoc_expanded(out: &mut String, rs: &RowSet, cfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let null_str = &cfg.null_display;

    for (rec_idx, row) in rows.iter().enumerate() {
        if rec_idx > 0 {
            out.push('\n');
        }
        let record_num = rec_idx + 1;
        out.push_str("[cols=\"h,l\",frame=\"none\",grid=\"none\"]\n|====\n");
        let _ = writeln!(out, "2+^|Record {record_num}");
        for (col_idx, col) in cols.iter().enumerate() {
            let val = row
                .get(col_idx)
                .and_then(|v| v.as_deref())
                .unwrap_or(null_str.as_str());
            let align = if col.is_numeric { ">l" } else { "<l" };
            let _ = writeln!(
                out,
                "<l|{} {align}|{}",
                asciidoc_escape(&col.name),
                asciidoc_escape(val)
            );
        }
        out.push_str("|====\n");
    }
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
// Wrapped format
// ---------------------------------------------------------------------------

fn total_line_width(widths: &[usize], border: u8) -> usize {
    let n = widths.len();
    if n == 0 {
        return 0;
    }
    let sum: usize = widths.iter().sum();
    match border {
        0 => sum + 2 * (n - 1), // `w0  w1  w2`
        2 => sum + 3 * n + 1,   // `| w0 | w1 | w2 |`
        _ => sum + 3 * n - 1,   // ` w0 | w1 | w2 ` (border 1)
    }
}

/// Shrink column widths so the total line fits within `target_width`.
///
/// Uses the same heuristic as psql: repeatedly shrink the column with the
/// highest ratio of current-width / average-width (with a slight bias toward
/// wider columns).  Columns cannot go below their header width.
///
/// - `widths`: mutable column widths (start at natural max).
/// - `width_header`: minimum per-column width (from header display width).
/// - `width_average`: average data cell width per column.
/// - `max_width`: original natural (max) widths (for the width bias term).
fn shrink_widths(
    widths: &mut [usize],
    width_header: &[usize],
    width_average: &[usize],
    max_width: &[usize],
    target_width: usize,
    border: u8,
) {
    while total_line_width(widths, border) > target_width {
        let mut max_ratio: f64 = 0.0;
        let mut worst_col: Option<usize> = None;

        for i in 0..widths.len() {
            if width_average[i] > 0 && widths[i] > width_header[i] {
                #[allow(clippy::cast_precision_loss)]
                let ratio = widths[i] as f64 / width_average[i] as f64 + max_width[i] as f64 * 0.01;
                if ratio > max_ratio {
                    max_ratio = ratio;
                    worst_col = Some(i);
                }
            }
        }

        match worst_col {
            Some(col) => widths[col] -= 1,
            None => break, // cannot shrink any further
        }
    }
}

/// Compute the average display width of data cells per column.
///
/// For multi-line cell values, the "display width" is the maximum display width
/// of any single embedded-newline-delimited line within the cell (matching how
/// psql computes `pg_wcssize` widths).
fn compute_width_average(
    cols: &[ColumnMeta],
    rows: &[Vec<Option<String>>],
    null_str: &str,
) -> Vec<usize> {
    let n = cols.len();
    if rows.is_empty() {
        return vec![0; n];
    }

    let mut sums = vec![0usize; n];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= n {
                break;
            }
            let cell_str = cell.as_deref().unwrap_or(null_str);
            // psql uses pg_wcssize which returns the max line width within the cell.
            let w = cell_str
                .split('\n')
                .map(display_width)
                .max()
                .unwrap_or(0);
            sums[i] += w;
        }
    }

    sums.iter().map(|s| s / rows.len()).collect()
}

/// Compute the header display width per column.
///
/// For headers with embedded newlines, this is the maximum width of any
/// single line within the header text.
fn compute_width_header(cols: &[ColumnMeta]) -> Vec<usize> {
    cols.iter()
        .map(|c| {
            c.name
                .split('\n')
                .map(display_width)
                .max()
                .unwrap_or(0)
        })
        .collect()
}

/// Calculate per-column display widths where multi-line cell values use the
/// maximum single-line width (matching psql's `pg_wcssize` behavior).
///
/// This is the correct width calculation for wrapped format and for aligned
/// format when cells contain embedded newlines.
fn column_widths_max_line(
    cols: &[ColumnMeta],
    rows: &[Vec<Option<String>>],
    null_str: &str,
) -> Vec<usize> {
    // Header widths: max line width within each header.
    let mut widths: Vec<usize> = cols
        .iter()
        .map(|c| {
            c.name
                .split('\n')
                .map(display_width)
                .max()
                .unwrap_or(0)
        })
        .collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let cell_str = cell.as_deref().unwrap_or(null_str);
            let w = cell_str
                .split('\n')
                .map(display_width)
                .max()
                .unwrap_or(0);
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    widths
}

/// A visual line fragment for one cell in wrapped output.
#[derive(Debug)]
struct CellLine {
    /// The text content for this line (may be shorter than the column width).
    text: String,
    /// `true` if this line comes from an embedded newline in the cell (show `+`).
    has_newline: bool,
    /// `true` if this line wraps to the next (show `.` at end/start).
    wraps_to_next: bool,
    /// `true` if this line is a continuation from a wrap on the previous line.
    continued_from_wrap: bool,
}

/// Split a cell value into visual lines based on the column width.
///
/// Embedded newlines produce lines with `has_newline = true`.
/// Lines that exceed `col_width` are hard-wrapped: the first part gets
/// `wraps_to_next = true` and the continuation gets `continued_from_wrap = true`.
fn split_cell_lines(value: &str, col_width: usize) -> Vec<CellLine> {
    let mut result = Vec::new();
    // First, split by embedded newlines (these produce the `+` marker in psql).
    let nl_parts: Vec<&str> = value.split('\n').collect();
    let num_nl_parts = nl_parts.len();

    for (nl_idx, part) in nl_parts.iter().enumerate() {
        let has_newline = nl_idx + 1 < num_nl_parts; // more newline-parts follow

        if col_width == 0 || display_width(part) <= col_width {
            result.push(CellLine {
                text: (*part).to_owned(),
                has_newline,
                wraps_to_next: false,
                continued_from_wrap: false,
            });
        } else {
            // Need to hard-wrap this part.
            let chars: Vec<char> = part.chars().collect();
            let mut pos = 0;
            let mut first_chunk = true;
            while pos < chars.len() {
                // Take up to `col_width` display-width characters.
                let mut end = pos;
                let mut w = 0;
                while end < chars.len() {
                    let cw = unicode_width::UnicodeWidthChar::width(chars[end]).unwrap_or(0);
                    if w + cw > col_width {
                        break;
                    }
                    w += cw;
                    end += 1;
                }
                if end == pos && end < chars.len() {
                    // Character wider than column; include at least one.
                    end += 1;
                }
                let chunk: String = chars[pos..end].iter().collect();
                let more_chunks = end < chars.len();
                result.push(CellLine {
                    text: chunk,
                    has_newline: if more_chunks { false } else { has_newline },
                    wraps_to_next: more_chunks,
                    continued_from_wrap: !first_chunk,
                });
                first_chunk = false;
                pos = end;
            }
            if pos == 0 {
                // Empty part from newline.
                result.push(CellLine {
                    text: String::new(),
                    has_newline,
                    wraps_to_next: false,
                    continued_from_wrap: false,
                });
            }
        }
    }

    if result.is_empty() {
        result.push(CellLine {
            text: String::new(),
            has_newline: false,
            wraps_to_next: false,
            continued_from_wrap: false,
        });
    }

    result
}

/// Render a [`RowSet`] in wrapped format, honouring `PsetConfig` for border,
/// tuples-only, footer, null display, and column-width target.
fn format_wrapped_pset(out: &mut String, rs: &RowSet, pcfg: &PsetConfig) {
    let cols = &rs.columns;
    let rows = &rs.rows;
    let border = pcfg.border;
    let null_str = &pcfg.null_display;

    if cols.is_empty() {
        if !pcfg.tuples_only {
            out.push_str("--\n");
            if pcfg.footer {
                write_row_count(out, rows.len());
            }
        }
        return;
    }

    let natural_widths = column_widths_max_line(cols, rows, null_str);
    let mut widths = natural_widths.clone();
    let target = pcfg.columns;

    // Compute header and average widths for the shrinking heuristic.
    let width_header = compute_width_header(cols);
    let width_average = compute_width_average(cols, rows, null_str);

    // Compute total header width (overhead + header widths) to check feasibility.
    let total_header_width = {
        let overhead = match border {
            0 => cols.len(),
            2 => cols.len() * 3 + 1,
            _ => cols
                .len()
                .saturating_mul(3)
                .saturating_sub(usize::from(!cols.is_empty())),
        };
        overhead + width_header.iter().sum::<usize>()
    };

    // Shrink columns if target width is set and the table is too wide.
    // Only shrink if the target is at least as wide as the total header width.
    if target > 0 && total_line_width(&widths, border) > target && target >= total_header_width {
        shrink_widths(
            &mut widths,
            &width_header,
            &width_average,
            &natural_widths,
            target,
            border,
        );
    }

    // border 2: top border line.
    if border == 2 && !pcfg.tuples_only {
        write_separator_border(out, &widths, border);
    }

    // Header (suppressed in tuples-only mode).
    if !pcfg.tuples_only {
        // Headers can also have embedded newlines (e.g. the test uses "ab\n\nc").
        write_wrapped_row(out, cols, &widths, border, |col, _| col.name.clone(), true);
        write_separator_border(out, &widths, border);
    }

    // Data rows.
    let null_rendered = if !pcfg.no_highlight && !null_str.is_empty() {
        format!("\x1b[2m{null_str}\x1b[0m")
    } else {
        null_str.to_owned()
    };
    for row in rows {
        let null = null_rendered.clone();
        write_wrapped_row(
            out,
            cols,
            &widths,
            border,
            |_col, cell_idx| {
                row.get(cell_idx)
                    .and_then(|v| v.as_deref().map(ToOwned::to_owned))
                    .unwrap_or_else(|| null.clone())
            },
            false,
        );
    }

    // border 2: bottom border line.
    if border == 2 {
        write_separator_border(out, &widths, border);
    }

    // Footer.
    if !pcfg.tuples_only && pcfg.footer {
        write_row_count(out, rows.len());
    }
}

/// Write one (potentially multi-line) row in wrapped format.
///
/// Each cell value is split into visual lines.  All columns are padded to the
/// same number of visual lines.  The `+` marker indicates an embedded newline
/// and `.` at end/start indicates a hard wrap.
///
/// The output structure for each visual line is:
///
/// **border 0:** `content₀marker₀ content₁marker₁`
///   - Between columns: marker + space (or marker + `.` for continuation).
///   - Last column: just marker (which may be space, `+`, or `.`).
///
/// **border 1:** `leading₀content₀marker₀|leading₁content₁marker₁`
///   - `leading` = ` ` normally, `.` for wrap continuation.
///   - `marker` = ` ` normally, `+` for newline, `.` for wrap.
///
/// **border 2:** `|leading₀content₀marker₀|leading₁content₁marker₁|`
///   - Same as border 1 but with `|` prefix and suffix.
#[allow(clippy::too_many_lines)]
fn write_wrapped_row<F>(
    out: &mut String,
    cols: &[ColumnMeta],
    widths: &[usize],
    border: u8,
    value_fn: F,
    is_header: bool,
) where
    F: Fn(&ColumnMeta, usize) -> String,
{
    // Split each cell into visual lines.
    let mut all_lines: Vec<Vec<CellLine>> = Vec::with_capacity(cols.len());
    let mut max_lines = 0;
    for (i, col) in cols.iter().enumerate() {
        let val = value_fn(col, i);
        let lines = split_cell_lines(&val, widths[i]);
        if lines.len() > max_lines {
            max_lines = lines.len();
        }
        all_lines.push(lines);
    }

    // Render each visual line.
    for line_idx in 0..max_lines {
        for (col_idx, col) in cols.iter().enumerate() {
            let w = widths[col_idx];
            let cell_lines = &all_lines[col_idx];
            let is_last_col = col_idx + 1 == cols.len();

            let (text, has_newline, wraps_to_next, continued_from_wrap) =
                if line_idx < cell_lines.len() {
                    let cl = &cell_lines[line_idx];
                    (
                        cl.text.as_str(),
                        cl.has_newline,
                        cl.wraps_to_next,
                        cl.continued_from_wrap,
                    )
                } else {
                    ("", false, false, false)
                };

            let text_width = display_width(text);
            let padding = w.saturating_sub(text_width);

            // Whether to pad with trailing spaces (psql: finalspaces).
            //
            // Headers: border >= 1 always pads; border 0 always pads because
            // wrap_right_border=true for ascii linestyle (the trailing marker
            // is always written as the inter-column separator).
            //
            // Data: always pad for border 2; for border 0/1 pad all columns
            // except the last one.  Exception: also pad the last column if
            // the next visual line needs a wrap/newline marker.
            let has_marker = has_newline || wraps_to_next;
            let final_spaces = if is_header {
                true // always pad headers (wrap_right_border=true for ascii)
            } else {
                border == 2 || !is_last_col || has_marker
            };

            // --- Leading ---
            // For border 0: the trailing marker of the previous column serves
            // as the inter-column separator, so there is NO separate leading
            // character for col_idx > 0.  For col_idx == 0, no leading at all.
            //
            // For border 1/2: each column has a leading space (or `.` for
            // wrap continuation), plus `|` separators between columns.
            match border {
                0 => {
                    // No leading in border 0 — handled by previous col's trailing.
                }
                2 => {
                    out.push('|');
                    if continued_from_wrap {
                        out.push('.');
                    } else {
                        out.push(' ');
                    }
                }
                _ => {
                    if col_idx > 0 {
                        out.push('|');
                    }
                    if continued_from_wrap {
                        out.push('.');
                    } else {
                        out.push(' ');
                    }
                }
            }

            // --- Content ---
            if col.is_numeric && !is_header {
                // Right-aligned: spaces before content.
                if final_spaces {
                    for _ in 0..padding {
                        out.push(' ');
                    }
                }
                out.push_str(text);
            } else if is_header && !col.is_numeric {
                // Center-aligned header.
                let left_pad = padding / 2;
                let right_pad = padding - left_pad;
                for _ in 0..left_pad {
                    out.push(' ');
                }
                out.push_str(text);
                if final_spaces {
                    for _ in 0..right_pad {
                        out.push(' ');
                    }
                }
            } else if is_header && col.is_numeric {
                // Right-aligned header.
                if final_spaces {
                    for _ in 0..padding {
                        out.push(' ');
                    }
                }
                out.push_str(text);
            } else {
                // Left-aligned data: content then spaces.
                out.push_str(text);
                if final_spaces {
                    for _ in 0..padding {
                        out.push(' ');
                    }
                }
            }

            // --- Trailing marker ---
            // Write `+` (newline), `.` (wrap), or ` ` (normal).
            // For the last column in border 0/1, only write if it's a
            // meaningful marker (not a plain space).
            if has_newline {
                out.push('+');
            } else if wraps_to_next {
                out.push('.');
            } else if final_spaces {
                out.push(' ');
            }
        }

        // Row-end border.
        if border == 2 {
            out.push('|');
        }
        out.push('\n');
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
        assert!(out.contains("<table"), "missing <table: {out}");
        assert!(out.contains("a</th>"), "missing a</th>: {out}");
        assert!(out.contains(">1<"), "missing >1<: {out}");
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
        assert!(!out.contains("<th"), "th header must be suppressed: {out}");
        assert!(out.contains("<td"), "data must be present: {out}");
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

    // -----------------------------------------------------------------------
    // write_error_position — non-ASCII caret placement
    // -----------------------------------------------------------------------

    #[test]
    fn test_error_position_non_ascii() {
        // PostgreSQL reports position as 1-based *character* offset.
        // "SELECT * FROM «тест»" — error at char 16 (the 'т' in тест).
        // Characters: S(1) E(2) L(3) E(4) C(5) T(6) (7) *(8) (9) F(10) R(11) O(12) M(13) (14) «(15) т(16)
        let sql = "SELECT * FROM «тест»";
        let pos = tokio_postgres::error::ErrorPosition::Original(16);
        let mut out = String::new();
        write_error_position(&mut out, sql, &pos);
        // The caret should point at char 16 (т), not at byte 16 (which is
        // inside the multi-byte « character).
        assert!(out.contains("LINE 1:"), "should have LINE 1: prefix: {out}");
        // Count spaces before ^ in the caret line.
        let caret_line = out.lines().nth(1).expect("should have caret line");
        let caret_col = caret_line.find('^').expect("should have ^ marker");
        // "LINE 1: " prefix is 8 chars, then 15 chars before position 16
        let expected_col = "LINE 1: ".len() + 15;
        assert_eq!(
            caret_col, expected_col,
            "caret at wrong position: expected {expected_col}, got {caret_col}\nfull output:\n{out}"
        );
    }
}
