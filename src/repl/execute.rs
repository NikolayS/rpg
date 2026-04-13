//! Query execution helpers for the REPL.
//!
//! Extracted from `mod.rs` — `execute_query`, `execute_query_extended`,
//! `execute_query_interactive`, `execute_query_extended_interactive`, and helpers.

#![allow(clippy::wildcard_imports)]

use super::ai_commands::{interpret_auto_explain, suggest_error_fix_inline};
use super::*;

// ---------------------------------------------------------------------------
// Query execution (stub — #19 will provide the proper implementation)
// ---------------------------------------------------------------------------

/// `PostgreSQL` built-in numeric type OIDs (typcategory = 'N' in `pg_type`).
/// These are the fixed OIDs assigned to numeric base types in the `PostgreSQL`
/// source; they never change across versions.
const NUMERIC_TYPE_OIDS: &[u32] = &[
    20,   // int8
    21,   // int2
    23,   // int4
    700,  // float4
    701,  // float8
    790,  // money
    1700, // numeric
];

/// `PostgreSQL` built-in string/text type OIDs that must never be right-aligned,
/// even when their values happen to look numeric (e.g. text column with "31").
const TEXT_TYPE_OIDS: &[u32] = &[
    16,   // bool
    17,   // bytea
    18,   // char (single-byte internal type)
    19,   // name (63-byte identifier)
    25,   // text
    114,  // json
    142,  // xml
    194,  // pg_node_tree
    1042, // bpchar (char(n))
    1043, // varchar
    1560, // bit (fixed-length bit string, e.g. "1010")
    1562, // varbit (variable-length bit string)
    3802, // jsonb
    4072, // jsonpath (path expressions like "0.0" look numeric)
];

/// Convert a cell from a tokio-postgres `Row` to an `Option<String>`.
///
/// Extended query results are typed, so we try String first (for text/varchar
/// columns), then common numeric/bool types, falling back to None for NULLs.
#[allow(dead_code)]
fn row_cell_to_string(row: &tokio_postgres::Row, i: usize) -> Option<String> {
    // Attempt to read as Option<String> — works for text, varchar, name, etc.
    if let Ok(v) = row.try_get::<_, Option<String>>(i) {
        return v;
    }
    // Try common numeric types.
    let oid = row.columns().get(i).map_or(0, |c| c.type_().oid());
    match oid {
        20 => {
            return row
                .try_get::<_, Option<i64>>(i)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        }
        21 => {
            return row
                .try_get::<_, Option<i16>>(i)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        }
        23 => {
            return row
                .try_get::<_, Option<i32>>(i)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        }
        700 => {
            return row
                .try_get::<_, Option<f32>>(i)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        }
        701 => {
            return row
                .try_get::<_, Option<f64>>(i)
                .ok()
                .flatten()
                .map(|v| v.to_string())
        }
        16 => {
            return row
                .try_get::<_, Option<bool>>(i)
                .ok()
                .flatten()
                .map(|v: bool| if v { "t".to_owned() } else { "f".to_owned() })
        }
        _ => {}
    }
    None
}

/// Classify a column as numeric based on its `PostgreSQL` type OID.
///
/// Returns `Some(true)` for built-in numeric types, `Some(false)` for
/// string/text types and user-defined types (OID ≥ 16384), and `None`
/// for other built-in types (e.g. xid, cid, regtype) that should fall
/// through to the value heuristic.
fn classify_numeric_by_oid(oid: u32) -> Option<bool> {
    if oid == 0 {
        return None; // OID unknown — use value heuristic
    }
    if NUMERIC_TYPE_OIDS.contains(&oid) {
        return Some(true);
    }
    if TEXT_TYPE_OIDS.contains(&oid) {
        return Some(false); // string type — never right-align
    }
    if oid >= 16384 {
        return Some(false); // user-defined type
    }
    // Other built-in types (xid, cid, regtype, etc.): fall through to value
    // heuristic for correct alignment.
    None
}

/// Infer whether a result-set column should be right-aligned (numeric).
///
/// Returns `true` when every non-NULL, non-empty cell parses as `f64` AND
/// at least one cell has a value, subject to the following exclusions:
///
/// - Column name ends with `_code` (e.g. `sql_error_code`) — these are
///   code/identifier columns that happen to contain digit strings such as
///   SQLSTATE codes (`22003`, `42601`), not numeric quantities.
/// - Value starts with `+` — indicates `to_char()`-formatted text output
///   (e.g. `+456`); raw integer/float columns never emit a leading `+`.
///
/// When `type_oid` is non-zero it is consulted first; user-defined types
/// (OID ≥ 16384) are always treated as non-numeric.  Built-in numeric types
/// (int2/int4/int8/float4/float8/numeric/money) are always numeric.
/// For all other non-zero OIDs the value heuristic is applied.
#[allow(clippy::too_many_lines)]
fn infer_numeric_column(
    col_idx: usize,
    name: &str,
    rows: &[Vec<Option<String>>],
    type_oid: u32,
) -> bool {
    // OID-based classification takes priority when available.
    if let Some(is_num) = classify_numeric_by_oid(type_oid) {
        return is_num;
    }
    // Column-name heuristics: certain names always indicate text, not numbers.
    let name_lc = name.to_lowercase();
    // Names ending with _code → identifier/code columns (e.g. sql_error_code).
    if name_lc.ends_with("_code") {
        return false;
    }
    // Common text-returning functions: unaliased calls produce column names
    // matching the function name. These never hold numeric data.
    if matches!(
        name_lc.as_str(),
        "to_char"
            | "substring"
            | "substr"
            | "concat"
            | "concat_ws"
            | "format"
            | "initcap"
            | "trim"
            | "ltrim"
            | "rtrim"
            | "replace"
            | "regexp_replace"
            | "regexp_substr"
            | "regexp_match"
            | "translate"
            | "overlay"
            | "lpad"
            | "rpad"
            | "repeat"
            | "reverse"
            | "left"
            | "right"
            | "md5"
            | "encode"
            | "decode"
            | "quote_ident"
            | "quote_literal"
            | "quote_nullable"
            | "to_hex"
            | "chr"
            // PostgreSQL type names used as column labels for casts like
            // '1'::json.  These are always text-typed and must be
            // left-aligned regardless of value contents.
            | "json"
            | "jsonb"
            | "xml"
            | "text"
            | "varchar"
            | "bytea"
            | "name"
            | "regtype"
            | "regclass"
            | "regproc"
            | "regprocedure"
            | "regoper"
            | "regoperator"
            | "regconfig"
            | "regdictionary"
            // System catalog columns with non-numeric types that can hold
            // numeric-looking values.
            // oidvector columns (space-separated OIDs): left-aligned in psql.
            | "proargtypes"
            | "indclass"
            | "indkey"
            | "indoption"
            | "proargmodes"
            | "proallargtypes"
    ) {
        return false;
    }

    let mut has_value = false;
    let all_parseable = rows.iter().all(|row| {
        match row.get(col_idx).and_then(|v| v.as_deref()) {
            None | Some("") => true,
            Some(val) => {
                has_value = true;
                // Leading '+' or a zero followed by another digit (e.g.
                // '0000000000000456', '010101') indicates to_char()-formatted
                // or bit-string output, not raw numeric data.
                if val.starts_with('+') {
                    return false;
                }
                if val.len() > 1
                    && val.starts_with('0')
                    && val.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                {
                    return false;
                }
                // PostgreSQL numeric NaN: always "NaN" (exactly).
                // Rust's f64 parser does not accept "NaN" on all platforms,
                // so check explicitly before falling through to parse::<f64>.
                if val == "NaN" {
                    return true;
                }
                // PostgreSQL money format: '$N.NN' or '-$N.NN'.
                // Strip the currency prefix before testing parseability.
                let numeric_part = if let Some(rest) = val.strip_prefix("$") {
                    rest
                } else if let Some(rest) = val.strip_prefix("-$") {
                    rest
                } else {
                    val
                };
                // Also accept "Infinity"/"-Infinity" (capital I) which
                // PostgreSQL uses for numeric/float types.
                if numeric_part == "Infinity" || numeric_part == "-Infinity" {
                    return true;
                }
                numeric_part.parse::<f64>().is_ok()
            }
        }
    });
    if !(all_parseable && has_value) {
        return false;
    }

    // Infinity guard: lowercase "infinity"/"−infinity" appears in timestamp
    // columns (left-aligned in psql) but NOT in numeric/float columns (which
    // always use capital "Infinity"/"-Infinity").  Only suppress numeric
    // inference for the lowercase variant; capital-I Infinity is unambiguous.
    let all_lowercase_infinity =
        rows.iter()
            .all(|row| match row.get(col_idx).and_then(|v| v.as_deref()) {
                None | Some("") => true,
                Some(v) => v == "infinity" || v == "-infinity",
            });
    if all_lowercase_infinity {
        return false;
    }

    // Cross-column interval guard: if this column's values are all "0"/"-0"/NULL
    // (ambiguous zero), check whether any sibling column has interval-like values
    // (e.g. "1-2", "1 2:03:04").  If so, treat this column as non-numeric to
    // match psql's type-OID-based left-alignment for interval zero.
    let only_zero_or_null = rows.iter().all(|row| {
        matches!(
            row.get(col_idx).and_then(|v| v.as_deref()),
            None | Some("" | "0" | "-0")
        )
    });
    if only_zero_or_null {
        let has_interval_sibling = rows.iter().any(|row| {
            row.iter().enumerate().any(|(idx, cell)| {
                if idx == col_idx {
                    return false;
                }
                let v = match cell.as_deref() {
                    Some(s) if !s.is_empty() => s,
                    _ => return false,
                };
                // Interval patterns: "N-M" (year-month), "N H:MM:SS" (day-time),
                // or a time-like value containing ':' but NOT a timestamp.
                //
                // Timestamps look like "1997-02-11 01:32:01+00" — they start
                // with a 4-digit year followed by '-'.  Pure interval time
                // components look like "02:03:04" or "1 02:03:04" (integer +
                // space + time).  We distinguish by checking whether the first
                // non-space token before any ':' looks like a date (4-digit
                // year) or a small integer (interval).
                //
                // Dates like "2005-07-21" have exactly 2 hyphens; year-month
                // intervals like "1-2" have exactly 1 hyphen.  Filter out dates
                // by requiring only one hyphen in the N-M check.
                let looks_like_timestamp = v.len() > 10
                    && v.as_bytes().get(4) == Some(&b'-')
                    && v[..4].bytes().all(|b| b.is_ascii_digit());
                (v.contains(':') && !looks_like_timestamp)
                    || (v.len() >= 3
                        && v.bytes().filter(|&b| b == b'-').count() == 1
                        && v.chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_digit() || c == '-')
                        && v.chars().any(|c| c.is_ascii_digit())
                        && v.parse::<f64>().is_err())
            })
        });
        if has_interval_sibling {
            return false;
        }
    }

    true
}

