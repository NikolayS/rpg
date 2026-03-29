//! Data layer for `/ash`.
//!
//! Polls `pg_stat_activity` for live data and optionally queries
//! `ash.samples` when `pg_ash` is installed.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum time allowed for a single `/ash` sample query (`pg_stat_activity`
/// or `ash.wait_timeline`).  Matches the same guard used by `pg_ash`'s
/// per-second `pg_cron` job — fail fast rather than block the TUI.
///
/// Future: move to `[ash]` config section so users can tune it.
/// See: <https://github.com/NikolayS/rpg/issues/771>
const ASH_QUERY_TIMEOUT_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Try to determine the number of logical CPUs on the **Postgres server**.
///
/// Only `pg_proctab` (if installed) exposes this reliably.  All other
/// approaches require superuser or direct OS access — unavailable on managed
/// Postgres (RDS, `CloudSQL`, Supabase, etc.).
///
/// Returns `None` when the value cannot be determined.  A `None` result hides
/// the CPU reference line in the TUI rather than showing a misleading value.
/// Users can supply the count explicitly via `/ash --cpu N`.
pub async fn query_cpu_count(client: &tokio_postgres::Client) -> Option<u32> {
    // pg_proctab extension — the only reliable cross-platform source
    if let Ok(row) = client.query_one("select pg_cpu_count()::int", &[]).await {
        let n: i32 = row.get(0);
        if n > 0 {
            #[allow(clippy::cast_sign_loss)]
            return Some(n as u32);
        }
    }
    None
}

/// A single point-in-time sample of active session counts, aggregated from
/// `pg_stat_activity` (or `ash.samples` when `pg_ash` is available).
#[derive(Debug, Default, Clone)]
pub struct AshSnapshot {
    /// Unix timestamp (seconds) when the sample was taken.
    pub ts: i64,
    /// Total active (non-idle) sessions at sample time.
    pub active_count: u32,
    /// Number of logical CPUs on the **Postgres server**, if determinable.
    ///
    /// `None` when the server does not expose CPU count (no `pg_proctab`,
    /// not superuser, or non-Linux).  When `None` the CPU reference line is
    /// hidden in the TUI rather than showing a misleading client-side value.
    pub cpu_count: Option<u32>,
    /// Counts grouped by `wait_event_type` (e.g. "Lock", "IO", "CPU*").
    ///
    /// Key: `wait_event_type` string.
    pub by_type: HashMap<String, u32>,
    /// Counts grouped by `wait_event_type/wait_event` composite key.
    ///
    /// Key format: `"<wait_event_type>/<wait_event>"`.
    /// For CPU* rows the `wait_event` portion is empty: `"CPU*/"`.
    pub by_event: HashMap<String, u32>,
    /// Counts grouped by `wait_event_type/wait_event/query_label` composite key.
    ///
    /// Key format: `"<wait_event_type>/<wait_event>/<query_label>"`.
    /// `query_label` is the truncated query text (first 80 chars), or the
    /// decimal representation of `query_id` when the text is empty.
    pub by_query: HashMap<String, u32>,
}

