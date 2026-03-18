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
/// `capabilities` provides version-gated feature detection.
/// `settings` is used to route output through the pager when appropriate.
///
/// Returns optional text for AI interpretation (e.g. `\dba waits+`).
#[allow(clippy::too_many_lines)]
pub async fn execute(
    client: &Client,
    subcommand: &str,
    verbose: bool,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
) -> Option<String> {
    match subcommand {
        "activity" | "act" => {
            dba_activity(client, verbose, capabilities, settings).await;
            None
        }
        "locks" | "lock" => {
            dba_locks(client, verbose, capabilities, settings).await;
            None
        }
        "bloat" => {
            dba_bloat(client, verbose, settings).await;
            None
        }
        "vacuum" | "vac" => {
            dba_vacuum(client, verbose, settings).await;
            None
        }
        "tablesize" | "ts" => {
            dba_tablesize(client, verbose, settings).await;
            None
        }
        "connections" | "conn" => {
            dba_connections(client, verbose, settings).await;
            None
        }
        "unused-idx" | "unused" => {
            dba_unused_indexes(client, verbose, settings).await;
            None
        }
        "invalid-idx" | "iidx" => {
            dba_invalid_indexes(client, verbose, settings).await;
            None
        }
        "redundant-idx" | "ridx" => {
            dba_redundant_indexes(client, verbose, settings).await;
            None
        }
        "missing-fk-idx" | "mfk" => {
            dba_missing_fk_indexes(client, verbose, settings).await;
            None
        }
        "seq-scans" | "seq" => {
            dba_seq_scans(client, verbose, settings).await;
            None
        }
        "cache-hit" | "cache" => {
            dba_cache_hit(client, verbose, settings).await;
            None
        }
        "replication" | "repl" => {
            dba_replication(client, verbose, settings).await;
            None
        }
        "config" | "conf" => {
            dba_config(client, verbose, settings).await;
            None
        }
        "waits" | "wait" => dba_waits(client, verbose, settings).await,
        "progress" | "prog" => {
            dba_progress(client, None, capabilities, settings).await;
            None
        }
        "io" => {
            dba_io(client, verbose, capabilities, settings).await;
            None
        }
        "" | "help" => {
            maybe_page(settings, &dba_help_text());
            None
        }
        _ => {
            // Handle two-word subcommands: `\dba progress vacuum`, etc.
            if let Some(rest) = subcommand
                .strip_prefix("progress ")
                .or_else(|| subcommand.strip_prefix("prog "))
            {
                dba_progress(client, Some(rest.trim()), capabilities, settings).await;
                return None;
            }
            eprintln!("\\dba: unknown subcommand \"{subcommand}\"");
            eprintln!("Try \\dba help for available subcommands.");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Pager helper
// ---------------------------------------------------------------------------

/// Print `text` through the pager if the content exceeds the terminal height,
/// otherwise print directly to stdout.
///
/// Mirrors the same logic used by `\?` (help) in `src/repl/mod.rs`.
fn maybe_page(settings: &mut crate::repl::ReplSettings, text: &str) {
    // Honour \o redirect: write to file instead of stdout/pager.
    if let Some(ref mut w) = settings.output_target {
        use std::io::Write;
        let _ = writeln!(w, "{text}");
        return;
    }
    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);
    if settings.pager_enabled
        && crate::pager::needs_paging_with_min(
            text,
            term_rows.saturating_sub(2),
            settings.pager_min_lines,
        )
    {
        if let Some(ref sl) = settings.statusline {
            sl.clear();
            sl.teardown_scroll_region();
        }
        // Use the public pager API directly.
        if let Some(ref cmd) = settings.pager_command {
            if let Err(e) = crate::pager::run_pager_external(cmd, text) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    eprintln!(
                        "rpg: pager '{cmd}' not found — check your PAGER setting \
                         (\\set PAGER off to disable)"
                    );
                } else {
                    eprintln!("rpg: pager error: {e}");
                }
                print!("{text}");
            }
        } else if let Err(e) = crate::pager::run_pager(text) {
            eprintln!("rpg: pager error: {e}");
            print!("{text}");
        }
        if let Some(ref sl) = settings.statusline {
            sl.setup_scroll_region();
            sl.render();
        }
    } else {
        print!("{text}");
    }
}

// ---------------------------------------------------------------------------
// Internal execution helper
// ---------------------------------------------------------------------------