/// Print a single result set using the active [`PsetConfig`].
///
/// `col_names` and `rows` describe the result set. `is_select` indicates
/// whether this was a SELECT-like statement (i.e. we received a
/// `RowDescription` message, even if zero rows followed). `rows_affected`
/// carries the `CommandComplete` count. `sql` is the original SQL statement,
/// used to reconstruct the full psql-style command tag (e.g. `"INSERT 0 1"`).
/// `is_first` is `false` when this is a subsequent result set in a
/// multi-statement query, in which case a blank separator line is printed
/// before the table (matching psql behaviour).
/// `writer` is the output destination (stdout or a redirected file).
#[allow(clippy::too_many_arguments)]
pub(super) fn print_result_set_pset(
    writer: &mut dyn io::Write,
    col_names: &[String],
    col_oids: &[u32],
    rows: &[Vec<Option<String>>],
    is_select: bool,
    rows_affected: u64,
    sql: &str,
    is_first: bool,
    pset: &crate::output::PsetConfig,
    quiet: bool,
) {
    use crate::output::format_rowset_pset;
    use crate::query::{ColumnMeta, RowSet};

    if is_select {
        // Heuristic: psql right-aligns numeric columns using type OIDs from
        // the wire protocol.  The simple query protocol does not expose OIDs,
        // so we infer numeric columns by inspecting cell values.  A column is
        // treated as numeric if every non-NULL, non-empty cell in that column
        // parses as an f64 (covers integers, decimals, and scientific notation).
        // Columns that are entirely NULL/empty are NOT marked numeric.
        //
        // Note: `col_names` may be empty for zero-column SELECTs such as
        // `SELECT FROM t WHERE ...`.  These are valid PostgreSQL queries that
        // return rows with no columns.  We must still render the row-count
        // footer (e.g. `(1 row)`) to match psql behaviour.
        //
        // SHOW commands return a single text column regardless of value content.
        // psql left-aligns SHOW output because the underlying type is always text.
        let is_show = sql
            .trim_start()
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case("show"));
        let columns: Vec<ColumnMeta> = col_names
            .iter()
            .enumerate()
            .map(|(col_idx, n)| ColumnMeta {
                name: n.clone(),
                is_numeric: !is_show
                    && infer_numeric_column(
                        col_idx,
                        n,
                        rows,
                        col_oids.get(col_idx).copied().unwrap_or(0),
                    ),
            })
            .collect();

        let rs = RowSet {
            columns,
            rows: rows.to_vec(),
        };

        let mut out = String::new();
        format_rowset_pset(&mut out, &rs, pset);
        // format_rowset_pset appends a trailing blank line so that output
        // matches psql's consistent blank line after every result set.
        // No extra separator is needed before subsequent results.
        let _ = writer.write_all(out.as_bytes());

        // DML with RETURNING also emits a command tag in psql (e.g. INSERT 0 1).
        // Detect INSERT/UPDATE/DELETE/MERGE that produced a RowDescription.
        if !quiet {
            let tag = crate::query::reconstruct_command_tag(sql, rows_affected);
            if !tag.is_empty()
                && tag.split_once(' ').is_some_and(|(verb, _)| {
                    matches!(verb, "INSERT" | "UPDATE" | "DELETE" | "MERGE")
                })
            {
                let _ = writeln!(writer, "{tag}");
            }
        }
    } else if !quiet {
        // Non-SELECT statement: show the psql-style command tag.
        // tokio-postgres 0.7 only exposes the numeric count from
        // CommandComplete; reconstruct the full tag from the SQL.
        // Suppressed in quiet mode (-q), matching psql behaviour.
        let tag = crate::query::reconstruct_command_tag(sql, rows_affected);
        if !tag.is_empty() {
            if !is_first {
                let _ = writeln!(writer);
            }
            let _ = writeln!(writer, "{tag}");
        }
    }
}