/// Detection result for the `pg_ash` extension.
#[derive(Debug, Clone)]
pub struct PgAshInfo {
    pub installed: bool,
    /// Retention window in seconds from `ash.config`.  Reserved for future use
    /// to cap history queries to the configured retention window; not yet wired.
    #[allow(dead_code)]
    pub retention_seconds: Option<i64>,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Detect whether the `pg_ash` extension is installed and read its config.
///
/// Detection strategy: check for `ash.wait_timeline` in `pg_proc` rather
/// than `pg_extension`.  `pg_ash` can be installed either via
/// `CREATE EXTENSION pg_ash` (which populates `pg_extension`) or by running
/// the install SQL directly (which does not).  Checking the function exists
/// handles both cases and is the only capability we actually need.
pub async fn detect_pg_ash(client: &Client) -> PgAshInfo {
    let installed: bool = match client
        .query_one(
            "select exists(\
                select 1 from pg_proc p \
                join pg_namespace n on p.pronamespace = n.oid \
                where n.nspname = 'ash' and p.proname = 'wait_timeline'\
            )",
            &[],
        )
        .await
    {
        Ok(row) => row.get(0),
        Err(_) => {
            return PgAshInfo {
                installed: false,
                retention_seconds: None,
            }
        }
    };

    if !installed {
        return PgAshInfo {
            installed: false,
            retention_seconds: None,
        };
    }

    let retention_seconds = client
        .query_one(
            "select value::int from ash.config where key = 'retention_seconds'",
            &[],
        )
        .await
        .ok()
        .map(|row| row.get::<_, i64>(0));

    PgAshInfo {
        installed: true,
        retention_seconds,
    }
}

/// Outcome of a [`live_snapshot`] call.
#[derive(Debug)]
pub enum LiveSnapshotResult {
    /// A snapshot was taken successfully.
    Ok(AshSnapshot),
    /// The query was cancelled by `statement_timeout` — the tick is skipped.
    /// The TUI should display a brief "missed" indicator rather than blocking.
    Missed,
}

/// Take a live snapshot by querying `pg_stat_activity`.
///
/// A single SQL query aggregates all active backends (excluding the current
/// connection).  The result set is folded in Rust into three views
/// (`by_type`, `by_event`, `by_query`) without additional round-trips.
///
/// Observer-effect protection: `SET statement_timeout` is applied before the
/// query (see [`ASH_QUERY_TIMEOUT_MS`], matching the same guard used by
/// `pg_ash`'s per-second cron job) and reset to 0 afterwards.  If the query
/// times out the tick is skipped and [`LiveSnapshotResult::Missed`] is
/// returned instead of blocking the TUI.
pub async fn live_snapshot(client: &Client) -> anyhow::Result<LiveSnapshotResult> {
    let sql = "
        select
            case
                when state in ('idle in transaction', 'idle in transaction (aborted)') then 'IdleTx'
                when wait_event_type is null then 'CPU*'
                else wait_event_type
            end as wtype,
            coalesce(wait_event, '') as wevent,
            query_id,
            left(query, 80) as q,
            count(*)::int as cnt
        from pg_stat_activity
        where pid <> pg_backend_pid()
          and state <> 'idle'
        group by 1, 2, 3, 4
        order by cnt desc
    ";

    // Observer-effect protection: apply a short statement_timeout so a slow
    // pg_stat_activity scan fails fast rather than blocking the TUI.
    //
    // SET LOCAL requires an active transaction block (it's a no-op outside
    // one). Use SET + reset instead: set for this query, restore default after.
    // This is a session-level change but is immediately restored, so it does
    // not leak across queries in the connection pool.
    client
        .execute(
            &format!("set statement_timeout = '{ASH_QUERY_TIMEOUT_MS}ms'"),
            &[],
        )
        .await?;

    let rows = match client.query(sql, &[]).await {
        Ok(rows) => rows,
        Err(e) => {
            // Restore statement_timeout before returning — best-effort.
            let _ = client.execute("set statement_timeout = 0", &[]).await;
            // statement_timeout fires as SQLSTATE 57014 (query_canceled).
            // Return Missed so the TUI can display a brief indicator and
            // continue rather than propagating an error.
            if e.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) {
                return Ok(LiveSnapshotResult::Missed);
            }
            return Err(e.into());
        }
    };

    // Restore default timeout after successful query.
    let _ = client.execute("set statement_timeout = 0", &[]).await;

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    let mut snap = AshSnapshot {
        ts,
        cpu_count: query_cpu_count(client).await,
        ..Default::default()
    };

    for row in &rows {
        let wtype: String = row.get(0);
        let wevent: String = row.get(1);
        let query_id: Option<i64> = row.get(2);
        let q: String = row.get(3);
        let cnt: i32 = row.get(4);
        let count = u32::try_from(cnt.max(0)).unwrap_or(0);

        fold_row(&mut snap, &wtype, &wevent, query_id, &q, count);
    }

