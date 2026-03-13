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
/// `governance` is used for Supervised mode checks (index health proposals).
/// `capabilities` provides version-gated feature detection.
///
/// Returns optional text for AI interpretation (e.g. `\dba waits+`).
pub async fn execute(
    client: &Client,
    subcommand: &str,
    verbose: bool,
    governance: Option<&crate::config::GovernanceConfig>,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
) -> Option<String> {
    match subcommand {
        "activity" | "act" => {
            dba_activity(client, verbose).await;
            None
        }
        "locks" | "lock" => {
            dba_locks(client, verbose).await;
            None
        }
        "bloat" => {
            dba_bloat(client, verbose).await;
            None
        }
        "vacuum" | "vac" => {
            dba_vacuum(client, verbose).await;
            None
        }
        "tablesize" | "ts" => {
            dba_tablesize(client, verbose).await;
            None
        }
        "connections" | "conn" => {
            dba_connections(client, verbose).await;
            None
        }
        "unused-idx" | "unused" => {
            dba_unused_indexes(client, verbose).await;
            None
        }
        "seq-scans" | "seq" => {
            dba_seq_scans(client, verbose).await;
            None
        }
        "cache-hit" | "cache" => {
            dba_cache_hit(client, verbose).await;
            None
        }
        "replication" | "repl" => {
            dba_replication(client, verbose).await;
            None
        }
        "config" | "conf" => {
            dba_config(client, verbose).await;
            None
        }
        "waits" | "wait" => dba_waits(client, verbose).await,
        "indexes" | "idx" => {
            dba_indexes(client, verbose, governance).await;
            None
        }
        "progress" | "prog" => {
            dba_progress(client, None).await;
            None
        }
        "io" => {
            dba_io(client, verbose, capabilities).await;
            None
        }
        "" | "help" => {
            print_dba_help();
            None
        }
        _ => {
            // Handle two-word subcommands: `\dba progress vacuum`, etc.
            if let Some(rest) = subcommand
                .strip_prefix("progress ")
                .or_else(|| subcommand.strip_prefix("prog "))
            {
                dba_progress(client, Some(rest.trim())).await;
                return None;
            }
            eprintln!("\\dba: unknown subcommand \"{subcommand}\"");
            eprintln!("Try \\dba help for available subcommands.");
            None
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
    println!("  \\dba waits       Wait event breakdown (+ for AI interpretation)");
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
    println!("  \\dba progress    Long-running operation progress (pg_stat_progress_*)");
    println!("  \\dba io          I/O statistics by backend type (PG 16+, verbose: \\dba+ io)");
    println!();
    println!("Aliases: act, lock, wait, vac, ts, conn, idx, unused, seq, cache, repl, conf, prog");
    println!();
    println!("Progress sub-commands:");
    println!("  \\dba progress             All in-progress operations");
    println!("  \\dba progress vacuum      VACUUM progress");
    println!("  \\dba progress analyze     ANALYZE progress");
    println!("  \\dba progress create_index CREATE INDEX progress");
    println!("  \\dba progress cluster     CLUSTER / VACUUM FULL progress");
    println!("  \\dba progress copy        COPY progress");
    println!("  \\dba progress basebackup  Base backup progress");
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

/// Returns AI context text when `verbose` is true.
async fn dba_waits(client: &Client, verbose: bool) -> Option<String> {
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

    if !verbose {
        return None;
    }

    // Collect the same data as text for AI interpretation.
    let Ok(messages) = client.simple_query(sql).await else {
        return None;
    };

    let mut lines = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let wait_type = row.get(0).unwrap_or("?");
            let wait_event = row.get(1).unwrap_or("?");
            let sessions = row.get(2).unwrap_or("0");
            let active = row.get(3).unwrap_or("0");
            let slow = row.get(4).unwrap_or("0");
            lines.push(format!(
                "{wait_type}/{wait_event}: {sessions} sessions ({active} active, {slow} slow)"
            ));
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

async fn dba_indexes(
    client: &Client,
    _verbose: bool,
    governance: Option<&crate::config::GovernanceConfig>,
) {
    let report = crate::index_health::analyze(client).await;
    report.display();

    // In Supervised mode, offer to execute proposed remediation actions.
    let autonomy = governance.map_or(crate::governance::AutonomyLevel::Observe, |g| {
        g.autonomy_for(crate::governance::FeatureArea::IndexHealth)
    });
    if autonomy == crate::governance::AutonomyLevel::Supervised {
        let proposals = report.to_proposals();
        if !proposals.is_empty() {
            let mut audit_log = crate::governance::AuditLog::new();
            crate::rca_actions::run_supervised_flow(client, &proposals, &mut audit_log).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Progress monitoring (pg_stat_progress_*)
// ---------------------------------------------------------------------------

/// Show progress of long-running operations.
///
/// If `filter` is `None`, shows all in-progress operations.
/// If `filter` is `Some("vacuum")`, shows only VACUUM progress, etc.
async fn dba_progress(client: &Client, filter: Option<&str>) {
    match filter {
        None | Some("all") => {
            dba_progress_vacuum(client).await;
            dba_progress_analyze(client).await;
            dba_progress_create_index(client).await;
            dba_progress_cluster(client).await;
            dba_progress_copy(client).await;
            dba_progress_basebackup(client).await;
        }
        Some("vacuum" | "vac") => dba_progress_vacuum(client).await,
        Some("analyze") => dba_progress_analyze(client).await,
        Some("create_index" | "index" | "idx") => dba_progress_create_index(client).await,
        Some("cluster") => dba_progress_cluster(client).await,
        Some("copy") => dba_progress_copy(client).await,
        Some("basebackup" | "backup") => dba_progress_basebackup(client).await,
        Some(other) => {
            eprintln!("\\dba progress: unknown operation \"{other}\"");
            eprintln!("Available: vacuum, analyze, create_index, cluster, copy, basebackup");
        }
    }
}

async fn dba_progress_vacuum(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            a.datname, \
            p.relid::regclass as relation, \
            p.phase, \
            p.heap_blks_total, \
            p.heap_blks_scanned, \
            p.heap_blks_vacuumed, \
            case when p.heap_blks_total > 0 \
                 then round(100.0 * p.heap_blks_vacuumed \
                          / p.heap_blks_total, 1) \
                 else 0 end as pct_done, \
            p.index_vacuum_count, \
            p.max_dead_tuples, \
            p.num_dead_tuples \
        from pg_stat_progress_vacuum as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid";
    eprintln!("-- VACUUM progress --");
    run_and_print(client, sql).await;
}

async fn dba_progress_analyze(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            a.datname, \
            p.relid::regclass as relation, \
            p.phase, \
            p.sample_blks_total, \
            p.sample_blks_scanned, \
            case when p.sample_blks_total > 0 \
                 then round(100.0 * p.sample_blks_scanned \
                          / p.sample_blks_total, 1) \
                 else 0 end as pct_done, \
            p.ext_stats_total, \
            p.ext_stats_computed, \
            p.child_tables_total, \
            p.child_tables_done \
        from pg_stat_progress_analyze as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid";
    eprintln!("-- ANALYZE progress --");
    run_and_print(client, sql).await;
}

async fn dba_progress_create_index(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            a.datname, \
            p.relid::regclass as relation, \
            p.index_relid::regclass as index, \
            p.command, \
            p.phase, \
            p.lockers_total, \
            p.lockers_done, \
            p.blocks_total, \
            p.blocks_done, \
            case when p.blocks_total > 0 \
                 then round(100.0 * p.blocks_done \
                          / p.blocks_total, 1) \
                 else 0 end as pct_done, \
            p.tuples_total, \
            p.tuples_done \
        from pg_stat_progress_create_index as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid";
    eprintln!("-- CREATE INDEX progress --");
    run_and_print(client, sql).await;
}

async fn dba_progress_cluster(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            a.datname, \
            p.relid::regclass as relation, \
            p.command, \
            p.phase, \
            p.heap_blks_total, \
            p.heap_blks_scanned, \
            case when p.heap_blks_total > 0 \
                 then round(100.0 * p.heap_blks_scanned \
                          / p.heap_blks_total, 1) \
                 else 0 end as pct_done, \
            p.heap_tuples_scanned, \
            p.heap_tuples_written, \
            p.index_rebuild_count \
        from pg_stat_progress_cluster as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid";
    eprintln!("-- CLUSTER / VACUUM FULL progress --");
    run_and_print(client, sql).await;
}

async fn dba_progress_copy(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            a.datname, \
            p.relid::regclass as relation, \
            p.command, \
            p.type, \
            p.bytes_processed, \
            p.bytes_total, \
            case when p.bytes_total > 0 \
                 then round(100.0 * p.bytes_processed \
                          / p.bytes_total, 1) \
                 else null end as pct_done, \
            p.tuples_processed, \
            p.tuples_excluded \
        from pg_stat_progress_copy as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid";
    eprintln!("-- COPY progress --");
    run_and_print(client, sql).await;
}

async fn dba_progress_basebackup(client: &Client) {
    let sql = "\
        select \
            p.pid, \
            p.phase, \
            case when p.backup_total > 0 \
                 then round(100.0 * p.backup_streamed \
                          / p.backup_total, 1) \
                 else null end as pct_done, \
            pg_size_pretty(p.backup_streamed) as streamed, \
            pg_size_pretty(p.backup_total) as total, \
            p.tablespaces_total, \
            p.tablespaces_streamed \
        from pg_stat_progress_basebackup as p \
        order by p.pid";
    eprintln!("-- Base backup progress --");
    run_and_print(client, sql).await;
}

// ---------------------------------------------------------------------------
// I/O statistics (pg_stat_io, PG 16+)
// ---------------------------------------------------------------------------

async fn dba_io(
    client: &Client,
    verbose: bool,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
) {
    let has_io = capabilities.is_some_and(crate::capabilities::DbCapabilities::has_pg_stat_io);
    if !has_io {
        let ver = capabilities
            .and_then(|c| c.server_version.as_deref())
            .unwrap_or("unknown");
        eprintln!(
            "\\dba io: pg_stat_io requires PostgreSQL 16+. \
             Current server version: {ver}"
        );
        return;
    }

    if verbose {
        // Verbose: full breakdown including zero-activity rows.
        let sql = "\
            select \
                backend_type, \
                object, \
                context, \
                reads, \
                read_time, \
                writes, \
                write_time, \
                writebacks, \
                writeback_time, \
                extends, \
                extend_time, \
                hits, \
                evictions, \
                reuses, \
                fsyncs, \
                fsync_time, \
                stats_reset \
            from pg_stat_io \
            order by backend_type, object, context";
        run_and_print(client, sql).await;
    } else {
        // Non-verbose: only rows with actual activity.
        let sql = "\
            select \
                backend_type, \
                object, \
                context, \
                reads, \
                read_time, \
                writes, \
                write_time, \
                hits, \
                evictions, \
                fsyncs, \
                fsync_time \
            from pg_stat_io \
            where reads > 0 \
               or writes > 0 \
               or hits > 0 \
               or evictions > 0 \
               or fsyncs > 0 \
            order by reads + writes + hits desc";
        run_and_print(client, sql).await;
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

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
