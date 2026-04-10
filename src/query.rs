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
#[allow(dead_code)]
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
    /// The simple query protocol does not expose column OIDs.  The REPL path
    /// infers this heuristically by inspecting cell values (see `repl.rs`).
    /// The extended query path (issue #21) will populate this from `pg_type`.
    pub is_numeric: bool,
}

/// The result of a non-SELECT statement.
#[derive(Debug)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
// Reconstruct command tag from SQL + row count
// ---------------------------------------------------------------------------

/// Reconstruct the full `PostgreSQL` command tag from the SQL statement and row count.
///
/// `tokio-postgres 0.7` exposes only the numeric row count from `CommandComplete`;
/// the full tag string (e.g. `"INSERT 0 3"`, `"CREATE TABLE"`) is discarded by the
/// library before it reaches our code.  We recover it by inspecting the first
/// keyword(s) of the SQL statement and applying the same rules that `PostgreSQL`
/// uses to form the tag (defined in `src/include/tcop/cmdtaglist.h`).
///
/// Tags that carry a row count (per `rowcount = true` in cmdtaglist.h):
///   COPY, DELETE, FETCH, INSERT, MERGE, MOVE, SELECT, UPDATE
///
/// All other commands (DDL, utility) produce a fixed tag with no number.
///
/// # Format
/// - `INSERT`  → `"INSERT 0 {n}"` (the `0` is the historical OID placeholder)
/// - `UPDATE`  → `"UPDATE {n}"`
/// - `DELETE`  → `"DELETE {n}"`
/// - `MERGE`   → `"MERGE {n}"`
/// - `COPY`    → `"COPY {n}"`
/// - `FETCH`   → `"FETCH {n}"`
/// - `MOVE`    → `"MOVE {n}"`
/// - DDL / utility → the tag text (e.g. `"CREATE TABLE"`, `"SET"`, `"BEGIN"`)
#[allow(clippy::too_many_lines)]
pub fn reconstruct_command_tag(sql: &str, n: u64) -> String {
    // Skip leading whitespace and block/line comments to find the first keyword.
    let sql = skip_leading_comments(sql);
    let upper: String = sql
        .split_whitespace()
        .take(6)
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>()
        .join(" ");

    let words: Vec<&str> = upper.split_whitespace().collect();
    let w0 = words.first().copied().unwrap_or("");
    let w1 = words.get(1).copied().unwrap_or("");
    let w2 = words.get(2).copied().unwrap_or("");
    let w3 = words.get(3).copied().unwrap_or("");

    match w0 {
        // --- DML: tag includes row count ---
        "INSERT" => format!("INSERT 0 {n}"),
        "UPDATE" => format!("UPDATE {n}"),
        "DELETE" => format!("DELETE {n}"),
        "MERGE" => format!("MERGE {n}"),
        "COPY" => format!("COPY {n}"),
        "FETCH" => format!("FETCH {n}"),
        "MOVE" => format!("MOVE {n}"),
        // SELECT / TABLE / VALUES / WITH: these normally go via the Rows path;
        // if they somehow reach here it means 0 rows with no RowDescription.
        "SELECT" | "TABLE" | "VALUES" => format!("SELECT {n}"),
        "WITH" => format!("SELECT {n}"),

        // --- CREATE variants ---
        "CREATE" => match w1 {
            "OR" => {
                // CREATE OR REPLACE FUNCTION/PROCEDURE/VIEW/RULE/TRANSFORM
                let kind = match w3 {
                    "FUNCTION" | "PROCEDURE" | "VIEW" | "RULE" | "AGGREGATE" | "TRANSFORM"
                    | "TRIGGER" => w3,
                    _ => w3,
                };
                format!("CREATE {kind}")
            }
            "TEMP" | "TEMPORARY" => {
                // CREATE [TEMP|TEMPORARY] [UNLOGGED] TABLE ...
                match w2 {
                    "UNLOGGED" => "CREATE TABLE".to_string(),
                    "TABLE" => "CREATE TABLE".to_string(),
                    _ => format!("CREATE {w2}"),
                }
            }
            "UNLOGGED" => "CREATE TABLE".to_string(),
            "UNIQUE" | "CONCURRENTLY" => "CREATE INDEX".to_string(),
            "MATERIALIZED" => "CREATE MATERIALIZED VIEW".to_string(),
            "FOREIGN" => match w2 {
                "TABLE" => "CREATE FOREIGN TABLE".to_string(),
                "DATA" => "CREATE FOREIGN DATA WRAPPER".to_string(),
                _ => format!("CREATE FOREIGN {w2}"),
            },
            "TEXT" => format!("CREATE TEXT SEARCH {w3}"),
            "OPERATOR" => match w2 {
                "CLASS" => "CREATE OPERATOR CLASS".to_string(),
                "FAMILY" => "CREATE OPERATOR FAMILY".to_string(),
                _ => "CREATE OPERATOR".to_string(),
            },
            "USER" => match w2 {
                "MAPPING" => "CREATE USER MAPPING".to_string(),
                _ => "CREATE ROLE".to_string(),
            },
            "GROUP" => "CREATE ROLE".to_string(),
            "ACCESS" => "CREATE ACCESS METHOD".to_string(),
            "DEFAULT" => "CREATE CONVERSION".to_string(),
            "EVENT" => "CREATE EVENT TRIGGER".to_string(),
            "" => "CREATE".to_string(),
            _ => format!("CREATE {w1}"),
        },

        // --- DROP variants ---
        "DROP" => match w1 {
            "MATERIALIZED" => "DROP MATERIALIZED VIEW".to_string(),
            "FOREIGN" => match w2 {
                "TABLE" => "DROP FOREIGN TABLE".to_string(),
                "DATA" => "DROP FOREIGN DATA WRAPPER".to_string(),
                _ => format!("DROP FOREIGN {w2}"),
            },
            "TEXT" => format!("DROP TEXT SEARCH {w3}"),
            "OPERATOR" => match w2 {
                "CLASS" => "DROP OPERATOR CLASS".to_string(),
                "FAMILY" => "DROP OPERATOR FAMILY".to_string(),
                _ => "DROP OPERATOR".to_string(),
            },
            "USER" => match w2 {
                "MAPPING" => "DROP USER MAPPING".to_string(),
                _ => "DROP ROLE".to_string(),
            },
            "GROUP" => "DROP ROLE".to_string(),
            "ACCESS" => "DROP ACCESS METHOD".to_string(),
            "EVENT" => "DROP EVENT TRIGGER".to_string(),
            "OWNED" => "DROP OWNED".to_string(),
            "" => "DROP".to_string(),
            _ => format!("DROP {w1}"),
        },

        // --- ALTER variants ---
        "ALTER" => match w1 {
            "DEFAULT" => "ALTER DEFAULT PRIVILEGES".to_string(),
            "TEXT" => format!("ALTER TEXT SEARCH {w3}"),
            "FOREIGN" => match w2 {
                "TABLE" => "ALTER FOREIGN TABLE".to_string(),
                "DATA" => "ALTER FOREIGN DATA WRAPPER".to_string(),
                _ => format!("ALTER FOREIGN {w2}"),
            },
            "MATERIALIZED" => "ALTER MATERIALIZED VIEW".to_string(),
            "OPERATOR" => match w2 {
                "CLASS" => "ALTER OPERATOR CLASS".to_string(),
                "FAMILY" => "ALTER OPERATOR FAMILY".to_string(),
                _ => "ALTER OPERATOR".to_string(),
            },
            "USER" => match w2 {
                "MAPPING" => "ALTER USER MAPPING".to_string(),
                _ => "ALTER ROLE".to_string(), // ALTER USER → ALTER ROLE tag
            },
            "GROUP" => "ALTER ROLE".to_string(),
            "ACCESS" => "ALTER ACCESS METHOD".to_string(),
            "" => "ALTER".to_string(),
            _ => format!("ALTER {w1}"),
        },

        // --- Transaction control ---
        "BEGIN" | "START" => "BEGIN".to_string(),
        "COMMIT" | "END" => match w1 {
            "PREPARED" => "COMMIT PREPARED".to_string(),
            _ => "COMMIT".to_string(),
        },
        "ROLLBACK" => match w1 {
            "PREPARED" => "ROLLBACK PREPARED".to_string(),
            _ => "ROLLBACK".to_string(),
        },
        "SAVEPOINT" => "SAVEPOINT".to_string(),
        "RELEASE" => "RELEASE".to_string(),

        // --- Cursor commands ---
        "DECLARE" => "DECLARE CURSOR".to_string(),
        "CLOSE" => "CLOSE CURSOR".to_string(),

        // --- Prepare / execute ---
        "PREPARE" => "PREPARE".to_string(),
        "EXECUTE" => "EXECUTE".to_string(),
        "DEALLOCATE" => match w1 {
            "ALL" => "DEALLOCATE ALL".to_string(),
            _ => "DEALLOCATE".to_string(),
        },

        // --- DISCARD ---
        "DISCARD" => match w1 {
            "ALL" => "DISCARD ALL".to_string(),
            "PLANS" => "DISCARD PLANS".to_string(),
            "SEQUENCES" => "DISCARD SEQUENCES".to_string(),
            "TEMP" | "TEMPORARY" => "DISCARD TEMP".to_string(),
            _ => "DISCARD".to_string(),
        },

        // --- GRANT / REVOKE ---
        "GRANT" => match w1 {
            "ROLE" => "GRANT ROLE".to_string(),
            _ => "GRANT".to_string(),
        },
        "REVOKE" => match w1 {
            "ROLE" => "REVOKE ROLE".to_string(),
            _ => "REVOKE".to_string(),
        },

        // --- SET / RESET / SHOW ---
        "SET" => match w1 {
            "CONSTRAINTS" => "SET CONSTRAINTS".to_string(),
            _ => "SET".to_string(),
        },
        "RESET" => "RESET".to_string(),
        "SHOW" => "SHOW".to_string(),

        // --- TRUNCATE ---
        "TRUNCATE" => "TRUNCATE TABLE".to_string(),

        // --- Maintenance ---
        "VACUUM" => "VACUUM".to_string(),
        "ANALYZE" | "ANALYSE" => "ANALYZE".to_string(),
        "CLUSTER" => "CLUSTER".to_string(),
        "REINDEX" => "REINDEX".to_string(),
        "CHECKPOINT" => "CHECKPOINT".to_string(),

        // --- LOCK ---
        "LOCK" => "LOCK TABLE".to_string(),

        // --- Async messaging ---
        "LISTEN" => "LISTEN".to_string(),
        "UNLISTEN" => "UNLISTEN".to_string(),
        "NOTIFY" => "NOTIFY".to_string(),

        // --- Misc ---
        "LOAD" => "LOAD".to_string(),
        "CALL" => "CALL".to_string(),
        "DO" => "DO".to_string(),
        "COMMENT" => "COMMENT".to_string(),
        "SECURITY" => "SECURITY LABEL".to_string(),
        "REASSIGN" => "REASSIGN OWNED".to_string(),
        "IMPORT" => "IMPORT FOREIGN SCHEMA".to_string(),
        "REFRESH" => "REFRESH MATERIALIZED VIEW".to_string(),
        "EXPLAIN" => "EXPLAIN".to_string(),

        // --- Fallback: return the first word ---
        other => other.to_string(),
    }
}

