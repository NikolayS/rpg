//! `\observe [duration]` command — time-boxed read-only monitoring.
//!
//! Collects metric snapshots every 5 seconds, runs every analyzer once on
//! exit, and prints a formatted summary to stderr.  Read-only: no writes
//! are performed at any point.
//!
//! Exit triggers:
//! - Duration elapsed (when a duration is specified).
//! - User presses Ctrl-C.

use std::time::{Duration, Instant};

use tokio_postgres::Client;

use crate::anomaly::{AnomalyDetector, AnomalyKind, MetricSnapshot};
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// Observe SQL — shared with daemon.rs
// ---------------------------------------------------------------------------

pub(crate) const OBSERVE_SQL: &str = "\
    select \
        count(*) filter (where state = 'active') as active, \
        count(*) as total, \
        count(*) filter (where wait_event_type = 'Lock') as blocked, \
        count(*) filter (where state = 'active' \
            and query_start < now() - interval '30 seconds') as long_running \
    from pg_stat_activity \
    where pid != pg_backend_pid() \
      and backend_type = 'client backend'";

pub(crate) const TOP_WAIT_SQL: &str = "\
    select count(*) as cnt \
    from pg_stat_activity \
    where state = 'active' and wait_event is not null \
      and pid != pg_backend_pid() \
    order by 1 desc limit 1";

// ---------------------------------------------------------------------------
// Connection stats accumulator
// ---------------------------------------------------------------------------

/// Accumulated connection statistics over the observation period.
#[derive(Debug, Default)]
struct ConnStats {
    /// Number of snapshots collected.
    count: u64,
    /// Minimum active sessions observed.
    min_active: u32,
    /// Maximum active sessions observed (peak).
    max_active: u32,
    /// Sum of active sessions (used to compute average).
    sum_active: u64,
    /// Maximum blocked sessions observed.
    max_blocked: u32,
}

impl ConnStats {
    fn update(&mut self, snap: &MetricSnapshot) {
        if self.count == 0 {
            self.min_active = snap.active_sessions;
            self.max_active = snap.active_sessions;
        } else {
            self.min_active = self.min_active.min(snap.active_sessions);
            self.max_active = self.max_active.max(snap.active_sessions);
        }
        self.sum_active += u64::from(snap.active_sessions);
        self.max_blocked = self.max_blocked.max(snap.blocked_sessions);
        self.count += 1;
    }

    #[allow(clippy::cast_precision_loss)]
    fn avg_active(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_active as f64 / self.count as f64
        }
    }
}

// ---------------------------------------------------------------------------
// Analyzer result summary
// ---------------------------------------------------------------------------

/// Counts of findings by severity for one analyzer.
#[derive(Debug, Default)]
struct AnalyzerSummary {
    name: &'static str,
    warnings: usize,
    critical: usize,
}

