//! Connection Management Analyzer — detects connection saturation,
//! idle connection accumulation, long-idle connections, and per-database
//! connection distribution.
//!
//! Operates at Observe level: reads `pg_stat_activity` and `pg_settings`
//! to produce structured findings. No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | Connection saturation (>80%) | Heuristic | `pg_stat_activity` + `max_connections` |
//! | Idle connection accumulation (>10) | Heuristic | `pg_stat_activity` |
//! | Long-idle connections (idle >30 min) | Heuristic | `pg_stat_activity` |
//! | Per-database connection distribution | Factual | `pg_stat_activity` |

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Connection management finding types
// ---------------------------------------------------------------------------

/// Category of connection management finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionManagementFindingKind {
    /// Connection count exceeds 80% of `max_connections`.
    ConnectionSaturation,
    /// More than 10 idle connections are open.
    IdleConnectionAccumulation,
    /// One or more connections have been idle for more than 30 minutes.
    LongIdleConnection,
    /// Factual breakdown of connections per database (informational).
    PerDatabaseDistribution,
}

impl ConnectionManagementFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::PerDatabaseDistribution => EvidenceClass::Factual,
            Self::ConnectionSaturation
            | Self::IdleConnectionAccumulation
            | Self::LongIdleConnection => EvidenceClass::Heuristic,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::ConnectionSaturation => "connection_saturation",
            Self::IdleConnectionAccumulation => "idle_connection_accumulation",
            Self::LongIdleConnection => "long_idle_connection",
            Self::PerDatabaseDistribution => "per_database_distribution",
        }
    }
}

/// A single connection management finding.
#[derive(Debug, Clone)]
pub struct ConnectionManagementFinding {
    /// What kind of finding.
    pub kind: ConnectionManagementFindingKind,
    /// Schema name (empty for instance-level findings).
    pub schema: String,
    /// Table name (empty; reused as database name for distribution findings).
    pub table: String,
    /// Human-readable description.
    pub description: String,
    /// Severity level.
    pub severity: Severity,
    /// Evidence class.
    #[allow(dead_code)]
    pub evidence_class: EvidenceClass,
    /// Suggested remediation (Observe mode: informational only).
    pub suggested_action: Option<String>,
}

/// Complete connection management report.
#[derive(Debug, Clone)]
pub struct ConnectionManagementReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<ConnectionManagementFinding>,
}

impl ConnectionManagementFinding {
    /// Convert this finding into an [`crate::governance::ActionProposal`] for
    /// Supervised mode.
    ///
    /// Returns `None` for informational findings that have no concrete
    /// suggested action (`IdleConnectionAccumulation`, `PerDatabaseDistribution`).
    #[allow(dead_code)]
    pub fn to_proposal(&self) -> Option<crate::governance::ActionProposal> {
        let (proposed_action, risk, expected) = match self.kind {
            ConnectionManagementFindingKind::LongIdleConnection => {
                let action = self.suggested_action.as_ref()?;
                (
                    action.clone(),
                    "Terminating a backend closes the client connection immediately. \
                     Verify the connection is not part of an active transaction \
                     before proceeding.",
                    format!("Terminate long-idle connection — {}", self.description),
                )
            }
            ConnectionManagementFindingKind::ConnectionSaturation => (
                "alter system set idle_session_timeout = '600000'; \
                 select pg_reload_conf()"
                    .to_owned(),
                "Setting idle_session_timeout via ALTER SYSTEM requires a \
                 pg_reload_conf() call to take effect. Existing sessions are \
                 not affected until they next go idle. Test in staging first.",
                "Reduce connection saturation by reclaiming idle sessions \
                 via idle_session_timeout"
                    .to_owned(),
            ),
            ConnectionManagementFindingKind::IdleConnectionAccumulation
            | ConnectionManagementFindingKind::PerDatabaseDistribution => return None,
        };

        Some(crate::governance::ActionProposal {
            feature: crate::governance::FeatureArea::ConnectionManagement,
            severity: self.severity,
            evidence_class: self.evidence_class,
            finding: self.description.clone(),
            proposed_action,
            expected_outcome: expected,
            risk: risk.to_owned(),
            created_at: std::time::SystemTime::now(),
        })
    }
}

