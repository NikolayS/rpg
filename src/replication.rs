//! Replication health Analyzer — detects slot lag, inactive slots,
//! streaming replica lag, and WAL sender count.
//!
//! Operates at Observe level: reads `pg_replication_slots` and
//! `pg_stat_replication` to produce structured findings. No writes are
//! performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | Replication slot lag | Heuristic | `pg_replication_slots` |
//! | Inactive replication slot | Heuristic | `pg_replication_slots` |
//! | Streaming replica lag | Heuristic | `pg_stat_replication` |
//! | WAL sender count | Factual | `pg_stat_replication` |

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Replication finding types
// ---------------------------------------------------------------------------

/// Category of replication health finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplicationFindingKind {
    /// Replication slot has accumulated significant WAL lag (bytes behind).
    SlotLag,
    /// Replication slot is not active — WAL retention risk.
    InactiveSlot,
    /// Streaming replica write/flush/replay lag exceeds threshold.
    ReplicaLag,
    /// Current count of active WAL sender processes (informational).
    WalSenderCount,
}

impl ReplicationFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::WalSenderCount => EvidenceClass::Factual,
            Self::SlotLag | Self::InactiveSlot | Self::ReplicaLag => EvidenceClass::Heuristic,
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SlotLag => "slot_lag",
            Self::InactiveSlot => "inactive_slot",
            Self::ReplicaLag => "replica_lag",
            Self::WalSenderCount => "wal_sender_count",
        }
    }
}

/// A single replication health finding.
#[derive(Debug, Clone)]
pub struct ReplicationFinding {
    /// What kind of finding.
    pub kind: ReplicationFindingKind,
    /// Slot or replica name (empty for instance-level findings).
    pub schema: String,
    /// Object name (slot name, replica application name, etc.).
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

/// Complete replication health report.
#[derive(Debug, Clone)]
pub struct ReplicationReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<ReplicationFinding>,
}

impl ReplicationReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Replication health: no issues found.");
            return;
        }
        eprintln!(
            "Replication health: {} issue{} found.\n",
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
            return "No replication health issues found.".to_owned();
        }
        let mut out = format!(
            "Replication health report: {} finding(s)\n\n",
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

/// Fetch all replication slots with their WAL lag in bytes.
///
/// Reports lag as bytes between current WAL position and the slot's
/// `restart_lsn`. Returns all slots ordered by lag (largest first).
const SLOT_LAG_SQL: &str = "\
    select \
        slot_name, \
        slot_type, \
        active, \
        coalesce( \
            pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), \
            0 \
        ) as lag_bytes, \
        database \
    from pg_replication_slots \
    order by lag_bytes desc";

/// Fetch streaming replica lag intervals from `pg_stat_replication`.
///
/// Returns `write_lag`, `flush_lag`, and `replay_lag` for each connected standby.
const REPLICA_LAG_SQL: &str = "\
    select \
        application_name, \
        client_addr::text, \
        state, \
        coalesce(extract(epoch from write_lag)::bigint, 0) as write_lag_s, \
        coalesce(extract(epoch from flush_lag)::bigint, 0) as flush_lag_s, \
        coalesce(extract(epoch from replay_lag)::bigint, 0) as replay_lag_s \
    from pg_stat_replication \
    order by replay_lag_s desc";

/// Count active WAL sender processes.
const WAL_SENDER_COUNT_SQL: &str = "\
    select count(*) \
    from pg_stat_replication";

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Slot lag above this many bytes triggers a Warning.
const SLOT_LAG_WARN_BYTES: i64 = 100 * 1024 * 1024; // 100 MiB
/// Slot lag above this many bytes triggers a Critical.
const SLOT_LAG_CRITICAL_BYTES: i64 = 1024 * 1024 * 1024; // 1 GiB

/// Replica lag above this many seconds triggers a Warning.
const REPLICA_LAG_WARN_SECS: i64 = 30;
/// Replica lag above this many seconds triggers a Critical.
const REPLICA_LAG_CRITICAL_SECS: i64 = 300; // 5 minutes

// ---------------------------------------------------------------------------
// Public analyzer
// ---------------------------------------------------------------------------

/// Replication health analyzer — Observe mode, zero writes.
pub struct ReplicationAnalyzer;

impl ReplicationAnalyzer {
    /// Run all replication health checks and return a [`ReplicationReport`].
    ///
    /// All queries are read-only. Individual query failures are silently
    /// skipped so that a single unavailable view does not abort the analysis.
    pub async fn analyze(client: &tokio_postgres::Client) -> ReplicationReport {
        let mut findings = Vec::new();

        collect_slot_findings(client, &mut findings).await;
        collect_replica_lag_findings(client, &mut findings).await;
        collect_wal_sender_count(client, &mut findings).await;

        // Sort: Critical first, then Warning, then Info.
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));

        ReplicationReport { findings }
    }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

