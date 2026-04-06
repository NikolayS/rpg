//! `\crosstabview` — pivot a query result into a cross-tabulation table.
//!
//! # Syntax
//!
//! ```text
//! \crosstabview [colV [colH [colD [sortcolH]]]]
//! ```
//!
//! - `colV`     — column whose distinct values form the row labels (default: 1).
//! - `colH`     — column whose distinct values form the column headers (default: 2).
//! - `colD`     — column whose values populate the cells (default: 3).
//! - `sortcolH` — column used to sort the horizontal headers (optional).
//!
//! Column arguments may be specified as 1-based index numbers or as column
//! names.  The query must return at least 3 columns.  Each `(colV, colH)` pair
//! must be unique; a duplicate is a fatal error.
//!
//! Output format mirrors psql: an aligned table whose first column contains the
//! row-label values and subsequent columns correspond to each distinct `colH`
//! value, with the matching `colD` value (or empty string) in each cell.

use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Column-specification argument: either a name or a 1-based index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColSpec {
    /// Column identified by its header name.
    Name(String),
    /// Column identified by 1-based position (matches psql convention).
    Index(usize),
}

impl ColSpec {
    /// Parse a string as either a 1-based index (all digits, >= 1) or a name.
    ///
    /// The value `"0"` is treated as a name (matching psql: column numbers
    /// start at 1).
    fn from_str(s: &str) -> Self {
        if let Ok(n) = s.parse::<usize>() {
            if n >= 1 {
                return Self::Index(n);
            }
        }
        Self::Name(s.to_owned())
    }

    /// Resolve to a zero-based column index given the header list.
    ///
    /// The stored index is 1-based; this converts to zero-based internally.
    ///
    /// Returns `Err` with an informative message if the column is not found or
    /// the index is out of range.
    fn resolve(&self, headers: &[String]) -> Result<usize, String> {
        match self {
            Self::Index(n) => {
                // n is 1-based; convert to zero-based.
                let zero = n - 1;
                if zero < headers.len() {
                    Ok(zero)
                } else {
                    Err(format!(
                        "\\crosstabview: column number {} is out of range \
                         (query has {} columns)",
                        n,
                        headers.len()
                    ))
                }
            }
            Self::Name(name) => headers.iter().position(|h| h == name).ok_or_else(|| {
                format!("\\crosstabview: column \"{name}\" not found in query result")
            }),
        }
    }
}

/// Arguments parsed from `\crosstabview [colV [colH [colD [sortcolH]]]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrosstabArgs {
    /// Row-label column (default: column 0).
    pub col_v: Option<ColSpec>,
    /// Column-header column (default: column 1).
    pub col_h: Option<ColSpec>,
    /// Cell-data column (default: column 2).
    pub col_d: Option<ColSpec>,
    /// Sort-order column for horizontal headers (optional).
    pub sort_col_h: Option<ColSpec>,
}

/// Parse the argument string from `\crosstabview [args…]`.
///
/// Arguments are whitespace-separated tokens.  Excess tokens beyond the
/// fourth are silently ignored (matching psql behaviour).
pub fn parse_args(raw: &str) -> CrosstabArgs {
    let mut args = CrosstabArgs::default();
    let mut tokens = raw.split_whitespace();

    if let Some(t) = tokens.next() {
        args.col_v = Some(ColSpec::from_str(t));
    }
    if let Some(t) = tokens.next() {
        args.col_h = Some(ColSpec::from_str(t));
    }
    if let Some(t) = tokens.next() {
        args.col_d = Some(ColSpec::from_str(t));
    }
    if let Some(t) = tokens.next() {
        args.sort_col_h = Some(ColSpec::from_str(t));
    }

    args
}

// ---------------------------------------------------------------------------
// Pivot logic
// ---------------------------------------------------------------------------