impl ConnectionManagementReport {
    /// Convert all actionable findings into proposals for Supervised mode.
    #[allow(dead_code)]
    pub fn to_proposals(&self) -> Vec<crate::governance::ActionProposal> {
        self.findings
            .iter()
            .filter_map(ConnectionManagementFinding::to_proposal)
            .collect()
    }

    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Connection management: no issues found.");
            return;
        }
        eprintln!(
            "Connection management: {} issue{} found.\n",
            self.findings.len(),
            if self.findings.len() == 1 { "" } else { "s" }
        );
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical => "!!",
                Severity::Warning => "! ",
                Severity::Info => "  ",
            };
            if f.schema.is_empty() {
                eprintln!("{icon} [{}] {}", f.kind.label(), f.description);
            } else {
                eprintln!("{icon} [{}] {}.{}", f.kind.label(), f.schema, f.table);
                eprintln!("   {}", f.description);
            }
            if let Some(ref action) = f.suggested_action {
                eprintln!("   suggestion: {action}");
            }
            eprintln!();
        }
    }

    /// Build a text summary for LLM consumption.
    #[allow(dead_code)]
    pub fn to_prompt(&self) -> String {
        if self.findings.is_empty() {
            return "No connection management issues found.".to_owned();
        }
        let mut out = format!(
            "Connection management report: {} finding(s)\n\n",
            self.findings.len()
        );
        for (i, f) in self.findings.iter().enumerate() {
            if f.schema.is_empty() {
                let _ = writeln!(out, "{}. [{}] {}", i + 1, f.kind.label(), f.description);
            } else {
                let _ = writeln!(
                    out,
                    "{}. [{}] {}.{}: {}",
                    i + 1,
                    f.kind.label(),
                    f.schema,
                    f.table,
                    f.description
                );
            }
            if let Some(ref action) = f.suggested_action {
                let _ = writeln!(out, "   Suggested: {action}");
            }
            out.push('\n');
        }
        out
    }
}

// ---------------------------------------------------------------------------
// SQL queries
// ---------------------------------------------------------------------------

/// Fetch current client backend count and `max_connections` setting together.
///
/// Returns a single row: (`current_connections`, `max_connections`).
const SATURATION_SQL: &str = "\
    select \
        count(*) as current_connections, \
        current_setting('max_connections')::int as max_connections \
    from pg_stat_activity \
    where backend_type = 'client backend'";

/// Count connections grouped by state.
///
/// Returns one row per state: (state, count).
const IDLE_COUNT_SQL: &str = "\
    select \
        coalesce(state, 'unknown') as state, \
        count(*) as count \
    from pg_stat_activity \
    where backend_type = 'client backend' \
    group by state \
    order by count desc";

/// Detect connections that have been idle for more than 30 minutes.
///
/// Returns one row per long-idle connection with its idle duration in seconds.
const LONG_IDLE_SQL: &str = "\
    select \
        pid, \
        usename as username, \
        datname as database, \
        extract(epoch from (now() - state_change))::bigint as idle_seconds \
    from pg_stat_activity \
    where \
        backend_type = 'client backend' \
        and state = 'idle' \
        and now() - state_change > interval '30 minutes' \
    order by idle_seconds desc \
    limit 20";

/// Per-database connection distribution.
///
/// Returns one row per database: (database, `connection_count`).
const DB_DISTRIBUTION_SQL: &str = "\
    select \
        coalesce(datname, 'unknown') as database, \
        count(*) as connections \
    from pg_stat_activity \
    where backend_type = 'client backend' \
    group by datname \
    order by connections desc";

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Connection management analyzer — Observe mode, zero writes.
pub struct ConnectionManagementAnalyzer;

