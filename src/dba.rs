//! Diagnostic commands for the `\dba` family.
//!
//! Each subcommand runs a diagnostic query against `pg_catalog` views
//! and displays the results in a formatted table.
//!
//! # PG compatibility
//! Queries target PG 14–18.

use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Execute a `\dba` diagnostic subcommand.
///
/// `subcommand` is the first word after `\dba` (e.g. `"activity"`, `"locks"`).
/// `verbose` is `true` when the `+` modifier was specified.
///
/// Returns `true` if the subcommand was recognised, `false` otherwise.
pub async fn execute(client: &Client, subcommand: &str, verbose: bool) -> bool {
    match subcommand {
        "activity" | "act" => {
            dba_activity(client, verbose).await;
            true
        }
        "locks" | "lock" => {
            dba_locks(client, verbose).await;
            true
        }
        "bloat" => {
            dba_bloat(client, verbose).await;
            true
        }
        "vacuum" | "vac" => {
            dba_vacuum(client, verbose).await;
            true
        }
        "tablesize" | "ts" => {
            dba_tablesize(client, verbose).await;
            true
        }
        "connections" | "conn" => {
            dba_connections(client, verbose).await;
            true
        }
        "unused-idx" | "unused" => {
            dba_unused_indexes(client, verbose).await;
            true
        }
        "seq-scans" | "seq" => {
            dba_seq_scans(client, verbose).await;
            true
        }
        "cache-hit" | "cache" => {
            dba_cache_hit(client, verbose).await;
            true
        }
        "replication" | "repl" => {
            dba_replication(client, verbose).await;
            true
        }
        "config" | "conf" => {
            dba_config(client, verbose).await;
            true
        }
        "waits" | "wait" => {
            dba_waits(client, verbose).await;
            true
        }
        "indexes" | "idx" => {
            dba_indexes(client, verbose).await;
            true
        }
        "" | "help" => {
            print_dba_help();
            true
        }
        _ => {
            eprintln!("\\dba: unknown subcommand \"{subcommand}\"");
            eprintln!("Try \\dba help for available subcommands.");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Internal execution helper
// ---------------------------------------------------------------------------

/// Execute `sql` via `simple_query`, collect results, and print a formatted
/// table.
///
/// If the query fails, the error is printed to stderr and the function
/// returns without panicking.
async fn run_and_print(client: &Client, sql: &str) {
    crate::logging::trace("dba", &format!("diagnostic query: {}", sql.trim()));
    match client.simple_query(sql).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;

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

            print_table(&col_names, &rows);
        }
        Err(e) => {
            eprintln!("\\dba: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Table formatter
// ---------------------------------------------------------------------------

/// Print a column-aligned table to stdout.
///
/// Output matches the psql default aligned format:
/// ```text
///  col1 | col2
/// ------+------
///  val  | val
/// (N rows)
/// ```
fn print_table(col_names: &[String], rows: &[Vec<String>]) {
    if col_names.is_empty() {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        println!("({n} {word})");
        return;
    }

    // Compute column widths.
    let mut widths: Vec<usize> = col_names.iter().map(String::len).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let max_line = val.lines().map(str::len).max().unwrap_or(val.len());
                widths[i] = widths[i].max(max_line);
            }
        }
    }

    // Header — center-align each header within its column width.
    let header_cells: Vec<String> = col_names
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = widths[i];
            let clen = c.len();
            if clen >= w {
                c.clone()
            } else {
                let pad_total = w - clen;
                let pad_left = pad_total / 2;
                let pad_right = pad_total - pad_left;
                format!("{:>width$}", c, width = clen + pad_left) + &" ".repeat(pad_right)
            }
        })
        .collect();

    // Print header row.
    let header_line = header_cells
        .iter()
        .enumerate()
        .map(|(i, h)| format!(" {:<width$}", h, width = widths[i]))
        .collect::<Vec<_>>()
        .join(" |");
    println!("{header_line}");

    // Print separator.
    let sep_line = widths
        .iter()
        .map(|w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("+");
    println!("{sep_line}");

    // Print data rows.
    for row in rows {
        let row_line = row
            .iter()
            .enumerate()
            .map(|(i, val)| {
                let w = if i < widths.len() { widths[i] } else { 0 };
                format!(" {val:<w$}")
            })
            .collect::<Vec<_>>()
            .join(" |");
        println!("{row_line}");
    }

    // Footer.
    let n = rows.len();
    let word = if n == 1 { "row" } else { "rows" };
    println!("({n} {word})");
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn print_dba_help() {
    println!("\\dba diagnostic commands:");
    println!("  \\dba activity    Active queries and sessions");
    println!("  \\dba locks       Lock tree (blocked/blocking)");
    println!("  \\dba waits       Wait event breakdown");
    println!("  \\dba bloat       Table bloat estimates");
    println!("  \\dba vacuum      Vacuum status and dead tuples");
    println!("  \\dba tablesize   Largest tables");
    println!("  \\dba connections Connection counts by state");
    println!("  \\dba indexes     Index health report (unused, redundant, invalid, bloated)");
    println!("  \\dba unused-idx  Unused indexes (simple view)");
    println!("  \\dba seq-scans   Tables with high sequential scan ratio");
    println!("  \\dba cache-hit   Buffer cache hit ratios");
    println!("  \\dba replication Replication slot status");
    println!("  \\dba config      Non-default configuration parameters");
    println!();
    println!("Aliases: act, lock, wait, vac, ts, conn, idx, unused, seq, cache, repl, conf");
}

// ---------------------------------------------------------------------------
// Subcommand handlers
// ---------------------------------------------------------------------------

async fn dba_activity(client: &Client, _verbose: bool) {
    let sql = "select \
        pid, \
        state, \
        case when wait_event_type is not null \
             then wait_event_type || ':' || wait_event \
             else '' end as wait, \
        now() - xact_start as xact_age, \
        now() - query_start as query_age, \
        usename, \
        datname, \
        application_name, \
        left(query, 80) as query \
    from pg_stat_activity \
    where pid != pg_backend_pid() \
      and backend_type = 'client backend' \
    order by xact_start nulls last";
    run_and_print(client, sql).await;
}

async fn dba_locks(client: &Client, _verbose: bool) {
    let sql = "select \
        blocked_locks.pid as blocked_pid, \
        blocked_activity.usename as blocked_user, \
        blocking_locks.pid as blocking_pid, \
        blocking_activity.usename as blocking_user, \
        blocked_activity.query as blocked_query, \
        blocking_activity.query as blocking_query \
    from pg_catalog.pg_locks as blocked_locks \
    join pg_catalog.pg_stat_activity as blocked_activity \
        on blocked_activity.pid = blocked_locks.pid \
    join pg_catalog.pg_locks as blocking_locks \
        on blocking_locks.locktype = blocked_locks.locktype \
        and blocking_locks.database \
                is not distinct from blocked_locks.database \
        and blocking_locks.relation \
                is not distinct from blocked_locks.relation \
        and blocking_locks.page \
                is not distinct from blocked_locks.page \
        and blocking_locks.tuple \
                is not distinct from blocked_locks.tuple \
        and blocking_locks.virtualxid \
                is not distinct from blocked_locks.virtualxid \
        and blocking_locks.transactionid \
                is not distinct from blocked_locks.transactionid \
        and blocking_locks.classid \
                is not distinct from blocked_locks.classid \
        and blocking_locks.objid \
                is not distinct from blocked_locks.objid \
        and blocking_locks.objsubid \
                is not distinct from blocked_locks.objsubid \
        and blocking_locks.pid != blocked_locks.pid \
    join pg_catalog.pg_stat_activity as blocking_activity \
        on blocking_activity.pid = blocking_locks.pid \
    where not blocked_locks.granted \
    order by blocked_activity.query_start";
    run_and_print(client, sql).await;
}

async fn dba_bloat(client: &Client, _verbose: bool) {
    let sql = "select \
        schemaname, \
        tablename, \
        pg_size_pretty(pg_total_relation_size( \
            schemaname || '.' || tablename)) as total_size, \
        case when n_live_tup > 0 \
             then round(100.0 * n_dead_tup \
                      / (n_live_tup + n_dead_tup), 1) \
             else 0 end as dead_pct, \
        n_live_tup, \
        n_dead_tup \
    from pg_stat_user_tables \
    where n_dead_tup > 0 \
    order by n_dead_tup desc \
    limit 20";
    run_and_print(client, sql).await;
}

async fn dba_vacuum(client: &Client, _verbose: bool) {
    let sql = "select \
        s.schemaname, \
        s.relname, \
        s.n_live_tup, \
        s.n_dead_tup, \
        s.last_vacuum, \
        s.last_autovacuum, \
        s.last_analyze, \
        s.last_autoanalyze, \
        s.vacuum_count, \
        s.autovacuum_count, \
        age(c.relfrozenxid) as xid_age \
    from pg_stat_user_tables as s \
    join pg_class as c \
        on c.relname = s.relname \
        and c.relnamespace = ( \
            select oid \
            from pg_namespace \
            where nspname = s.schemaname \
        ) \
    order by s.n_dead_tup desc \
    limit 30";
    run_and_print(client, sql).await;
}

async fn dba_tablesize(client: &Client, _verbose: bool) {
    let sql = "select \
        schemaname || '.' || tablename as relation, \
        pg_size_pretty(pg_total_relation_size( \
            schemaname || '.' || tablename)) as total, \
        pg_size_pretty(pg_relation_size( \
            schemaname || '.' || tablename)) as table_size, \
        pg_size_pretty( \
            pg_total_relation_size( \
                schemaname || '.' || tablename) \
            - pg_relation_size( \
                schemaname || '.' || tablename) \
        ) as indexes_toast, \
        pg_total_relation_size( \
            schemaname || '.' || tablename) as raw_total \
    from pg_tables \
    where schemaname not in ('pg_catalog', 'information_schema') \
    order by raw_total desc \
    limit 20";
    run_and_print(client, sql).await;
}

async fn dba_connections(client: &Client, _verbose: bool) {
    let sql = "select \
        state, \
        usename, \
        datname, \
        application_name, \
        count(*) as count \
    from pg_stat_activity \
    where backend_type = 'client backend' \
    group by state, usename, datname, application_name \
    order by count desc";
    run_and_print(client, sql).await;
}

async fn dba_unused_indexes(client: &Client, _verbose: bool) {
    let sql = "select \
        schemaname, \
        indexrelname, \
        relname, \
        pg_size_pretty(pg_relation_size(indexrelid)) as index_size, \
        idx_scan \
    from pg_stat_user_indexes \
    where idx_scan = 0 \
      and indexrelname not like 'pg_%' \
    order by pg_relation_size(indexrelid) desc \
    limit 20";
    run_and_print(client, sql).await;
}

async fn dba_seq_scans(client: &Client, _verbose: bool) {
    let sql = "select \
        schemaname, \
        relname, \
        seq_scan, \
        seq_tup_read, \
        idx_scan, \
        idx_tup_fetch, \
        case when seq_scan + idx_scan > 0 \
             then round(100.0 * seq_scan \
                      / (seq_scan + idx_scan), 1) \
             else 0 end as seq_pct \
    from pg_stat_user_tables \
    where seq_scan > 0 \
    order by seq_scan desc \
    limit 20";
    run_and_print(client, sql).await;
}

async fn dba_cache_hit(client: &Client, _verbose: bool) {
    let sql = "select \
        schemaname, \
        relname, \
        heap_blks_hit, \
        heap_blks_read, \
        case when heap_blks_hit + heap_blks_read > 0 \
             then round(100.0 * heap_blks_hit \
                      / (heap_blks_hit + heap_blks_read), 2) \
             else 100 end as hit_pct \
    from pg_statio_user_tables \
    where heap_blks_hit + heap_blks_read > 0 \
    order by heap_blks_hit + heap_blks_read desc \
    limit 20";
    run_and_print(client, sql).await;
}

async fn dba_replication(client: &Client, _verbose: bool) {
    let sql = "select \
        slot_name, \
        slot_type, \
        active, \
        pg_size_pretty(pg_wal_lsn_diff( \
            pg_current_wal_lsn(), restart_lsn)) as lag_bytes, \
        restart_lsn, \
        confirmed_flush_lsn \
    from pg_replication_slots \
    order by slot_name";
    run_and_print(client, sql).await;
}

async fn dba_waits(client: &Client, _verbose: bool) {
    let sql = "SELECT \
        coalesce(wait_event_type, 'CPU/Running') AS wait_type, \
        coalesce(wait_event, 'active') AS wait_event, \
        count(*) AS sessions, \
        count(*) FILTER (WHERE state = 'active') AS active, \
        count(*) FILTER (WHERE now() - query_start > interval '5 seconds') AS slow \
    FROM pg_stat_activity \
    WHERE pid != pg_backend_pid() \
      AND backend_type = 'client backend' \
    GROUP BY wait_event_type, wait_event \
    ORDER BY sessions DESC \
    LIMIT 25";
    run_and_print(client, sql).await;
}

async fn dba_indexes(client: &Client, _verbose: bool) {
    let report = crate::index_health::analyze(client).await;
    report.display();
}

async fn dba_config(client: &Client, _verbose: bool) {
    let sql = "select \
        name, \
        setting, \
        unit, \
        source, \
        context \
    from pg_settings \
    where source != 'default' \
      and source != 'override' \
    order by name";
    run_and_print(client, sql).await;
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture stdout output from a closure.
    ///
    /// Because tests run concurrently and stdout capture in Rust requires
    /// mutable access to global state, we use a `Vec<u8>` buffer and the
    /// output captured via `print_table` is verified on the buffer in tests
    /// that call `print_table` directly.
    #[test]
    fn print_table_empty_col_names() {
        // When no columns are present, only the row-count footer is printed.
        // This test verifies there is no panic; it does not capture stdout.
        let col_names: Vec<String> = Vec::new();
        let rows: Vec<Vec<String>> = Vec::new();
        print_table(&col_names, &rows);
    }

    #[test]
    fn print_table_with_data() {
        let col_names = vec!["name".to_owned(), "value".to_owned()];
        let rows = vec![
            vec!["alpha".to_owned(), "1".to_owned()],
            vec!["beta".to_owned(), "2".to_owned()],
        ];
        // Verify no panic on typical output.
        print_table(&col_names, &rows);
    }

    #[test]
    fn print_table_single_row() {
        let col_names = vec!["state".to_owned()];
        let rows = vec![vec!["active".to_owned()]];
        print_table(&col_names, &rows);
    }

    #[test]
    fn print_dba_help_no_panic() {
        print_dba_help();
    }
}
