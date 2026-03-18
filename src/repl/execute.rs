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

/// Print a single result set using the active [`PsetConfig`].
///
/// `col_names` and `rows` describe the result set. `is_select` indicates
/// whether this was a SELECT-like statement (i.e. we received a
/// `RowDescription` message, even if zero rows followed). `rows_affected`
/// carries the `CommandComplete` count. `is_first` is `false` when this is
/// a subsequent result set in a multi-statement query, in which case a blank
/// separator line is printed before the table (matching psql behaviour).
/// `writer` is the output destination (stdout or a redirected file).
pub(super) fn print_result_set_pset(
    writer: &mut dyn io::Write,
    col_names: &[String],
    rows: &[Vec<Option<String>>],
    is_select: bool,
    rows_affected: u64,
    is_first: bool,
    pset: &crate::output::PsetConfig,
) {
    use crate::output::format_rowset_pset;
    use crate::query::{ColumnMeta, RowSet};

    if is_select && !col_names.is_empty() {
        // Heuristic: psql right-aligns numeric columns using type OIDs from
        // the wire protocol.  The simple query protocol does not expose OIDs,
        // so we infer numeric columns by inspecting cell values.  A column is
        // treated as numeric if every non-NULL, non-empty cell in that column
        // parses as an f64 (covers integers, decimals, and scientific notation).
        // Columns that are entirely NULL/empty are NOT marked numeric.
        let columns: Vec<ColumnMeta> = col_names
            .iter()
            .enumerate()
            .map(|(col_idx, n)| {
                let mut has_value = false;
                let is_numeric = rows.iter().all(|row| {
                    match row.get(col_idx).and_then(|v| v.as_deref()) {
                        None | Some("") => true, // NULL or empty: skip, don't disqualify
                        Some(val) => {
                            has_value = true;
                            val.parse::<f64>().is_ok()
                        }
                    }
                }) && has_value;
                ColumnMeta {
                    name: n.clone(),
                    is_numeric,
                }
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
    } else if !is_select {
        // Non-SELECT statement: show rows affected if > 0.
        if rows_affected > 0 {
            if !is_first {
                let _ = writeln!(writer);
            }
            let _ = writeln!(writer, "{rows_affected}");
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
    if needs_split_execution(interpolated.as_str()) {
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

    let success = match client.simple_query(sql_to_send).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;
            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<Option<String>>> = Vec::new();
            // `is_select` is set to true when we receive a RowDescription
            // message (or any Row message).  This distinguishes an empty
            // SELECT (zero rows but column headers) from a DML command.
            let mut is_select = false;
            let mut result_set_index: usize = 0;

            for msg in messages {
                match msg {
                    SimpleQueryMessage::RowDescription(cols) => {
                        // Emitted before data rows (or before CommandComplete
                        // when zero rows matched).  Capture column names here
                        // so that empty result sets still show their headers.
                        is_select = true;
                        if col_names.is_empty() {
                            col_names = cols.iter().map(|c| c.name().to_owned()).collect();
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
                        }
                        let vals: Vec<Option<String>> = (0..row.len())
                            .map(|i| row.get(i).map(str::to_owned))
                            .collect();
                        rows.push(vals);
                    }
                    SimpleQueryMessage::CommandComplete(n) => {
                        // Capture plan text from auto-EXPLAIN before clearing
                        // rows. EXPLAIN output is a single-column result set.
                        if auto_explain_active && result_set_index == 0 {
                            let plan_text: String = rows
                                .iter()
                                .filter_map(|r| {
                                    r.first().and_then(|v| v.as_deref()).map(str::to_owned)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            if !plan_text.is_empty() {
                                auto_explain_plan = Some(plan_text);
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
                            &rows,
                            is_select,
                            n,
                            result_set_index == 0,
                            &settings.pset,
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

                        result_set_index += 1;
                        col_names.clear();
                        rows.clear();
                        is_select = false;
                    }
                    _ => {}
                }
            }

            // Update transaction state based on what SQL was sent.
            tx.update_from_sql(sql_to_send);

            true
        }
        Err(e) => {
            // -b / --echo-errors: echo the failing query to stderr.
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();

            // Capture context for /fix.
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: error_message.clone(),
                sqlstate,
            });

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
        }
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

    // Prepare the statement so we can execute with typed parameters.
    let stmt = match client.prepare(sql_to_send).await {
        Ok(s) => s,
        Err(e) => {
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message: e
                    .as_db_error()
                    .map_or_else(|| e.to_string(), |db| db.message().to_owned()),
                sqlstate,
            });
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

    // Build the parameter list as &str references (text format).
    let param_refs: Vec<&str> = params.iter().map(String::as_str).collect();
    let dyn_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_refs
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let success = match client.query(&stmt, dyn_params.as_slice()).await {
        Ok(rows) => {
            // Print results using the same pset formatting as simple_query.
            use crate::output::format_rowset_pset;
            use crate::query::{ColumnMeta, RowSet};

            if !rows.is_empty() || !stmt.columns().is_empty() {
                let col_names: Vec<String> =
                    stmt.columns().iter().map(|c| c.name().to_owned()).collect();

                let row_data: Vec<Vec<Option<String>>> = rows
                    .iter()
                    .map(|row| {
                        (0..col_names.len())
                            .map(|i| row.try_get::<_, Option<String>>(i).unwrap_or(None))
                            .collect()
                    })
                    .collect();

                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .enumerate()
                    .map(|(col_idx, n)| {
                        let mut has_value = false;
                        let is_numeric = row_data.iter().all(|r| {
                            match r.get(col_idx).and_then(|v| v.as_deref()) {
                                None | Some("") => true,
                                Some(val) => {
                                    has_value = true;
                                    val.parse::<f64>().is_ok()
                                }
                            }
                        }) && has_value;
                        ColumnMeta {
                            name: n.clone(),
                            is_numeric,
                        }
                    })
                    .collect();

                let rs = RowSet {
                    columns,
                    rows: row_data,
                };

                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &settings.pset);

                let out_bytes = out.as_bytes();

                if let Some(ref mut lf) = settings.log_file {
                    let _ = lf.write_all(out_bytes);
                }

                if let Some(ref mut w) = settings.output_target {
                    let _ = w.write_all(out_bytes);
                } else {
                    let _ = io::stdout().write_all(out_bytes);
                }
            }

            // Store row count for audit log entry.
            settings.last_row_count = Some(rows.len() as u64);

            tx.update_from_sql(sql_to_send);
            true
        }
        Err(e) => {
            if settings.echo_errors {
                eprintln!("{sql_to_send}");
            }
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();

            // Capture context for /fix.
            let sqlstate = e.as_db_error().map(|db| db.code().code().to_owned());
            let is_sql_error = e.as_db_error().is_some();
            let error_message = e
                .as_db_error()
                .map_or_else(|| e.to_string(), |db| db.message().to_owned());
            settings.last_error = Some(LastError {
                query: sql_to_send.to_owned(),
                error_message,
                sqlstate,
            });

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
    // Clone the statement out of the map so we don't hold a borrow on
    // settings while calling async client methods.
    let Some(stmt) = settings.named_statements.get(stmt_name).cloned() else {
        eprintln!("\\bind_named: prepared statement \"{stmt_name}\" does not exist");
        return false;
    };

    if settings.single_step {
        let preview = format!("[execute stmt \"{stmt_name}\"]");
        if !confirm_single_step(&preview) {
            return true;
        }
    }

    if settings.echo_queries {
        eprintln!("[execute stmt \"{stmt_name}\"]");
    }

    let start = if settings.timing {
        Some(Instant::now())
    } else {
        None
    };

    let param_refs: Vec<&str> = params.iter().map(String::as_str).collect();
    let dyn_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = param_refs
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let success = match client.query(&stmt, dyn_params.as_slice()).await {
        Ok(rows) => {
            use crate::output::format_rowset_pset;
            use crate::query::{ColumnMeta, RowSet};

            if !rows.is_empty() || !stmt.columns().is_empty() {
                let col_names: Vec<String> =
                    stmt.columns().iter().map(|c| c.name().to_owned()).collect();

                let row_data: Vec<Vec<Option<String>>> = rows
                    .iter()
                    .map(|row| {
                        (0..col_names.len())
                            .map(|i| row.try_get::<_, Option<String>>(i).unwrap_or(None))
                            .collect()
                    })
                    .collect();

                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .enumerate()
                    .map(|(col_idx, n)| {
                        let mut has_value = false;
                        let is_numeric = row_data.iter().all(|r| {
                            match r.get(col_idx).and_then(|v| v.as_deref()) {
                                None | Some("") => true,
                                Some(val) => {
                                    has_value = true;
                                    val.parse::<f64>().is_ok()
                                }
                            }
                        }) && has_value;
                        ColumnMeta {
                            name: n.clone(),
                            is_numeric,
                        }
                    })
                    .collect();

                let rs = RowSet {
                    columns,
                    rows: row_data,
                };

                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &settings.pset);

                let out_bytes = out.as_bytes();

                if let Some(ref mut w) = settings.output_target {
                    let _ = w.write_all(out_bytes);
                } else {
                    let _ = io::stdout().write_all(out_bytes);
                }
            }

            tx.update_from_sql(&format!("[bind_named {stmt_name}]"));
            true
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, None, settings.verbose_errors);
            tx.on_error();
            false
        }
    };

    if let Some(t) = start {
        let elapsed = t.elapsed();
        // Timing output is written through the active output target so that
        // it appears after the result set (matching psql behaviour).
        let line = format!("Time: {:.3} ms\n", elapsed.as_secs_f64() * 1000.0);
        if let Some(ref mut w) = settings.output_target {
            let _ = w.write_all(line.as_bytes());
        } else {
            let _ = io::stdout().write_all(line.as_bytes());
        }
    }

    if success {
        settings.last_query = Some(format!("[bind_named {stmt_name}]"));
    }

    success
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
        Err(e) => eprintln!("\\g: cannot open file \"{path}\": {e}"),
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

/// Execute a SQL string in interactive mode, routing output through the
/// built-in pager when appropriate.
///
/// When `settings.pager_enabled` is `true` and the formatted output exceeds
/// the current terminal height, the output is displayed in the built-in TUI
/// pager instead of being written directly to stdout.
///
/// This wrapper is used only by the interactive REPL loops.  Non-interactive
/// paths (`-c`, `-f`, piped stdin) call `execute_query` directly.
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

    let text = String::from_utf8_lossy(&captured);

    // Determine terminal height; fall back to 24 if unavailable.
    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);

    if crate::pager::needs_paging_with_min(
        &text,
        term_rows.saturating_sub(2),
        settings.pager_min_lines,
    ) {
        // Clear status bar before handing off to pager (pager takes full screen).
        if let Some(ref sl) = settings.statusline {
            sl.clear();
            sl.teardown_scroll_region();
        }
        run_pager_for_text(settings, &text, &captured);
        // Re-establish scroll region, reposition cursor to bottom of scroll
        // region, and re-render status bar after pager exits.
        if let Some(ref sl) = settings.statusline {
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
    if let Some(ref mut sl) = settings.statusline {
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
        if let Some(ref sl) = settings.statusline {
            sl.clear();
            sl.teardown_scroll_region();
        }
        run_pager_for_text(settings, &text, &captured);
        // Re-establish scroll region, reposition cursor to bottom of scroll
        // region, and re-render after pager exits.
        if let Some(ref sl) = settings.statusline {
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
    if let Some(ref mut sl) = settings.statusline {
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
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            return;
        }
    };

    // Execute each cell value as a SQL statement.
    for cell_sql in cell_sqls {
        match client.simple_query(&cell_sql).await {
            Ok(messages) => {
                for msg in messages {
                    if let SimpleQueryMessage::CommandComplete(n) = msg {
                        // Extract the command tag from the completion count.
                        // tokio-postgres 0.7 CommandComplete carries only the
                        // row count as u64; derive the tag by inspecting the
                        // first keyword of the cell SQL.
                        let tag = command_tag_for(&cell_sql, n);
                        println!("{tag}");
                    }
                }
                tx.update_from_sql(&cell_sql);
            }
            Err(e) => {
                crate::output::eprint_db_error(&e, Some(&cell_sql), settings.verbose_errors);
                tx.on_error();
            }
        }
    }
}

/// Derive a psql-style command tag string from the first keyword of `sql`
/// and the affected-row count `n`.
///
/// For most DDL statements the tag is just the uppercased verb + noun
/// (e.g. `"CREATE TABLE"`).  For INSERT/UPDATE/DELETE/SELECT we append the
/// row count.
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
                0 => eprintln!("\\gset: query returned no rows"),
                1 => {
                    tx.update_from_sql(sql_to_send);
                    // Store last query for \watch compatibility.
                    settings.last_query = Some(buf.to_owned());
                    let row = &rows[0];
                    for (col, val) in col_names.iter().zip(row.iter()) {
                        let var_name = format!("{prefix}{col}");
                        let var_value = val.as_deref().unwrap_or("");
                        settings.vars.set(&var_name, var_value);
                    }
                }
                n => eprintln!("\\gset: more than one row returned ({n} rows)"),
            }
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
        }
    }
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
            let mut rows: Vec<Vec<String>> = Vec::new();

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
                    let vals: Vec<String> = (0..row.len())
                        .map(|i| row.get(i).unwrap_or("").to_owned())
                        .collect();
                    rows.push(vals);
                }
            }

            tx.update_from_sql(sql_to_send);
            settings.last_query = Some(buf.to_owned());
            Some((col_names, rows))
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql_to_send), settings.verbose_errors);
            tx.on_error();
            None
        }
    };

    let Some((col_names, rows)) = result else {
        return;
    };

    // Parse and apply the pivot specification.
    let args = crate::crosstab::parse_args(raw_args);
    match crate::crosstab::pivot(&col_names, &rows, &args) {
        Ok((pivot_headers, pivot_rows)) => {
            let mut out = String::new();
            crate::crosstab::format_pivot(&mut out, &pivot_headers, &pivot_rows);
            let _ = io::stdout().write_all(out.as_bytes());
        }
        Err(e) => {
            eprintln!("{e}");
        }
    }
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
            crate::output::eprint_db_error(&e, Some(buf), verbose_errors);
            return;
        }
    };

    let cols = stmt.columns();
    if cols.is_empty() {
        println!("This command doesn't return data.");
        return;
    }

    // Collect (name, oid) pairs.
    let col_info: Vec<(String, u32)> = cols
        .iter()
        .map(|c| (c.name().to_owned(), c.type_().oid()))
        .collect();

    // Resolve OIDs to display type names in a single query.
    // Build: SELECT format_type($1, NULL), format_type($2, NULL), …
    let select_exprs: Vec<String> = (1..=col_info.len())
        .map(|i| format!("pg_catalog.format_type(${i}, NULL)"))
        .collect();
    let type_query = format!("select {}", select_exprs.join(", "));

    let oid_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = col_info
        .iter()
        .map(|(_, oid)| oid as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let type_names: Vec<String> = match client.query_one(&type_query, &oid_params).await {
        Ok(row) => (0..col_info.len())
            .map(|i| row.get::<_, String>(i))
            .collect(),
        Err(e) => {
            crate::output::eprint_db_error(&e, None, verbose_errors);
            return;
        }
    };

    // Compute column widths for aligned output.
    let header_col = "Column";
    let header_type = "Type";
    let col_w = col_info
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0)
        .max(header_col.len());
    let type_w = type_names
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max(header_type.len());

    // Header.
    println!(" {header_col:<col_w$} | {header_type:<type_w$}");
    // Separator.
    println!("-{}-+-{}-", "-".repeat(col_w), "-".repeat(type_w));
    // Rows.
    for ((name, _), type_name) in col_info.iter().zip(type_names.iter()) {
        println!(" {name:<col_w$} | {type_name:<type_w$}");
    }
    // Footer.
    let n = col_info.len();
    if n == 1 {
        println!("(1 row)");
    } else {
        println!("({n} rows)");
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{is_no_tx_statement, needs_split_execution};

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
}