/// Execute `sql` via `simple_query`, collect results, format a table, and
/// route the output through the pager when the content is long enough.
///
/// If the query fails, the error is printed to stderr and the function
/// returns without panicking.
async fn run_and_print(client: &Client, sql: &str, settings: &mut crate::repl::ReplSettings) {
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

            maybe_page(settings, &format_table(&col_names, &rows));
        }
        Err(e) => {
            if let Some(db_err) = e.as_db_error() {
                eprintln!("\\dba: {}", db_err.message());
            } else {
                eprintln!("\\dba: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Table formatter
// ---------------------------------------------------------------------------

/// Format a column-aligned table as a `String`.
///
/// Output matches the psql default aligned format:
/// ```text
///  col1 | col2
/// ------+------
///  val  | val
/// (N rows)
/// ```
fn format_table(col_names: &[String], rows: &[Vec<String>]) -> String {
    use std::fmt::Write as FmtWrite;

    let mut out = String::new();

    if col_names.is_empty() {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "({n} {word})");
        return out;
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

    // Header row.
    let header_line = header_cells
        .iter()
        .enumerate()
        .map(|(i, h)| format!(" {:<width$}", h, width = widths[i]))
        .collect::<Vec<_>>()
        .join(" |");
    let _ = writeln!(out, "{header_line}");

    // Separator.
    let sep_line = widths
        .iter()
        .map(|w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("+");
    let _ = writeln!(out, "{sep_line}");

    // Data rows.
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
        let _ = writeln!(out, "{row_line}");
    }

    // Footer.
    let n = rows.len();
    let word = if n == 1 { "row" } else { "rows" };
    let _ = writeln!(out, "({n} {word})");

    out
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn dba_help_text() -> String {
    let mut out = String::new();
    out.push_str("\\dba diagnostic commands:\n");
    out.push_str(
        "  \\dba activity    pg_stat_activity: grouped by state, \
         duration, wait events\n",
    );
    out.push_str("  \\dba locks       Lock tree (blocked/blocking)\n");
    out.push_str("  \\dba waits       Wait event breakdown (+ for AI interpretation)\n");
    out.push_str("  \\dba bloat       Table bloat estimates (dead tuples)\n");
    out.push_str("  \\dba vacuum      Vacuum status and dead tuples\n");
    out.push_str("  \\dba tablesize   Largest tables\n");
    out.push_str("  \\dba connections Connection counts by state\n");
    out.push_str("  \\dba unused-idx  Unused indexes\n");
    out.push_str("  \\dba invalid-idx Invalid indexes with drop/recreate DDL\n");
    out.push_str("  \\dba redundant-idx Indexes covered by a broader index\n");
    out.push_str("  \\dba missing-fk-idx FK constraints without a supporting index\n");
    out.push_str("  \\dba seq-scans   Tables with high sequential scan ratio\n");
    out.push_str("  \\dba cache-hit   Buffer cache hit ratios\n");
    out.push_str("  \\dba replication Replication slot status\n");
    out.push_str("  \\dba config      Non-default configuration parameters\n");
    out.push_str("  \\dba progress    Long-running operation progress (pg_stat_progress_*)\n");
    out.push_str(
        "  \\dba io          I/O statistics by backend type (PG 16+, verbose: \\dba+ io)\n",
    );
    out.push('\n');
    out.push_str(
        "Aliases: act, lock, wait, vac, ts, conn, \
         unused, iidx, ridx, mfk, seq, cache, repl, conf, prog\n",
    );
    out.push('\n');
    out.push_str("Progress sub-commands:\n");
    out.push_str("  \\dba progress             All in-progress operations\n");
    out.push_str("  \\dba progress vacuum      VACUUM progress\n");
    out.push_str("  \\dba progress analyze     ANALYZE progress\n");
    out.push_str("  \\dba progress create_index CREATE INDEX progress\n");
    out.push_str("  \\dba progress cluster     CLUSTER / VACUUM FULL progress\n");
    out.push_str("  \\dba progress copy        COPY progress\n");
    out.push_str("  \\dba progress basebackup  Base backup progress\n");
    out
}

// ---------------------------------------------------------------------------
// Activity summary helpers
// ---------------------------------------------------------------------------

/// Counts of sessions by state, used for the summary line in `\dba activity`.
#[derive(Debug, Default, PartialEq, Eq)]
struct ActivityCounts {
    active: usize,
    idle_in_xact: usize,
    idle: usize,
    other: usize,
}

impl ActivityCounts {
    /// Total sessions across all states.
    fn total(&self) -> usize {
        self.active + self.idle_in_xact + self.idle + self.other
    }

    /// Tally a single state string into the correct bucket.
    fn tally(&mut self, state: &str) {
        match state {
            "active" => self.active += 1,
            "idle in transaction" | "idle in transaction (aborted)" => {
                self.idle_in_xact += 1;
            }
            "idle" => self.idle += 1,
            _ => self.other += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Subcommand handlers
// ---------------------------------------------------------------------------

async fn dba_activity(
    client: &Client,
    _verbose: bool,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
) {
    use std::fmt::Write as FmtWrite;
    use tokio_postgres::SimpleQueryMessage;

    // PG 14-18: all required columns exist.  The query_id column was added
    // in PG 14 but is not projected here, so no version branching is needed
    // for the core columns.  We keep this note for future maintainers.
    let pg_ver = capabilities
        .and_then(crate::capabilities::DbCapabilities::pg_major_version)
        .unwrap_or(14);
    crate::logging::trace("dba", &format!("dba_activity: pg_major_version={pg_ver}"));

    // duration column:
    //   active / idle in transaction  → time since query_start
    //   idle / other                  → time since state_change
    // Sorted: active (longest first), idle in transaction, then idle/other.
    // Excludes rpg's own backend via pg_backend_pid().
    let sql = "\
        select \
            pid, \
            usename as user, \
            datname as database, \
            coalesce(client_addr::text, '') as client_addr, \
            coalesce(state, '') as state, \
            coalesce(wait_event_type, '') as wait_event_type, \
            coalesce(wait_event, '') as wait_event, \
            backend_type, \
            case \
                when state in ('active', 'idle in transaction', \
                               'idle in transaction (aborted)') \
                    then to_char( \
                        extract(epoch from (now() - query_start))::int, \
                        '99999999') || 's' \
                when state_change is not null \
                    then to_char( \
                        extract(epoch from (now() - state_change))::int, \
                        '99999999') || 's' \
                else '' \
            end as duration, \
            left(coalesce(query, ''), 80) as query \
        from pg_stat_activity \
        where pid != pg_backend_pid() \
        order by \
            case state \
                when 'active'                          then 1 \
                when 'idle in transaction'             then 2 \
                when 'idle in transaction (aborted)'   then 3 \
                else                                        4 \
            end, \
            case \
                when state in ('active', 'idle in transaction', \
                               'idle in transaction (aborted)') \
                    then now() - query_start \
                else now() - state_change \
            end desc nulls last";

    crate::logging::trace("dba", &format!("diagnostic query: {}", sql.trim()));

    let messages = match client.simple_query(sql).await {
        Ok(msgs) => msgs,
        Err(e) => {
            eprintln!("\\dba activity: {e}");
            return;
        }
    };

    let mut col_names: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut counts = ActivityCounts::default();

    // Column indices — resolved once we have the first row.
    let mut state_col: Option<usize> = None;

    for msg in &messages {
        if let SimpleQueryMessage::Row(row) = msg {
            if col_names.is_empty() {
                col_names = (0..row.len())
                    .map(|i| {
                        row.columns()
                            .get(i)
                            .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                    })
                    .collect();
                state_col = col_names.iter().position(|n| n == "state");
            }
            let vals: Vec<String> = (0..row.len())
                .map(|i| row.get(i).unwrap_or("").to_owned())
                .collect();

            // Tally state counts.
            let state = state_col
                .and_then(|ci| vals.get(ci))
                .map_or("", String::as_str);
            counts.tally(state);

            rows.push(vals);
        }
    }

    let mut text = format_table(&col_names, &rows);

    // Summary line.
    let _ = writeln!(
        text,
        "{} active, {} idle in transaction, {} idle, {} total",
        counts.active,
        counts.idle_in_xact,
        counts.idle,
        counts.total()
    );

    maybe_page(settings, &text);
}

// ---------------------------------------------------------------------------
// Lock-tree data structures
// ---------------------------------------------------------------------------

/// One row returned by the lock-tree diagnostic query.
struct LockEdge {
    /// PID that is waiting (blocked).
    blocked_pid: i32,
    /// Username of the blocked session.
    blocked_user: String,
    /// Database name of the blocked session.
    blocked_db: String,
    /// PID that holds the conflicting lock (blocker).
    blocking_pid: i32,
    /// Username of the blocking session.
    blocking_user: String,
    /// Database name of the blocking session.
    blocking_db: String,
    /// Lock type (e.g. `"relation"`, `"transactionid"`).
    lock_type: String,
    /// Lock mode the blocked session is requesting.
    lock_mode: String,
    /// Relation name the lock is on, if applicable.
    relation: String,
    /// How long the blocked session has been waiting (human-readable).
    wait_duration: String,
    /// Query text of the blocked session (first 80 chars).
    blocked_query: String,
    /// Query text of the blocking session (first 80 chars).
    blocking_query: String,
}

/// Collect lock edges from `pg_locks` joined with `pg_stat_activity`.
///
/// Uses `waitstart` (PG 14+) for accurate wait duration; falls back to
/// `query_start` on older versions where `waitstart` may be absent.
async fn collect_lock_edges(
    client: &Client,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
) -> Vec<LockEdge> {
    // waitstart was added in PG 14. Since we target PG 14-18 it is always
    // available, but guard defensively in case the capabilities probe failed.
    let has_waitstart = capabilities.is_none_or(|c| c.pg_major_version().is_none_or(|v| v >= 14));

    let sql = build_locks_sql(has_waitstart);
    crate::logging::trace("dba", &format!("locks diagnostic query: {}", sql.trim()));

    let messages = match client.simple_query(&sql).await {
        Ok(m) => m,
        Err(e) => {
            if let Some(db_err) = e.as_db_error() {
                eprintln!("\\dba locks: {}", db_err.message());
            } else {
                eprintln!("\\dba locks: {e}");
            }
            return Vec::new();
        }
    };

    parse_lock_edges(messages)
}

/// Build the lock-tree diagnostic SQL.
///
/// `has_waitstart` selects between the `waitstart` column (PG 14+) and the
/// `query_start` fallback for the wait-duration calculation.
fn build_locks_sql(has_waitstart: bool) -> String {
    let wait_expr = if has_waitstart {
        "extract(epoch from (now() - blocked_locks.waitstart))"
    } else {
        "extract(epoch from (now() - blocked_activity.query_start))"
    };

    // SQL keywords are lowercase per CLAUDE.md style guide.
    format!(
        "\
        with lock_pairs as (\
            select \
                blocked_locks.pid                          as blocked_pid, \
                blocked_activity.usename                   as blocked_user, \
                coalesce(blocked_activity.datname, '')     as blocked_db, \
                blocking_locks.pid                         as blocking_pid, \
                blocking_activity.usename                  as blocking_user, \
                coalesce(blocking_activity.datname, '')    as blocking_db, \
                blocked_locks.locktype                     as lock_type, \
                blocked_locks.mode                         as lock_mode, \
                coalesce(\
                    (select relname \
                     from pg_catalog.pg_class \
                     where oid = blocked_locks.relation), \
                    '') as relation, \
                {wait_expr}                                as wait_secs, \
                left(coalesce(blocked_activity.query, ''), 80)  as blocked_query, \
                left(coalesce(blocking_activity.query, ''), 80) as blocking_query \
            from pg_catalog.pg_locks as blocked_locks \
            join pg_catalog.pg_stat_activity as blocked_activity \
                on blocked_activity.pid = blocked_locks.pid \
            join pg_catalog.pg_locks as blocking_locks \
                on  blocking_locks.locktype = blocked_locks.locktype \
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
        ) \
        select \
            blocked_pid, \
            blocked_user, \
            blocked_db, \
            blocking_pid, \
            blocking_user, \
            blocking_db, \
            lock_type, \
            lock_mode, \
            relation, \
            coalesce(wait_secs::text, '0') as wait_secs, \
            blocked_query, \
            blocking_query \
        from lock_pairs \
        order by wait_secs desc nulls last"
    )
}

/// Parse `SimpleQueryMessage` rows into `LockEdge` values.
fn parse_lock_edges(messages: Vec<tokio_postgres::SimpleQueryMessage>) -> Vec<LockEdge> {
    let mut edges = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let get = |i: usize| row.get(i).unwrap_or("").to_owned();
            let blocked_pid: i32 = get(0).parse().unwrap_or(0);
            let blocked_user = get(1);
            let blocked_db = get(2);
            let blocking_pid: i32 = get(3).parse().unwrap_or(0);
            let blocking_user = get(4);
            let blocking_db = get(5);
            let lock_type = get(6);
            let lock_mode = get(7);
            let relation = get(8);
            let wait_secs: f64 = get(9).parse().unwrap_or(0.0);
            let blocked_query = get(10);
            let blocking_query = get(11);
            let wait_duration = format_duration_secs(wait_secs);

            edges.push(LockEdge {
                blocked_pid,
                blocked_user,
                blocked_db,
                blocking_pid,
                blocking_user,
                blocking_db,
                lock_type,
                lock_mode,
                relation,
                wait_duration,
                blocked_query,
                blocking_query,
            });
        }
    }
    edges
}

/// Format a duration given as fractional seconds into a short human string.
///
/// Examples: `"0.3s"`, `"5.2s"`, `"2m 14.0s"`.
fn format_duration_secs(secs: f64) -> String {
    if secs < 0.0 {
        return "0.0s".to_owned();
    }
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        // Safe cast: secs >= 60.0 and floor(secs) fits in u64 for any
        // realistic lock-wait duration.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let total_secs = secs.floor() as u64;
        let mins = total_secs / 60;
        // Safe: mins is a small u64, well within f64 mantissa precision.
        #[allow(clippy::cast_precision_loss)]
        let rem = secs - (mins as f64 * 60.0);
        format!("{mins}m {rem:.1}s")
    }
}

/// One node in the rendered lock tree.
struct LockNode {
    pid: i32,
    user: String,
    db: String,
    /// Granted lock mode if this is a root (blocker), or requested mode if
    /// this is a waiting child.
    mode: String,
    relation: String,
    lock_type: String,
    /// `None` for root nodes (blockers); `Some(duration)` for waiters.
    wait_duration: Option<String>,
    /// Query text (first 80 chars).
    query: String,
    /// PIDs that are waiting on this node.
    children: Vec<LockNode>,
}

/// Build a forest of `LockNode` trees from the collected edges.
///
/// Roots are blocker PIDs that do not themselves appear as `blocked_pid`.
/// Children are the sessions waiting on each root (recursively).
fn build_lock_forest(edges: &[LockEdge]) -> Vec<LockNode> {
    use std::collections::{HashMap, HashSet};

    if edges.is_empty() {
        return Vec::new();
    }

    // Collect the set of all blocked PIDs.
    let blocked_pids: HashSet<i32> = edges.iter().map(|e| e.blocked_pid).collect();

    // Build a map: blocker_pid -> list of edges where that PID blocks.
    let mut blocker_map: HashMap<i32, Vec<&LockEdge>> = HashMap::new();
    for edge in edges {
        blocker_map.entry(edge.blocking_pid).or_default().push(edge);
    }

    // Root blockers: appear as blocking_pid but NOT as blocked_pid.
    // Use a BTreeMap for deterministic ordering by PID.
    let mut root_pids: Vec<i32> = blocker_map
        .keys()
        .filter(|pid| !blocked_pids.contains(pid))
        .copied()
        .collect();
    root_pids.sort_unstable();

    // Gather blocker info from the first edge that mentions each root.
    let root_nodes: Vec<LockNode> = root_pids
        .into_iter()
        .filter_map(|root_pid| {
            let children_edges = blocker_map.get(&root_pid)?;
            // Take identity from the first edge.
            let first = children_edges[0];
            // Determine the granted mode for the blocker: look for a
            // granted lock in pg_locks. We don't have that here, so we
            // display the mode the blocker holds as the conflicting mode
            // of the first waiting child.
            let mode = first.lock_mode.clone();
            let relation = first.relation.clone();
            let lock_type = first.lock_type.clone();

            let children = build_children(root_pid, &blocker_map, &blocked_pids);

            Some(LockNode {
                pid: root_pid,
                user: first.blocking_user.clone(),
                db: first.blocking_db.clone(),
                mode,
                relation,
                lock_type,
                wait_duration: None,
                query: first.blocking_query.clone(),
                children,
            })
        })
        .collect();

    root_nodes
}

/// Recursively build child nodes for `parent_pid`.
fn build_children(
    parent_pid: i32,
    blocker_map: &std::collections::HashMap<i32, Vec<&LockEdge>>,
    blocked_pids: &std::collections::HashSet<i32>,
) -> Vec<LockNode> {
    let Some(edges) = blocker_map.get(&parent_pid) else {
        return Vec::new();
    };

    // Deduplicate by blocked_pid (a session may appear multiple times due
    // to multiple lock types being contested simultaneously).
    let mut seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut children: Vec<LockNode> = Vec::new();

    for edge in edges {
        if seen.insert(edge.blocked_pid) {
            let grandchildren = if blocked_pids.contains(&edge.blocked_pid) {
                build_children(edge.blocked_pid, blocker_map, blocked_pids)
            } else {
                Vec::new()
            };

            children.push(LockNode {
                pid: edge.blocked_pid,
                user: edge.blocked_user.clone(),
                db: edge.blocked_db.clone(),
                mode: edge.lock_mode.clone(),
                relation: edge.relation.clone(),
                lock_type: edge.lock_type.clone(),
                wait_duration: Some(edge.wait_duration.clone()),
                query: edge.blocked_query.clone(),
                children: grandchildren,
            });
        }
    }

    // Sort children by wait duration descending for visual clarity.
    children.sort_by(|a, b| b.wait_duration.cmp(&a.wait_duration));

    children
}

/// Render the lock forest to a `String` with tree-drawing characters.
///
/// Example output:
/// ```text
/// PID 1234 (alice@mydb) GRANTED AccessExclusiveLock on users
///   query: update users set ...
/// ├─ PID 5678 (bob@mydb) WAITING 5.2s RowExclusiveLock on users
/// │  query: insert into users ...
/// │  └─ PID 9012 (eve@mydb) WAITING 3.1s AccessShareLock on users
/// │     query: select * from users ...
/// └─ PID 3456 (carol@mydb) WAITING 4.8s RowShareLock on users
///    query: select * from users ...
/// ```
fn render_lock_forest(forest: &[LockNode]) -> String {
    let mut out = String::new();

    if forest.is_empty() {
        out.push_str("No blocking locks detected.\n");
        return out;
    }

    for (i, root) in forest.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_node(root, "", true, true, &mut out);
    }

    out
}

/// Render a single node and its subtree into `out`.
///
/// `prefix` is the leading string for all lines of this node's children.
/// `is_root` means the node is a top-level blocker (no tree connector).
/// `is_last` means this is the last child of its parent.
fn render_node(node: &LockNode, prefix: &str, is_root: bool, is_last: bool, out: &mut String) {
    use std::fmt::Write as FmtWrite;

    // Choose connector.
    let connector = if is_root {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };

    let relation_part = if node.relation.is_empty() {
        node.lock_type.clone()
    } else {
        format!("{} on {}", node.lock_type, node.relation)
    };

    if let Some(ref dur) = node.wait_duration {
        let _ = writeln!(
            out,
            "{prefix}{connector}PID {} ({}@{}) WAITING {} {} {}",
            node.pid, node.user, node.db, dur, node.mode, relation_part
        );
    } else {
        let _ = writeln!(
            out,
            "{prefix}{connector}PID {} ({}@{}) GRANTED {} {}",
            node.pid, node.user, node.db, node.mode, relation_part
        );
    }

    // Query line — indented one extra level.
    let query_prefix = if is_root {
        "  ".to_owned()
    } else if is_last {
        format!("{prefix}   ")
    } else {
        format!("{prefix}│  ")
    };
    if !node.query.is_empty() {
        let _ = writeln!(out, "{query_prefix}query: {}", node.query);
    }

    // Render children.
    let child_prefix = if is_root {
        String::new()
    } else if is_last {
        format!("{prefix}   ")
    } else {
        format!("{prefix}│  ")
    };

    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let child_is_last = i + 1 == n;
        render_node(child, &child_prefix, false, child_is_last, out);
    }
}

/// `\dba locks` — display a lock tree visualization.
async fn dba_locks(
    client: &Client,
    _verbose: bool,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
) {
    let edges = collect_lock_edges(client, capabilities).await;
    let forest = build_lock_forest(&edges);
    maybe_page(settings, &render_lock_forest(&forest));
}

async fn dba_bloat(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
    // pg_stat_user_tables uses `relname`, not `tablename`.
    let sql = "select \
        schemaname, \
        relname as tablename, \
        pg_size_pretty(pg_total_relation_size( \
            schemaname || '.' || relname)) as total_size, \
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
    run_and_print(client, sql, settings).await;
}

async fn dba_vacuum(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
    // Raw vacuum status table (psql-style tabular output).
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
    run_and_print(client, sql, settings).await;
}

async fn dba_tablesize(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_connections(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_unused_indexes(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_invalid_indexes(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
    let sql = "\
        select \
            n.nspname as schema_name, \
            t.relname as table_name, \
            i.relname as index_name, \
            pg_size_pretty(pg_relation_size(ix.indexrelid)) as index_size, \
            'drop index concurrently ' \
                || quote_ident(n.nspname) || '.' \
                || quote_ident(i.relname) || ';' as drop_code, \
            pg_get_indexdef(ix.indexrelid) || ';' as revert_code \
        from \
            pg_catalog.pg_index as ix \
            join pg_catalog.pg_class as t on t.oid = ix.indrelid \
            join pg_catalog.pg_class as i on i.oid = ix.indexrelid \
            join pg_catalog.pg_namespace as n on n.oid = t.relnamespace \
        where \
            not ix.indisvalid \
            and n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast') \
        order by \
            pg_relation_size(ix.indexrelid) desc";
    run_and_print(client, sql, settings).await;
}

async fn dba_redundant_indexes(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
    let sql = "\
        select \
            s.schemaname, \
            s.relname, \
            s.indexrelname as redundant_index, \
            s.idx_scan as redundant_scans, \
            pg_size_pretty(pg_relation_size(s.indexrelid)) as redundant_size, \
            s2.indexrelname as covering_index, \
            s2.idx_scan as covering_scans \
        from \
            pg_catalog.pg_stat_user_indexes as s \
            join pg_catalog.pg_index as i on i.indexrelid = s.indexrelid \
            join pg_catalog.pg_stat_user_indexes as s2 \
                on s2.relid = s.relid \
                and s2.indexrelid <> s.indexrelid \
            join pg_catalog.pg_index as i2 on i2.indexrelid = s2.indexrelid \
        where \
            s.schemaname not in ('pg_catalog', 'information_schema') \
            and not i.indisprimary \
            and not i2.indisprimary \
            and i.indkey::text like (i2.indkey::text || '%') \
            and i.indkey <> i2.indkey \
        order by \
            pg_relation_size(s.indexrelid) desc \
        limit 20";
    run_and_print(client, sql, settings).await;
}

async fn dba_missing_fk_indexes(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
    let sql = "\
        select \
            n.nspname as schema_name, \
            t.relname as table_name, \
            array_to_string( \
                array_agg(a.attname order by x.n), \
                ', ' \
            ) as fk_columns, \
            c.conname as fk_constraint, \
            pg_size_pretty(pg_relation_size(t.oid)) as table_size \
        from \
            pg_catalog.pg_constraint as c \
            join pg_catalog.pg_class as t on t.oid = c.conrelid \
            join pg_catalog.pg_namespace as n on n.oid = t.relnamespace \
            join lateral unnest(c.conkey) with ordinality as x(attnum, n) on true \
            join pg_catalog.pg_attribute as a \
                on a.attrelid = t.oid and a.attnum = x.attnum \
        where \
            c.contype = 'f' \
            and n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast') \
            and not exists ( \
                select 1 \
                from pg_catalog.pg_index as i \
                where \
                    i.indrelid = t.oid \
                    and (i.indkey::int2[])[0:array_length(c.conkey, 1) - 1] @> c.conkey \
            ) \
        group by \
            n.nspname, t.relname, c.conname, t.oid \
        having \
            pg_relation_size(t.oid) > 1048576 \
        order by \
            pg_relation_size(t.oid) desc \
        limit 20";
    run_and_print(client, sql, settings).await;
}

async fn dba_seq_scans(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_cache_hit(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_replication(
    client: &Client,
    _verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) {
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
    run_and_print(client, sql, settings).await;
}

/// Returns AI context text when `verbose` is true.
async fn dba_waits(
    client: &Client,
    verbose: bool,
    settings: &mut crate::repl::ReplSettings,
) -> Option<String> {
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
    run_and_print(client, sql, settings).await;

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

// ---------------------------------------------------------------------------
// Progress monitoring (pg_stat_progress_*)
// ---------------------------------------------------------------------------

/// Show progress of long-running operations.
///
/// If `filter` is `None`, shows all in-progress operations.
/// If `filter` is `Some("vacuum")`, shows only VACUUM progress, etc.
async fn dba_progress(
    client: &Client,
    filter: Option<&str>,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
) {
    match filter {
        None | Some("all") => {
            dba_progress_vacuum(client, capabilities, settings).await;
            dba_progress_analyze(client, settings).await;
            dba_progress_create_index(client, settings).await;
            dba_progress_cluster(client, settings).await;
            dba_progress_copy(client, settings).await;
            dba_progress_basebackup(client, settings).await;
        }
        Some("vacuum" | "vac") => dba_progress_vacuum(client, capabilities, settings).await,
        Some("analyze") => dba_progress_analyze(client, settings).await,
        Some("create_index" | "index" | "idx") => {
            dba_progress_create_index(client, settings).await;
        }
        Some("cluster") => dba_progress_cluster(client, settings).await,
        Some("copy") => dba_progress_copy(client, settings).await,
        Some("basebackup" | "backup") => dba_progress_basebackup(client, settings).await,
        Some(other) => {
            eprintln!("\\dba progress: unknown operation \"{other}\"");
            eprintln!("Available: vacuum, analyze, create_index, cluster, copy, basebackup");
        }
    }
}

async fn dba_progress_vacuum(
    client: &Client,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
) {
    // In PG 17 max_dead_tuples/num_dead_tuples were renamed to
    // max_dead_tuple_bytes/num_dead_item_ids.
    let use_pg17_cols = capabilities
        .and_then(crate::capabilities::DbCapabilities::pg_major_version)
        .is_some_and(|v| v >= 17);
    let dead_cols = if use_pg17_cols {
        "p.max_dead_tuple_bytes, \
            p.num_dead_item_ids"
    } else {
        "p.max_dead_tuples, \
            p.num_dead_tuples"
    };
    let sql = format!(
        "\
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
            {dead_cols} \
        from pg_stat_progress_vacuum as p \
        join pg_stat_activity as a \
            on a.pid = p.pid \
        order by p.pid"
    );
    eprintln!("-- VACUUM progress --");
    run_and_print(client, &sql, settings).await;
}

async fn dba_progress_analyze(client: &Client, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_progress_create_index(client: &Client, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_progress_cluster(client: &Client, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_progress_copy(client: &Client, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

async fn dba_progress_basebackup(client: &Client, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
}

// ---------------------------------------------------------------------------
// I/O statistics (pg_stat_io, PG 16+)
// ---------------------------------------------------------------------------

async fn dba_io(
    client: &Client,
    verbose: bool,
    capabilities: Option<&crate::capabilities::DbCapabilities>,
    settings: &mut crate::repl::ReplSettings,
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
        run_and_print(client, sql, settings).await;
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
        run_and_print(client, sql, settings).await;
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

async fn dba_config(client: &Client, _verbose: bool, settings: &mut crate::repl::ReplSettings) {
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
    run_and_print(client, sql, settings).await;
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
    /// output captured via `format_table` is verified on the buffer in tests
    /// that call `format_table` directly.
    #[test]
    fn format_table_empty_col_names() {
        // When no columns are present, only the row-count footer is returned.
        let col_names: Vec<String> = Vec::new();
        let rows: Vec<Vec<String>> = Vec::new();
        let out = format_table(&col_names, &rows);
        assert!(
            out.contains("(0 rows)"),
            "expected row-count footer, got: {out}"
        );
    }

    #[test]
    fn format_table_with_data() {
        let col_names = vec!["name".to_owned(), "value".to_owned()];
        let rows = vec![
            vec!["alpha".to_owned(), "1".to_owned()],
            vec!["beta".to_owned(), "2".to_owned()],
        ];
        let out = format_table(&col_names, &rows);
        assert!(out.contains("name"), "header missing: {out}");
        assert!(out.contains("alpha"), "data missing: {out}");
        assert!(out.contains("(2 rows)"), "footer missing: {out}");
    }

    #[test]
    fn format_table_single_row() {
        let col_names = vec!["state".to_owned()];
        let rows = vec![vec!["active".to_owned()]];
        let out = format_table(&col_names, &rows);
        assert!(
            out.contains("(1 row)"),
            "expected singular footer, got: {out}"
        );
    }

    #[test]
    fn dba_help_text_no_panic() {
        let text = dba_help_text();
        assert!(!text.is_empty());
    }

    // -- ActivityCounts tests ------------------------------------------------

    #[test]
    fn activity_counts_default_is_zero() {
        let c = ActivityCounts::default();
        assert_eq!(c.active, 0);
        assert_eq!(c.idle_in_xact, 0);
        assert_eq!(c.idle, 0);
        assert_eq!(c.other, 0);
        assert_eq!(c.total(), 0);
    }

    #[test]
    fn activity_counts_tally_active() {
        let mut c = ActivityCounts::default();
        c.tally("active");
        assert_eq!(c.active, 1);
        assert_eq!(c.total(), 1);
    }

    #[test]
    fn activity_counts_tally_idle_in_transaction() {
        let mut c = ActivityCounts::default();
        c.tally("idle in transaction");
        assert_eq!(c.idle_in_xact, 1);
        assert_eq!(c.total(), 1);
    }

    #[test]
    fn activity_counts_tally_idle_in_transaction_aborted() {
        let mut c = ActivityCounts::default();
        c.tally("idle in transaction (aborted)");
        assert_eq!(c.idle_in_xact, 1);
    }

    #[test]
    fn activity_counts_tally_idle() {
        let mut c = ActivityCounts::default();
        c.tally("idle");
        assert_eq!(c.idle, 1);
    }

    #[test]
    fn activity_counts_tally_other_states() {
        let mut c = ActivityCounts::default();
        c.tally("fastpath function call");
        c.tally("disabled");
        c.tally("");
        assert_eq!(c.other, 3);
    }

    #[test]
    fn activity_counts_total_is_sum() {
        let mut c = ActivityCounts::default();
        c.tally("active");
        c.tally("active");
        c.tally("idle in transaction");
        c.tally("idle");
        c.tally("disabled");
        assert_eq!(c.active, 2);
        assert_eq!(c.idle_in_xact, 1);
        assert_eq!(c.idle, 1);
        assert_eq!(c.other, 1);
        assert_eq!(c.total(), 5);
    }

    // -----------------------------------------------------------------------
    // Lock-tree helpers
    // -----------------------------------------------------------------------

    #[test]
    fn format_duration_secs_sub_minute() {
        assert_eq!(format_duration_secs(5.2), "5.2s");
        assert_eq!(format_duration_secs(0.3), "0.3s");
        assert_eq!(format_duration_secs(59.9), "59.9s");
    }

    #[test]
    fn format_duration_secs_over_minute() {
        // 90 s → 1m 30.0s  (rounds to nearest second for minutes part)
        let s = format_duration_secs(90.0);
        assert!(s.starts_with("1m"), "expected 1m ..., got {s}");
        assert!(s.contains("30.0s"), "expected 30.0s, got {s}");
    }

    #[test]
    fn format_duration_secs_negative_is_zero() {
        assert_eq!(format_duration_secs(-1.0), "0.0s");
    }

    #[test]
    fn build_lock_forest_empty() {
        let forest = build_lock_forest(&[]);
        assert!(forest.is_empty());
    }

    /// Helper to build a minimal `LockEdge` for testing.
    fn make_edge(
        blocking_pid: i32,
        blocking_user: &str,
        blocked_pid: i32,
        blocked_user: &str,
        lock_mode: &str,
        relation: &str,
        wait_secs: f64,
    ) -> LockEdge {
        LockEdge {
            blocked_pid,
            blocked_user: blocked_user.to_owned(),
            blocked_db: "testdb".to_owned(),
            blocking_pid,
            blocking_user: blocking_user.to_owned(),
            blocking_db: "testdb".to_owned(),
            lock_type: "relation".to_owned(),
            lock_mode: lock_mode.to_owned(),
            relation: relation.to_owned(),
            wait_duration: format_duration_secs(wait_secs),
            blocked_query: "select 1".to_owned(),
            blocking_query: "update t set x=1".to_owned(),
        }
    }

    #[test]
    fn build_lock_forest_single_edge() {
        let edges = vec![make_edge(
            1234,
            "alice",
            5678,
            "bob",
            "RowExclusiveLock",
            "users",
            5.2,
        )];
        let forest = build_lock_forest(&edges);
        assert_eq!(forest.len(), 1, "expected one root node");
        let root = &forest[0];
        assert_eq!(root.pid, 1234);
        assert!(root.wait_duration.is_none(), "root should be GRANTED");
        assert_eq!(root.children.len(), 1);
        let child = &root.children[0];
        assert_eq!(child.pid, 5678);
        assert!(child.wait_duration.is_some(), "child should be WAITING");
    }

    #[test]
    fn build_lock_forest_chain() {
        // 1234 blocks 5678 which blocks 9012
        let edges = vec![
            make_edge(1234, "alice", 5678, "bob", "AccessExclusiveLock", "t", 10.0),
            make_edge(5678, "bob", 9012, "eve", "RowExclusiveLock", "t", 5.0),
        ];
        let forest = build_lock_forest(&edges);
        assert_eq!(forest.len(), 1);
        let root = &forest[0];
        assert_eq!(root.pid, 1234);
        assert_eq!(root.children.len(), 1);
        let mid = &root.children[0];
        assert_eq!(mid.pid, 5678);
        assert_eq!(mid.children.len(), 1);
        let leaf = &mid.children[0];
        assert_eq!(leaf.pid, 9012);
        assert!(leaf.children.is_empty());
    }

    #[test]
    fn build_lock_forest_fan_out() {
        // 1234 blocks both 5678 and 9012
        let edges = vec![
            make_edge(1234, "alice", 5678, "bob", "AccessExclusiveLock", "t", 5.0),
            make_edge(1234, "alice", 9012, "eve", "RowExclusiveLock", "t", 3.0),
        ];
        let forest = build_lock_forest(&edges);
        assert_eq!(forest.len(), 1);
        let root = &forest[0];
        assert_eq!(root.pid, 1234);
        assert_eq!(root.children.len(), 2);
        let pids: Vec<i32> = root.children.iter().map(|c| c.pid).collect();
        assert!(pids.contains(&5678));
        assert!(pids.contains(&9012));
    }

    #[test]
    fn render_lock_forest_empty() {
        let out = render_lock_forest(&[]);
        assert!(out.contains("No blocking locks detected."), "got: {out}");
    }

    #[test]
    fn render_lock_forest_no_panic_with_data() {
        let edges = vec![make_edge(
            1234,
            "alice",
            5678,
            "bob",
            "RowExclusiveLock",
            "users",
            5.2,
        )];
        let forest = build_lock_forest(&edges);
        // Should not panic and should produce non-empty output.
        let out = render_lock_forest(&forest);
        assert!(!out.is_empty());
    }

    #[test]
    fn render_lock_forest_no_panic_chain() {
        let edges = vec![
            make_edge(1234, "alice", 5678, "bob", "AccessExclusiveLock", "t", 10.0),
            make_edge(5678, "bob", 9012, "eve", "RowExclusiveLock", "t", 5.0),
        ];
        let forest = build_lock_forest(&edges);
        let out = render_lock_forest(&forest);
        assert!(!out.is_empty());
    }
}