/// In single-step mode, prompt the user before each command.
///
/// Prints the command to stderr and asks "Execute? (y/n)".
/// Returns `true` if the user confirms (or single-step is not enabled).
pub(super) fn confirm_single_step(sql: &str) -> bool {
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

    // Split-execution guard: if the batch mixes regular statements with
    // statements that cannot run inside a transaction block (ALTER SYSTEM,
    // VACUUM, etc.), execute each statement individually.  PostgreSQL wraps
    // multi-statement simple-query strings in an implicit transaction, which
    // would otherwise cause "cannot run inside a transaction block" errors.
    //
    // When `exec_verbatim` is set (e.g. for \; combined batches), skip this
    // guard so the batch is sent as a single Query, preserving PostgreSQL's
    // implicit-transaction semantics (matching psql behaviour).
    if !settings.exec_verbatim && needs_split_execution(interpolated.as_str()) {
        let stmts = crate::query::split_statements(interpolated.as_str());
        let mut all_ok = true;
        for stmt in stmts {
            // Each statement goes through the full execute_query pipeline
            // (auto-explain, safety checks, echo, timing, etc.).
            let ok = Box::pin(execute_query(client, &stmt, settings, tx)).await;
            if !ok {
                all_ok = false;
                // Continue executing remaining statements (psql behaviour).
            }
        }
        return all_ok;
    }

    // Auto-EXPLAIN: prepend EXPLAIN prefix when enabled.
    // Skip for statements that are already EXPLAIN, or for
    // non-query statements (SET, BEGIN, COMMIT, etc.).
    let auto_explained;
    let mut auto_explain_active = false;
    let auto_explain_label = settings.auto_explain.label();
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
                return false; // not executed — caller must not assume DDL ran
            }
        }
    }

    // -a / --echo-all: print every statement to stdout before executing.
    // This matches psql's `-a` flag and is required to reproduce the output
    // format of pg_regress (which runs `psql -a -q`).
    if settings.echo_all {
        if let Some(ref mut w) = settings.output_target {
            let _ = writeln!(w, "{sql_to_send}");
        } else {
            println!("{sql_to_send}");
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

    // Always capture start time when timing display or status bar is active.
    let needs_timing = settings.timing || settings.statusline.is_some();
    let start = if needs_timing {
        Some(Instant::now())
    } else {
        None
    };

    // Capture auto-EXPLAIN plan text for optional AI interpretation.
    let mut auto_explain_plan: Option<String> = None;
    // Whether the original SQL is a manual EXPLAIN statement (not auto-explain).
    let is_manual_explain = is_explain_statement(sql_to_send) && !auto_explain_active;

    // ON_ERROR_ROLLBACK: when enabled and inside a transaction, wrap the
    // statement with an implicit savepoint so that errors do not abort the
    // entire transaction — matching psql behaviour.
    let use_implicit_savepoint = *tx == TxState::InTransaction && {
        let oer = settings.vars.get("ON_ERROR_ROLLBACK").unwrap_or("off");
        // "interactive" is treated same as "on" for now.
        (oer.eq_ignore_ascii_case("on") || oer.eq_ignore_ascii_case("interactive"))
            && !is_transaction_control_command(sql_to_send)
    };

    if use_implicit_savepoint {
        // Create the savepoint silently before executing the user's statement.
        let _ = client.simple_query("SAVEPOINT pg_psql_savepoint").await;
    }

    // Use simple_query_raw to stream messages one at a time.  This allows us
    // to display intermediate result sets from statements that completed
    // before an error occurs in a later statement of the same batch, matching
    // psql's behaviour for \; multi-statement queries.
    use futures::StreamExt as _;
    use tokio_postgres::SimpleQueryMessage;

    let stream_result = client.simple_query_raw(sql_to_send).await;
    // Pin the stream so we can call .next() on it (SimpleQueryStream is !Unpin).
    let mut stream = match stream_result {
        Ok(s) => Box::pin(s),
        Err(e) => {
            // Connection-level error before any messages were sent.
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            settings.last_stmt_produced_rows = false;
            tx.on_error();
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: error_message.clone(),
                sqlstate: sqlstate.clone(),
            });
            settings.vars.set("LAST_ERROR_MESSAGE", &error_message);
            settings
                .vars
                .set("LAST_ERROR_SQLSTATE", sqlstate.as_deref().unwrap_or(""));
            if settings.config.ai.auto_explain_errors {
                suggest_error_fix_inline(sql_to_send, &error_message, settings).await;
            }
            if is_sql_error
                && settings.auto_suggest_fix
                && !settings.last_was_fix
                && settings
                    .config
                    .ai
                    .provider
                    .as_deref()
                    .is_some_and(|p| !p.is_empty())
            {
                eprintln!("\x1b[2mHint: type /fix to auto-correct this query\x1b[0m");
            }
            // Store timing before returning.
            if let Some(t) = start {
                let elapsed = t.elapsed();
                #[allow(clippy::cast_possible_truncation)]
                let elapsed_ms = elapsed.as_millis() as u64;
                if settings.timing {
                    let line = format!("Time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0);
                    if let Some(ref mut w) = settings.output_target {
                        let _ = w.write_all(line.as_bytes());
                    } else {
                        let _ = io::stdout().write_all(line.as_bytes());
                    }
                }
                settings.last_query_duration_ms = Some(elapsed_ms);
            }
            settings.last_was_fix = false;
            return false;
        }
    };

    let mut col_names: Vec<String> = Vec::new();
    let mut col_oids: Vec<u32> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    // `is_select` is set to true when we receive a RowDescription
    // message (or any Row message).  This distinguishes an empty
    // SELECT (zero rows but column headers) from a DML command.
    let mut is_select = false;
    let mut result_set_index: usize = 0;
    // Tracks the db error from a failed statement in the stream, if any.
    let mut stream_error: Option<tokio_postgres::Error> = None;

    while let Some(item) = stream.next().await {
        match item {
            Ok(msg) => match msg {
                SimpleQueryMessage::RowDescription(cols) => {
                    // Emitted before data rows (or before CommandComplete
                    // when zero rows matched).  Capture column names here
                    // so that empty result sets still show their headers.
                    is_select = true;
                    if col_names.is_empty() {
                        col_names = cols.iter().map(|c| c.name().to_owned()).collect();
                        col_oids = cols
                            .iter()
                            .map(tokio_postgres::SimpleColumn::type_oid)
                            .collect();
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
                        col_oids = (0..row.len())
                            .map(|i| {
                                row.columns()
                                    .get(i)
                                    .map_or(0, tokio_postgres::SimpleColumn::type_oid)
                            })
                            .collect();
                    }
                    let vals: Vec<Option<String>> = (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect();
                    rows.push(vals);
                }
                SimpleQueryMessage::CommandComplete(n) => {
                    // Capture plan text from auto-EXPLAIN before clearing
                    // rows. EXPLAIN output is a single-column result set.
                    if (auto_explain_active || is_manual_explain) && result_set_index == 0 {
                        let plan_text: String = rows
                            .iter()
                            .filter_map(|r| r.first().and_then(|v| v.as_deref()).map(str::to_owned))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !plan_text.is_empty() {
                            if auto_explain_active {
                                auto_explain_plan = Some(plan_text.clone());
                            }
                            // Store for `\explain share` regardless of mode.
                            settings.last_explain_text = Some(plan_text);
                        }
                    }

                    // Flush the current result set, then reset for next
                    // statement in a multi-statement query.
                    // Capture rendered output so we can mirror to log.
                    let mut out_buf = Vec::<u8>::new();

                    // Print "[auto-explain: <mode>]" header before the
                    // plan output so users know EXPLAIN was prepended.
                    if auto_explain_active && result_set_index == 0 {
                        let _ = writeln!(out_buf, "[auto-explain: {auto_explain_label}]");
                    }

                    print_result_set_pset(
                        &mut out_buf,
                        &col_names,
                        &col_oids,
                        &rows,
                        is_select,
                        n,
                        sql_to_send,
                        result_set_index == 0,
                        &settings.pset,
                        settings.quiet,
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

                    // Store row count for audit log entry.
                    if result_set_index == 0 {
                        settings.last_row_count = Some(n);
                    }
                    // Signal to exec_lines that a result set was produced
                    // (used to decide whether to echo following blank lines).
                    // psql echoes blank lines ONLY after pure SELECT-like
                    // statements in aligned format. DML+RETURNING statements
                    // (INSERT/UPDATE/DELETE) also produce rows but psql does
                    // NOT echo blanks after them (they emit a separate command
                    // tag and format_rowset_pset already appends a blank).
                    // Unaligned and tuples-only modes are excluded too.
                    use crate::output::OutputFormat;
                    if is_select
                        && !settings.pset.tuples_only
                        && matches!(
                            settings.pset.format,
                            OutputFormat::Aligned | OutputFormat::Wrapped
                        )
                        && is_pure_select(sql_to_send)
                    {
                        settings.last_stmt_produced_rows = true;
                    }

                    result_set_index += 1;
                    col_names.clear();
                    col_oids.clear();
                    rows.clear();
                    is_select = false;
                }
                _ => {}
            },
            Err(e) => {
                // A statement in the batch failed.  Display the error, then
                // break — the stream is done (ReadyForQuery follows the error).
                stream_error = Some(e);
                break;
            }
        }
    }
    // Drop the stream so the connection is ready for the next query.
    drop(stream);

    let success = if let Some(e) = stream_error {
        // ON_ERROR_ROLLBACK: roll back to the implicit savepoint so
        // the transaction stays alive (not aborted), then release it
        // to avoid accumulating savepoints — matching psql behaviour.
        if use_implicit_savepoint {
            let _ = client
                .simple_query("ROLLBACK TO SAVEPOINT pg_psql_savepoint")
                .await;
            let _ = client
                .simple_query("RELEASE SAVEPOINT pg_psql_savepoint")
                .await;
        }

        // -b / --echo-errors: echo the failing query to stderr.
        if settings.echo_errors {
            eprintln!("{sql_to_send}");
        }
        crate::output::eprint_db_error_located(
            settings.error_location_prefix().as_deref(),
            &e,
            Some(sql_to_send),
            settings.verbose_errors,
            settings.terse_errors,
            settings.sqlstate_errors,
        );
        // A failed query doesn't produce rows; psql does not echo blank
        // lines after error messages.
        settings.last_stmt_produced_rows = false;

        // Only transition to Failed state when implicit savepoint is
        // NOT active — the rollback-to-savepoint keeps the tx alive.
        if !use_implicit_savepoint {
            tx.on_error();
        }

        // For multi-statement batches, PostgreSQL processes ALL statements
        // in simple-query mode even after an error (subsequent ones fail
        // with "current transaction is aborted").  If the batch ends with
        // COMMIT / ROLLBACK / END / ABORT, the server rolled back and
        // returned to Idle — mirror that here so rpg's state stays in sync.
        {
            let stmts = crate::query::split_statements(&interpolated);
            // Skip trailing comment-only entries (artefacts of inline SQL
            // comments after the last real statement in a \; batch).
            let last_sql_stmt = stmts
                .iter()
                .rev()
                .find(|s| !s.trim_start().starts_with("--"))
                .map_or("", String::as_str);
            if !last_sql_stmt.is_empty() {
                tx.apply_terminal(last_sql_stmt);
            }

            // In multi-statement batches, COMMIT/ROLLBACK inside the batch
            // cannot roll back a failed transaction — they also fail with
            // "transaction aborted".  psql avoids this by sending statements
            // individually.  We attempt to replicate psql's per-statement
            // semantics by re-sending recovery statements after a failure.
            if stmts.len() > 1 {
                let first_upper = stmts[0].trim().to_uppercase();
                let first_word = first_upper.split_whitespace().next().unwrap_or("");
                let last_upper = last_sql_stmt.to_uppercase();
                let mut last_words = last_upper.split_whitespace();
                let last_first = last_words.next().unwrap_or("");
                let last_second = last_words.next().unwrap_or("");

                if matches!(first_word, "BEGIN" | "START") {
                    // The batch opened a transaction that failed midway.
                    // COMMIT/ROLLBACK inside the batch also failed, so the
                    // connection is left in E state.  Send a standalone
                    // ROLLBACK to restore to Idle, matching psql semantics.
                    //
                    // Exception: if the batch ends with COMMIT/ROLLBACK AND
                    // CHAIN, an earlier COMMIT/ROLLBACK already closed the
                    // original transaction.  The server is already Idle, so
                    // sending ROLLBACK would produce a spurious "no transaction
                    // in progress" warning.
                    let ends_with_and_chain = last_upper.contains("AND CHAIN");
                    if !ends_with_and_chain {
                        let _ = client.simple_query("ROLLBACK").await;
                        *tx = crate::repl::TxState::Idle;
                    }
                } else if matches!(last_first, "ROLLBACK" | "ABORT")
                    && last_second == "TO"
                    && *tx == crate::repl::TxState::Failed
                {
                    // Batch ends with ROLLBACK TO <savepoint>: the DELETE/etc.
                    // failed in the middle, leaving the batch's ROLLBACK TO
                    // unable to execute.  Re-send it standalone so the
                    // savepoint can actually be rolled back, matching psql's
                    // per-statement execution where this succeeds.
                    if client.simple_query(last_sql_stmt).await.is_ok() {
                        *tx = crate::repl::TxState::InTransaction;
                    }
                }
            }
        }

        // Capture context for /fix.
        let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
        let is_sql_error = e.as_db_error().is_some();
        let error_message = e
            .as_db_error()
            .map_or_else(|| e.to_string(), |db| db.message().to_owned());
        settings.last_error = Some(LastError {
            query: sql_to_send.to_owned(),
            error_message: error_message.clone(),
            sqlstate: sqlstate.clone(),
        });
        // Update psql-compatible error variables for use in subsequent commands.
        settings.vars.set("LAST_ERROR_MESSAGE", &error_message);
        settings
            .vars
            .set("LAST_ERROR_SQLSTATE", sqlstate.as_deref().unwrap_or(""));

        // Inline error suggestion: if AI is configured and
        // auto_explain_errors is on, show a brief LLM hint.
        if settings.config.ai.auto_explain_errors {
            suggest_error_fix_inline(sql_to_send, &error_message, settings).await;
        }

        // Auto-suggest /fix: show a dim hint pointing the user to /fix.
        // Only shown for SQL errors (not connection errors), when AI is
        // configured, auto_suggest_fix is enabled, and the user did not
        // just invoke /fix (to avoid hint loops).
        if is_sql_error
            && settings.auto_suggest_fix
            && !settings.last_was_fix
            && settings
                .config
                .ai
                .provider
                .as_deref()
                .is_some_and(|p| !p.is_empty())
        {
            eprintln!("\x1b[2mHint: type /fix to auto-correct this query\x1b[0m");
        }

        false
    } else {
        // ON_ERROR_ROLLBACK: release the implicit savepoint on success.
        if use_implicit_savepoint {
            let _ = client
                .simple_query("RELEASE SAVEPOINT pg_psql_savepoint")
                .await;
        }

        // Update transaction state based on what SQL was sent.
        tx.update_from_sql(sql_to_send);

        // Detect mid-session changes to standard_conforming_strings.
        // When the user runs `SET standard_conforming_strings = off/on`,
        // we need to update the tokenizer's SCS tracking immediately so
        // subsequent input is parsed correctly.
        update_scs_if_changed(sql_to_send, client, settings).await;

        true
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        // as_millis() returns u128; truncate to u64 (safe for any realistic duration).
        #[allow(clippy::cast_possible_truncation)]
        let elapsed_ms = elapsed.as_millis() as u64;
        // Timing output is written through the active output target so that
        // it appears after the result set (matching psql behaviour).  When a
        // pager-capture buffer is active the timing line ends up in the same
        // buffer as the results and is displayed in the correct order.
        if settings.timing {
            let line = format!("Time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0);
            if let Some(ref mut w) = settings.output_target {
                let _ = w.write_all(line.as_bytes());
            } else {
                let _ = io::stdout().write_all(line.as_bytes());
            }
        }
        // Store duration for the status bar.
        settings.last_query_duration_ms = Some(elapsed_ms);
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

    // Always clear the /fix-loop guard after each execution so the next
    // query (regardless of whether this one succeeded or failed) can show
    // the hint again if appropriate.
    settings.last_was_fix = false;

    success
}

/// Re-query `standard_conforming_strings` when the executed SQL looks like it
/// may have changed the GUC.
///
/// This is intentionally conservative: we only fire the extra `SHOW` when the
/// SQL contains both `standard_conforming_strings` and a SET-like keyword.
async fn update_scs_if_changed(sql: &str, client: &Client, settings: &mut ReplSettings) {
    let upper = sql.to_uppercase();
    if upper.contains("STANDARD_CONFORMING_STRINGS")
        && (upper.starts_with("SET ") || upper.starts_with("RESET "))
    {
        settings.db_capabilities.standard_conforming_strings =
            crate::capabilities::detect_standard_conforming_strings_pub(client).await;
    }
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
                return false; // not executed — caller must not assume DDL ran
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

    // Always capture start time when timing display or status bar is active.
    let needs_timing_ext = settings.timing || settings.statusline.is_some();
    let start = if needs_timing_ext {
        Some(Instant::now())
    } else {
        None
    };

    // ON_ERROR_ROLLBACK: same implicit savepoint logic as simple_query path.
    let use_implicit_savepoint_ext = *tx == TxState::InTransaction && {
        let oer = settings.vars.get("ON_ERROR_ROLLBACK").unwrap_or("off");
        (oer.eq_ignore_ascii_case("on") || oer.eq_ignore_ascii_case("interactive"))
            && !is_transaction_control_command(sql_to_send)
    };

    if use_implicit_savepoint_ext {
        let _ = client.simple_query("SAVEPOINT pg_psql_savepoint").await;
    }

    // Prepare the statement so that the server can describe its columns.
    let stmt = match client.prepare(sql_to_send).await {
        Ok(s) => s,
        Err(e) => {
            // ON_ERROR_ROLLBACK: roll back to the implicit savepoint
            // then release it to avoid accumulating savepoints.
            if use_implicit_savepoint_ext {
                let _ = client
                    .simple_query("ROLLBACK TO SAVEPOINT pg_psql_savepoint")
                    .await;
                let _ = client
                    .simple_query("RELEASE SAVEPOINT pg_psql_savepoint")
                    .await;
            }

            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            if !use_implicit_savepoint_ext {
                tx.on_error();
            }
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: error_message.clone(),
                sqlstate: sqlstate.clone(),
            });
            settings.vars.set("LAST_ERROR_MESSAGE", &error_message);
            settings
                .vars
                .set("LAST_ERROR_SQLSTATE", sqlstate.as_deref().unwrap_or(""));
            // Auto-suggest /fix hint for SQL errors when AI is configured.
            if is_sql_error
                && settings.auto_suggest_fix
                && !settings.last_was_fix
                && settings
                    .config
                    .ai
                    .provider
                    .as_deref()
                    .is_some_and(|p| !p.is_empty())
            {
                eprintln!("\x1b[2mHint: type /fix to auto-correct this query\x1b[0m");
            }
            settings.last_was_fix = false;
            return false;
        }
    };

    // Get column metadata from the prepared statement for display.
    let col_names: Vec<String> = stmt.columns().iter().map(|c| c.name().to_owned()).collect();
    let col_oids: Vec<u32> = stmt.columns().iter().map(|c| c.type_().oid()).collect();

    // Substitute $N parameters directly into the SQL and execute via
    // simple_query.  This matches psql's text-parameter semantics: each
    // bound value is treated as a text literal that the server coerces
    // to the expected type (e.g. "2" → int via `$1::int`).
    let parameterised_sql = substitute_bind_params(sql_to_send, params);

    let success = match client.simple_query(&parameterised_sql).await {
        Ok(messages) => {
            use crate::output::format_rowset_pset;
            use crate::query::{ColumnMeta, RowSet};
            use tokio_postgres::SimpleQueryMessage;

            let mut row_data: Vec<Vec<Option<String>>> = Vec::new();
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    let vals: Vec<Option<String>> = (0..row.len())
                        .map(|i| row.get(i).map(str::to_owned))
                        .collect();
                    row_data.push(vals);
                }
            }

            if !col_names.is_empty() || !row_data.is_empty() {
                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .enumerate()
                    .map(|(col_idx, n)| ColumnMeta {
                        name: n.clone(),
                        is_numeric: infer_numeric_column(
                            col_idx,
                            n,
                            &row_data,
                            col_oids.get(col_idx).copied().unwrap_or(0),
                        ),
                    })
                    .collect();
                let row_count = row_data.len();
                let rs = RowSet {
                    columns,
                    rows: row_data,
                };
                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &settings.pset);
                let out_bytes = out.as_bytes();
                settings.last_row_count = Some(row_count as u64);
                settings.last_stmt_produced_rows = true;
                if let Some(ref mut lf) = settings.log_file {
                    let _ = lf.write_all(out_bytes);
                }
                if let Some(ref mut w) = settings.output_target {
                    let _ = w.write_all(out_bytes);
                } else {
                    let _ = io::stdout().write_all(out_bytes);
                }
            }

            // ON_ERROR_ROLLBACK: release the implicit savepoint on success.
            if use_implicit_savepoint_ext {
                let _ = client
                    .simple_query("RELEASE SAVEPOINT pg_psql_savepoint")
                    .await;
            }

            tx.update_from_sql(sql_to_send);
            true
        }
        Err(e) => {
            // ON_ERROR_ROLLBACK: roll back to the implicit savepoint
            // then release it to avoid accumulating savepoints.
            if use_implicit_savepoint_ext {
                let _ = client
                    .simple_query("ROLLBACK TO SAVEPOINT pg_psql_savepoint")
                    .await;
                let _ = client
                    .simple_query("RELEASE SAVEPOINT pg_psql_savepoint")
                    .await;
            }

            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            if !use_implicit_savepoint_ext {
                tx.on_error();
            }

            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: error_message.clone(),
                sqlstate: sqlstate.clone(),
            });
            settings.vars.set("LAST_ERROR_MESSAGE", &error_message);
            settings
                .vars
                .set("LAST_ERROR_SQLSTATE", sqlstate.as_deref().unwrap_or(""));

            // Auto-suggest /fix hint for SQL errors when AI is configured.
            if is_sql_error
                && settings.auto_suggest_fix
                && !settings.last_was_fix
                && settings
                    .config
                    .ai
                    .provider
                    .as_deref()
                    .is_some_and(|p| !p.is_empty())
            {
                eprintln!("\x1b[2mHint: type /fix to auto-correct this query\x1b[0m");
            }

            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        #[allow(clippy::cast_possible_truncation)]
        let elapsed_ms = elapsed.as_millis() as u64;
        // Timing output is written through the active output target so that
        // it appears after the result set (matching psql behaviour).
        if settings.timing {
            let line = format!("Time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0);
            if let Some(ref mut w) = settings.output_target {
                let _ = w.write_all(line.as_bytes());
            } else {
                let _ = io::stdout().write_all(line.as_bytes());
            }
        }
        settings.last_query_duration_ms = Some(elapsed_ms);
    }

    if success {
        settings.last_query = Some(sql.to_owned());
        // Clear last_error on success so /fix isn't stale.
        settings.last_error = None;
        // Increment session query counter.
        settings.query_count = settings.query_count.saturating_add(1);
    }

    // Always clear the /fix-loop guard after each execution.
    settings.last_was_fix = false;

    success
}