impl ConnectionManagementAnalyzer {
    /// Run all connection management checks and return a
    /// [`ConnectionManagementReport`].
    ///
    /// All queries are read-only. Individual query failures are silently
    /// skipped so a single unavailable view does not abort the analysis.
    pub async fn analyze(client: &tokio_postgres::Client) -> ConnectionManagementReport {
        let mut findings = Vec::new();

        collect_saturation_findings(client, &mut findings).await;
        collect_idle_accumulation_findings(client, &mut findings).await;
        collect_long_idle_findings(client, &mut findings).await;
        collect_db_distribution_findings(client, &mut findings).await;

        // Sort: Critical first, then Warning, then Info.
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));

        ConnectionManagementReport { findings }
    }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

async fn collect_saturation_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ConnectionManagementFinding>,
) {
    let Ok(messages) = client.simple_query(SATURATION_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let current: i64 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let max: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

            if let Some(finding) = saturation_finding(current, max) {
                findings.push(finding);
            }
        }
    }
}

/// Evaluate saturation and return a finding if the threshold is exceeded.
///
/// Extracted so unit tests can call this directly with mock values.
fn saturation_finding(current: i64, max: i64) -> Option<ConnectionManagementFinding> {
    if max == 0 {
        return None;
    }

    // Safe precision loss: connection counts are small integers well within
    // f64 mantissa precision.
    #[allow(clippy::cast_precision_loss)]
    let pct = (current as f64 / max as f64) * 100.0;

    if pct < 80.0 {
        return None;
    }

    let severity = if pct >= 95.0 {
        Severity::Critical
    } else {
        Severity::Warning
    };

    Some(ConnectionManagementFinding {
        kind: ConnectionManagementFindingKind::ConnectionSaturation,
        schema: String::new(),
        table: String::new(),
        description: format!(
            "{current} of {max} connections in use ({pct:.1}% of max_connections)",
        ),
        severity,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(
            "Consider deploying a connection pooler such as PgBouncer \
             or reducing idle connections."
                .to_owned(),
        ),
    })
}

async fn collect_idle_accumulation_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ConnectionManagementFinding>,
) {
    let Ok(messages) = client.simple_query(IDLE_COUNT_SQL).await else {
        return;
    };
    let mut idle_count: i64 = 0;
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let state = row.get(0).unwrap_or("");
            if state == "idle" {
                idle_count = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            }
        }
    }

    if let Some(finding) = idle_accumulation_finding(idle_count) {
        findings.push(finding);
    }
}

/// Evaluate idle accumulation and return a finding if the threshold is exceeded.
///
/// Extracted so unit tests can call this directly with mock values.
fn idle_accumulation_finding(idle_count: i64) -> Option<ConnectionManagementFinding> {
    if idle_count <= 10 {
        return None;
    }

    let severity = if idle_count >= 50 {
        Severity::Critical
    } else {
        Severity::Warning
    };

    Some(ConnectionManagementFinding {
        kind: ConnectionManagementFindingKind::IdleConnectionAccumulation,
        schema: String::new(),
        table: String::new(),
        description: format!(
            "{idle_count} idle client connections are open — \
             excessive idle connections waste memory and file descriptors"
        ),
        severity,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(
            "Use a connection pooler (PgBouncer) or set \
             idle_in_transaction_session_timeout to reclaim idle slots."
                .to_owned(),
        ),
    })
}

async fn collect_long_idle_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ConnectionManagementFinding>,
) {
    let Ok(messages) = client.simple_query(LONG_IDLE_SQL).await else {
        return;
    };

    let mut long_idle_rows: Vec<(i32, String, String, i64)> = Vec::new();

    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pid: i32 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
            let username = row.get(1).unwrap_or("unknown").to_owned();
            let database = row.get(2).unwrap_or("unknown").to_owned();
            let idle_secs: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            long_idle_rows.push((pid, username, database, idle_secs));
        }
    }

    for (pid, username, database, idle_secs) in long_idle_rows {
        findings.push(long_idle_finding(pid, &username, &database, idle_secs));
    }
}