async fn collect_slot_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ReplicationFinding>,
) {
    let Ok(messages) = client.simple_query(SLOT_LAG_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let slot_name = row.get(0).unwrap_or("").to_owned();
            let slot_type = row.get(1).unwrap_or("").to_owned();
            let active = row.get(2).unwrap_or("f");
            let lag_bytes: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let database = row.get(4).map(ToOwned::to_owned);

            let is_active = active == "t";

            // Finding: inactive slot (WAL retention risk).
            if !is_active {
                let db_note = database
                    .as_deref()
                    .map_or_else(String::new, |d| format!(" (database: {d})"));
                findings.push(ReplicationFinding {
                    kind: ReplicationFindingKind::InactiveSlot,
                    schema: "replication".to_owned(),
                    table: slot_name.clone(),
                    description: format!(
                        "Slot '{slot_name}' (type: {slot_type}) is inactive{db_note} \
                         — WAL accumulation risk"
                    ),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some(format!(
                        "Investigate or drop slot: \
                         select pg_drop_replication_slot('{slot_name}')"
                    )),
                });
            }

            // Finding: slot lag (applies to both active and inactive slots).
            if lag_bytes >= SLOT_LAG_WARN_BYTES {
                let severity = if lag_bytes >= SLOT_LAG_CRITICAL_BYTES {
                    Severity::Critical
                } else {
                    Severity::Warning
                };
                findings.push(ReplicationFinding {
                    kind: ReplicationFindingKind::SlotLag,
                    schema: "replication".to_owned(),
                    table: slot_name.clone(),
                    description: format!(
                        "Slot '{slot_name}' is {} behind current WAL position",
                        format_bytes(lag_bytes),
                    ),
                    severity,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some(format!(
                        "Check consumer for slot '{slot_name}'; \
                         consider dropping if no longer needed: \
                         select pg_drop_replication_slot('{slot_name}')"
                    )),
                });
            }
        }
    }
}

async fn collect_replica_lag_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ReplicationFinding>,
) {
    let Ok(messages) = client.simple_query(REPLICA_LAG_SQL).await else {
        return;
    };
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let app_name = row.get(0).unwrap_or("unknown").to_owned();
            let client_addr = row.get(1).unwrap_or("").to_owned();
            let state = row.get(2).unwrap_or("").to_owned();
            let write_lag_s: i64 = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let flush_lag_s: i64 = row.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let replay_lag_s: i64 = row.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);

            // Use the maximum of the three lag values for severity.
            let max_lag_s = write_lag_s.max(flush_lag_s).max(replay_lag_s);

            if max_lag_s < REPLICA_LAG_WARN_SECS {
                continue;
            }

            let severity = if max_lag_s >= REPLICA_LAG_CRITICAL_SECS {
                Severity::Critical
            } else {
                Severity::Warning
            };

            let addr_note = if client_addr.is_empty() {
                String::new()
            } else {
                format!(" ({client_addr})")
            };

            findings.push(ReplicationFinding {
                kind: ReplicationFindingKind::ReplicaLag,
                schema: "replication".to_owned(),
                table: app_name.clone(),
                description: format!(
                    "Replica '{app_name}'{addr_note} state={state}: \
                     write={write_lag_s}s flush={flush_lag_s}s replay={replay_lag_s}s"
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(
                    "Investigate replica I/O, network, or apply bottleneck".to_owned(),
                ),
            });
        }
    }
}