/// Execute a named prepared statement with the given parameters.
///
/// Returns `true` on success, `false` on error.  If `stmt_name` is not
/// found in `settings.named_statements`, an error message is printed.
pub(super) async fn execute_named_stmt(
    client: &Client,
    stmt_name: &str,
    params: &[String],
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    if !settings.named_statements.contains(stmt_name) {
        eprintln!("ERROR:  prepared statement \"{stmt_name}\" does not exist");
        return false;
    }

    // Use SQL EXECUTE for all statements (both named and empty-name).
    // Empty name → EXECUTE __rpg_unnamed (internal alias).
    let sql_name = if stmt_name.is_empty() {
        "\"__rpg_unnamed\"".to_owned()
    } else {
        // Quote as SQL identifier to prevent injection.
        format!("\"{}\"", stmt_name.replace('"', "\"\""))
    };

    let execute_sql = if params.is_empty() {
        format!("EXECUTE {sql_name}")
    } else {
        let param_list: Vec<String> = params
            .iter()
            .map(|p| {
                // Escape single quotes by doubling them (SQL standard).
                let escaped = p.replace('\'', "''");
                format!("'{escaped}'")
            })
            .collect();
        format!("EXECUTE {sql_name}({})", param_list.join(", "))
    };

    // Suppress echo_all: psql uses the extended protocol for \bind_named,
    // so the EXECUTE statement is never echoed.  We use simple-query EXECUTE
    // but should not echo the internally-generated SQL.
    let saved_echo = settings.echo_all;
    settings.echo_all = false;
    let result = execute_query(client, &execute_sql, settings, tx).await;
    settings.echo_all = saved_echo;
    result
}

// ---------------------------------------------------------------------------
// \g / \gx buffer execution helpers (#46)
// ---------------------------------------------------------------------------

/// Execute `buf` and write output to `path`, creating or truncating the file.
///
/// The caller is responsible for clearing `buf` after this returns.
pub(super) async fn execute_to_file(
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
        Err(e) => {
            // Match psql's error format: "error: {path}: {os error string}"
            // Rust appends " (os error N)" to the OS message; strip it.
            let full = e.to_string();
            let msg = full
                .find(" (os error ")
                .map_or(full.as_str(), |pos| &full[..pos]);
            eprintln!("error: {path}: {msg}");
        }
    }
}

/// A [`Write`] wrapper backed by a shared `Arc<Mutex<Vec<u8>>>` so that the
/// captured bytes can be retrieved after the writer is boxed and erased.
pub(super) struct CapturingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

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
pub(super) async fn execute_piped(
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

/// Return the first keyword of `sql` in uppercase, ignoring leading whitespace.
///
/// Used by multiple helpers that need to classify SQL statements by their
/// opening keyword without allocating a full uppercase copy of the input.
fn first_keyword_upper(sql: &str) -> String {
    sql.split_whitespace().next().unwrap_or("").to_uppercase()
}

/// Return `true` if `sql` is an EXPLAIN statement (any variant).
fn is_explain_statement(sql: &str) -> bool {
    first_keyword_upper(sql) == "EXPLAIN"
}

/// Return `true` if `sql` is a transaction control command that should NOT
/// be wrapped with implicit savepoints by ON_ERROR_ROLLBACK.
///
/// psql excludes: BEGIN, START TRANSACTION, COMMIT, END, ROLLBACK, ABORT,
/// SAVEPOINT, RELEASE SAVEPOINT, ROLLBACK TO SAVEPOINT, PREPARE TRANSACTION.
fn is_transaction_control_command(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    let mut tokens = upper.split_whitespace();
    let first = tokens.next().unwrap_or("");
    let second = tokens.next().unwrap_or("");

    matches!(first, "BEGIN" | "COMMIT" | "END" | "ABORT" | "SAVEPOINT")
        || (first == "START" && second == "TRANSACTION")
        || (first == "PREPARE" && second == "TRANSACTION")
        || first == "ROLLBACK"
        || first == "RELEASE"
}

/// Strip psql's aligned table formatting from EXPLAIN output.
///
/// When EXPLAIN results come back through `print_result_set_pset`, they are
/// rendered as an aligned table with a `QUERY PLAN` header, border lines, and
/// a `(N rows)` footer.  The EXPLAIN text parser expects plain lines without
/// this decoration, but WITH the original indentation preserved.
///
/// `PostgreSQL`'s aligned format adds a single leading space before each
/// column value.  Plan node indentation (2+ spaces) is part of the plan
/// text itself and must be preserved so that [`super::explain::parse`] can
/// reconstruct the parent-child tree from raw indent levels.
fn strip_psql_table_format(formatted: &str) -> String {
    let mut lines = Vec::new();
    for line in formatted.lines() {
        let trimmed = line.trim();
        // Skip table border lines: "---+---", "------", etc.
        if trimmed.starts_with('-') && trimmed.chars().all(|c| c == '-' || c == '+') {
            continue;
        }
        // Skip the QUERY PLAN header line.
        if trimmed == "QUERY PLAN" {
            continue;
        }
        // Skip (N rows) / (N row) footer lines.
        if trimmed.starts_with('(') && (trimmed.ends_with("rows)") || trimmed.ends_with("row)")) {
            continue;
        }
        // Skip blank lines.
        if trimmed.is_empty() {
            continue;
        }

        // Strip table decoration while PRESERVING internal indentation.
        //
        // psql aligned format:
        //   - Pipe-delimited: "| <content> |" — strip the "| " prefix and " |" suffix.
        //   - Space-padded:   " <content>  " — strip exactly one leading space.
        //     psql adds a single space before column content; the plan's own
        //     indentation (2+ spaces) follows that single space and must be kept.
        let content: &str = if let Some(inner) = trimmed.strip_prefix("| ") {
            // Pipe-delimited format: strip the leading "| " and trailing " |".
            inner.strip_suffix(" |").unwrap_or(inner).trim_end()
        } else if let Some(inner) = trimmed.strip_prefix('|') {
            inner.strip_suffix('|').unwrap_or(inner).trim()
        } else {
            // Space-padded format: psql adds exactly one space before column
            // content.  Strip that single leading space to get the raw plan
            // text.  Additional leading spaces are the plan's own indentation.
            line.strip_prefix(' ').unwrap_or(line).trim_end()
        };
        if !content.is_empty() {
            lines.push(content.to_owned());
        }
    }
    lines.join("\n")
}

/// Given the raw (psql-formatted) output of an EXPLAIN query, parse and render
/// it as an enhanced view.  Returns `Some(enhanced_text)` on success, or
/// `None` if parsing fails (caller falls back to raw output).
///
/// Enhanced mode: summary header (timing/cost, issues) + full raw plan text
/// with ANSI color and inline `⚠` markers — nothing is hidden or removed.
fn try_render_explain(raw_text: &str, format: crate::explain::ExplainFormat) -> Option<String> {
    use crate::explain::{self, ExplainFormat};

    let stripped = strip_psql_table_format(raw_text);
    let parsed = explain::parse(&stripped).ok()?;

    let issues_plan = explain::to_issues_plan(&parsed);
    let raw_issues = crate::explain::issues::detect_issues(&issues_plan);
    let render_issues = explain::issues_to_render(&raw_issues);
    let render_plan = explain::to_render_plan(&parsed, &issues_plan);

    match format {
        ExplainFormat::Raw => None,
        ExplainFormat::Compact => Some(crate::explain::render::render_summary(
            &render_plan,
            &render_issues,
        )),
        ExplainFormat::Enhanced => Some(crate::explain::render::render_enhanced(
            &render_plan,
            &render_issues,
            &stripped,
        )),
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
#[allow(clippy::too_many_lines)]
pub(super) async fn execute_query_interactive(
    client: &Client,
    sql: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Record start time for audit log duration.
    let audit_start = if settings.audit_log_file.is_some() {
        Some(Instant::now())
    } else {
        None
    };

    // Only intercept when pager is enabled and no output redirection is active.
    if !settings.pager_enabled || settings.output_target.is_some() {
        let ok = execute_query(client, sql, settings, tx).await;
        if ok && is_ddl_statement(sql) {
            auto_refresh_schema(client, settings).await;
        }
        if ok {
            if let Some(start) = audit_start {
                let entry = format_audit_entry(&AuditEntryCtx {
                    sql,
                    dbname: &settings.audit_dbname.clone(),
                    user: &settings.audit_user.clone(),
                    duration: start.elapsed(),
                    row_count: settings.last_row_count,
                    text2sql_prompt: None,
                });
                flush_audit_entry(settings, &entry);
            }
        }
        return ok;
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

    // For EXPLAIN queries with format != Raw, attempt enhanced rendering.
    let explain_format = settings.explain_format;
    let display: std::borrow::Cow<'_, [u8]>;
    let enhanced: String;
    let (text, display_bytes) = if ok
        && is_explain_statement(sql)
        && explain_format != crate::explain::ExplainFormat::Raw
    {
        let raw_text = String::from_utf8_lossy(&captured);
        // `last_explain_text` was already stored with correct indentation by
        // `execute_query` (from the raw row values).  Do not overwrite it here
        // with the psql-table-stripped version, which loses indentation and
        // causes depesz/dalibo to reject the plan.
        if let Some(rendered) = try_render_explain(&raw_text, explain_format) {
            enhanced = rendered;
            display = std::borrow::Cow::Borrowed(b"");
            (enhanced.as_str(), enhanced.as_bytes())
        } else {
            display = std::borrow::Cow::Borrowed(captured.as_slice());
            let s = std::str::from_utf8(&captured).unwrap_or("");
            (s, captured.as_slice())
        }
    } else if ok && is_explain_statement(sql) {
        // Raw format: apply syntax highlighting to plan lines within the
        // psql-formatted table output.  `highlight_explain` only colorizes
        // plan node names, timing, and filter lines — table borders, the
        // QUERY PLAN header, and the (N rows) footer pass through unchanged.
        let raw_text = String::from_utf8_lossy(&captured);
        enhanced = crate::explain::highlight::highlight_explain(&raw_text, settings.no_highlight);
        display = std::borrow::Cow::Borrowed(b"");
        (enhanced.as_str(), enhanced.as_bytes())
    } else {
        display = std::borrow::Cow::Borrowed(captured.as_slice());
        let s = std::str::from_utf8(&captured).unwrap_or("");
        (s, captured.as_slice())
    };
    let _ = &display; // suppress unused warning

    // Determine terminal height; fall back to 24 if unavailable.
    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);

    if crate::pager::needs_paging_with_min(
        text,
        term_rows.saturating_sub(2),
        settings.pager_min_lines,
    ) {
        // Clear status bar before handing off to pager (pager takes full screen).
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.clear();
            sl.teardown_scroll_region();
        }
        run_pager_for_text(settings, text, display_bytes);
        // Re-establish scroll region, reposition cursor to bottom of scroll
        // region, and re-render status bar after pager exits.
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.setup_scroll_region_and_restore_cursor();
            sl.render();
        }
    } else {
        let _ = io::stdout().write_all(display_bytes);
    }

    if ok && is_ddl_statement(sql) {
        auto_refresh_schema(client, settings).await;
    }

    // Write audit log entry after output is delivered.
    if ok {
        if let Some(start) = audit_start {
            let entry = format_audit_entry(&AuditEntryCtx {
                sql,
                dbname: &settings.audit_dbname.clone(),
                user: &settings.audit_user.clone(),
                duration: start.elapsed(),
                row_count: settings.last_row_count,
                text2sql_prompt: None,
            });
            flush_audit_entry(settings, &entry);
        }
    }

    // Update status bar with latest state after query completes.
    let duration_ms = settings.last_query_duration_ms.unwrap_or(0);
    let tokens_used = settings.tokens_used;
    let token_budget = u32::try_from(settings.config.ai.token_budget).unwrap_or(u32::MAX);
    let input_mode = settings.input_mode;
    let exec_mode = settings.exec_mode;
    let auto_explain = settings.auto_explain;
    let tx_state = *tx;
    if let Some(ref sl_arc) = settings.statusline {
        let mut sl = sl_arc.lock().unwrap();
        sl.update(
            tx_state,
            duration_ms,
            tokens_used,
            token_budget,
            input_mode,
            exec_mode,
        );
        sl.set_auto_explain(auto_explain);
    }

    ok
}

/// Execute a SQL string using the extended query protocol in interactive mode,
/// routing output through the built-in pager when appropriate.
pub(super) async fn execute_query_extended_interactive(
    client: &Client,
    sql: &str,
    params: &[String],
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> bool {
    // Record start time for audit log duration.
    let audit_start = if settings.audit_log_file.is_some() {
        Some(Instant::now())
    } else {
        None
    };

    // Only intercept when pager is enabled and no output redirection is active.
    if !settings.pager_enabled || settings.output_target.is_some() {
        let ok = execute_query_extended(client, sql, params, settings, tx).await;
        if ok && is_ddl_statement(sql) {
            auto_refresh_schema(client, settings).await;
        }
        if ok {
            if let Some(start) = audit_start {
                let entry = format_audit_entry(&AuditEntryCtx {
                    sql,
                    dbname: &settings.audit_dbname.clone(),
                    user: &settings.audit_user.clone(),
                    duration: start.elapsed(),
                    row_count: settings.last_row_count,
                    text2sql_prompt: None,
                });
                flush_audit_entry(settings, &entry);
            }
        }
        return ok;
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
        // Clear status bar before handing off to pager.
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.clear();
            sl.teardown_scroll_region();
        }
        run_pager_for_text(settings, &text, &captured);
        // Re-establish scroll region, reposition cursor to bottom of scroll
        // region, and re-render after pager exits.
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.setup_scroll_region_and_restore_cursor();
            sl.render();
        }
    } else {
        let _ = io::stdout().write_all(&captured);
    }

    if ok && is_ddl_statement(sql) {
        auto_refresh_schema(client, settings).await;
    }

    // Write audit log entry after output is delivered.
    if ok {
        if let Some(start) = audit_start {
            let entry = format_audit_entry(&AuditEntryCtx {
                sql,
                dbname: &settings.audit_dbname.clone(),
                user: &settings.audit_user.clone(),
                duration: start.elapsed(),
                row_count: settings.last_row_count,
                text2sql_prompt: None,
            });
            flush_audit_entry(settings, &entry);
        }
    }

    // Update status bar with latest state after query completes.
    let duration_ms = settings.last_query_duration_ms.unwrap_or(0);
    let tokens_used = settings.tokens_used;
    let token_budget = u32::try_from(settings.config.ai.token_budget).unwrap_or(u32::MAX);
    let input_mode = settings.input_mode;
    let exec_mode = settings.exec_mode;
    let auto_explain = settings.auto_explain;
    let tx_state = *tx;
    if let Some(ref sl_arc) = settings.statusline {
        let mut sl = sl_arc.lock().unwrap();
        sl.update(
            tx_state,
            duration_ms,
            tokens_used,
            token_budget,
            input_mode,
            exec_mode,
        );
        sl.set_auto_explain(auto_explain);
    }

    ok
}