/// Pivot `rows` (described by `headers`) into a cross-tabulation table.
///
/// # Arguments
///
/// * `headers`   — column names returned by the query.
/// * `rows`      — each row is a `Vec<String>` of cell values in header order.
/// * `args`      — the parsed `\crosstabview` column specifications.
///
/// # Returns
///
/// On success, returns a `(pivot_headers, pivot_rows)` pair where
/// `pivot_headers` is the header line for the output table and `pivot_rows`
/// is a `Vec<Vec<String>>` of cell values ready for aligned printing.
///
/// On error, returns a human-readable error string.
#[allow(clippy::too_many_lines)]
pub fn pivot(
    headers: &[String],
    rows: &[Vec<String>],
    args: &CrosstabArgs,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    if headers.len() < 3 {
        return Err(format!(
            "\\crosstabview: query must return at least 3 columns \
             (got {})",
            headers.len()
        ));
    }

    // Resolve column indices (defaults: 0, 1, 2).
    let idx_v = args.col_v.as_ref().map_or(Ok(0), |s| s.resolve(headers))?;
    let idx_h = args.col_h.as_ref().map_or(Ok(1), |s| s.resolve(headers))?;
    let idx_d = args.col_d.as_ref().map_or(Ok(2), |s| s.resolve(headers))?;

    // Validate: colV / colH / colD must all be distinct.
    if idx_v == idx_h {
        return Err("\\crosstabview: column for row headers (colV) must be \
             different from column for column headers (colH)"
            .to_owned());
    }
    if idx_v == idx_d {
        return Err("\\crosstabview: column for row headers (colV) must be \
             different from data column (colD)"
            .to_owned());
    }
    if idx_h == idx_d {
        return Err("\\crosstabview: column for column headers (colH) must be \
             different from data column (colD)"
            .to_owned());
    }

    // Collect distinct colH values in encounter order; then optionally sort.
    let mut col_headers: Vec<String> = Vec::new();
    for row in rows {
        let h_val = row.get(idx_h).cloned().unwrap_or_default();
        if !col_headers.contains(&h_val) {
            col_headers.push(h_val);
        }
    }

    // Apply sortcolH: if specified, sort col_headers by the value of that
    // column in the first row where the colH value appears.
    if let Some(ref sort_spec) = args.sort_col_h {
        let sort_idx = sort_spec.resolve(headers)?;
        // Build a map: colH value → sort key string (first occurrence).
        let mut sort_key: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for row in rows {
            let h_val = row.get(idx_h).cloned().unwrap_or_default();
            sort_key
                .entry(h_val)
                .or_insert_with(|| row.get(sort_idx).cloned().unwrap_or_default());
        }
        col_headers.sort_by(|a, b| {
            let ka = sort_key.get(a).map_or("", String::as_str);
            let kb = sort_key.get(b).map_or("", String::as_str);
            ka.cmp(kb)
        });
    }

    // Build a map: (row_label, col_header) → cell value.
    // Detect duplicate (colV, colH) pairs.
    let mut cell_map: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for row in rows {
        let v_val = row.get(idx_v).cloned().unwrap_or_default();
        let h_val = row.get(idx_h).cloned().unwrap_or_default();
        let d_val = row.get(idx_d).cloned().unwrap_or_default();
        let key = (v_val.clone(), h_val.clone());
        if cell_map.contains_key(&key) {
            return Err(format!(
                "\\crosstabview: duplicate value in column \"{}\" for \
                 row \"{}\", column \"{}\"",
                headers[idx_h], v_val, h_val,
            ));
        }
        cell_map.insert(key, d_val);
    }

    // Collect distinct row labels in encounter order.
    let mut row_labels: Vec<String> = Vec::new();
    for row in rows {
        let v_val = row.get(idx_v).cloned().unwrap_or_default();
        if !row_labels.contains(&v_val) {
            row_labels.push(v_val);
        }
    }

    // Build pivot headers: first column is the colV header name, rest are
    // the distinct colH values.
    let mut pivot_headers = Vec::with_capacity(1 + col_headers.len());
    pivot_headers.push(headers[idx_v].clone());
    pivot_headers.extend(col_headers.iter().cloned());

    // Build pivot rows.
    let pivot_rows: Vec<Vec<String>> = row_labels
        .iter()
        .map(|label| {
            let mut row = Vec::with_capacity(1 + col_headers.len());
            row.push(label.clone());
            for ch in &col_headers {
                let cell = cell_map
                    .get(&(label.clone(), ch.clone()))
                    .cloned()
                    .unwrap_or_default();
                row.push(cell);
            }
            row
        })
        .collect();

    Ok((pivot_headers, pivot_rows))
}