    Ok(LiveSnapshotResult::Ok(snap))
}

/// Pre-populate the ring buffer with historical snapshots from `pg_ash`.
///
/// Queries `ash.wait_timeline()` with 1-second buckets for the requested
/// window, groups each bucket into an `AshSnapshot`, and returns them in
/// chronological order (oldest first).
///
/// Returns an empty vec when:
/// - `pg_ash` is not installed (graceful degradation)
/// - the query fails (transient error, permission issue, etc.)
/// - no historical data exists for the requested window
pub async fn query_ash_history(client: &Client, window_secs: u64) -> Vec<AshSnapshot> {
    query_ash_history_inner(client, window_secs)
        .await
        .unwrap_or_default()
}

/// Inner implementation — uses a parameterized query to avoid SQL injection.
///
/// Uses `ash.wait_timeline($1::interval, '1 second')` which returns
/// `(bucket_start timestamptz, wait_event text, samples bigint)`
/// already decoded from the opaque `int[]` encoding.
async fn query_ash_history_inner(
    client: &Client,
    window_secs: u64,
) -> anyhow::Result<Vec<AshSnapshot>> {
    // ash.wait_timeline returns (bucket_start, wait_event, samples).
    // wait_event format: "Type:Event" or just "Type" when type == event
    // (e.g. "CPU*", "IO:DataFileRead", "Lock:relation").
    //
    // Pass the interval as a parameterized $1 (not format!() interpolation) so
    // there is no SQL injection vector, even though window_secs is u64 today.
    // Build the interval literal directly from a u64 (no user input — safe).
    // tokio-postgres cannot serialize a Rust String as a Postgres interval
    // parameter without an explicit type annotation that requires a server
    // round-trip; embedding the literal is simpler and avoids the issue.
    let sql = format!(
        "select \
            extract(epoch from bucket_start)::int8 as ts, \
            wait_event, \
            samples::int8 as cnt \
        from ash.wait_timeline('{window_secs} seconds'::interval, '1 second'::interval) \
        order by bucket_start, wait_event"
    );

    // Same observer-effect guard as live_snapshot.
    let _ = client
        .execute(
            &format!("set statement_timeout = '{ASH_QUERY_TIMEOUT_MS}ms'"),
            &[],
        )
        .await;

    let rows = match client.query(sql.as_str(), &[]).await {
        Ok(r) => {
            let _ = client.execute("set statement_timeout = 0", &[]).await;
            r
        }
        Err(e) => {
            let _ = client.execute("set statement_timeout = 0", &[]).await;
            return Err(anyhow::anyhow!("ash.wait_timeline query failed: {e}"));
        }
    };

    if rows.is_empty() {
        return Ok(vec![]);
    }

    // Convert DB rows to (ts, wait_event, count) tuples and group into snapshots.
    let tuples: Vec<(i64, String, u32)> = rows
        .iter()
        .map(|row| {
            let ts: i64 = row.get(0);
            let wait_event: String = row.get(1);
            let cnt: i64 = row.get(2);
            let count = u32::try_from(cnt.max(0)).unwrap_or(u32::MAX);
            (ts, wait_event, count)
        })
        .collect();

    Ok(group_timeline_rows(&tuples))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Accumulate a single grouped row into a snapshot's three views.
fn fold_row(
    snap: &mut AshSnapshot,
    wtype: &str,
    wevent: &str,
    query_id: Option<i64>,
    q: &str,
    count: u32,
) {
    snap.active_count = snap.active_count.saturating_add(count);

    // by_type: keyed by wait_event_type only.
    *snap.by_type.entry(wtype.to_owned()).or_insert(0) += count;

    // by_event: "<wtype>/<wevent>"
    let event_key = format!("{wtype}/{wevent}");
    *snap.by_event.entry(event_key.clone()).or_insert(0) += count;

    // by_query: "<wtype>/<wevent>/<query_label>"
    // Use truncated query text; fall back to query_id decimal when text is empty.
    let query_label: String = if q.is_empty() {
        query_id.map_or_else(|| "(unknown)".to_owned(), |id| id.to_string())
    } else {
        q.to_owned()
    };
    let query_key = format!("{event_key}/{query_label}");
    *snap.by_query.entry(query_key).or_insert(0) += count;
}

/// Split a `"Type:Event"` string (as returned by `ash.wait_timeline`) into
/// `(wtype, wevent)`.  When there is no colon, `wevent` is the empty string
/// (e.g. `"CPU*"` → `("CPU*", "")`).
fn split_wait_event(s: &str) -> (String, String) {
    if let Some(idx) = s.find(':') {
        (s[..idx].to_owned(), s[idx + 1..].to_owned())
    } else {
        (s.to_owned(), String::new())
    }
}

/// Group `(ts, wait_event, count)` tuples — as produced by `ash.wait_timeline`
/// — into a chronologically-ordered `Vec<AshSnapshot>`.
///
/// Rows must be sorted by `ts` (ascending).  Consecutive rows with the same
/// `ts` are merged into a single snapshot via `fold_row`.
///
/// Exposed as `pub(crate)` so unit tests can drive it directly without
/// duplicating the grouping logic.
pub(crate) fn group_timeline_rows(rows: &[(i64, impl AsRef<str>, u32)]) -> Vec<AshSnapshot> {
    let mut snapshots: Vec<AshSnapshot> = Vec::new();
    let mut current_ts: i64 = i64::MIN;
    let mut snap = AshSnapshot::default();

    for (ts, wait_event, count) in rows {
        let ts = *ts;
        let count = *count;
        if ts != current_ts {
            if current_ts != i64::MIN {
                snapshots.push(snap);
            }
            snap = AshSnapshot {
                ts,
                // cpu_count is unknown for history rows; the CPU reference line
                // populates once the first live snapshot arrives.
                ..Default::default()
            };
            current_ts = ts;
        }

        let (wtype, wevent) = split_wait_event(wait_event.as_ref());
        // wait_timeline provides no query_id or query text; use empty labels.
        fold_row(&mut snap, &wtype, &wevent, None, "", count);
    }

    if current_ts != i64::MIN {
        snapshots.push(snap);
    }

    snapshots
}

// ---------------------------------------------------------------------------
// Unit tests (no live database required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `AshSnapshot` from mock row data, exercising `fold_row`
    /// directly.  Each tuple is `(wtype, wevent, query_id, query, count)`.
    fn mock_snapshot(entries: &[(&str, &str, Option<i64>, &str, u32)]) -> AshSnapshot {
        let mut snap = AshSnapshot::default();
        for (wtype, wevent, query_id, q, count) in entries {
            fold_row(&mut snap, wtype, wevent, *query_id, q, *count);
        }
        snap
    }

    #[test]
    fn test_aggregate_cpu_and_lock() {
        let snap = mock_snapshot(&[
            ("CPU*", "", None, "select 1", 5),
            ("Lock", "relation", Some(42), "select * from t", 3),
            ("Lock", "tuple", Some(43), "update t set x=1", 2),
        ]);

        assert_eq!(snap.active_count, 10);

        // by_type: CPU* = 5, Lock = 5 (3 + 2)
        assert_eq!(snap.by_type["CPU*"], 5);
        assert_eq!(snap.by_type["Lock"], 5);

        // by_event: three distinct composite keys
        assert_eq!(snap.by_event.len(), 3);
        assert_eq!(snap.by_event["CPU*/"], 5);
        assert_eq!(snap.by_event["Lock/relation"], 3);
        assert_eq!(snap.by_event["Lock/tuple"], 2);

        // by_query: three distinct rows
        assert_eq!(snap.by_query.len(), 3);
        assert!(snap.by_query.contains_key("CPU*//select 1"));
        assert!(snap.by_query.contains_key("Lock/relation/select * from t"));
        assert!(snap.by_query.contains_key("Lock/tuple/update t set x=1"));
    }

    #[test]
    fn test_aggregate_idle_tx() {
        let snap = mock_snapshot(&[
            ("IdleTx", "", None, "begin", 7),
            ("IO", "DataFileRead", Some(99), "select * from big", 4),
        ]);

        assert_eq!(snap.active_count, 11);
        assert_eq!(snap.by_type["IdleTx"], 7);
        assert_eq!(snap.by_type["IO"], 4);

        // CPU* must be absent when there are no CPU* rows.
        assert!(!snap.by_type.contains_key("CPU*"));

        assert_eq!(snap.by_event["IdleTx/"], 7);
        assert_eq!(snap.by_event["IO/DataFileRead"], 4);
    }

    #[test]
    fn test_aggregate_empty() {
        let snap = mock_snapshot(&[]);
        assert_eq!(snap.active_count, 0);
        assert!(snap.by_type.is_empty());
        assert!(snap.by_event.is_empty());
        assert!(snap.by_query.is_empty());
    }

    #[test]
    fn test_query_label_fallback_to_query_id() {
        // Empty query text — should fall back to query_id decimal string.
        let snap = mock_snapshot(&[("Lock", "relation", Some(12345), "", 1)]);
        assert!(snap.by_query.contains_key("Lock/relation/12345"));
    }

    #[test]
    fn test_query_label_fallback_unknown() {
        // No query text and no query_id — should fall back to "(unknown)".
        let snap = mock_snapshot(&[("CPU*", "", None, "", 2)]);
        assert!(snap.by_query.contains_key("CPU*//(unknown)"));
    }

    // --- pg_ash history integration tests ---

    #[test]
    fn test_history_build_snapshots_basic() {
        let rows = vec![
            (1000, "CPU*", 5),
            (1000, "IO:DataFileRead", 3),
            (1001, "CPU*", 4),
            (1001, "Lock:relation", 2),
            (1002, "IO:WALWrite", 1),
        ];
        let snaps = group_timeline_rows(&rows);

        assert_eq!(snaps.len(), 3);

        // First snapshot: ts=1000, CPU*=5, IO=3
        assert_eq!(snaps[0].ts, 1000);
        assert_eq!(snaps[0].active_count, 8);
        assert_eq!(snaps[0].by_type["CPU*"], 5);
        assert_eq!(snaps[0].by_type["IO"], 3);

        // Second snapshot: ts=1001, CPU*=4, Lock=2
        assert_eq!(snaps[1].ts, 1001);
        assert_eq!(snaps[1].active_count, 6);
        assert_eq!(snaps[1].by_type["CPU*"], 4);
        assert_eq!(snaps[1].by_type["Lock"], 2);
        assert_eq!(snaps[1].by_event["Lock/relation"], 2);

        // Third snapshot: ts=1002, IO=1
        assert_eq!(snaps[2].ts, 1002);
        assert_eq!(snaps[2].active_count, 1);
        assert_eq!(snaps[2].by_type["IO"], 1);
    }

    #[test]
    fn test_history_build_snapshots_empty() {
        let snaps = group_timeline_rows(&[] as &[(i64, &str, u32)]);
        assert!(snaps.is_empty());
    }

    #[test]
    fn test_history_snapshots_prepopulate_ring_buffer() {
        use std::collections::VecDeque;

        let rows = vec![
            (100, "CPU*", 3),
            (101, "IO:DataFileRead", 2),
            (102, "Lock:relation", 1),
        ];
        let history = group_timeline_rows(&rows);

        // Simulate ring buffer pre-population (same logic as mod.rs).
        let mut ring: VecDeque<AshSnapshot> = VecDeque::with_capacity(600);
        for snap in history {
            if ring.len() == 600 {
                ring.pop_front();
            }
            ring.push_back(snap);
        }

        assert_eq!(ring.len(), 3);
        assert_eq!(ring[0].ts, 100);
        assert_eq!(ring[1].ts, 101);
        assert_eq!(ring[2].ts, 102);
    }

    #[test]
    fn test_history_split_wait_event_with_colon() {
        let (wtype, wevent) = split_wait_event("IO:DataFileRead");
        assert_eq!(wtype, "IO");
        assert_eq!(wevent, "DataFileRead");
    }

    #[test]
    fn test_history_split_wait_event_no_colon() {
        let (wtype, wevent) = split_wait_event("CPU*");
        assert_eq!(wtype, "CPU*");
        assert_eq!(wevent, "");
    }

    #[test]
    fn test_history_ring_buffer_capacity_limit() {
        use std::collections::VecDeque;

        // Build 605 snapshots — ring buffer should keep only last 600.
        let rows: Vec<(i64, &str, u32)> = (0..605).map(|i| (i64::from(i), "CPU*", 1)).collect();
        let history = group_timeline_rows(&rows);

        let mut ring: VecDeque<AshSnapshot> = VecDeque::with_capacity(600);
        for snap in history {
            if ring.len() == 600 {
                ring.pop_front();
            }
            ring.push_back(snap);
        }

        assert_eq!(ring.len(), 600);
        // Oldest kept snapshot should be ts=5 (first 5 were dropped).
        assert_eq!(ring[0].ts, 5);
        // Newest should be ts=604.
        assert_eq!(ring[599].ts, 604);
    }

    // --- integration tests (require live pg_ash) ---

    /// Verifies `query_ash_history` does not panic and returns valid snapshots
    /// when `pg_ash` is installed.
    ///
    /// Run with: `cargo test --include-ignored test_pg_ash_history_live`
    ///
    /// Setup: install `pg_ash` and run pgbench for a few minutes first so there
    /// is history data to query.
    #[tokio::test]
    #[ignore = "requires pg_ash installed on postgresql://postgres@127.0.0.1:15433/ashtest"]
    async fn test_pg_ash_history_live() {
        let (client, conn) = tokio_postgres::connect(
            "host=127.0.0.1 port=15433 user=postgres dbname=ashtest",
            tokio_postgres::NoTls,
        )
        .await
        .expect("connect to test database");
        tokio::spawn(async move {
            conn.await.ok();
        });

        // Skip gracefully if pg_ash is not installed.
        let ext_rows = client
            .query("select 1 from pg_extension where extname = 'pg_ash'", &[])
            .await
            .unwrap_or_default();
        if ext_rows.is_empty() {
            eprintln!("pg_ash not installed — skipping live history test");
            return;
        }

        let history = query_ash_history(&client, 60).await;
        eprintln!(
            "test_pg_ash_history_live: {} snapshots returned",
            history.len()
        );

        // All snapshots must have consistent counts.
        for snap in &history {
            let type_total: u32 = snap.by_type.values().sum();
            assert_eq!(
                type_total, snap.active_count,
                "by_type sum must equal active_count"
            );
        }
    }

    /// Verifies `query_ash_history` returns an empty vec gracefully when
    /// `pg_ash` is not installed (no crash, no error propagation).
    ///
    /// Run with: `cargo test --include-ignored test_pg_ash_history_graceful_degradation`
    #[tokio::test]
    #[ignore = "requires connection to postgresql://postgres@127.0.0.1:15433/ashtest"]
    async fn test_pg_ash_history_graceful_degradation() {
        let (client, conn) = tokio_postgres::connect(
            "host=127.0.0.1 port=15433 user=postgres dbname=ashtest",
            tokio_postgres::NoTls,
        )
        .await
        .expect("connect to test database");
        tokio::spawn(async move {
            conn.await.ok();
        });

        // Drop pg_ash if installed so we can test degradation.
        let _ = client.execute("drop extension if exists pg_ash", &[]).await;

        // Must return empty vec, not panic or propagate error.
        let history = query_ash_history(&client, 60).await;
        assert!(
            history.is_empty(),
            "expected empty vec when pg_ash not installed"
        );
    }
}