/// Return `true` if `sql` starts with a DDL keyword (CREATE, ALTER, DROP,
/// or COMMENT), ignoring leading whitespace and case.
pub(super) fn is_ddl_statement(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    upper.starts_with("CREATE")
        || upper.starts_with("ALTER")
        || upper.starts_with("DROP")
        || upper.starts_with("COMMENT")
}

/// Return `true` if `sql` is a statement that `PostgreSQL` forbids inside any
/// transaction block (explicit or implicit).
///
/// `PostgreSQL` wraps multi-statement simple-query strings in an implicit
/// transaction.  Statements matched here must therefore be sent as
/// individual `simple_query` calls to avoid
/// `ERROR: <command> cannot run inside a transaction block`.
///
/// Covered statements (per PG docs):
/// - `ALTER SYSTEM`
/// - `VACUUM` (bare or `VACUUM ANALYZE`; excludes `VACUUM (…)` with options
///   — that form is also forbidden but uses the same keyword so it is caught)
/// - `CLUSTER` (all forms — re-cluster all tables, specific table, or specific index)
/// - `CREATE DATABASE` / `DROP DATABASE`
/// - `CREATE TABLESPACE` / `DROP TABLESPACE`
/// - `REINDEX DATABASE` / `REINDEX SYSTEM`
pub(super) fn is_no_tx_statement(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    // Collect the first two whitespace-separated tokens for pattern matching.
    let mut tokens = upper.split_whitespace();
    let first = tokens.next().unwrap_or("");
    let second = tokens.next().unwrap_or("");

    match first {
        "ALTER" => second == "SYSTEM",
        // All forms of VACUUM and CLUSTER are forbidden inside a transaction.
        // For VACUUM: both bare `VACUUM` and `VACUUM (options…)` are blocked.
        // For CLUSTER: bare, per-table, and per-index forms are all blocked.
        "VACUUM" | "CLUSTER" => true,
        "CREATE" => matches!(second, "DATABASE" | "TABLESPACE"),
        "DROP" => matches!(second, "DATABASE" | "TABLESPACE"),
        "REINDEX" => matches!(second, "DATABASE" | "SYSTEM"),
        _ => false,
    }
}

/// Return `true` when `sql` contains multiple statements and at least one of
/// them is a no-transaction statement (see [`is_no_tx_statement`]).
///
/// In that case `execute_query` must split the batch and send each statement
/// individually so that `PostgreSQL`'s implicit-transaction wrapping of
/// multi-statement simple-query strings does not cause
/// `ERROR: … cannot run inside a transaction block`.
pub(super) fn needs_split_execution(sql: &str) -> bool {
    let stmts = crate::query::split_statements(sql);
    stmts.len() > 1 && stmts.iter().any(|s| is_no_tx_statement(s))
}

// ---------------------------------------------------------------------------
// Query audit log (FR-23)
// ---------------------------------------------------------------------------

/// Context for writing a single audit log entry.
pub struct AuditEntryCtx<'a> {
    /// The SQL statement that was executed.
    pub sql: &'a str,
    /// Database name at time of execution.
    pub dbname: &'a str,
    /// Connected user at time of execution.
    pub user: &'a str,
    /// Wall-clock duration of the execution.
    pub duration: std::time::Duration,
    /// Number of rows returned or affected (`None` when not available).
    pub row_count: Option<u64>,
    /// When `Some`, the query came from text2sql and this holds the
    /// original natural-language prompt.
    pub text2sql_prompt: Option<&'a str>,
}