/// Skip leading whitespace and SQL comments (line `--` and block `/* */`)
/// to find the first meaningful keyword in a SQL statement.
/// Strip leading blank lines and line/block comments from a SQL string.
///
/// `PostgreSQL` counts lines from the start of the query string it receives.
/// psql strips these leading decorations before sending so that LINE N in
/// error messages is relative to the first real SQL token (LINE 1 for a
/// single-statement query).  rpg must do the same to match psql output.
pub fn strip_leading_preamble(sql: &str) -> &str {
    skip_leading_comments(sql)
}

fn skip_leading_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Line comment: skip to end of line.
            s = s.find('\n').map_or("", |i| s[i + 1..].trim_start());
        } else if s.starts_with("/*") {
            // Block comment: skip to matching `*/`, handling nesting.
            let bytes = s.as_bytes();
            let len = bytes.len();
            let mut depth: u32 = 0;
            let mut i = 0;
            while i < len {
                if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            s = s[i..].trim_start();
        } else {
            break;
        }
    }
    s
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
///
/// # Implementation note
///
/// All delimiter characters (`'`, `"`, `$`, `;`, `-`, `/`, `*`, `\n`) are
/// ASCII and therefore single-byte in UTF-8.  The implementation works on
/// the raw byte slice and uses byte offsets to extract `&str` slices,
/// avoiding the `Vec<char>` allocation of a char-by-char approach.
/// Benchmarks show a 45–68% speedup over the char-indexed version.
#[allow(clippy::too_many_lines)]
#[allow(unused_assignments)] // false positive from flush_to! macro expansion
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut stmts: Vec<String> = Vec::new();
    let bytes = sql.as_bytes();
    let len = bytes.len();
    // `seg_start` tracks the start of the not-yet-flushed byte range so we
    // can append whole slices to `current` instead of pushing char-by-char.
    let mut seg_start = 0_usize;
    let mut i = 0_usize;

    // Flush bytes[seg_start..end] into `current` and advance seg_start.
    //
    // All delimiter characters are ASCII (single bytes), so every position
    // we assign to `seg_start` or use as a slice boundary is guaranteed to
    // land on a valid UTF-8 code-point boundary.
    macro_rules! flush_to {
        ($current:expr, $end:expr) => {
            if seg_start < $end {
                #[allow(unsafe_code)]
                // SAFETY: seg_start and $end are always on valid UTF-8
                // boundaries (see doc comment above).
                $current
                    .push_str(unsafe { std::str::from_utf8_unchecked(&bytes[seg_start..$end]) });
                seg_start = $end;
            }
        };
    }

    let mut current = String::new();
    // Track parenthesis depth so that semicolons inside `(...)` do not split
    // the statement.  This handles cases like CREATE RULE ... DO ALSO (s1; s2)
    // and function calls with multiple arguments.
    let mut paren_depth: u32 = 0;
    // Track BEGIN ATOMIC depth so that semicolons inside SQL-language function
    // bodies are not treated as statement terminators.
    let mut begin_atomic_depth: u32 = 0;

    while i < len {
        let b = bytes[i];

        // -- line comment: -- … \n -----------------------------------------
        if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            flush_to!(current, i);
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            flush_to!(current, i);
            continue;
        }

        // -- block comment: /* … */ (supports nesting: /* /* */ */) ----------
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            flush_to!(current, i);
            let mut depth: u32 = 0;
            while i + 1 < len {
                if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            // If comment was not closed, consume to end of input.
            if depth > 0 {
                i = len;
            }
            flush_to!(current, i);
            continue;
        }

        // -- single-quoted string: '...' (''-escaped quotes inside) ---------
        if b == b'\'' {
            flush_to!(current, i + 1); // include opening quote
            i += 1;
            while i < len {
                if bytes[i] == b'\'' {
                    i += 1;
                    flush_to!(current, i);
                    if i < len && bytes[i] == b'\'' {
                        // Doubled quote — escape, not end of string.
                        i += 1;
                        flush_to!(current, i);
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // -- double-quoted identifier: "..." (""-escaped quotes inside) ------
        if b == b'"' {
            flush_to!(current, i + 1); // include opening quote
            i += 1;
            while i < len {
                if bytes[i] == b'"' {
                    i += 1;
                    flush_to!(current, i);
                    if i < len && bytes[i] == b'"' {
                        i += 1;
                        flush_to!(current, i);
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // -- dollar-quoting: $tag$...$tag$ -----------------------------------
        if b == b'$' {
            let tag_start = i;
            let mut j = i + 1;
            while j < len && bytes[j] != b'$' {
                if bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' {
                    j += 1;
                } else {
                    break;
                }
            }
            if j < len && bytes[j] == b'$' {
                let tag_end = j + 1; // exclusive; tag = bytes[tag_start..tag_end]
                let tag = &bytes[tag_start..tag_end];
                flush_to!(current, tag_end);
                i = tag_end;
                // Scan forward for the matching closing tag.
                'dollar: while i < len {
                    if bytes[i] == b'$' {
                        let end = i + tag.len();
                        if end <= len && &bytes[i..end] == tag {
                            i = end;
                            flush_to!(current, i);
                            break 'dollar;
                        }
                    }
                    i += 1;
                }
                if i == len {
                    flush_to!(current, i);
                }
                continue;
            }
            // Not a valid dollar-quote — fall through.
        }

        // -- parenthesis depth (outside strings/comments) ------------------
        if b == b'(' {
            paren_depth += 1;
        } else if b == b')' {
            paren_depth = paren_depth.saturating_sub(1);
        }

        // -- BEGIN ATOMIC / END depth (SQL-language function bodies) -------
        if b.eq_ignore_ascii_case(&b'b') && i + 12 <= len {
            let ahead = &bytes[i..i + 12];
            if ahead.eq_ignore_ascii_case(b"begin atomic") {
                let after = if i + 12 < len { bytes[i + 12] } else { b' ' };
                let before_ok =
                    i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
                if before_ok && !after.is_ascii_alphanumeric() && after != b'_' {
                    begin_atomic_depth += 1;
                }
            }
        }
        // Only decrement for END that closes a BEGIN ATOMIC body.
        // The closing END must be preceded by `;` (the last statement in
        // the body), optionally with whitespace between.  This avoids
        // confusing CASE...END, LOOP...END LOOP, etc.
        if begin_atomic_depth > 0
            && b.eq_ignore_ascii_case(&b'e')
            && i + 3 <= len
            && bytes[i..i + 3].eq_ignore_ascii_case(b"end")
        {
            let after = if i + 3 < len { bytes[i + 3] } else { b';' };
            if !after.is_ascii_alphanumeric() && after != b'_' {
                // Walk backwards over whitespace; the character before must
                // be `;` (end of the last statement in the ATOMIC body).
                let mut k = i;
                while k > 0 && bytes[k - 1].is_ascii_whitespace() {
                    k -= 1;
                }
                if k > 0 && bytes[k - 1] == b';' {
                    begin_atomic_depth -= 1;
                }
            }
        }

        // -- statement terminator (only at top-level, depth == 0) ----------
        if b == b';' && paren_depth == 0 && begin_atomic_depth == 0 {
            flush_to!(current, i);
            let trimmed = current.trim().to_owned();
            if !trimmed.is_empty() {
                stmts.push(trimmed);
            }
            current.clear();
            i += 1;
            seg_start = i;
            continue;
        }

        i += 1;
    }

    // Trailing statement without a final semicolon.
    flush_to!(current, len);
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
    // Set to true when a RowDescription message is received, indicating this
    // is a SELECT-like statement even if it returns zero rows.
    let mut saw_row_description = false;
    let mut tag: Option<String> = None;

    for msg in messages {
        match msg {
            SimpleQueryMessage::RowDescription(cols) => {
                // A RowDescription message precedes data rows (or CommandComplete
                // for zero-row results).  Capture column names so that empty
                // result sets still render their headers correctly.
                saw_row_description = true;
                if columns.is_none() {
                    columns = Some(
                        cols.iter()
                            .map(|c| ColumnMeta {
                                name: c.name().to_owned(),
                                is_numeric: false,
                            })
                            .collect(),
                    );
                }
            }
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
            SimpleQueryMessage::CommandComplete(n) => {
                // tokio-postgres 0.7 exposes only the numeric count from
                // CommandComplete, not the full tag string (e.g. "INSERT 0 3").
                // Reconstruct the full tag from the SQL statement and count.
                tag = Some(reconstruct_command_tag(stmt, n));
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
    } else if saw_row_description {
        // Empty SELECT (0 rows) — RowDescription was received but no Row
        // messages followed.  Columns are unavailable via the simple query
        // protocol in this case; render with no column headers.
        Ok(StatementResult::Rows(RowSet {
            columns: vec![],
            rows: vec![],
        }))
    } else if !rows.is_empty() {
        // Defensive: rows without a column descriptor — treat as row set.
        Ok(StatementResult::Rows(RowSet {
            columns: vec![],
            rows,
        }))
    } else if let Some(t) = tag {
        let rows_affected = parse_rows_affected(&t);
        // SELECT-like tags (no column descriptor received) → empty row set.
        if t.starts_with("SELECT") {
            return Ok(StatementResult::Rows(RowSet {
                columns: vec![],
                rows: vec![],
            }));
        }
        // DDL and utility tags with zero rows → show the tag (psql does this).
        // Only truly "empty" (no-op) statements return no tag at all.
        Ok(StatementResult::CommandTag(CommandTag {
            tag: t,
            rows_affected,
        }))
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

    // -----------------------------------------------------------------------
    // reconstruct_command_tag
    // -----------------------------------------------------------------------

    #[test]
    fn test_reconstruct_insert() {
        assert_eq!(
            reconstruct_command_tag("INSERT INTO t VALUES (1)", 1),
            "INSERT 0 1"
        );
        assert_eq!(
            reconstruct_command_tag("INSERT INTO t VALUES (1),(2),(3)", 3),
            "INSERT 0 3"
        );
        assert_eq!(
            reconstruct_command_tag("insert into t values (1)", 1),
            "INSERT 0 1"
        );
    }

    #[test]
    fn test_reconstruct_update() {
        assert_eq!(reconstruct_command_tag("UPDATE t SET x = 1", 5), "UPDATE 5");
        assert_eq!(
            reconstruct_command_tag("UPDATE t SET x = 1 WHERE false", 0),
            "UPDATE 0"
        );
    }

    #[test]
    fn test_reconstruct_delete() {
        assert_eq!(
            reconstruct_command_tag("DELETE FROM t WHERE id = 1", 1),
            "DELETE 1"
        );
        assert_eq!(reconstruct_command_tag("delete from t", 0), "DELETE 0");
    }

    #[test]
    fn test_reconstruct_copy() {
        assert_eq!(
            reconstruct_command_tag("COPY t FROM 'file.csv'", 42),
            "COPY 42"
        );
        assert_eq!(reconstruct_command_tag("COPY t TO STDOUT", 10), "COPY 10");
    }

    #[test]
    fn test_reconstruct_ddl() {
        assert_eq!(
            reconstruct_command_tag("CREATE TABLE foo (id int)", 0),
            "CREATE TABLE"
        );
        assert_eq!(reconstruct_command_tag("DROP TABLE foo", 0), "DROP TABLE");
        assert_eq!(
            reconstruct_command_tag("ALTER TABLE foo ADD COLUMN x int", 0),
            "ALTER TABLE"
        );
        assert_eq!(
            reconstruct_command_tag("CREATE INDEX idx ON foo (id)", 0),
            "CREATE INDEX"
        );
        assert_eq!(
            reconstruct_command_tag("CREATE UNIQUE INDEX idx ON foo (id)", 0),
            "CREATE INDEX"
        );
        assert_eq!(
            reconstruct_command_tag("CREATE MATERIALIZED VIEW v AS SELECT 1", 0),
            "CREATE MATERIALIZED VIEW"
        );
        assert_eq!(
            reconstruct_command_tag("DROP MATERIALIZED VIEW v", 0),
            "DROP MATERIALIZED VIEW"
        );
    }

    #[test]
    fn test_reconstruct_create_or_replace() {
        assert_eq!(
            reconstruct_command_tag(
                "CREATE OR REPLACE FUNCTION foo() RETURNS void AS $$ $$ LANGUAGE sql",
                0
            ),
            "CREATE FUNCTION"
        );
        assert_eq!(
            reconstruct_command_tag("CREATE OR REPLACE VIEW v AS SELECT 1", 0),
            "CREATE VIEW"
        );
    }

    #[test]
    fn test_reconstruct_transaction() {
        assert_eq!(reconstruct_command_tag("BEGIN", 0), "BEGIN");
        assert_eq!(reconstruct_command_tag("COMMIT", 0), "COMMIT");
        assert_eq!(reconstruct_command_tag("ROLLBACK", 0), "ROLLBACK");
        assert_eq!(reconstruct_command_tag("SAVEPOINT sp1", 0), "SAVEPOINT");
        assert_eq!(
            reconstruct_command_tag("RELEASE SAVEPOINT sp1", 0),
            "RELEASE"
        );
    }

    #[test]
    fn test_reconstruct_utility() {
        assert_eq!(
            reconstruct_command_tag("SET search_path = public", 0),
            "SET"
        );
        assert_eq!(reconstruct_command_tag("TRUNCATE foo", 0), "TRUNCATE TABLE");
        assert_eq!(reconstruct_command_tag("VACUUM", 0), "VACUUM");
        assert_eq!(reconstruct_command_tag("ANALYZE foo", 0), "ANALYZE");
        assert_eq!(
            reconstruct_command_tag("COMMENT ON TABLE foo IS 'bar'", 0),
            "COMMENT"
        );
    }

    #[test]
    fn test_reconstruct_create_temp_table() {
        assert_eq!(
            reconstruct_command_tag("CREATE TEMP TABLE foo (id int)", 0),
            "CREATE TABLE"
        );
        assert_eq!(
            reconstruct_command_tag("CREATE TEMPORARY TABLE foo (id int)", 0),
            "CREATE TABLE"
        );
    }

    #[test]
    fn test_reconstruct_create_user_vs_user_mapping() {
        // CREATE USER is an alias for CREATE ROLE — tag should be CREATE ROLE
        assert_eq!(
            reconstruct_command_tag("CREATE USER alice PASSWORD 'secret'", 0),
            "CREATE ROLE"
        );
        // CREATE USER MAPPING is a separate command
        assert_eq!(
            reconstruct_command_tag("CREATE USER MAPPING FOR alice SERVER s", 0),
            "CREATE USER MAPPING"
        );
        // DROP USER (alias for DROP ROLE)
        assert_eq!(reconstruct_command_tag("DROP USER alice", 0), "DROP ROLE");
        // DROP USER MAPPING
        assert_eq!(
            reconstruct_command_tag("DROP USER MAPPING FOR alice SERVER s", 0),
            "DROP USER MAPPING"
        );
    }

    #[test]
    fn test_reconstruct_close_all() {
        // CLOSE ALL produces tag "CLOSE CURSOR", not "CLOSE CURSOR ALL"
        assert_eq!(reconstruct_command_tag("CLOSE ALL", 0), "CLOSE CURSOR");
        assert_eq!(
            reconstruct_command_tag("CLOSE my_cursor", 0),
            "CLOSE CURSOR"
        );
    }

    #[test]
    fn test_reconstruct_group_aliases() {
        // CREATE/DROP/ALTER GROUP are aliases for ROLE operations.
        assert_eq!(
            reconstruct_command_tag("CREATE GROUP staff", 0),
            "CREATE ROLE"
        );
        assert_eq!(reconstruct_command_tag("DROP GROUP staff", 0), "DROP ROLE");
        assert_eq!(
            reconstruct_command_tag("ALTER GROUP staff ADD USER alice", 0),
            "ALTER ROLE"
        );
    }

    #[test]
    fn test_reconstruct_create_default_conversion() {
        // CREATE DEFAULT CONVERSION should produce CREATE CONVERSION.
        assert_eq!(
            reconstruct_command_tag(
                "CREATE DEFAULT CONVERSION myconv FOR 'UTF8' TO 'LATIN1' FROM utf8_to_iso8859_1",
                0
            ),
            "CREATE CONVERSION"
        );
    }

    #[test]
    fn test_split_begin_atomic() {
        let sql =
            "CREATE FUNCTION f() RETURNS void LANGUAGE SQL BEGIN ATOMIC SELECT 1; SELECT 2; END;";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            1,
            "BEGIN ATOMIC body should not be split: {stmts:?}"
        );
    }

    #[test]
    fn test_split_plain_begin_still_splits() {
        // Plain BEGIN (transaction) should still split normally.
        let stmts = split_statements("BEGIN; SELECT 1; COMMIT;");
        assert_eq!(stmts, vec!["BEGIN", "SELECT 1", "COMMIT"]);
    }

    #[test]
    fn test_split_begin_atomic_case_insensitive() {
        let sql =
            "create function f() returns void language sql begin atomic select 1; select 2; end;";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            1,
            "lowercase BEGIN ATOMIC should work: {stmts:?}"
        );
    }

    #[test]
    fn test_split_begin_atomic_with_case_end() {
        // CASE...END inside BEGIN ATOMIC must not prematurely close the block.
        let sql = "CREATE FUNCTION f() RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT CASE WHEN true THEN 1 ELSE 0 END; END;";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            1,
            "CASE...END should not close BEGIN ATOMIC: {stmts:?}"
        );
    }

    #[test]
    fn test_split_begin_atomic_in_dollar_quote() {
        // BEGIN ATOMIC inside a dollar-quoted string should not be detected.
        let sql = "SELECT $$BEGIN ATOMIC SELECT 1; END$$; SELECT 2;";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            2,
            "dollar-quoted BEGIN ATOMIC is just a string: {stmts:?}"
        );
    }

    #[test]
    fn test_split_two_begin_atomic_functions() {
        let sql = "CREATE FUNCTION f() RETURNS void LANGUAGE SQL BEGIN ATOMIC SELECT 1; END; CREATE FUNCTION g() RETURNS void LANGUAGE SQL BEGIN ATOMIC SELECT 2; END;";
        let stmts = split_statements(sql);
        assert_eq!(
            stmts.len(),
            2,
            "two BEGIN ATOMIC functions should be 2 statements: {stmts:?}"
        );
    }

    #[test]
    fn test_reconstruct_with_leading_comment() {
        assert_eq!(
            reconstruct_command_tag("-- drop the old table\nDROP TABLE foo", 0),
            "DROP TABLE"
        );
        assert_eq!(
            reconstruct_command_tag("/* insert */\nINSERT INTO t VALUES (1)", 1),
            "INSERT 0 1"
        );
    }
}