/// Build a long-idle finding for one connection.
///
/// Extracted so unit tests can call this directly with mock values.
fn long_idle_finding(
    pid: i32,
    username: &str,
    database: &str,
    idle_secs: i64,
) -> ConnectionManagementFinding {
    let duration = format_idle_duration(idle_secs);
    ConnectionManagementFinding {
        kind: ConnectionManagementFindingKind::LongIdleConnection,
        schema: String::new(),
        table: String::new(),
        description: format!("pid {pid} ({username}@{database}) has been idle for {duration}"),
        severity: Severity::Warning,
        evidence_class: EvidenceClass::Heuristic,
        suggested_action: Some(format!(
            "Investigate or close pid {pid}: \
             SELECT pg_terminate_backend({pid}); \
             -- verify it is safe first"
        )),
    }
}

async fn collect_db_distribution_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ConnectionManagementFinding>,
) {
    let Ok(messages) = client.simple_query(DB_DISTRIBUTION_SQL).await else {
        return;
    };

    let mut rows: Vec<(String, i64)> = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let database = row.get(0).unwrap_or("unknown").to_owned();
            let count: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            rows.push((database, count));
        }
    }

    if rows.is_empty() {
        return;
    }

    let total: i64 = rows.iter().map(|(_, c)| c).sum();
    let summary = rows
        .iter()
        .map(|(db, c)| format!("{db}: {c}"))
        .collect::<Vec<_>>()
        .join(", ");

    findings.push(ConnectionManagementFinding {
        kind: ConnectionManagementFindingKind::PerDatabaseDistribution,
        schema: String::new(),
        table: String::new(),
        description: format!("{total} total client connection(s) across databases — {summary}"),
        severity: Severity::Info,
        evidence_class: EvidenceClass::Factual,
        suggested_action: None,
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format an idle duration (given in seconds) into a human-readable string.
///
/// Examples: `"31m 0s"`, `"2h 5m"`, `"1d 3h"`.
fn format_idle_duration(secs: i64) -> String {
    if secs < 0 {
        return "0s".to_owned();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let rem_secs = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m {rem_secs}s")
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- FindingKind labels ---

    #[test]
    fn finding_kind_labels() {
        assert_eq!(
            ConnectionManagementFindingKind::ConnectionSaturation.label(),
            "connection_saturation"
        );
        assert_eq!(
            ConnectionManagementFindingKind::IdleConnectionAccumulation.label(),
            "idle_connection_accumulation"
        );
        assert_eq!(
            ConnectionManagementFindingKind::LongIdleConnection.label(),
            "long_idle_connection"
        );
        assert_eq!(
            ConnectionManagementFindingKind::PerDatabaseDistribution.label(),
            "per_database_distribution"
        );
    }

    // --- FindingKind evidence classes ---

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            ConnectionManagementFindingKind::PerDatabaseDistribution.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            ConnectionManagementFindingKind::ConnectionSaturation.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            ConnectionManagementFindingKind::IdleConnectionAccumulation.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            ConnectionManagementFindingKind::LongIdleConnection.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    // --- Connection saturation logic ---

    #[test]
    fn saturation_below_threshold_returns_none() {
        // 70% — below the 80% warning threshold.
        assert!(saturation_finding(70, 100).is_none());
    }

    #[test]
    fn saturation_at_80_pct_gives_warning() {
        let f = saturation_finding(80, 100).unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(
            f.kind,
            ConnectionManagementFindingKind::ConnectionSaturation
        );
        assert!(f.description.contains("80 of 100"));
    }

    #[test]
    fn saturation_at_95_pct_gives_critical() {
        let f = saturation_finding(95, 100).unwrap();
        assert_eq!(f.severity, Severity::Critical);
    }

    #[test]
    fn saturation_zero_max_returns_none() {
        assert!(saturation_finding(10, 0).is_none());
    }

    // --- Idle accumulation logic ---

    #[test]
    fn idle_accumulation_at_10_returns_none() {
        // Threshold is > 10; exactly 10 should not trigger.
        assert!(idle_accumulation_finding(10).is_none());
    }

    #[test]
    fn idle_accumulation_at_11_gives_warning() {
        let f = idle_accumulation_finding(11).unwrap();
        assert_eq!(f.severity, Severity::Warning);
        assert_eq!(
            f.kind,
            ConnectionManagementFindingKind::IdleConnectionAccumulation
        );
        assert!(f.description.contains("11 idle"));
    }

    #[test]
    fn idle_accumulation_at_50_gives_critical() {
        let f = idle_accumulation_finding(50).unwrap();
        assert_eq!(f.severity, Severity::Critical);
    }

    // --- Long-idle finding ---

    #[test]
    fn long_idle_finding_formats_correctly() {
        let f = long_idle_finding(1234, "alice", "mydb", 3661);
        assert_eq!(f.kind, ConnectionManagementFindingKind::LongIdleConnection);
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.description.contains("pid 1234"));
        assert!(f.description.contains("alice@mydb"));
        // 3661s = 1h 1m
        assert!(f.description.contains("1h 1m"));
        assert!(f.suggested_action.as_deref().unwrap().contains("1234"));
    }

    // --- format_idle_duration ---

    #[test]
    fn format_idle_duration_seconds() {
        assert_eq!(format_idle_duration(95), "1m 35s");
    }

    #[test]
    fn format_idle_duration_hours() {
        // 7200 s = 2h 0m
        assert_eq!(format_idle_duration(7200), "2h 0m");
    }

    #[test]
    fn format_idle_duration_days() {
        // 90000 s = 1d 1h
        assert_eq!(format_idle_duration(90000), "1d 1h");
    }

    #[test]
    fn format_idle_duration_negative_is_zero() {
        assert_eq!(format_idle_duration(-1), "0s");
    }

    // --- Report display / to_prompt ---

    #[test]
    fn empty_report_to_prompt() {
        let report = ConnectionManagementReport {
            findings: Vec::new(),
        };
        assert!(report
            .to_prompt()
            .contains("No connection management issues found."));
    }

    #[test]
    fn report_to_prompt_with_findings() {
        let report = ConnectionManagementReport {
            findings: vec![ConnectionManagementFinding {
                kind: ConnectionManagementFindingKind::ConnectionSaturation,
                schema: String::new(),
                table: String::new(),
                description: "90 of 100 connections in use (90.0% of max_connections)".to_owned(),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("Use PgBouncer".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[connection_saturation]"));
        assert!(prompt.contains("90 of 100"));
        assert!(prompt.contains("PgBouncer"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = ConnectionManagementReport {
            findings: vec![
                ConnectionManagementFinding {
                    kind: ConnectionManagementFindingKind::IdleConnectionAccumulation,
                    schema: String::new(),
                    table: String::new(),
                    description: "many idle".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                ConnectionManagementFinding {
                    kind: ConnectionManagementFindingKind::ConnectionSaturation,
                    schema: String::new(),
                    table: String::new(),
                    description: "saturated".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                ConnectionManagementFinding {
                    kind: ConnectionManagementFindingKind::PerDatabaseDistribution,
                    schema: String::new(),
                    table: String::new(),
                    description: "distribution".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
        assert_eq!(report.findings[2].severity, Severity::Info);
    }

    // --- SQL constants ---

    #[test]
    fn saturation_sql_checks_client_backends() {
        assert!(SATURATION_SQL.contains("client backend"));
        assert!(SATURATION_SQL.contains("max_connections"));
        assert!(SATURATION_SQL.contains("pg_stat_activity"));
    }

    #[test]
    fn idle_count_sql_groups_by_state() {
        assert!(IDLE_COUNT_SQL.contains("state"));
        assert!(IDLE_COUNT_SQL.contains("pg_stat_activity"));
        assert!(IDLE_COUNT_SQL.contains("group by"));
    }

    #[test]
    fn long_idle_sql_filters_30_minutes() {
        assert!(LONG_IDLE_SQL.contains("30 minutes"));
        assert!(LONG_IDLE_SQL.contains("idle"));
        assert!(LONG_IDLE_SQL.contains("pg_stat_activity"));
    }

    #[test]
    fn db_distribution_sql_groups_by_datname() {
        assert!(DB_DISTRIBUTION_SQL.contains("datname"));
        assert!(DB_DISTRIBUTION_SQL.contains("group by"));
        assert!(DB_DISTRIBUTION_SQL.contains("pg_stat_activity"));
    }

    // --- to_proposal / to_proposals ---

    #[test]
    fn long_idle_finding_produces_proposal() {
        let f = long_idle_finding(5678, "bob", "appdb", 4200);
        let proposal = f
            .to_proposal()
            .expect("LongIdleConnection should produce a proposal");
        assert_eq!(
            proposal.feature,
            crate::governance::FeatureArea::ConnectionManagement
        );
        assert_eq!(proposal.severity, Severity::Warning);
        assert_eq!(proposal.evidence_class, EvidenceClass::Heuristic);
        assert!(proposal.proposed_action.contains("5678"));
        assert!(proposal.finding.contains("bob@appdb"));
        assert!(proposal.expected_outcome.contains("long-idle"));
        assert!(proposal.risk.contains("Terminating"));
    }

    #[test]
    fn saturation_finding_produces_proposal() {
        let f = saturation_finding(90, 100).expect("90% saturation should produce a finding");
        let proposal = f
            .to_proposal()
            .expect("ConnectionSaturation should produce a proposal");
        assert_eq!(
            proposal.feature,
            crate::governance::FeatureArea::ConnectionManagement
        );
        assert!(proposal.proposed_action.contains("idle_session_timeout"));
        assert!(proposal.expected_outcome.contains("saturation"));
        assert!(proposal.risk.contains("ALTER SYSTEM"));
    }

    #[test]
    fn idle_accumulation_finding_produces_no_proposal() {
        let f =
            idle_accumulation_finding(20).expect("20 idle connections should produce a finding");
        assert!(
            f.to_proposal().is_none(),
            "IdleConnectionAccumulation should not produce a proposal"
        );
    }

    #[test]
    fn per_database_distribution_produces_no_proposal() {
        let f = ConnectionManagementFinding {
            kind: ConnectionManagementFindingKind::PerDatabaseDistribution,
            schema: String::new(),
            table: String::new(),
            description: "5 total client connection(s) across databases — appdb: 5".to_owned(),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Factual,
            suggested_action: None,
        };
        assert!(
            f.to_proposal().is_none(),
            "PerDatabaseDistribution should not produce a proposal"
        );
    }

    #[test]
    fn report_to_proposals_filters_non_actionable() {
        let report = ConnectionManagementReport {
            findings: vec![
                long_idle_finding(111, "alice", "db1", 2000),
                idle_accumulation_finding(15)
                    .expect("15 idle connections should produce a finding"),
                saturation_finding(85, 100).expect("85% saturation should produce a finding"),
                ConnectionManagementFinding {
                    kind: ConnectionManagementFindingKind::PerDatabaseDistribution,
                    schema: String::new(),
                    table: String::new(),
                    description: "distribution info".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                },
            ],
        };
        let proposals = report.to_proposals();
        // LongIdleConnection + ConnectionSaturation = 2 proposals;
        // IdleConnectionAccumulation and PerDatabaseDistribution are filtered.
        assert_eq!(proposals.len(), 2);
        let features: Vec<_> = proposals.iter().map(|p| p.feature).collect();
        assert!(features
            .iter()
            .all(|f| *f == crate::governance::FeatureArea::ConnectionManagement));
    }

    #[test]
    fn report_to_proposals_empty_when_no_findings() {
        let report = ConnectionManagementReport {
            findings: Vec::new(),
        };
        assert!(report.to_proposals().is_empty());
    }
}