impl AnalyzerSummary {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            warnings: 0,
            critical: 0,
        }
    }

    fn total(&self) -> usize {
        self.warnings + self.critical
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the `\observe [duration]` command.
///
/// Collects metric snapshots every 5 seconds.  Exits when `duration_secs`
/// elapses (if `Some`) or when the user presses Ctrl-C.  On exit, runs each
/// analyzer once and prints a formatted summary to stderr.
#[allow(clippy::too_many_lines)]
pub async fn run_observe(client: &Client, duration_secs: Option<u64>) {
    let poll_interval = Duration::from_secs(5);
    let start = Instant::now();

    // ---- Announce --------------------------------------------------------

    match duration_secs {
        Some(secs) => eprintln!("-- Observing for {secs}s (Ctrl-C to stop early)..."),
        None => eprintln!("-- Observing (Ctrl-C to stop)..."),
    }

    // ---- Metric collection loop ------------------------------------------

    let mut stats = ConnStats::default();
    let mut detector = AnomalyDetector::new();
    let mut all_anomalies: Vec<(AnomalyKind, String)> = Vec::new();

    loop {
        let mut snap = MetricSnapshot::default();

        // Collect primary metrics.
        if let Ok(messages) = client.simple_query(OBSERVE_SQL).await {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    snap.active_sessions = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.total_sessions = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.blocked_sessions = row.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.long_queries = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        // Collect top wait count.
        if let Ok(messages) = client.simple_query(TOP_WAIT_SQL).await {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    snap.top_wait_count = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        stats.update(&snap);

        // Anomaly detection — accumulate for summary.
        let anomalies = detector.check(&snap);
        for a in anomalies {
            all_anomalies.push((a.kind, a.description));
        }

        // Check duration.
        if let Some(secs) = duration_secs {
            if start.elapsed() >= Duration::from_secs(secs) {
                break;
            }
        }

        // Sleep, but exit immediately on Ctrl-C.
        let remaining =
            duration_secs.map(|secs| Duration::from_secs(secs).saturating_sub(start.elapsed()));
        let sleep_time = match remaining {
            Some(r) if r < poll_interval => r,
            _ => poll_interval,
        };
        if sleep_time.is_zero() {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(sleep_time) => {},
            _ = tokio::signal::ctrl_c() => { break; },
        }
    }

    let elapsed_secs = start.elapsed().as_secs();

    eprintln!("-- Observation ended ({} snapshot(s)).", stats.count);

    // ---- Run analyzers ---------------------------------------------------

    eprintln!("-- Running analyzers...");

    let vacuum_r = crate::vacuum::analyze(client).await;
    let bloat_r = crate::bloat::BloatAnalyzer::analyze(client).await;
    let index_r = crate::index_health::analyze(client).await;
    let replication_r = crate::replication::ReplicationAnalyzer::analyze(client).await;
    let conn_mgmt_r =
        crate::connection_management::ConnectionManagementAnalyzer::analyze(client).await;
    let config_r = crate::config_tuning::analyze(client).await;
    let security_r = crate::security::SecurityAnalyzer::analyze(client).await;
    let query_opt_r = crate::query_optimization::analyze(client).await;

    // ---- Build analyzer summaries ----------------------------------------

    let mut summaries: Vec<AnalyzerSummary> = Vec::new();

    // Each findings vec has items with a `.severity` field.
    macro_rules! push_summary {
        ($name:literal, $findings:expr) => {{
            let mut s = AnalyzerSummary::new($name);
            for f in &$findings {
                match f.severity {
                    Severity::Critical => s.critical += 1,
                    Severity::Warning => s.warnings += 1,
                    Severity::Info => {}
                }
            }
            summaries.push(s);
        }};
    }

    push_summary!("vacuum", vacuum_r.findings);
    push_summary!("bloat", bloat_r.findings);
    push_summary!("index_health", index_r.findings);
    push_summary!("replication", replication_r.findings);
    push_summary!("connection_management", conn_mgmt_r.findings);
    push_summary!("config_tuning", config_r.findings);
    push_summary!("security", security_r.findings);
    push_summary!("query_optimization", query_opt_r.findings);

    // ---- Print summary ---------------------------------------------------

    eprintln!();
    eprintln!("=== Observe summary ===");
    eprintln!();

    // Duration.
    let (d_h, d_m, d_s) = (
        elapsed_secs / 3600,
        (elapsed_secs % 3600) / 60,
        elapsed_secs % 60,
    );
    if d_h > 0 {
        eprintln!("  Duration observed : {d_h}h {d_m}m {d_s}s");
    } else if d_m > 0 {
        eprintln!("  Duration observed : {d_m}m {d_s}s");
    } else {
        eprintln!("  Duration observed : {d_s}s");
    }

    // Connection stats.
    eprintln!();
    eprintln!("  Connection stats  ({} snapshot(s)):", stats.count);
    if stats.count > 0 {
        eprintln!(
            "    Active sessions : avg {:.1}, peak {}",
            stats.avg_active(),
            stats.max_active,
        );
        eprintln!("    Min active      : {}", stats.min_active);
        eprintln!("    Max blocked     : {}", stats.max_blocked);
    } else {
        eprintln!("    (no data collected)");
    }

    // Anomalies.
    if !all_anomalies.is_empty() {
        eprintln!();
        eprintln!("  Anomalies detected ({}):", all_anomalies.len());
        for (kind, desc) in &all_anomalies {
            eprintln!("    [{}] {}", kind.label(), desc);
        }
    }

    // Analyzer findings.
    eprintln!();
    eprintln!("  Analyzer findings :");
    let any_findings = summaries.iter().any(|s| s.total() > 0);
    if any_findings {
        for s in &summaries {
            if s.total() > 0 {
                eprintln!(
                    "    {:28} warnings: {:3}  critical: {:3}",
                    s.name, s.warnings, s.critical,
                );
            }
        }
    } else {
        eprintln!("    (none)");
    }

    eprintln!();
    eprintln!("=== End of observe summary ===");
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conn_stats_update_single() {
        let mut s = ConnStats::default();
        let snap = MetricSnapshot {
            active_sessions: 5,
            total_sessions: 10,
            blocked_sessions: 1,
            ..Default::default()
        };
        s.update(&snap);
        assert_eq!(s.count, 1);
        assert_eq!(s.min_active, 5);
        assert_eq!(s.max_active, 5);
        assert_eq!(s.max_blocked, 1);
        assert!((s.avg_active() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn conn_stats_update_multiple() {
        let mut s = ConnStats::default();
        let snaps = [
            MetricSnapshot {
                active_sessions: 2,
                blocked_sessions: 0,
                ..Default::default()
            },
            MetricSnapshot {
                active_sessions: 8,
                blocked_sessions: 3,
                ..Default::default()
            },
            MetricSnapshot {
                active_sessions: 4,
                blocked_sessions: 1,
                ..Default::default()
            },
        ];
        for snap in &snaps {
            s.update(snap);
        }
        assert_eq!(s.count, 3);
        assert_eq!(s.min_active, 2);
        assert_eq!(s.max_active, 8);
        assert_eq!(s.max_blocked, 3);
        // avg = (2+8+4)/3 = 14/3 ≈ 4.666...
        assert!((s.avg_active() - 14.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn conn_stats_empty_avg() {
        let s = ConnStats::default();
        assert_eq!(s.avg_active(), 0.0);
    }

    #[test]
    fn analyzer_summary_total() {
        let mut s = AnalyzerSummary::new("test");
        s.warnings = 2;
        s.critical = 1;
        assert_eq!(s.total(), 3);
    }
}
