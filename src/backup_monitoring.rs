//! Backup monitoring Analyzer — detects WAL archiving failures, archive lag,
//! WAL file accumulation, and disabled archiving.
//!
//! Operates at Observe level: reads `pg_stat_archiver`, `pg_settings`, and
//! `pg_ls_waldir()` to produce structured findings. No writes are performed.
//!
//! # Sub-findings
//!
//! | Sub-finding | Evidence Class | Source |
//! |---|---|---|
//! | WAL archiving failure | Heuristic | `pg_stat_archiver.failed_count > 0` |
//! | Archive lag | Heuristic | time since `last_archived_time` > 5 min |
//! | WAL file accumulation | Heuristic | WAL file count > 100 |
//! | Archiving disabled | Advisory | `archive_mode = off` |
//! | Current WAL position | Factual | `pg_current_wal_lsn()` |

#![allow(dead_code)]

use crate::governance::{EvidenceClass, Severity};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Backup monitoring finding types
// ---------------------------------------------------------------------------

/// Category of backup monitoring finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupMonitoringFindingKind {
    /// WAL archiving has recorded failures (`failed_count > 0`).
    WalArchiveFailure,
    /// Time since last successful WAL archive exceeds the warning threshold.
    ArchiveLag,
    /// Number of WAL files on disk exceeds the accumulation threshold.
    WalFileAccumulation,
    /// WAL archiving is disabled (`archive_mode = off`) — informational.
    ArchivingDisabled,
    /// Current WAL LSN position — factual, always included.
    CurrentWalPosition,
}

impl BackupMonitoringFindingKind {
    /// Evidence class for this finding kind.
    #[allow(dead_code)]
    pub fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::CurrentWalPosition => EvidenceClass::Factual,
            Self::ArchivingDisabled => EvidenceClass::Advisory,
            Self::WalArchiveFailure | Self::ArchiveLag | Self::WalFileAccumulation => {
                EvidenceClass::Heuristic
            }
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::WalArchiveFailure => "wal_archive_failure",
            Self::ArchiveLag => "archive_lag",
            Self::WalFileAccumulation => "wal_file_accumulation",
            Self::ArchivingDisabled => "archiving_disabled",
            Self::CurrentWalPosition => "current_wal_position",
        }
    }
}