/// Convert Unix seconds to a `"YYYY-MM-DD HH:MM:SS UTC"` string.
///
/// Uses only `std` (no `chrono`).  Implements the Gregorian calendar
/// proleptic rules sufficient for dates from 1970 to ~2100.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
pub(super) fn format_utc_timestamp(secs: u64) -> String {
    // Decompose into days + time-of-day.
    let days_since_epoch = secs / 86_400;
    let time_of_day = secs % 86_400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Gregorian calendar from epoch (1970-01-01).
    // Algorithm: cycles of 400, 100, 4, and 1 years.
    let n = days_since_epoch as i64 + 719_468; // shift to 0000-03-01
    let era = if n >= 0 {
        n / 146_097
    } else {
        (n - 146_096) / 146_097
    };
    let doe = (n - era * 146_097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = y + i64::from(m <= 2);

    format!("{year:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} UTC")
}

/// Format and write a single audit log entry to `writer`.
///
/// Each entry is formatted as a SQL comment block so the log file can be
/// fed back to psql:
///
///
/// `text2sql` queries include the original prompt:
///
///
/// Passwords and connection strings are never written.
pub fn format_audit_entry(ctx: &AuditEntryCtx<'_>) -> String {
    use std::fmt::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    // Format current UTC time as "YYYY-MM-DD HH:MM:SS UTC" using only std.
    let secs_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ts = format_utc_timestamp(secs_since_epoch);
    let duration_ms = ctx.duration.as_secs_f64() * 1000.0;

    let mut buf = String::new();

    // Header comment line.
    let source_tag = if ctx.text2sql_prompt.is_some() {
        " | source=text2sql"
    } else {
        ""
    };
    let _ = writeln!(
        buf,
        "-- {ts} | {dbname} | user={user} | duration={duration_ms:.0}ms{source_tag}",
        ts = ts,
        dbname = ctx.dbname,
        user = ctx.user,
        duration_ms = duration_ms,
        source_tag = source_tag,
    );

    // Optional prompt line for text2sql queries.
    if let Some(prompt) = ctx.text2sql_prompt {
        let _ = writeln!(buf, "-- prompt: {prompt:?}");
    }

    // The SQL itself, ensuring it ends with a semicolon.
    let sql_trimmed = ctx.sql.trim();
    if sql_trimmed.ends_with(';') {
        let _ = writeln!(buf, "{sql_trimmed}");
    } else {
        let _ = writeln!(buf, "{sql_trimmed};");
    }

    // Row count footer.
    match ctx.row_count {
        Some(1) => {
            let _ = writeln!(buf, "-- (1 row)");
        }
        Some(n) => {
            let _ = writeln!(buf, "-- ({n} rows)");
        }
        None => {
            let _ = writeln!(buf, "-- (ok)");
        }
    }

    buf
}

/// Write  to the audit log file stored in .
///
/// Errors are silently ignored so a log-write failure never disrupts
/// normal query output.
pub(super) fn flush_audit_entry(settings: &mut ReplSettings, entry_text: &str) {
    if let Some(ref mut f) = settings.audit_log_file {
        use std::io::Write as _;
        let _ = f.write_all(entry_text.as_bytes());
        let _ = f.flush();
    }
}

/// Refresh the schema cache after a successful DDL statement.
///
/// Prints `-- Schema cache refreshed` on success.  Errors are silently
/// ignored so that a cache refresh failure never disrupts normal output.
pub(super) async fn auto_refresh_schema(client: &Client, settings: &mut ReplSettings) {
    if let Some(cache) = &settings.schema_cache {
        if let Ok(loaded) = load_schema_cache(client).await {
            *cache.write().unwrap() = loaded;
            println!("-- Schema cache refreshed");
        }
    }
}

/// Activate the appropriate pager for `text`.
///
/// Uses the external pager command when `settings.pager_command` is set,
/// falling back to the built-in TUI pager otherwise.  On any pager error,
/// falls back to printing directly to stdout.
pub(super) fn run_pager_for_text(settings: &ReplSettings, text: &str, raw_bytes: &[u8]) {
    if let Some(ref cmd) = settings.pager_command {
        if let Err(e) = crate::pager::run_pager_external(cmd, text) {
            if e.kind() == io::ErrorKind::NotFound {
                eprintln!(
                    "rpg: pager '{cmd}' not found — check your PAGER setting \
                     (\\set PAGER off to disable)"
                );
            } else {
                eprintln!("rpg: pager error: {e}");
            }
            let _ = io::stdout().write_all(raw_bytes);
        }
    } else if let Err(e) = crate::pager::run_pager(text) {
        // Unsupported means no TTY is available (e.g. piped / non-interactive
        // mode).  Fall back silently — no error message, just print.
        if e.kind() != io::ErrorKind::Unsupported {
            eprintln!("rpg: pager error: {e}");
        }
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
pub(super) async fn execute_gexec(
    client: &Client,
    buf: &str,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) {
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
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            tx.on_error();
            return;
        }
    };

    // Execute each cell value as a SQL statement, showing results.
    for cell_sql in cell_sqls {
        // psql echoes each \gexec-generated SQL statement in echo-all mode
        // (regardless of the quiet flag — matches psql -a behavior).
        if settings.echo_all {
            if let Some(ref mut w) = settings.output_target {
                let _ = writeln!(w, "{cell_sql}");
            } else {
                println!("{cell_sql}");
            }
        }
        // Use execute_query so SELECT/EXPLAIN results are rendered correctly.
        // Disable echo_all to avoid re-echoing (we already echoed above).
        let saved_echo = settings.echo_all;
        settings.echo_all = false;
        execute_query(client, &cell_sql, settings, tx).await;
        settings.echo_all = saved_echo;
    }
}

/// Derive a psql-style command tag string from the first keyword of `sql`
/// and the affected-row count `n`.
///
/// For most DDL statements the tag is just the uppercased verb + noun
/// (e.g. `"CREATE TABLE"`).  For INSERT/UPDATE/DELETE/SELECT we append the
/// row count.
#[allow(dead_code)]
pub(super) fn command_tag_for(sql: &str, n: u64) -> String {
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
pub(super) async fn execute_gset(
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
                0 => {
                    // Always print this error (not suppressed by \quiet).
                    eprintln!("error: no rows returned for \\gset");
                }
                1 => {
                    tx.update_from_sql(sql_to_send);
                    // Update ROW_COUNT to reflect the 1-row result.
                    settings.vars.set("ROW_COUNT", "1");
                    // Store last query for \watch compatibility.
                    settings.last_query = Some(buf.to_owned());
                    let row = &rows[0];
                    for (col, val) in col_names.iter().zip(row.iter()) {
                        let var_name = format!("{prefix}{col}");
                        // Validate that the resulting variable name is legal
                        // (no spaces, slashes, etc.).
                        if !crate::vars::is_valid_variable_name(&var_name) {
                            eprintln!("error: invalid variable name: \"{var_name}\"");
                            continue;
                        }
                        // Warn about specially treated variables that psql
                        // ignores when set via \gset.
                        if is_specially_treated_gset_var(&var_name) {
                            eprintln!(
                                "warning: attempt to \\gset into specially treated \
                                 variable \"{var_name}\" ignored"
                            );
                            continue;
                        }
                        match val {
                            Some(v) => settings.vars.set(&var_name, v),
                            // NULL result → unset the variable (psql behaviour).
                            None => {
                                settings.vars.unset(&var_name);
                            }
                        }
                    }
                }
                _ => eprintln!("error: more than one row returned for \\gset"),
            }
            // \gset stores results in variables, not displayed — psql does
            // not echo blank lines following \gset.
            settings.last_stmt_produced_rows = false;
        }
        Err(e) => {
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            settings.last_stmt_produced_rows = false;
            tx.on_error();
        }
    }
}

// ---------------------------------------------------------------------------
// \bind parameter substitution
// ---------------------------------------------------------------------------