// ---------------------------------------------------------------------------
// Aligned rendering
// ---------------------------------------------------------------------------

/// Render a pivot table as an aligned psql-style table.
///
/// `pivot_headers` is the first row (column names).
/// `pivot_rows` is the data rows.
///
/// The output is appended to `out`.
pub fn format_pivot(out: &mut String, pivot_headers: &[String], pivot_rows: &[Vec<String>]) {
    use std::fmt::Write as _;

    let ncols = pivot_headers.len();

    // Compute column widths: max of header width and all cell widths.
    let mut widths: Vec<usize> = pivot_headers.iter().map(|h| h.width()).collect();

    for row in pivot_rows {
        for (col_idx, cell) in row.iter().enumerate() {
            if col_idx < ncols {
                let w = cell.width();
                if w > widths[col_idx] {
                    widths[col_idx] = w;
                }
            }
        }
    }

    // Header line.
    let header_line = build_row(pivot_headers, &widths);
    let _ = writeln!(out, "{header_line}");

    // Separator line: `-{dash}-+-{dash}-+-...`
    let sep = build_separator(&widths);
    let _ = writeln!(out, "{sep}");

    // Data rows.
    for row in pivot_rows {
        // Pad row to ncols if needed.
        let mut cells: Vec<String> = row.clone();
        while cells.len() < ncols {
            cells.push(String::new());
        }
        let line = build_row(&cells, &widths);
        let _ = writeln!(out, "{line}");
    }

    // Footer: row count.
    let n = pivot_rows.len();
    if n == 1 {
        let _ = writeln!(out, "(1 row)");
    } else {
        let _ = writeln!(out, "({n} rows)");
    }
}

/// Build a separator line like `---------+--------+-...`.
fn build_separator(widths: &[usize]) -> String {
    widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            let dashes = "-".repeat(w + 2); // 1 space padding each side
            if i + 1 < widths.len() {
                format!("{dashes}+")
            } else {
                dashes
            }
        })
        .collect()
}