/// A single backup monitoring finding.
#[derive(Debug, Clone)]
pub struct BackupMonitoringFinding {
    /// What kind of finding.
    pub kind: BackupMonitoringFindingKind,
    /// Schema name (empty for instance-level findings).
    pub schema: String,
    /// Table name (empty for instance-level findings).
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

/// Complete backup monitoring report.
#[derive(Debug, Clone)]
pub struct BackupMonitoringReport {
    /// All findings, sorted by severity (critical first).
    pub findings: Vec<BackupMonitoringFinding>,
}

impl BackupMonitoringFinding {
    /// Convert this finding into an [`crate::governance::ActionProposal`].
    ///
    /// Backup monitoring has no safe auto-actions (all remediation involves
    /// external systems or configuration file changes), so this always returns
    /// `None`. The method exists for API consistency with other analyzers.
    #[allow(dead_code, clippy::unused_self)]
    pub fn to_proposal(&self) -> Option<crate::governance::ActionProposal> {
        None
    }
}

impl BackupMonitoringReport {
    /// Collect all [`crate::governance::ActionProposal`]s from this report.
    ///
    /// Backup monitoring findings have no safe auto-actions, so this always
    /// returns an empty `Vec`. The method exists for API consistency.
    #[allow(dead_code)]
    pub fn to_proposals(&self) -> Vec<crate::governance::ActionProposal> {
        self.findings
            .iter()
            .filter_map(BackupMonitoringFinding::to_proposal)
            .collect()
    }
}

impl BackupMonitoringReport {
    /// Display the report to the terminal.
    pub fn display(&self) {
        if self.findings.is_empty() {
            eprintln!("Backup monitoring: no issues found.");
            return;
        }
        eprintln!(
            "Backup monitoring: {} issue{} found.\n",
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
            return "No backup monitoring issues found.".to_owned();
        }
        let mut out = format!(
            "Backup monitoring report: {} finding(s)\n\n",
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

/// Read WAL archiver status from `pg_stat_archiver`.
///
/// Returns `archived_count`, `failed_count`, `last_archived_wal`,
/// `last_archived_time`, `last_failed_wal`, `last_failed_time`,
/// and seconds since last successful archive.
const ARCHIVER_STATUS_SQL: &str = "\
    select \
        archived_count, \
        failed_count, \
        coalesce(last_archived_wal, '') as last_archived_wal, \
        coalesce(last_archived_time::text, '') as last_archived_time, \
        coalesce(last_failed_wal, '') as last_failed_wal, \
        coalesce(last_failed_time::text, '') as last_failed_time, \
        coalesce( \
            extract(epoch from (now() - last_archived_time))::bigint::text, \
            '-1' \
        ) as seconds_since_archive \
    from pg_stat_archiver";

/// Check whether WAL archiving is enabled.
///
/// Returns the value of `archive_mode` from `pg_settings`.
const ARCHIVE_MODE_SQL: &str = "\
    select setting \
    from pg_settings \
    where name = 'archive_mode'";

/// Count WAL files currently on disk using `pg_ls_waldir()`.
///
/// Requires superuser or `pg_monitor` role (PG 10+).
/// Returns the count of `.` segments (excludes `.history`, `.partial`, etc.).
const WAL_FILE_COUNT_SQL: &str = "\
    select count(*) as wal_file_count \
    from pg_ls_waldir() \
    where name ~ '^[0-9A-F]{24}$'";

/// Fetch the current WAL LSN position.
const CURRENT_WAL_LSN_SQL: &str = "\
    select pg_current_wal_lsn()::text as current_lsn";

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Archive lag threshold in seconds (5 minutes).
const ARCHIVE_LAG_WARN_SECS: i64 = 300;
/// Archive lag threshold in seconds for critical (30 minutes).
const ARCHIVE_LAG_CRITICAL_SECS: i64 = 1_800;
/// WAL file count warning threshold.
const WAL_FILE_COUNT_WARN: i64 = 100;
/// WAL file count critical threshold.
const WAL_FILE_COUNT_CRITICAL: i64 = 500;

// ---------------------------------------------------------------------------
// Analyzer
// ---------------------------------------------------------------------------

/// Backup monitoring analyzer — Observe mode, zero writes.
pub struct BackupMonitoringAnalyzer;

impl BackupMonitoringAnalyzer {
    /// Run all backup monitoring checks and return a [`BackupMonitoringReport`].
    ///
    /// All queries are read-only. Individual query failures are silently
    /// skipped so a single unavailable view does not abort the whole analysis.
    pub async fn analyze(client: &tokio_postgres::Client) -> BackupMonitoringReport {
        let mut findings = Vec::new();

        // 1. Check archive_mode setting first (informs other checks).
        let archive_mode = collect_archive_mode(client).await;

        if archive_mode.as_deref() == Some("off") || archive_mode.is_none() {
            findings.push(BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::ArchivingDisabled,
                schema: String::new(),
                table: String::new(),
                description: "WAL archiving is disabled (archive_mode = off); \
                              PITR is not available"
                    .to_owned(),
                severity: Severity::Info,
                evidence_class: EvidenceClass::Advisory,
                suggested_action: Some(
                    "Set archive_mode = on and configure archive_command \
                     to enable point-in-time recovery"
                        .to_owned(),
                ),
            });
        } else {
            // 2. WAL archiver status (only meaningful when archiving is on).
            collect_archiver_findings(client, &mut findings).await;
        }

        // 3. WAL file accumulation (independent of archive_mode).
        collect_wal_file_count(client, &mut findings).await;

        // 4. Current WAL position (always factual, always included).
        collect_current_wal_position(client, &mut findings).await;

        // Sort: Critical first, then Warning, then Info.
        findings.sort_by(|a, b| b.severity.cmp(&a.severity));

        BackupMonitoringReport { findings }
    }
}

// ---------------------------------------------------------------------------
// Collection helpers
// ---------------------------------------------------------------------------

/// Read `archive_mode` from `pg_settings`.
///
/// Returns `None` if the query fails or the setting is not found.
async fn collect_archive_mode(client: &tokio_postgres::Client) -> Option<String> {
    let messages = client.simple_query(ARCHIVE_MODE_SQL).await.ok()?;
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            return Some(row.get(0).unwrap_or("off").to_owned());
        }
    }
    None
}

/// Collect findings from `pg_stat_archiver`.
async fn collect_archiver_findings(
    client: &tokio_postgres::Client,
    findings: &mut Vec<BackupMonitoringFinding>,
) {
    let Ok(messages) = client.simple_query(ARCHIVER_STATUS_SQL).await else {
        return;
    };
    for msg in messages {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg else {
            continue;
        };

        let failed_count: i64 = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let last_failed_wal = row.get(4).unwrap_or("").to_owned();
        let last_failed_time = row.get(5).unwrap_or("").to_owned();
        let seconds_since_archive: i64 = row.get(6).and_then(|s| s.parse().ok()).unwrap_or(-1);

        // Finding: archiving failures.
        if failed_count > 0 {
            findings.push(BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::WalArchiveFailure,
                schema: String::new(),
                table: String::new(),
                description: format!(
                    "{failed_count} WAL archive failure(s) recorded; \
                     last failed: {last_failed_wal} at {last_failed_time}"
                ),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(
                    "Investigate archive_command errors in PostgreSQL logs; \
                     check archive destination accessibility"
                        .to_owned(),
                ),
            });
        }

        // Finding: archive lag.
        if seconds_since_archive >= ARCHIVE_LAG_WARN_SECS {
            let minutes = seconds_since_archive / 60;
            let severity = if seconds_since_archive >= ARCHIVE_LAG_CRITICAL_SECS {
                Severity::Critical
            } else {
                Severity::Warning
            };
            findings.push(BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::ArchiveLag,
                schema: String::new(),
                table: String::new(),
                description: format!(
                    "No WAL segment archived in {minutes} minute(s); \
                     last archive lag exceeds threshold ({} min)",
                    ARCHIVE_LAG_WARN_SECS / 60,
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(
                    "Check archive_command, destination storage, and \
                     pg_stat_archiver for errors"
                        .to_owned(),
                ),
            });
        }
    }
}

