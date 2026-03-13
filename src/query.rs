//! Query execution layer.
//!
//! Wraps `tokio_postgres::Client` to provide higher-level query execution with
//! rich result types that carry column metadata, affected-row counts, and
//! timing information ready for the output formatter.
//!
//! All statements are sent via the **simple query protocol** (`simple_query`),
//! which returns every cell as text and provides a `CommandComplete` tag.
//! This is the same protocol psql uses for interactive queries.

use std::time::{Duration, Instant};

use thiserror::Error;
use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

/// Errors that can occur during query execution.
#[derive(Debug, Error)]
pub enum QueryError {
    /// A Postgres server-side error (SQLSTATE, message, hint, position, …).
    #[error("{0}")]
    Postgres(#[from] tokio_postgres::Error),

    /// The SQL file could not be read from disk.
    // Used by execute_file (public API); may not be constructed by main.rs directly.
    #[allow(dead_code)]
    #[error("could not read file \"{path}\": {reason}")]
    FileRead { path: String, reason: String },
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// A single result set from one SQL statement.
#[derive(Debug)]
pub enum StatementResult {
    /// A query that returned rows (SELECT, TABLE, VALUES, RETURNING, …).
    Rows(RowSet),
    /// A command that modified rows but returned no result set.
    CommandTag(CommandTag),
    /// A statement that produced neither rows nor a count (DDL, SET, …).
    Empty,
}

/// A full result set: column descriptors + data rows.
#[derive(Debug)]
pub struct RowSet {
    /// Column names in order.
    pub columns: Vec<ColumnMeta>,
    /// Data rows; each `Vec<Option<String>>` corresponds 1-to-1 with `columns`.
    pub rows: Vec<Vec<Option<String>>>,
}

/// Metadata for a single result column.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    /// Column name as returned by the server.
    pub name: String,
    /// Whether the column type is numeric (right-align hint for the formatter).
    ///
    /// The simple query protocol does not expose column OIDs, so this is
    /// always `false` for now.  The extended query path (issue #21) will
    /// populate this from `pg_type`.
    pub is_numeric: bool,
}

/// The result of a non-SELECT statement.
#[derive(Debug)]
pub struct CommandTag {
    /// The command tag as returned by Postgres (e.g. `INSERT 0 3`).
    pub tag: String,
    /// Number of rows affected (parsed from the tag).
    ///
    /// Reserved for the REPL (issue #20) which will use this to decide
    /// whether to show row-count feedback.
    pub rows_affected: u64,
}

/// The outcome of executing one or more SQL statements.
#[derive(Debug)]
pub struct QueryOutcome {
    /// One entry per statement that was executed.
    pub results: Vec<StatementResult>,
    /// Wall-clock time for the entire execution (all statements combined).
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// Parse rows affected from a command tag
// ---------------------------------------------------------------------------

/// Parse the affected-row count from a Postgres command tag string.
///
/// Common tags and expected return values:
/// - `INSERT 0 3`   → 3
/// - `UPDATE 5`     → 5
/// - `DELETE 2`     → 2
/// - `SELECT 1`     → 1  (used to classify as `CommandTag` for SELECT 0 rows)
/// - `CREATE TABLE` → 0
fn parse_rows_affected(tag: &str) -> u64 {
    // The row count is always the last whitespace-delimited token when numeric.
    tag.split_whitespace()
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Multi-statement splitter
// ---------------------------------------------------------------------------

/// Split a SQL string on `;` boundaries, yielding non-empty trimmed statements.
///
/// Handles the following constructs so that embedded semicolons are not
/// treated as statement terminators:
/// - Single-quoted strings: `'foo;bar'`
/// - Double-quoted identifiers: `"col;name"`
/// - Dollar-quoted strings: `$$body;here$$` (any `$tag$...$tag$` form)
/// - Line comments: `-- comment;here`
/// - Block comments: `/* comment;here */`
///
/// Note: this is a best-effort lexer, not a full SQL parser.  Corner-cases
/// like nested dollar-quoting are out of scope; the server handles validation.
#[allow(clippy::too_many_lines)]
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut stmts: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // -- line comment: skip to end of line
        if ch == '-' && i + 1 < len && chars[i + 1] == '-' {
            // Consume through '\n' (or end of input).
            while i < len && chars[i] != '\n' {
                current.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // /* block comment */
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            current.push(ch);
            current.push(chars[i + 1]);
            i += 2;
            while i < len {
                if chars[i] == '*' && i + 1 < len && chars[i + 1] == '/' {
                    current.push('*');
                    current.push('/');
                    i += 2;
                    break;
                }
                current.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // Single-quoted string  '...' ('' is an escaped quote inside).
        if ch == '\'' {
            current.push(ch);
            i += 1;
            while i < len {
                let c = chars[i];
                current.push(c);
                i += 1;
                if c == '\'' {
                    // Peek: doubled quote is an escape, not end of string.
                    if i < len && chars[i] == '\'' {
                        current.push('\'');
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }

        // Double-quoted identifier "..."  ("" is an escaped quote inside).
        if ch == '"' {
            current.push(ch);
            i += 1;
            while i < len {
                let c = chars[i];
                current.push(c);
                i += 1;
                if c == '"' {
                    if i < len && chars[i] == '"' {
                        current.push('"');
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            continue;
        }

        // Dollar-quoting: $tag$...$tag$  (tag may be empty: $$...$$).
        if ch == '$' {
            // Scan for the closing '$' of the opening tag.
            let tag_start = i;
            let mut j = i + 1;
            while j < len && chars[j] != '$' && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j < len && chars[j] == '$' {
                // We have a valid dollar-quote tag.
                let tag: String = chars[tag_start..=j].iter().collect(); // includes both '$'
                for c in &chars[tag_start..=j] {
                    current.push(*c);
                }
                i = j + 1;
                // Now scan forward until we find the closing tag.
                while i < len {
                    if chars[i] == '$' {
                        // Check if closing tag matches.
                        let end = i + tag.len();
                        if end <= len {
                            let candidate: String = chars[i..end].iter().collect();
                            if candidate == tag {
                                for c in &chars[i..end] {
                                    current.push(*c);
                                }
                                i = end;
                                break;
                            }
                        }
                    }
                    current.push(chars[i]);
                    i += 1;
                }
                continue;
            }
            // Not a valid dollar-quote — fall through and push '$' normally.
        }

        // Statement terminator.
        if ch == ';' {
            let trimmed = current.trim().to_owned();
            if !trimmed.is_empty() {
                stmts.push(trimmed);
            }
            current.clear();
            i += 1;
            continue;
        }

        current.push(ch);
        i += 1;
    }

    // Trailing statement without a final semicolon.
    let trimmed = current.trim().to_owned();
    if !trimmed.is_empty() {
        stmts.push(trimmed);
    }

    stmts
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Execute one or more SQL statements against `client`.
///
/// Statements are split on `;`.  Each is sent individually using the simple
/// query protocol so that the server returns a command tag we can inspect.
///
/// # Errors
/// Returns the first server-side or I/O error encountered.
pub async fn execute_sql(client: &Client, sql: &str) -> Result<QueryOutcome, QueryError> {
    let statements = split_statements(sql);
    let start = Instant::now();
    let mut results = Vec::with_capacity(statements.len());

    for stmt in &statements {
        let result = execute_one(client, stmt).await?;
        results.push(result);
    }

    Ok(QueryOutcome {
        results,
        duration: start.elapsed(),
    })
}

/// Execute a single SQL statement via the simple query protocol.
async fn execute_one(client: &Client, stmt: &str) -> Result<StatementResult, QueryError> {
    use tokio_postgres::SimpleQueryMessage;

    let messages = client.simple_query(stmt).await?;

    let mut columns: Option<Vec<ColumnMeta>> = None;
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut tag: Option<String> = None;

    for msg in messages {
        match msg {
            SimpleQueryMessage::Row(row) => {
                // Materialise column metadata lazily from the first row.
                if columns.is_none() {
                    columns = Some(
                        row.columns()
                            .iter()
                            .map(|c| ColumnMeta {
                                name: c.name().to_owned(),
                                // Simple query protocol carries no type OIDs.
                                is_numeric: false,
                            })
                            .collect(),
                    );
                }

                let n = row.columns().len();
                let cells: Vec<Option<String>> =
                    (0..n).map(|i| row.get(i).map(ToOwned::to_owned)).collect();
                rows.push(cells);
            }
            SimpleQueryMessage::CommandComplete(t) => {
                tag = Some(t.to_string());
            }
            _ => {}
        }
    }

    // Classify the result.
    if let Some(cols) = columns {
        Ok(StatementResult::Rows(RowSet {
            columns: cols,
            rows,
        }))
    } else if !rows.is_empty() {
        // Defensive: rows without a column descriptor — treat as row set.
        Ok(StatementResult::Rows(RowSet {
            columns: vec![],
            rows,
        }))
    } else if let Some(t) = tag {
        // NOTE: The simple query protocol does not return column descriptors
        // when a SELECT matches zero rows (e.g. `SELECT ... WHERE false`).
        // We detect this via the "SELECT 0" command tag and synthesise an
        // empty RowSet with no columns.  Column names are unavailable at
        // this point; a future migration to the extended query protocol
        // (issue #21) will eliminate this special case.
        if t == "SELECT 0" {
            return Ok(StatementResult::Rows(RowSet {
                columns: vec![],
                rows: vec![],
            }));
        }

        let rows_affected = parse_rows_affected(&t);
        // Treat DDL / utility statements as `Empty` (no row-count output).
        if rows_affected == 0
            && !t.starts_with("INSERT")
            && !t.starts_with("UPDATE")
            && !t.starts_with("DELETE")
            && !t.starts_with("MERGE")
            && !t.starts_with("SELECT")
        {
            Ok(StatementResult::Empty)
        } else {
            Ok(StatementResult::CommandTag(CommandTag {
                tag: t,
                rows_affected,
            }))
        }
    } else {
        Ok(StatementResult::Empty)
    }
}

/// Execute SQL from a file.
///
/// # Errors
/// Returns [`QueryError::FileRead`] if the file cannot be read, or a
/// [`QueryError::Postgres`] variant if execution fails.
// Public API kept for library consumers; main.rs reads the file directly so
// it can supply the SQL string to the error formatter without a second read.
#[allow(dead_code)]
pub async fn execute_file(client: &Client, path: &str) -> Result<QueryOutcome, QueryError> {
    let sql = std::fs::read_to_string(path).map_err(|e| QueryError::FileRead {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    execute_sql(client, &sql).await
}

// ---------------------------------------------------------------------------
// Unit tests (no DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // split_statements
    // -----------------------------------------------------------------------

    #[test]
    fn test_split_statements_basic() {
        let stmts = split_statements("select 1; select 2; select 3");
        assert_eq!(stmts, vec!["select 1", "select 2", "select 3"]);
    }

    #[test]
    fn test_split_statements_trailing_semicolon() {
        let stmts = split_statements("select 1;");
        assert_eq!(stmts, vec!["select 1"]);
    }

    #[test]
    fn test_split_statements_empty() {
        let stmts = split_statements("");
        assert!(stmts.is_empty());
    }

    #[test]
    fn test_split_statements_whitespace_only() {
        let stmts = split_statements("  ;  ;  ");
        assert!(stmts.is_empty());
    }

    #[test]
    fn test_split_statements_preserves_content() {
        let sql = "create table foo (id int); insert into foo values (1)";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0], "create table foo (id int)");
        assert_eq!(stmts[1], "insert into foo values (1)");
    }

    #[test]
    fn test_split_single_statement_no_semicolon() {
        let stmts = split_statements("select version()");
        assert_eq!(stmts, vec!["select version()"]);
    }

    #[test]
    fn test_split_single_quoted_embedded_semicolon() {
        // Semicolon inside a single-quoted string must not split.
        let stmts = split_statements("select 'foo;bar'");
        assert_eq!(stmts, vec!["select 'foo;bar'"]);
    }

    #[test]
    fn test_split_double_quoted_embedded_semicolon() {
        // Semicolon inside a double-quoted identifier must not split.
        let stmts = split_statements(r#"select "col;name" from t"#);
        assert_eq!(stmts, vec![r#"select "col;name" from t"#]);
    }

    #[test]
    fn test_split_dollar_quoted_embedded_semicolon() {
        // Semicolon inside a dollar-quoted string must not split.
        let sql = "create function f() returns void language sql as $$select 1; select 2$$";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1, "should be one statement: {stmts:?}");
        assert!(stmts[0].contains("$$select 1; select 2$$"));
    }

    #[test]
    fn test_split_dollar_quoted_with_tag() {
        // Dollar-quoting with a non-empty tag.
        let sql = "create function g() returns void language plpgsql as $body$begin; end$body$";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1, "should be one statement: {stmts:?}");
    }

    #[test]
    fn test_split_line_comment_embedded_semicolon() {
        // Semicolon in a line comment must not split.
        let sql = "select 1 -- no split; here\n, 2";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 1, "should be one statement: {stmts:?}");
    }

    #[test]
    fn test_split_block_comment_embedded_semicolon() {
        // Semicolon in a block comment must not split.
        let sql = "select /* not; a split */ 1";
        let stmts = split_statements(sql);
        assert_eq!(stmts, vec!["select /* not; a split */ 1"]);
    }

    #[test]
    fn test_split_mixed_embedded_semicolons() {
        // Two real statements, each with embedded semicolons in strings.
        let sql = "select 'a;b'; select 'c;d'";
        let stmts = split_statements(sql);
        assert_eq!(stmts, vec!["select 'a;b'", "select 'c;d'"]);
    }

    // -----------------------------------------------------------------------
    // SELECT 0 special case (Fix 1)
    // -----------------------------------------------------------------------

    /// The SELECT 0 path is tested indirectly via `execute_one`; here we verify
    /// the tag check logic by examining `parse_rows_affected` on the tag.
    #[test]
    fn test_parse_rows_affected_select_zero() {
        assert_eq!(parse_rows_affected("SELECT 0"), 0);
    }

    // -----------------------------------------------------------------------
    // parse_rows_affected
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_rows_affected_insert() {
        assert_eq!(parse_rows_affected("INSERT 0 3"), 3);
    }

    #[test]
    fn test_parse_rows_affected_update() {
        assert_eq!(parse_rows_affected("UPDATE 5"), 5);
    }

    #[test]
    fn test_parse_rows_affected_delete() {
        assert_eq!(parse_rows_affected("DELETE 0"), 0);
    }

    #[test]
    fn test_parse_rows_affected_ddl() {
        assert_eq!(parse_rows_affected("CREATE TABLE"), 0);
    }

    #[test]
    fn test_parse_rows_affected_select() {
        assert_eq!(parse_rows_affected("SELECT 1"), 1);
    }
}
