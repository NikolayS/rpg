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
/// This is intentionally simple: it does **not** parse string literals or
/// dollar-quoting.  Full parsing is left to the server.  This splitter drives
/// sequential execution so each statement gets its own result; the server
/// validates syntax and reports errors correctly regardless.
pub fn split_statements(sql: &str) -> Vec<String> {
    sql.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
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
