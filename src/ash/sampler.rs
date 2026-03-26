//! Data layer for `/ash`.
//!
//! Polls `pg_stat_activity` for live data and optionally queries
//! `ash.samples` when `pg_ash` is installed.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Query the number of logical CPUs on the **Postgres server** via SQL.
///
/// Tries `pg_cpu_count()` (requires `pg_proctab` extension) first, then
/// falls back to parsing `/proc/cpuinfo` via `pg_read_file` (superuser only),
/// and finally returns `None` if neither is available.  A `None` result hides
/// the CPU reference line in the TUI rather than showing a misleading value.
pub async fn query_cpu_count(client: &tokio_postgres::Client) -> Option<u32> {
    // 1. pg_proctab extension
    if let Ok(row) = client.query_one("select pg_cpu_count()::int", &[]).await {
        let n: i32 = row.get(0);
        if n > 0 {
            #[allow(clippy::cast_sign_loss)]
            return Some(n as u32);
        }
    }

    // 2. Parse /proc/cpuinfo on the server (superuser + Linux only)
    if let Ok(row) = client
        .query_one(
            "select count(*) from regexp_matches(
                pg_read_file('/proc/cpuinfo'), E'processor\\t:', 'g'
            )",
            &[],
        )
        .await
    {
        let n: i64 = row.get(0);
        if n > 0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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
    /// Retention window in seconds from `ash.config`.  Reserved for history
    /// mode (Layer 2); unused in the current live-only implementation.
    #[allow(dead_code)]
    pub retention_seconds: Option<i64>,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// Detect whether the `pg_ash` extension is installed and read its config.
pub async fn detect_pg_ash(client: &Client) -> PgAshInfo {
    let installed: bool = match client
        .query_one(
            "select exists(select 1 from pg_extension where extname = 'pg_ash')",
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

/// Take a live snapshot by querying `pg_stat_activity`.
///
/// A single SQL query aggregates all active backends (excluding the current
/// connection).  The result set is folded in Rust into three views
/// (`by_type`, `by_event`, `by_query`) without additional round-trips.
pub async fn live_snapshot(client: &Client) -> anyhow::Result<AshSnapshot> {
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

    let rows = client.query(sql, &[]).await?;

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

    Ok(snap)
}

/// Return historical snapshots from `pg_ash` if installed.
///
/// # Stub — history mode (`pg_ash` Layer 2) not yet implemented
///
/// TODO: history mode (`pg_ash` Layer 2) — not yet implemented.
/// `pg_ash` v1.2 encodes `ash.samples.data` as an opaque `int[]` whose
/// layout is not yet publicly documented.  Until the encoding is specified
/// and history mode is fully wired into the event loop, this function always
/// returns an empty vec.  The caller in `mod.rs` falls back to the live ring
/// buffer transparently, so the TUI never goes blank.
/// Track upstream: <https://github.com/NikolayS/rpg/issues/753>
pub async fn history_snapshots(
    client: &Client,
    from: SystemTime,
    to: SystemTime,
) -> anyhow::Result<Vec<AshSnapshot>> {
    let info = detect_pg_ash(client).await;
    if !info.installed {
        return Ok(vec![]);
    }

    // Validate range (suppress unused-variable warnings until encoding is done).
    let _ = (from, to);

    // TODO: history mode (pg_ash Layer 2) — decode ash.samples.data int[] encoding
    // once the format is documented and history mode is wired into the event loop.
    Ok(vec![])
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
}