/// Build a data row: ` cell1 | cell2 | cell3`.
///
/// The first column is left-aligned; all others are also left-aligned
/// (psql `\crosstabview` output uses left alignment for all columns).
fn build_row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths.iter())
        .enumerate()
        .map(|(i, (cell, &w))| {
            // Pad to visual width using spaces.
            let cell_w = cell.width();
            let padding = w.saturating_sub(cell_w);
            let padded = format!(" {cell}{} ", " ".repeat(padding));
            if i + 1 < cells.len() {
                format!("{padded}|")
            } else {
                padded
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> Vec<String> {
        vec!["row".to_owned(), "col".to_owned(), "val".to_owned()]
    }

    fn simple_rows() -> Vec<Vec<String>> {
        vec![
            vec!["a".to_owned(), "x".to_owned(), "1".to_owned()],
            vec!["a".to_owned(), "y".to_owned(), "2".to_owned()],
            vec!["b".to_owned(), "x".to_owned(), "3".to_owned()],
            vec!["b".to_owned(), "y".to_owned(), "4".to_owned()],
        ]
    }

    // -- parse_args ----------------------------------------------------------

    #[test]
    fn parse_args_empty() {
        let a = parse_args("");
        assert!(a.col_v.is_none());
        assert!(a.col_h.is_none());
        assert!(a.col_d.is_none());
        assert!(a.sort_col_h.is_none());
    }

    #[test]
    fn parse_args_index() {
        // 1-based column numbers: "1 2 3" refers to columns 1, 2, 3.
        let a = parse_args("1 2 3");
        assert_eq!(a.col_v, Some(ColSpec::Index(1)));
        assert_eq!(a.col_h, Some(ColSpec::Index(2)));
        assert_eq!(a.col_d, Some(ColSpec::Index(3)));
        assert!(a.sort_col_h.is_none());
    }

    #[test]
    fn parse_args_name() {
        let a = parse_args("row col val");
        assert_eq!(a.col_v, Some(ColSpec::Name("row".to_owned())));
        assert_eq!(a.col_h, Some(ColSpec::Name("col".to_owned())));
        assert_eq!(a.col_d, Some(ColSpec::Name("val".to_owned())));
        assert!(a.sort_col_h.is_none());
    }

    #[test]
    fn parse_args_sort() {
        let a = parse_args("row col val col");
        assert_eq!(a.sort_col_h, Some(ColSpec::Name("col".to_owned())));
    }

    #[test]
    fn parse_args_excess_tokens_ignored() {
        // More than 4 tokens: extras are silently dropped.
        // Columns are 1-based so "1 2 3 4" selects columns 1, 2, 3, 4.
        let a = parse_args("1 2 3 4 5 6");
        assert_eq!(a.col_v, Some(ColSpec::Index(1)));
        assert_eq!(a.col_d, Some(ColSpec::Index(3)));
        assert_eq!(a.sort_col_h, Some(ColSpec::Index(4)));
    }

    // -- pivot ---------------------------------------------------------------

    #[test]
    fn pivot_basic() {
        let (ph, pr) = pivot(&headers(), &simple_rows(), &CrosstabArgs::default()).unwrap();
        // Headers: ["row", "x", "y"]
        assert_eq!(ph, vec!["row", "x", "y"]);
        // Row 0: ["a", "1", "2"]
        assert_eq!(pr[0], vec!["a", "1", "2"]);
        // Row 1: ["b", "3", "4"]
        assert_eq!(pr[1], vec!["b", "3", "4"]);
    }

    #[test]
    fn pivot_missing_cells_become_empty() {
        let rows = vec![
            vec!["a".to_owned(), "x".to_owned(), "1".to_owned()],
            // no (a, y) row
            vec!["b".to_owned(), "y".to_owned(), "4".to_owned()],
        ];
        let (_, pr) = pivot(&headers(), &rows, &CrosstabArgs::default()).unwrap();
        assert_eq!(pr[0], vec!["a", "1", ""]); // col y is missing
        assert_eq!(pr[1], vec!["b", "", "4"]); // col x is missing
    }

    #[test]
    fn pivot_too_few_columns_error() {
        let hdrs = vec!["a".to_owned(), "b".to_owned()];
        let rows: Vec<Vec<String>> = vec![];
        let err = pivot(&hdrs, &rows, &CrosstabArgs::default()).unwrap_err();
        assert!(err.contains("at least 3 columns"), "got: {err}");
    }

    #[test]
    fn pivot_duplicate_pair_error() {
        let rows = vec![
            vec!["a".to_owned(), "x".to_owned(), "1".to_owned()],
            vec!["a".to_owned(), "x".to_owned(), "2".to_owned()], // duplicate
        ];
        let err = pivot(&headers(), &rows, &CrosstabArgs::default()).unwrap_err();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn pivot_col_v_eq_col_h_error() {
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Index(1)),
            col_h: Some(ColSpec::Index(1)),
            ..Default::default()
        };
        let err = pivot(&headers(), &simple_rows(), &args).unwrap_err();
        assert!(err.contains("colV"), "got: {err}");
    }

    #[test]
    fn pivot_col_by_name() {
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Name("row".to_owned())),
            col_h: Some(ColSpec::Name("col".to_owned())),
            col_d: Some(ColSpec::Name("val".to_owned())),
            sort_col_h: None,
        };
        let (ph, pr) = pivot(&headers(), &simple_rows(), &args).unwrap();
        assert_eq!(ph[0], "row");
        assert_eq!(pr.len(), 2);
    }

    #[test]
    fn pivot_unknown_name_error() {
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Name("no_such_col".to_owned())),
            ..Default::default()
        };
        let err = pivot(&headers(), &simple_rows(), &args).unwrap_err();
        assert!(err.contains("no_such_col"), "got: {err}");
    }

    #[test]
    fn pivot_index_out_of_range_error() {
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Index(99)),
            ..Default::default()
        };
        let err = pivot(&headers(), &simple_rows(), &args).unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    #[test]
    fn pivot_col_by_1based_index() {
        // "1 2 3" (1-based) selects row=col0, col=col1, val=col2.
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Index(1)),
            col_h: Some(ColSpec::Index(2)),
            col_d: Some(ColSpec::Index(3)),
            sort_col_h: None,
        };
        let (ph, pr) = pivot(&headers(), &simple_rows(), &args).unwrap();
        assert_eq!(ph, vec!["row", "x", "y"]);
        assert_eq!(pr[0], vec!["a", "1", "2"]);
        assert_eq!(pr[1], vec!["b", "3", "4"]);
    }

    #[test]
    fn pivot_with_sortcolh() {
        // Build rows where encounter order of col headers is y, x
        // but sort column (col index 2, i.e. the "val" column as sort key)
        // would order them x first.
        // We use an extra column for sorting.
        let sort_headers = vec![
            "row".to_owned(),
            "col".to_owned(),
            "sort_key".to_owned(),
            "val".to_owned(),
        ];
        let sort_rows = vec![
            vec![
                "a".to_owned(),
                "y".to_owned(),
                "2".to_owned(),
                "ay".to_owned(),
            ],
            vec![
                "a".to_owned(),
                "x".to_owned(),
                "1".to_owned(),
                "ax".to_owned(),
            ],
            vec![
                "b".to_owned(),
                "y".to_owned(),
                "2".to_owned(),
                "by".to_owned(),
            ],
            vec![
                "b".to_owned(),
                "x".to_owned(),
                "1".to_owned(),
                "bx".to_owned(),
            ],
        ];
        let args = CrosstabArgs {
            col_v: Some(ColSpec::Name("row".to_owned())),
            col_h: Some(ColSpec::Name("col".to_owned())),
            col_d: Some(ColSpec::Name("val".to_owned())),
            sort_col_h: Some(ColSpec::Name("sort_key".to_owned())),
        };
        let (ph, pr) = pivot(&sort_headers, &sort_rows, &args).unwrap();
        // sortcolH "sort_key" values: x→"1", y→"2" → x sorts before y.
        assert_eq!(ph, vec!["row", "x", "y"]);
        assert_eq!(pr[0], vec!["a", "ax", "ay"]);
        assert_eq!(pr[1], vec!["b", "bx", "by"]);
    }

    // -- format_pivot --------------------------------------------------------

    #[test]
    fn format_pivot_produces_header_sep_rows_footer() {
        let ph = vec!["row".to_owned(), "x".to_owned(), "y".to_owned()];
        let pr = vec![
            vec!["a".to_owned(), "1".to_owned(), "2".to_owned()],
            vec!["b".to_owned(), "3".to_owned(), "4".to_owned()],
        ];
        let mut out = String::new();
        format_pivot(&mut out, &ph, &pr);
        let lines: Vec<&str> = out.lines().collect();
        // Should have: header, separator, 2 data rows, footer = 5 lines.
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains("row"));
        assert!(lines[0].contains('x'));
        assert!(lines[0].contains('y'));
        assert!(lines[1].contains('-'));
        assert!(lines[2].contains('1'));
        assert!(lines[4].contains("(2 rows)"));
    }

    #[test]
    fn format_pivot_single_row_footer() {
        let ph = vec!["r".to_owned(), "c".to_owned()];
        let pr = vec![vec!["v".to_owned(), "w".to_owned()]];
        let mut out = String::new();
        format_pivot(&mut out, &ph, &pr);
        assert!(out.contains("(1 row)"));
    }

    // -- ColSpec::from_str ---------------------------------------------------

    #[test]
    fn col_spec_numeric() {
        // 1-based: "1" and "7" are valid column numbers.
        assert_eq!(ColSpec::from_str("1"), ColSpec::Index(1));
        assert_eq!(ColSpec::from_str("7"), ColSpec::Index(7));
    }

    #[test]
    fn col_spec_zero_is_name() {
        // "0" is not a valid 1-based column number; treated as a column name.
        assert_eq!(ColSpec::from_str("0"), ColSpec::Name("0".to_owned()));
    }

    #[test]
    fn col_spec_name() {
        assert_eq!(
            ColSpec::from_str("mycolumn"),
            ColSpec::Name("mycolumn".to_owned())
        );
    }
}