/// Collect WAL file count findings.
async fn collect_wal_file_count(
    client: &tokio_postgres::Client,
    findings: &mut Vec<BackupMonitoringFinding>,
) {
    let Ok(messages) = client.simple_query(WAL_FILE_COUNT_SQL).await else {
        return;
    };
    for msg in messages {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg else {
            continue;
        };
        let count: i64 = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);

        if count >= WAL_FILE_COUNT_WARN {
            let severity = if count >= WAL_FILE_COUNT_CRITICAL {
                Severity::Critical
            } else {
                Severity::Warning
            };
            findings.push(BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::WalFileAccumulation,
                schema: String::new(),
                table: String::new(),
                description: format!(
                    "{count} WAL files on disk (threshold: {WAL_FILE_COUNT_WARN}); \
                     archiving may be falling behind"
                ),
                severity,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some(
                    "Verify archiving is working and archive_cleanup_command \
                     is configured; check for replication slot retention"
                        .to_owned(),
                ),
            });
        }
    }
}

/// Collect the current WAL LSN position as a factual finding.
async fn collect_current_wal_position(
    client: &tokio_postgres::Client,
    findings: &mut Vec<BackupMonitoringFinding>,
) {
    let Ok(messages) = client.simple_query(CURRENT_WAL_LSN_SQL).await else {
        return;
    };
    for msg in messages {
        let tokio_postgres::SimpleQueryMessage::Row(row) = msg else {
            continue;
        };
        let lsn = row.get(0).unwrap_or("unknown").to_owned();
        findings.push(BackupMonitoringFinding {
            kind: BackupMonitoringFindingKind::CurrentWalPosition,
            schema: String::new(),
            table: String::new(),
            description: format!("Current WAL position: {lsn}"),
            severity: Severity::Info,
            evidence_class: EvidenceClass::Factual,
            suggested_action: None,
        });
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Finding kind label tests ----------------------------------------

    #[test]
    fn finding_kind_labels() {
        assert_eq!(
            BackupMonitoringFindingKind::WalArchiveFailure.label(),
            "wal_archive_failure"
        );
        assert_eq!(
            BackupMonitoringFindingKind::ArchiveLag.label(),
            "archive_lag"
        );
        assert_eq!(
            BackupMonitoringFindingKind::WalFileAccumulation.label(),
            "wal_file_accumulation"
        );
        assert_eq!(
            BackupMonitoringFindingKind::ArchivingDisabled.label(),
            "archiving_disabled"
        );
        assert_eq!(
            BackupMonitoringFindingKind::CurrentWalPosition.label(),
            "current_wal_position"
        );
    }

    // ---- Evidence class tests --------------------------------------------

    #[test]
    fn finding_kind_evidence_classes() {
        assert_eq!(
            BackupMonitoringFindingKind::CurrentWalPosition.evidence_class(),
            EvidenceClass::Factual
        );
        assert_eq!(
            BackupMonitoringFindingKind::ArchivingDisabled.evidence_class(),
            EvidenceClass::Advisory
        );
        assert_eq!(
            BackupMonitoringFindingKind::WalArchiveFailure.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            BackupMonitoringFindingKind::ArchiveLag.evidence_class(),
            EvidenceClass::Heuristic
        );
        assert_eq!(
            BackupMonitoringFindingKind::WalFileAccumulation.evidence_class(),
            EvidenceClass::Heuristic
        );
    }

    // ---- Empty report tests ----------------------------------------------

    #[test]
    fn empty_report_to_prompt() {
        let report = BackupMonitoringReport {
            findings: Vec::new(),
        };
        assert!(report
            .to_prompt()
            .contains("No backup monitoring issues found."));
    }

    #[test]
    fn empty_report_display_does_not_panic() {
        let report = BackupMonitoringReport {
            findings: Vec::new(),
        };
        // display() writes to stderr; just verify it doesn't panic.
        report.display();
    }

    // ---- to_prompt with findings -----------------------------------------

    #[test]
    fn report_to_prompt_with_wal_failure_finding() {
        let report = BackupMonitoringReport {
            findings: vec![BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::WalArchiveFailure,
                schema: String::new(),
                table: String::new(),
                description: "3 WAL archive failure(s) recorded".to_owned(),
                severity: Severity::Critical,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: Some("Check archive_command".to_owned()),
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("1 finding"));
        assert!(prompt.contains("[wal_archive_failure]"));
        assert!(prompt.contains("3 WAL archive failure"));
        assert!(prompt.contains("Check archive_command"));
    }

    #[test]
    fn report_to_prompt_with_archive_lag_finding() {
        let report = BackupMonitoringReport {
            findings: vec![BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::ArchiveLag,
                schema: String::new(),
                table: String::new(),
                description: "No WAL segment archived in 10 minute(s)".to_owned(),
                severity: Severity::Warning,
                evidence_class: EvidenceClass::Heuristic,
                suggested_action: None,
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("[archive_lag]"));
        assert!(prompt.contains("10 minute"));
    }

    #[test]
    fn report_to_prompt_with_current_wal_position() {
        let report = BackupMonitoringReport {
            findings: vec![BackupMonitoringFinding {
                kind: BackupMonitoringFindingKind::CurrentWalPosition,
                schema: String::new(),
                table: String::new(),
                description: "Current WAL position: 0/3000000".to_owned(),
                severity: Severity::Info,
                evidence_class: EvidenceClass::Factual,
                suggested_action: None,
            }],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("[current_wal_position]"));
        assert!(prompt.contains("0/3000000"));
    }

    // ---- Severity logic tests --------------------------------------------

    #[test]
    fn archive_lag_warning_threshold() {
        // seconds_since_archive >= ARCHIVE_LAG_WARN_SECS but < critical.
        let seconds: i64 = 600; // 10 minutes
        let severity = if seconds >= ARCHIVE_LAG_CRITICAL_SECS {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn archive_lag_critical_threshold() {
        let seconds: i64 = 1_800; // 30 minutes
        let severity = if seconds >= ARCHIVE_LAG_CRITICAL_SECS {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    #[test]
    fn wal_file_count_warning_threshold() {
        let count: i64 = 150;
        let severity = if count >= WAL_FILE_COUNT_CRITICAL {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Warning);
    }

    #[test]
    fn wal_file_count_critical_threshold() {
        let count: i64 = 500;
        let severity = if count >= WAL_FILE_COUNT_CRITICAL {
            Severity::Critical
        } else {
            Severity::Warning
        };
        assert_eq!(severity, Severity::Critical);
    }

    // ---- Sort order test -------------------------------------------------

    #[test]
    fn report_sorts_by_severity() {
        let mut report = BackupMonitoringReport {
            findings: vec![
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::ArchivingDisabled,
                    schema: String::new(),
                    table: String::new(),
                    description: "archiving off".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Advisory,
                    suggested_action: None,
                },
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::WalArchiveFailure,
                    schema: String::new(),
                    table: String::new(),
                    description: "5 failures".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::ArchiveLag,
                    schema: String::new(),
                    table: String::new(),
                    description: "10 min lag".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
            ],
        };
        report.findings.sort_by(|a, b| b.severity.cmp(&a.severity));
        assert_eq!(report.findings[0].severity, Severity::Critical);
        assert_eq!(report.findings[1].severity, Severity::Warning);
        assert_eq!(report.findings[2].severity, Severity::Info);
    }

    // ---- to_proposal / to_proposals tests --------------------------------

    #[test]
    fn finding_to_proposal_always_returns_none() {
        let finding = BackupMonitoringFinding {
            kind: BackupMonitoringFindingKind::WalArchiveFailure,
            schema: String::new(),
            table: String::new(),
            description: "3 WAL archive failures".to_owned(),
            severity: Severity::Critical,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: Some("Fix archive_command".to_owned()),
        };
        assert!(finding.to_proposal().is_none());
    }

    #[test]
    fn finding_to_proposal_returns_none_for_archive_lag() {
        let finding = BackupMonitoringFinding {
            kind: BackupMonitoringFindingKind::ArchiveLag,
            schema: String::new(),
            table: String::new(),
            description: "No WAL segment archived in 10 minute(s)".to_owned(),
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Heuristic,
            suggested_action: None,
        };
        assert!(finding.to_proposal().is_none());
    }

    #[test]
    fn report_to_proposals_is_always_empty() {
        let report = BackupMonitoringReport {
            findings: vec![
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::WalArchiveFailure,
                    schema: String::new(),
                    table: String::new(),
                    description: "5 failures".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some("Check logs".to_owned()),
                },
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::ArchivingDisabled,
                    schema: String::new(),
                    table: String::new(),
                    description: "archive_mode = off".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Advisory,
                    suggested_action: Some("Enable archiving".to_owned()),
                },
            ],
        };
        assert!(report.to_proposals().is_empty());
    }

    #[test]
    fn empty_report_to_proposals_is_empty() {
        let report = BackupMonitoringReport {
            findings: Vec::new(),
        };
        assert!(report.to_proposals().is_empty());
    }

    // ---- SQL constant tests ----------------------------------------------

    #[test]
    fn archiver_status_sql_references_pg_stat_archiver() {
        assert!(ARCHIVER_STATUS_SQL.contains("pg_stat_archiver"));
        assert!(ARCHIVER_STATUS_SQL.contains("failed_count"));
        assert!(ARCHIVER_STATUS_SQL.contains("archived_count"));
        assert!(ARCHIVER_STATUS_SQL.contains("last_archived_wal"));
    }

    #[test]
    fn archive_mode_sql_references_pg_settings() {
        assert!(ARCHIVE_MODE_SQL.contains("pg_settings"));
        assert!(ARCHIVE_MODE_SQL.contains("archive_mode"));
    }

    #[test]
    fn wal_file_count_sql_references_pg_ls_waldir() {
        assert!(WAL_FILE_COUNT_SQL.contains("pg_ls_waldir()"));
        // WAL segment names are 24-character hex strings.
        assert!(WAL_FILE_COUNT_SQL.contains("24"));
    }

    #[test]
    fn current_wal_lsn_sql_references_pg_current_wal_lsn() {
        assert!(CURRENT_WAL_LSN_SQL.contains("pg_current_wal_lsn()"));
        assert!(CURRENT_WAL_LSN_SQL.contains("current_lsn"));
    }

    // ---- Threshold constant tests ----------------------------------------

    #[test]
    fn archive_lag_warn_threshold_is_5_minutes() {
        assert_eq!(ARCHIVE_LAG_WARN_SECS, 300);
    }

    #[test]
    fn wal_file_count_warn_threshold_is_100() {
        assert_eq!(WAL_FILE_COUNT_WARN, 100);
    }

    // ---- Multi-finding report --------------------------------------------

    #[test]
    fn report_to_prompt_with_multiple_findings() {
        let report = BackupMonitoringReport {
            findings: vec![
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::WalArchiveFailure,
                    schema: String::new(),
                    table: String::new(),
                    description: "2 failures".to_owned(),
                    severity: Severity::Critical,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: Some("Fix archive_command".to_owned()),
                },
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::WalFileAccumulation,
                    schema: String::new(),
                    table: String::new(),
                    description: "200 WAL files on disk".to_owned(),
                    severity: Severity::Warning,
                    evidence_class: EvidenceClass::Heuristic,
                    suggested_action: None,
                },
                BackupMonitoringFinding {
                    kind: BackupMonitoringFindingKind::CurrentWalPosition,
                    schema: String::new(),
                    table: String::new(),
                    description: "Current WAL position: 1/A0000000".to_owned(),
                    severity: Severity::Info,
                    evidence_class: EvidenceClass::Factual,
                    suggested_action: None,
                },
            ],
        };
        let prompt = report.to_prompt();
        assert!(prompt.contains("3 finding"));
        assert!(prompt.contains("[wal_archive_failure]"));
        assert!(prompt.contains("[wal_file_accumulation]"));
        assert!(prompt.contains("[current_wal_position]"));
        assert!(prompt.contains("Fix archive_command"));
    }
}