/// Substitute `$N` positional parameters in `sql` with their quoted literal
/// values from `params`, producing a plain SQL string safe for `simple_query`.
///
/// Each parameter value is wrapped in dollar-quoting (`$param$...$param$`)
/// so that any embedded single quotes or backslashes are handled correctly.
/// Parameters inside single-quoted strings, dollar-quoted strings, or
/// comments are left untouched.
#[allow(clippy::too_many_lines)]
fn substitute_bind_params(sql: &str, params: &[String]) -> String {
    if params.is_empty() {
        return sql.to_owned();
    }

    let mut out =
        String::with_capacity(sql.len() + params.iter().map(|p| p.len() + 20).sum::<usize>());
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single = false;
    let mut in_dollar: Option<String> = None;
    let mut in_line_comment = false;
    let mut in_block_comment: u32 = 0;

    while i < len {
        // Track line comments.
        if !in_single
            && in_dollar.is_none()
            && in_block_comment == 0
            && !in_line_comment
            && i + 1 < len
            && bytes[i] == b'-'
            && bytes[i + 1] == b'-'
        {
            in_line_comment = true;
            out.push('-');
            out.push('-');
            i += 2;
            continue;
        }
        if in_line_comment {
            if bytes[i] == b'\n' {
                in_line_comment = false;
            }
            let ch = sql[i..].chars().next().expect("valid utf8");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }

        // Track block comments.
        if !in_single
            && in_dollar.is_none()
            && i + 1 < len
            && bytes[i] == b'/'
            && bytes[i + 1] == b'*'
        {
            in_block_comment += 1;
            out.push('/');
            out.push('*');
            i += 2;
            continue;
        }
        if in_block_comment > 0 {
            if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                in_block_comment -= 1;
                out.push('*');
                out.push('/');
                i += 2;
            } else {
                let ch = sql[i..].chars().next().expect("valid utf8");
                out.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }

        // Track single-quoted strings.
        if bytes[i] == b'\'' {
            if in_single {
                // Check for escaped quote.
                if i + 1 < len && bytes[i + 1] == b'\'' {
                    out.push('\'');
                    out.push('\'');
                    i += 2;
                    continue;
                }
                in_single = false;
            } else if in_dollar.is_none() {
                in_single = true;
            }
            out.push('\'');
            i += 1;
            continue;
        }

        // Track dollar-quoted strings.
        if bytes[i] == b'$' && !in_single {
            let rest = &sql[i..];
            // Find closing $.
            if let Some(end) = rest[1..].find('$') {
                let tag = &rest[..end + 2]; // includes both $
                let inner = &rest[1..=end];
                let is_valid_tag = inner.is_empty()
                    || (inner
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                        && inner.chars().all(|c| c.is_alphanumeric() || c == '_'));
                if is_valid_tag {
                    if let Some(ref open_tag) = in_dollar.clone() {
                        if tag == open_tag {
                            in_dollar = None;
                            out.push_str(tag);
                            i += tag.len();
                            continue;
                        }
                    } else {
                        in_dollar = Some(tag.to_owned());
                        out.push_str(tag);
                        i += tag.len();
                        continue;
                    }
                }
            }
        }

        // Substitute $N when outside strings/comments.
        if bytes[i] == b'$'
            && !in_single
            && in_dollar.is_none()
            && !in_line_comment
            && in_block_comment == 0
        {
            // Parse the number after $.
            let start = i + 1;
            let mut end = start;
            while end < len && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start {
                let num_str = &sql[start..end];
                if let Ok(n) = num_str.parse::<usize>() {
                    if n >= 1 {
                        if let Some(val) = params.get(n - 1) {
                            // Use dollar-quoting to safely embed the value.
                            // Pick a tag that doesn't appear in the value.
                            let tag = find_dollar_quote_tag(val);
                            out.push_str(&tag);
                            out.push_str(val);
                            out.push_str(&tag);
                            i = end;
                            continue;
                        }
                    }
                }
            }
        }

        let ch = sql[i..].chars().next().expect("valid utf8");
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

/// Find a dollar-quoting tag that does not appear inside `val`.
fn find_dollar_quote_tag(val: &str) -> String {
    let base = "$param$";
    if !val.contains(base) {
        return base.to_owned();
    }
    for n in 0..100 {
        let tag = format!("$param{n}$");
        if !val.contains(&tag) {
            return tag;
        }
    }
    // Final fallback: check "$p$" first, then try "$pN$" variants.
    if !val.contains("$p$") {
        return "$p$".to_owned();
    }
    for n in 0..1000 {
        let tag = format!("$p{n}$");
        if !val.contains(&tag) {
            return tag;
        }
    }
    // Practically unreachable: value contains $param0$–$param99$, $p$, and $p0$–$p999$.
    "$rpg_param$".to_owned()
}

// ---------------------------------------------------------------------------
// \gset helpers
// ---------------------------------------------------------------------------

/// Variables that psql treats specially and ignores when set via `\gset`.
///
/// These are read-only or specially handled internal variables.  When a
/// `\gset` prefix+column would produce one of these names, psql prints a
/// warning and skips the assignment.
fn is_specially_treated_gset_var(name: &str) -> bool {
    matches!(
        name,
        "IGNOREEOF"
            | "DBNAME"
            | "USER"
            | "PORT"
            | "HOST"
            | "ENCODING"
            | "HISTFILE"
            | "HISTSIZE"
            | "LASTOID"
            | "PROMPT1"
            | "PROMPT2"
            | "PROMPT3"
            | "VERBOSITY"
            | "SHOW_CONTEXT"
    )
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
pub(super) async fn execute_crosstabview(
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
            let mut col_oids: Vec<u32> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();

            let null_str = settings.pset.null_display.clone();
            for msg in messages {
                match msg {
                    SimpleQueryMessage::RowDescription(cols) => {
                        if col_names.is_empty() {
                            col_names = cols.iter().map(|c| c.name().to_owned()).collect();
                            col_oids = cols
                                .iter()
                                .map(tokio_postgres::SimpleColumn::type_oid)
                                .collect();
                        }
                    }
                    SimpleQueryMessage::Row(row) => {
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
                            .map(|i| row.get(i).unwrap_or(null_str.as_str()).to_owned())
                            .collect();
                        rows.push(vals);
                    }
                    _ => {}
                }
            }

            tx.update_from_sql(sql_to_send);
            settings.last_query = Some(buf.to_owned());
            Some((col_names, col_oids, rows))
        }
        Err(e) => {
            crate::output::eprint_db_error_located(
                settings.error_location_prefix().as_deref(),
                &e,
                Some(sql_to_send),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            tx.on_error();
            None
        }
    };

    let Some((col_names, col_oids, rows)) = result else {
        return;
    };

    // Parse and apply the pivot specification.
    let args = crate::crosstab::parse_args(raw_args);
    match crate::crosstab::pivot(&col_names, &rows, &args) {
        Ok((pivot_headers, pivot_rows)) => {
            // Determine column alignment: right-align numeric columns.
            let row_right_align = {
                let idx_v = args
                    .col_v
                    .as_ref()
                    .map_or(0, |s| s.resolve(&col_names).unwrap_or(0));
                col_oids.get(idx_v).copied().is_some_and(is_numeric_oid)
            };
            let data_right_align = {
                let idx_d = args
                    .col_d
                    .as_ref()
                    .map_or(2, |s| s.resolve(&col_names).unwrap_or(2));
                col_oids.get(idx_d).copied().is_some_and(is_numeric_oid)
            };
            let mut out = String::new();
            crate::crosstab::format_pivot(
                &mut out,
                &pivot_headers,
                &pivot_rows,
                row_right_align,
                data_right_align,
            );
            // psql always outputs a blank line after the crosstabview
            // result table in echo-all (-a) mode.
            out.push('\n');
            let _ = io::stdout().write_all(out.as_bytes());
            // Do NOT set last_stmt_produced_rows: the trailing blank line
            // was already output above, so blank lines that follow in the
            // input file should NOT be echoed a second time.
        }
        Err(e) => {
            eprintln!("error: {e}");
        }
    }
}

/// Return true for `PostgreSQL` OIDs that represent numeric types
/// (which should be right-aligned in table output).
/// Returns true if `sql` is a pure SELECT-like statement (SELECT, WITH, VALUES,
/// TABLE, FETCH, EXPLAIN SELECT, etc.) — i.e. not DML+RETURNING (INSERT/UPDATE/
/// DELETE). psql echoes blank lines after pure SELECT results but not after DML.
fn is_pure_select(sql: &str) -> bool {
    let upper = sql.trim().to_uppercase();
    // Skip leading single-line comments (--). Note: block comments (/* */)
    // are not currently stripped; the split_whitespace heuristic below only
    // handles `--` prefixed words.
    let upper = upper.trim_start();
    // Find first non-comment, non-whitespace token.
    let first_word = upper
        .split_whitespace()
        .find(|w| !w.starts_with("--"))
        .unwrap_or("");
    matches!(
        first_word,
        "SELECT" | "WITH" | "VALUES" | "TABLE" | "FETCH" | "EXPLAIN"
    )
}

fn is_numeric_oid(oid: u32) -> bool {
    matches!(
        oid,
        20   // int8 / bigint
        | 21 // int2 / smallint
        | 23 // int4 / integer
        | 26 // oid
        | 700 // float4 / real
        | 701 // float8 / double precision
        | 790 // money
        | 1700 // numeric / decimal
        | 2278 // void (rare)
    )
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
pub(super) async fn describe_buffer(client: &Client, buf: &str, verbose_errors: bool) {
    if buf.is_empty() {
        println!("Query buffer is empty.");
        return;
    }

    let stmt = match client.prepare(buf).await {
        Ok(s) => s,
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(buf), verbose_errors, false, false);
            return;
        }
    };

    let cols = stmt.columns();
    if cols.is_empty() {
        println!("The command has no result, or the result has no columns.");
        return;
    }

    // Collect (name, oid, typmod) triples.
    let col_info: Vec<(String, u32, i32)> = cols
        .iter()
        .map(|c| (c.name().to_owned(), c.type_().oid(), c.type_modifier()))
        .collect();

    // Resolve OIDs + typmods to display type names in a single query.
    // Build: SELECT format_type($1, $2), format_type($3, $4), …
    // The typmod is passed so that precision/scale is included
    // (e.g. `character varying(4)` instead of `character varying`).
    let select_exprs: Vec<String> = col_info
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            let oid_param = idx * 2 + 1;
            let mod_param = idx * 2 + 2;
            format!("pg_catalog.format_type(${oid_param}, ${mod_param})")
        })
        .collect();
    let type_query = format!("select {}", select_exprs.join(", "));

    // Interleave oid and typmod parameters: $1=oid1, $2=typmod1, $3=oid2, …
    let mut param_values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync>> = Vec::new();
    for (_, oid, typmod) in &col_info {
        param_values.push(Box::new(*oid));
        param_values.push(Box::new(*typmod));
    }
    let oid_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_values
        .iter()
        .map(std::convert::AsRef::as_ref)
        .collect();

    let type_names: Vec<String> = match client.query_one(&type_query, &oid_params).await {
        Ok(row) => (0..col_info.len())
            .map(|i| row.get::<_, String>(i))
            .collect(),
        Err(e) => {
            crate::output::eprint_db_error(&e, None, verbose_errors, false, false);
            return;
        }
    };

    // Build a RowSet and use format_rowset_pset for proper alignment
    // (center-aligned headers, trailing blank line — matching psql).
    use crate::output::format_rowset_pset;
    use crate::query::{ColumnMeta, RowSet};

    let columns = vec![
        ColumnMeta {
            name: "Column".to_owned(),
            is_numeric: false,
        },
        ColumnMeta {
            name: "Type".to_owned(),
            is_numeric: false,
        },
    ];

    let rows: Vec<Vec<Option<String>>> = col_info
        .iter()
        .zip(type_names.iter())
        .map(|((name, _, _), type_name)| vec![Some(name.clone()), Some(type_name.clone())])
        .collect();

    let rs = RowSet { columns, rows };

    let mut out = String::new();
    format_rowset_pset(&mut out, &rs, &crate::output::PsetConfig::default());

    let _ = io::stdout().write_all(out.as_bytes());
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        is_explain_statement, is_no_tx_statement, needs_split_execution, print_result_set_pset,
        strip_psql_table_format,
    };
    use crate::output::PsetConfig;

    // -- is_no_tx_statement ---------------------------------------------------

    #[test]
    fn no_tx_alter_system() {
        assert!(is_no_tx_statement(
            "ALTER SYSTEM SET autovacuum_insert_scale_factor = 0.01"
        ));
    }

    #[test]
    fn no_tx_alter_system_lowercase() {
        assert!(is_no_tx_statement("alter system set work_mem = '64MB'"));
    }

    #[test]
    fn no_tx_alter_system_reset() {
        assert!(is_no_tx_statement("ALTER SYSTEM RESET autovacuum_naptime"));
    }

    #[test]
    fn no_tx_alter_system_reset_all() {
        assert!(is_no_tx_statement("ALTER SYSTEM RESET ALL"));
    }

    #[test]
    fn no_tx_vacuum_bare() {
        assert!(is_no_tx_statement("VACUUM"));
    }

    #[test]
    fn no_tx_vacuum_table() {
        assert!(is_no_tx_statement("VACUUM my_table"));
    }

    #[test]
    fn no_tx_vacuum_analyze() {
        assert!(is_no_tx_statement("VACUUM ANALYZE my_table"));
    }

    #[test]
    fn no_tx_vacuum_full() {
        assert!(is_no_tx_statement("VACUUM (FULL, ANALYZE) my_table"));
    }

    #[test]
    fn no_tx_vacuum_lowercase() {
        assert!(is_no_tx_statement("vacuum my_table"));
    }

    #[test]
    fn no_tx_cluster_bare() {
        assert!(is_no_tx_statement("CLUSTER"));
    }

    #[test]
    fn no_tx_cluster_table() {
        assert!(is_no_tx_statement("CLUSTER my_table"));
    }

    #[test]
    fn no_tx_cluster_using() {
        assert!(is_no_tx_statement("CLUSTER my_table USING my_index"));
    }

    #[test]
    fn no_tx_create_database() {
        assert!(is_no_tx_statement("CREATE DATABASE mydb"));
    }

    #[test]
    fn no_tx_drop_database() {
        assert!(is_no_tx_statement("DROP DATABASE mydb"));
    }

    #[test]
    fn no_tx_create_tablespace() {
        assert!(is_no_tx_statement(
            "CREATE TABLESPACE ts1 LOCATION '/data/ts1'"
        ));
    }

    #[test]
    fn no_tx_drop_tablespace() {
        assert!(is_no_tx_statement("DROP TABLESPACE ts1"));
    }

    #[test]
    fn no_tx_reindex_database() {
        assert!(is_no_tx_statement("REINDEX DATABASE mydb"));
    }

    #[test]
    fn no_tx_reindex_system() {
        assert!(is_no_tx_statement("REINDEX SYSTEM mydb"));
    }

    #[test]
    fn no_tx_leading_whitespace() {
        assert!(is_no_tx_statement(
            "  ALTER SYSTEM SET shared_buffers = '1GB'"
        ));
    }

    // Statements that ARE allowed in transactions.
    #[test]
    fn tx_ok_alter_table() {
        assert!(!is_no_tx_statement("ALTER TABLE foo ADD COLUMN bar text"));
    }

    #[test]
    fn tx_ok_create_table() {
        assert!(!is_no_tx_statement("CREATE TABLE foo (id int)"));
    }

    #[test]
    fn tx_ok_drop_table() {
        assert!(!is_no_tx_statement("DROP TABLE foo"));
    }

    #[test]
    fn tx_ok_reindex_table() {
        assert!(!is_no_tx_statement("REINDEX TABLE foo"));
    }

    #[test]
    fn tx_ok_reindex_index() {
        assert!(!is_no_tx_statement("REINDEX INDEX foo_idx"));
    }

    #[test]
    fn tx_ok_select() {
        assert!(!is_no_tx_statement("SELECT pg_reload_conf()"));
    }

    #[test]
    fn tx_ok_insert() {
        assert!(!is_no_tx_statement("INSERT INTO t VALUES (1)"));
    }

    // -- needs_split_execution ------------------------------------------------

    #[test]
    fn split_needed_alter_system_with_reload() {
        // The canonical two-statement pattern from the bug report.
        assert!(needs_split_execution(
            "ALTER SYSTEM SET autovacuum_insert_scale_factor = 0.01;\
             SELECT pg_reload_conf()"
        ));
    }

    #[test]
    fn split_not_needed_single_alter_system() {
        // Single statement never needs split.
        assert!(!needs_split_execution(
            "ALTER SYSTEM SET autovacuum_insert_scale_factor = 0.01"
        ));
    }

    #[test]
    fn split_not_needed_two_regular_stmts() {
        // Two normal statements: no split needed (server handles them fine).
        assert!(!needs_split_execution("SELECT 1; SELECT 2"));
    }

    #[test]
    fn split_needed_vacuum_plus_select() {
        assert!(needs_split_execution("VACUUM my_table; SELECT 1"));
    }

    #[test]
    fn split_needed_create_database_plus_select() {
        assert!(needs_split_execution(
            "CREATE DATABASE newdb; SELECT current_database()"
        ));
    }

    #[test]
    fn split_not_needed_empty() {
        assert!(!needs_split_execution(""));
    }

    // -- print_result_set_pset — zero-column SELECT (issue #643) --------------

    /// `SELECT FROM t WHERE i = 10` is valid SQL that returns 1 row with zero
    /// columns.  Before the fix, `print_result_set_pset` skipped the display
    /// block entirely, producing no output.  After the fix it must emit the
    /// row-count footer to match psql.
    #[test]
    fn zero_col_select_one_row_shows_row_count() {
        let mut buf: Vec<u8> = Vec::new();
        print_result_set_pset(
            &mut buf,
            &[],       // zero column names
            &[],       // zero type OIDs
            &[vec![]], // one row, no cells
            true,      // is_select
            1,         // rows_affected (not used for SELECT)
            "SELECT FROM t WHERE i = 10",
            true, // is_first
            &PsetConfig::default(),
            false, // not quiet
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("(1 row)"),
            "zero-col SELECT must show row-count footer: {out:?}"
        );
    }

    #[test]
    fn zero_col_select_zero_rows_shows_row_count() {
        let mut buf: Vec<u8> = Vec::new();
        print_result_set_pset(
            &mut buf,
            &[], // zero column names
            &[], // zero type OIDs
            &[], // zero rows
            true,
            0,
            "SELECT FROM t WHERE false",
            true,
            &PsetConfig::default(),
            false, // not quiet
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("(0 rows)"),
            "zero-col zero-row SELECT must show footer: {out:?}"
        );
    }

    #[test]
    fn zero_col_select_shows_separator() {
        let mut buf: Vec<u8> = Vec::new();
        print_result_set_pset(
            &mut buf,
            &[],
            &[],
            &[vec![]],
            true,
            1,
            "SELECT FROM t",
            true,
            &PsetConfig::default(),
            false, // not quiet
        );
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("--"),
            "zero-col SELECT must show `--` separator: {out:?}"
        );
    }

    #[test]
    fn ddl_shows_command_tag() {
        // DDL commands (rows_affected=0) must show their command tag to match psql.
        let mut buf: Vec<u8> = Vec::new();
        print_result_set_pset(
            &mut buf,
            &[],
            &[],
            &[],
            false, // not a SELECT
            0,     // DDL always has rows_affected=0
            "CREATE TABLE foo (id int)",
            true,
            &PsetConfig::default(),
            false, // not quiet
        );
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(
            out.trim(),
            "CREATE TABLE",
            "CREATE TABLE must print its command tag: {out:?}"
        );
    }

    #[test]
    fn update_zero_rows_shows_tag() {
        // UPDATE with 0 matching rows must print "UPDATE 0" (matches psql).
        let mut buf: Vec<u8> = Vec::new();
        print_result_set_pset(
            &mut buf,
            &[],
            &[],
            &[],
            false,
            0, // 0 rows affected
            "UPDATE foo SET x = 1 WHERE false",
            true,
            &PsetConfig::default(),
            false, // not quiet
        );
        let out = String::from_utf8(buf).unwrap();
        assert_eq!(out.trim(), "UPDATE 0", "UPDATE 0 must print tag: {out:?}");
    }

    // -- is_explain_statement ------------------------------------------------

    #[test]
    fn is_explain_plain() {
        assert!(is_explain_statement("EXPLAIN SELECT 1"));
    }

    #[test]
    fn is_explain_lowercase() {
        assert!(is_explain_statement("explain select 1"));
    }

    #[test]
    fn is_explain_with_analyze() {
        assert!(is_explain_statement("EXPLAIN ANALYZE SELECT * FROM t"));
    }

    #[test]
    fn is_explain_with_leading_whitespace() {
        assert!(is_explain_statement("  EXPLAIN SELECT 1"));
        assert!(is_explain_statement("\t\nexplain select 1"));
    }

    #[test]
    fn is_explain_mixed_case() {
        assert!(is_explain_statement("Explain Select 1"));
    }

    #[test]
    fn is_explain_select_is_not_explain() {
        assert!(!is_explain_statement("SELECT 1"));
    }

    #[test]
    fn is_explain_insert_is_not_explain() {
        assert!(!is_explain_statement("INSERT INTO t VALUES (1)"));
    }

    #[test]
    fn is_explain_empty_is_not_explain() {
        assert!(!is_explain_statement(""));
    }

    #[test]
    fn is_explain_whitespace_only_is_not_explain() {
        assert!(!is_explain_statement("   "));
    }

    // -- strip_psql_table_format ---------------------------------------------

    #[test]
    fn strip_psql_table_format_removes_header_and_footer() {
        let formatted = " QUERY PLAN\n----------\n Seq Scan on t\n(1 row)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            !result.contains("QUERY PLAN"),
            "header must be stripped: {result:?}"
        );
        assert!(
            !result.contains("(1 row)"),
            "footer must be stripped: {result:?}"
        );
        assert!(
            result.contains("Seq Scan on t"),
            "plan content must be preserved: {result:?}"
        );
    }

    #[test]
    fn strip_psql_table_format_removes_separator_lines() {
        let formatted = " QUERY PLAN\n-----------\n Seq Scan on t\n(1 row)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            !result.contains("---"),
            "separator dashes must be stripped: {result:?}"
        );
    }

    #[test]
    fn strip_psql_table_format_preserves_plan_indentation() {
        // The space-padded format: psql adds one leading space before content.
        // Internal indentation (2+ spaces for child nodes) must be preserved.
        let formatted =
            " QUERY PLAN\n----------\n Seq Scan on t  (cost=0..1 rows=1)\n   ->  Index Scan\n(2 rows)\n";
        let result = strip_psql_table_format(formatted);
        // "Seq Scan" should appear without leading space (the single psql space is stripped).
        assert!(
            result.contains("Seq Scan"),
            "plan node must be present: {result:?}"
        );
        // Child node indentation ("  ->") should be preserved.
        assert!(
            result.contains("  ->"),
            "child node indentation must be preserved: {result:?}"
        );
    }

    #[test]
    fn strip_psql_table_format_handles_pipe_delimited_content() {
        // Pipe-wrapped content has its "| " prefix and " |" suffix removed.
        // Only lines starting with a bare '-' are treated as border separators.
        let formatted = " QUERY PLAN\n----------\n| Seq Scan on t |\n(1 row)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            result.contains("Seq Scan on t"),
            "pipe-delimited plan content must be extracted: {result:?}"
        );
        assert!(
            !result.contains("QUERY PLAN"),
            "header must be stripped: {result:?}"
        );
        assert!(
            !result.contains("(1 row)"),
            "footer must be stripped: {result:?}"
        );
    }

    #[test]
    fn strip_psql_table_format_empty_input() {
        assert_eq!(strip_psql_table_format(""), "");
    }

    #[test]
    fn strip_psql_table_format_skips_blank_lines() {
        let formatted = " QUERY PLAN\n----------\n\n Seq Scan on t\n\n(1 row)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            !result.contains("\n\n"),
            "blank lines must be stripped: {result:?}"
        );
        assert!(result.contains("Seq Scan on t"));
    }

    #[test]
    fn strip_psql_table_format_plural_rows_footer() {
        let formatted = " QUERY PLAN\n----------\n Seq Scan\n(5 rows)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            !result.contains("(5 rows)"),
            "plural rows footer must be stripped: {result:?}"
        );
    }

    #[test]
    fn strip_psql_table_format_separator_with_plus_sign() {
        // Some psql border styles use '+' between column separators.
        let formatted = " QUERY PLAN\n----+----\n Seq Scan\n(1 row)\n";
        let result = strip_psql_table_format(formatted);
        assert!(
            !result.contains("----+----"),
            "separator with '+' must be stripped: {result:?}"
        );
        assert!(result.contains("Seq Scan"));
    }

    // -- is_transaction_control_command ----------------------------------------

    #[test]
    fn tx_ctrl_begin() {
        assert!(super::is_transaction_control_command("BEGIN"));
    }

    #[test]
    fn tx_ctrl_begin_lowercase() {
        assert!(super::is_transaction_control_command("begin"));
    }

    #[test]
    fn tx_ctrl_commit() {
        assert!(super::is_transaction_control_command("COMMIT"));
    }

    #[test]
    fn tx_ctrl_rollback() {
        assert!(super::is_transaction_control_command("ROLLBACK"));
    }

    #[test]
    fn tx_ctrl_rollback_to_savepoint() {
        assert!(super::is_transaction_control_command(
            "ROLLBACK TO SAVEPOINT sp1"
        ));
    }

    #[test]
    fn tx_ctrl_savepoint() {
        assert!(super::is_transaction_control_command("SAVEPOINT sp1"));
    }

    #[test]
    fn tx_ctrl_release() {
        assert!(super::is_transaction_control_command(
            "RELEASE SAVEPOINT sp1"
        ));
    }

    #[test]
    fn tx_ctrl_end() {
        assert!(super::is_transaction_control_command("END"));
    }

    #[test]
    fn tx_ctrl_abort() {
        assert!(super::is_transaction_control_command("ABORT"));
    }

    #[test]
    fn tx_ctrl_start_transaction() {
        assert!(super::is_transaction_control_command("START TRANSACTION"));
    }

    #[test]
    fn tx_ctrl_prepare_transaction() {
        assert!(super::is_transaction_control_command(
            "PREPARE TRANSACTION 'tx1'"
        ));
    }

    #[test]
    fn tx_ctrl_leading_whitespace() {
        assert!(super::is_transaction_control_command("  BEGIN"));
    }

    #[test]
    fn tx_ctrl_negative_select() {
        assert!(!super::is_transaction_control_command("SELECT 1"));
    }

    #[test]
    fn tx_ctrl_negative_insert() {
        assert!(!super::is_transaction_control_command(
            "INSERT INTO t VALUES (1)"
        ));
    }

    #[test]
    fn tx_ctrl_negative_prepare_stmt() {
        // PREPARE (without TRANSACTION) is a prepared statement, not tx control.
        assert!(!super::is_transaction_control_command(
            "PREPARE stmt AS SELECT 1"
        ));
    }

    // -- substitute_bind_params ------------------------------------------------

    #[test]
    fn bind_plain_substitution() {
        let result = super::substitute_bind_params(
            "SELECT $1, $2",
            &["hello".to_owned(), "world".to_owned()],
        );
        assert!(
            result.contains("hello"),
            "param $1 should be substituted: {result:?}"
        );
        assert!(
            result.contains("world"),
            "param $2 should be substituted: {result:?}"
        );
    }

    #[test]
    fn bind_inside_single_quoted_string_no_sub() {
        let result =
            super::substitute_bind_params("SELECT '$1'", &["should_not_appear".to_owned()]);
        assert!(
            !result.contains("should_not_appear"),
            "$1 inside single quotes must not be substituted: {result:?}"
        );
        assert!(
            result.contains("'$1'"),
            "single-quoted $1 must be preserved: {result:?}"
        );
    }

    #[test]
    fn bind_inside_dollar_quoted_body_no_sub() {
        let result =
            super::substitute_bind_params("SELECT $$ $1 $$", &["should_not_appear".to_owned()]);
        assert!(
            !result.contains("should_not_appear"),
            "$1 inside $$ body must not be substituted: {result:?}"
        );
    }

    #[test]
    fn bind_inside_line_comment_no_sub() {
        let result =
            super::substitute_bind_params("SELECT 1 -- $1\n", &["should_not_appear".to_owned()]);
        assert!(
            !result.contains("should_not_appear"),
            "$1 inside line comment must not be substituted: {result:?}"
        );
    }

    #[test]
    fn bind_inside_block_comment_no_sub() {
        let result =
            super::substitute_bind_params("SELECT /* $1 */ 1", &["should_not_appear".to_owned()]);
        assert!(
            !result.contains("should_not_appear"),
            "$1 inside block comment must not be substituted: {result:?}"
        );
    }

    #[test]
    fn bind_out_of_range_param_left_as_is() {
        // $2 when only 1 param is provided should be left as-is.
        let result = super::substitute_bind_params("SELECT $2", &["only_one".to_owned()]);
        assert!(
            result.contains("$2"),
            "out-of-range param must be left as-is: {result:?}"
        );
    }

    #[test]
    fn bind_empty_params_returns_original() {
        let sql = "SELECT $1";
        let result = super::substitute_bind_params(sql, &[]);
        assert_eq!(result, sql, "empty params must return original SQL");
    }

    // -- find_dollar_quote_tag -------------------------------------------------

    #[test]
    fn dollar_tag_base_case() {
        let tag = super::find_dollar_quote_tag("hello world");
        assert_eq!(tag, "$param$");
    }

    #[test]
    fn dollar_tag_contains_base() {
        let tag = super::find_dollar_quote_tag("contains $param$ inside");
        assert_ne!(
            tag, "$param$",
            "must not use base tag when value contains it"
        );
        assert!(!tag.is_empty(), "must return a non-empty tag");
        assert!(
            !"contains $param$ inside".contains(&tag),
            "returned tag must not appear in value"
        );
    }

    #[test]
    fn dollar_tag_fallback_checked() {
        use std::fmt::Write as _;
        // Build a pathological value that contains $param$, $param0$–$param99$, and $p$.
        let mut evil = String::from("$param$ $p$");
        for n in 0..100 {
            write!(evil, " $param{n}$").unwrap();
        }
        let tag = super::find_dollar_quote_tag(&evil);
        assert!(
            !evil.contains(&tag),
            "tag must not appear in value even for pathological input: tag={tag:?}"
        );
    }
}