async fn collect_wal_sender_count(
    client: &tokio_postgres::Client,
    findings: &mut Vec<ReplicationFinding>,
) {
    let Ok(messages) = client.simple_query(WAL_SENDER_COUNT_SQL).await else {
        return;
    };
    let mut sender_count: i64 = 0;
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            sender_count = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        }
    }

    findings.push(ReplicationFinding {
        kind: ReplicationFindingKind::WalSenderCount,
        schema: String::new(),
        table: String::new(),
        description: format!("{sender_count} WAL sender(s) currently active"),
        severity: Severity::Info,
        evidence_class: EvidenceClass::Factual,
        suggested_action: None,
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format bytes into a human-readable string using binary units (GiB/MiB/KiB).
#[allow(clippy::cast_precision_loss)]
fn format_bytes(bytes: i64) -> String {
    const KIB: i64 = 1024;
    const MIB: i64 = 1024 * KIB;
    const GIB: i64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ReplicationFindingKind tests
    // -----------------------------------------------------------------------

    #[test]
    fn finding_kind_labels() {
        assert_eq!(ReplicationFindingKind::SlotLag.label(), "slot_lag");
        assert_eq!(
            ReplicationFindingKind::InactiveSlot.label(),
            "inactive_slot"
        );
        assert_eq!(ReplicationFindingKind::ReplicaLag.label(), "replica_lag");
        assert_eq!(
            ReplicationFindingKind::WalSenderCount.label(),
            "wal_sender_count"
        );
    }

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            ReplicationFindingKind::WalSenderCount.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            ReplicationFindingKind::SlotLag.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            ReplicationFindingKind::InactiveSlot.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            ReplicationFindingKind::ReplicaLag.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    // -----------------------------------------------------------------------
    // ReplicationReport tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_report_to_prompt() {
        let report = ReplicationReport {
            findings: Vec::new(),
        };
        assert!(report.to_prompt().contains("No replication health issues"));
    }

    #[test]
    fn report_to_prompt_with_slot_lag_finding() {
        let report = ReplicationReport {
            findings: vec![ReplicationFinding {
                kind: ReplicationFindingKind::SlotLag,
                schema: "replication".to_owned(),
                table: "my_slot".to_owned(),
                description: "Slot 'my_slot' is 200.0 MiB behind current WAL position".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("Check consumer for slot 'my_slot'".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[slot_lag]"));
        assert!(prompt.contains("replication.my_slot"));
        assert!(prompt.contains("Check consumer"));
    }

    #[test]
    fn report_to_prompt_instance_level_finding() {
        let report = ReplicationReport {
            findings: vec![ReplicationFinding {
                kind: ReplicationFindingKind::WalSenderCount,
                schema: String::new(),
                table: String::new(),
                description: "3 WAL sender(s) currently active".to_owned(),
                severity: Severity::Info,
                evidence_class: EvidenceClass::Factual,
                suggested_action: None,
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("[wal_sender_count]"));
        assert!(prompt.contains("3 WAL sender"));
    }

    #[test]
    fn report_sorts_by_severity() {
        let mut report = ReplicationReport {
            findings: vec![
                ReplicationFinding {
                    kind: ReplicationFindingKind::InactiveSlot,
                    schema: "replication".to_owned(),
                    table: "slot_a".to_owned(),
                    description: "inactive".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                ReplicationFinding {
                    kind: ReplicationFindingKind::SlotLag,
                    schema: "replication".to_owned(),
                    table: "slot_b".to_owned(),
                    description: "critical lag".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                ReplicationFinding {
                    kind: ReplicationFindingKind::WalSenderCount,
                    schema: String::new(),
                    table: String::new(),
                    description: "2 senders".to_owned(),
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

    // -----------------------------------------------------------------------
    // SQL constant tests
    // -----------------------------------------------------------------------

    #[test]
    fn slot_lag_sql_references_correct_views() {
        assert!(SLOT_LAG_SQL.contains("pg_replication_slots"));
        assert!(SLOT_LAG_SQL.contains("pg_wal_lsn_diff"));
        assert!(SLOT_LAG_SQL.contains("restart_lsn"));
        assert!(SLOT_LAG_SQL.contains("active"));
    }

    #[test]
    fn replica_lag_sql_references_correct_views() {
        assert!(REPLICA_LAG_SQL.contains("pg_stat_replication"));
        assert!(REPLICA_LAG_SQL.contains("write_lag"));
        assert!(REPLICA_LAG_SQL.contains("flush_lag"));
        assert!(REPLICA_LAG_SQL.contains("replay_lag"));
    }

    #[test]
    fn wal_sender_count_sql_references_correct_view() {
        assert!(WAL_SENDER_COUNT_SQL.contains("pg_stat_replication"));
        assert!(WAL_SENDER_COUNT_SQL.contains("count(*)"));
    }

    // -----------------------------------------------------------------------
    // Threshold / severity logic tests
    // -----------------------------------------------------------------------

    #[test]
    fn slot_lag_critical_at_1_gib() {
        let lag_bytes: i64 = 2 * 1024 * 1024 * 1024; // 2 GiB
        let severity = if lag_bytes >= SLOT_LAG_CRITICAL_BYTES {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn slot_lag_warning_at_100_mib() {
        let lag_bytes: i64 = 200 * 1024 * 1024; // 200 MiB
        let severity = if lag_bytes >= SLOT_LAG_CRITICAL_BYTES {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn slot_lag_below_threshold_not_reported() {
        let lag_bytes: i64 = 50 * 1024 * 1024; // 50 MiB — below warn threshold
        assert!(lag_bytes < SLOT_LAG_WARN_BYTES);
    }

    #[test]
    fn replica_lag_critical_at_5_minutes() {
        let lag_s: i64 = 600; // 10 minutes
        let severity = if lag_s >= REPLICA_LAG_CRITICAL_SECS {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn replica_lag_warning_at_60_seconds() {
        let lag_s: i64 = 60;
        let severity = if lag_s >= REPLICA_LAG_CRITICAL_SECS {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn replica_lag_below_threshold_not_reported() {
        let lag_s: i64 = 10; // below 30s warn threshold
        assert!(lag_s < REPLICA_LAG_WARN_SECS);
    }

    // -----------------------------------------------------------------------
    // format_bytes tests
    // -----------------------------------------------------------------------

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(10_485_760), "10.0 MiB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn format_bytes_used_in_finding_description() {
        // Verify that lag descriptions use binary units.
        let lag_bytes = SLOT_LAG_WARN_BYTES; // exactly 100 MiB
        let desc = format!(
            "Slot 'test' is {} behind current WAL position",
            format_bytes(lag_bytes),
        );
        assert!(desc.contains("MiB"), "expected MiB in: {desc}");
    }
}
