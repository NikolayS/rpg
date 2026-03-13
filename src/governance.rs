//! AAA Governance Framework — Analyzer, Actor, Auditor.
//!
//! Provides the infrastructure for autonomous database management:
//! - **Analyzer**: observes, diagnoses, recommends (LLM-powered)
//! - **Actor**: executes approved actions within boundaries (no LLM)
//! - **Auditor**: reviews proposals and outcomes (rule-based initially)
//!
//! Per-feature autonomy levels control how much Samo can do without
//! human approval.

// Many types are defined ahead of their consumers (Phase 3 integration).
#![allow(dead_code)]

use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Feature areas
// ---------------------------------------------------------------------------

/// Feature areas that can be independently configured for autonomy level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureArea {
    /// Dead tuples, autovacuum health, freezing/wraparound prevention.
    Vacuum,
    /// Table and index bloat management.
    Bloat,
    /// Unused, duplicate, missing, invalid indexes.
    IndexHealth,
    /// `PostgreSQL` parameter optimization.
    ConfigTuning,
    /// Long-running query cancel, idle-in-transaction termination.
    QueryOptimization,
    /// Pool saturation, idle connection cleanup.
    ConnectionManagement,
    /// Replication lag, slot management.
    Replication,
    /// Root cause analysis — `pg_ash` powered investigation.
    Rca,
    /// Backup freshness, WAL archiving, PITR readiness.
    BackupMonitoring,
    /// Role audit, password policy, `pg_hba` review.
    Security,
}

impl FeatureArea {
    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Vacuum => "vacuum",
            Self::Bloat => "bloat",
            Self::IndexHealth => "index_health",
            Self::ConfigTuning => "config_tuning",
            Self::QueryOptimization => "query_optimization",
            Self::ConnectionManagement => "connection_management",
            Self::Replication => "replication",
            Self::Rca => "rca",
            Self::BackupMonitoring => "backup_monitoring",
            Self::Security => "security",
        }
    }
}

// ---------------------------------------------------------------------------
// Autonomy levels (per-feature)
// ---------------------------------------------------------------------------

/// Autonomy level for a feature area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Read-only: observe, diagnose, report. Zero writes.
    #[default]
    Observe,
    /// Propose actions, human confirms before execution.
    Supervised,
    /// Act autonomously within policy and DB permissions.
    Auto,
}

impl AutonomyLevel {
    /// Short code for display.
    pub fn code(self) -> &'static str {
        match self {
            Self::Observe => "O",
            Self::Supervised => "S",
            Self::Auto => "A",
        }
    }
}

// ---------------------------------------------------------------------------
// Evidence classification
// ---------------------------------------------------------------------------

/// Evidence quality classification for findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceClass {
    /// Deterministic, directly observable from `pg_catalog`/`pg_stat_*`.
    Factual,
    /// Statistical inference, may have false positives.
    Heuristic,
    /// Subjective assessment, depends on workload context.
    Advisory,
}

impl EvidenceClass {
    /// Maximum autonomy level appropriate for this evidence class.
    pub fn max_autonomy(self) -> AutonomyLevel {
        match self {
            Self::Factual => AutonomyLevel::Auto,
            Self::Heuristic => AutonomyLevel::Supervised,
            Self::Advisory => AutonomyLevel::Observe,
        }
    }
}

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

/// Severity level for findings and proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Informational — no action needed.
    Info,
    /// Warning — action recommended but not urgent.
    Warning,
    /// Critical — action required soon.
    Critical,
}

// ---------------------------------------------------------------------------
// Action proposal (Analyzer → Auditor → Actor)
// ---------------------------------------------------------------------------

/// A structured action proposal from the Analyzer.
///
/// The Analyzer produces these; the Auditor reviews them; the Actor
/// executes approved ones.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActionProposal {
    /// Which feature area this belongs to.
    pub feature: FeatureArea,
    /// Severity of the finding.
    pub severity: Severity,
    /// Evidence classification.
    pub evidence_class: EvidenceClass,
    /// Human-readable description of the finding.
    pub finding: String,
    /// The SQL or action to execute (if approved).
    pub proposed_action: String,
    /// Expected outcome of the action.
    pub expected_outcome: String,
    /// Risk assessment.
    pub risk: String,
    /// Timestamp when the proposal was created.
    pub created_at: SystemTime,
}

// ---------------------------------------------------------------------------
// Action result (Actor output)
// ---------------------------------------------------------------------------

/// Outcome of an executed action.
#[derive(Debug, Clone, serde::Serialize)]
pub enum ActionOutcome {
    /// Action completed successfully.
    Success {
        /// Brief description of what happened.
        detail: String,
    },
    /// Action failed.
    Failure {
        /// Error message.
        error: String,
    },
    /// Action was vetoed by the Auditor.
    Vetoed {
        /// Reason the Auditor rejected the proposal.
        reason: String,
    },
    /// Action was skipped by the user (Supervised mode).
    Skipped,
}

// ---------------------------------------------------------------------------
// Audit log entry
// ---------------------------------------------------------------------------

/// A single entry in the action audit log.
///
/// Every action — proposed, executed, vetoed, or skipped — is logged
/// here for accountability and learning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditLogEntry {
    /// Monotonic sequence number within this session.
    pub seq: u64,
    /// When this entry was recorded.
    pub timestamp: SystemTime,
    /// Feature area.
    pub feature: FeatureArea,
    /// Autonomy level at the time.
    pub autonomy_level: AutonomyLevel,
    /// The proposed action (SQL or description).
    pub action: String,
    /// Justification from the Analyzer.
    pub justification: String,
    /// What happened.
    pub outcome: ActionOutcome,
    /// Auditor's assessment (if any).
    pub auditor_note: Option<String>,
}

// ---------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------

/// In-memory action audit log for the current session.
///
/// All proposals and their outcomes are recorded here. This log is
/// never summarized by the LLM (per SPEC: only FIFO-evicted if it
/// exceeds its allocated budget).
#[derive(Debug, Default)]
pub struct AuditLog {
    entries: Vec<AuditLogEntry>,
    next_seq: u64,
}

impl AuditLog {
    /// Create a new empty audit log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new entry in the log.
    pub fn record(
        &mut self,
        feature: FeatureArea,
        autonomy_level: AutonomyLevel,
        action: String,
        justification: String,
        outcome: ActionOutcome,
        auditor_note: Option<String>,
    ) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(AuditLogEntry {
            seq,
            timestamp: SystemTime::now(),
            feature,
            autonomy_level,
            action,
            justification,
            outcome,
            auditor_note,
        });
        seq
    }

    /// Number of entries in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get all entries (most recent last).
    pub fn entries(&self) -> &[AuditLogEntry] {
        &self.entries
    }

    /// Get entries for a specific feature area.
    pub fn entries_for_feature(&self, feature: FeatureArea) -> Vec<&AuditLogEntry> {
        self.entries
            .iter()
            .filter(|e| e.feature == feature)
            .collect()
    }

    /// Serialize the log to JSON (for export/persistence).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.entries)
    }
}

// ---------------------------------------------------------------------------
// Auditor (rule-based)
// ---------------------------------------------------------------------------

/// Rule-based Auditor that validates proposals before execution.
///
/// Initially deterministic (no LLM). Validates:
/// - Action type is in the whitelist for the feature area
/// - Autonomy level permits the action
/// - Evidence class is appropriate for the autonomy level
#[derive(Debug, Default)]
pub struct Auditor;

/// Result of an Auditor review.
#[derive(Debug, Clone)]
pub enum AuditDecision {
    /// Proposal is approved.
    Approved {
        /// Optional note from the Auditor.
        note: Option<String>,
    },
    /// Proposal is rejected.
    Rejected {
        /// Reason for rejection.
        reason: String,
    },
}

impl Auditor {
    /// Review an action proposal.
    ///
    /// Checks that the evidence class is appropriate for the current
    /// autonomy level and that the proposal is well-formed.
    #[allow(clippy::unused_self)]
    pub fn review(
        &self,
        proposal: &ActionProposal,
        current_autonomy: AutonomyLevel,
    ) -> AuditDecision {
        // Rule 1: Evidence class must support the autonomy level.
        let max = proposal.evidence_class.max_autonomy();
        if !autonomy_permits(current_autonomy, max) {
            return AuditDecision::Rejected {
                reason: format!(
                    "Evidence class {:?} only supports up to {:?} autonomy, \
                     but current level is {:?}",
                    proposal.evidence_class, max, current_autonomy,
                ),
            };
        }

        // Rule 2: Proposed action must not be empty.
        if proposal.proposed_action.trim().is_empty() {
            return AuditDecision::Rejected {
                reason: "Empty proposed action".to_owned(),
            };
        }

        // Rule 3: Finding must not be empty.
        if proposal.finding.trim().is_empty() {
            return AuditDecision::Rejected {
                reason: "Empty finding description".to_owned(),
            };
        }

        AuditDecision::Approved { note: None }
    }
}

/// Check if `current` autonomy level is within the bounds of `max_allowed`.
fn autonomy_permits(current: AutonomyLevel, max_allowed: AutonomyLevel) -> bool {
    match max_allowed {
        AutonomyLevel::Auto => true, // Auto permits everything.
        AutonomyLevel::Supervised => current != AutonomyLevel::Auto,
        AutonomyLevel::Observe => current == AutonomyLevel::Observe,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_area_labels() {
        assert_eq!(FeatureArea::Vacuum.label(), "vacuum");
        assert_eq!(FeatureArea::IndexHealth.label(), "index_health");
        assert_eq!(FeatureArea::Rca.label(), "rca");
    }

    #[test]
    fn autonomy_level_default_is_observe() {
        assert_eq!(AutonomyLevel::default(), AutonomyLevel::Observe);
    }

    #[test]
    fn autonomy_level_codes() {
        assert_eq!(AutonomyLevel::Observe.code(), "O");
        assert_eq!(AutonomyLevel::Supervised.code(), "S");
        assert_eq!(AutonomyLevel::Auto.code(), "A");
    }

    #[test]
    fn evidence_class_max_autonomy() {
        assert_eq!(EvidenceClass::Factual.max_autonomy(), AutonomyLevel::Auto);
        assert_eq!(
            EvidenceClass::Heuristic.max_autonomy(),
            AutonomyLevel::Supervised
        );
        assert_eq!(
            EvidenceClass::Advisory.max_autonomy(),
            AutonomyLevel::Observe
        );
    }

    #[test]
    fn autonomy_permits_observe_in_observe() {
        assert!(autonomy_permits(
            AutonomyLevel::Observe,
            AutonomyLevel::Observe
        ));
    }

    #[test]
    fn autonomy_denies_supervised_for_advisory() {
        // Advisory evidence only supports Observe.
        assert!(!autonomy_permits(
            AutonomyLevel::Supervised,
            AutonomyLevel::Observe
        ));
    }

    #[test]
    fn autonomy_permits_supervised_for_heuristic() {
        assert!(autonomy_permits(
            AutonomyLevel::Supervised,
            AutonomyLevel::Supervised
        ));
    }

    #[test]
    fn autonomy_denies_auto_for_heuristic() {
        assert!(!autonomy_permits(
            AutonomyLevel::Auto,
            AutonomyLevel::Supervised
        ));
    }

    #[test]
    fn autonomy_permits_auto_for_factual() {
        assert!(autonomy_permits(AutonomyLevel::Auto, AutonomyLevel::Auto));
    }

    #[test]
    fn audit_log_new_is_empty() {
        let log = AuditLog::new();
        assert!(log.is_empty());
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn audit_log_record_increments_seq() {
        let mut log = AuditLog::new();
        let s1 = log.record(
            FeatureArea::IndexHealth,
            AutonomyLevel::Observe,
            "REINDEX CONCURRENTLY idx_foo".to_owned(),
            "Index bloat at 35%".to_owned(),
            ActionOutcome::Success {
                detail: "Reindexed".to_owned(),
            },
            None,
        );
        let s2 = log.record(
            FeatureArea::Vacuum,
            AutonomyLevel::Supervised,
            "VACUUM orders".to_owned(),
            "500k dead tuples".to_owned(),
            ActionOutcome::Skipped,
            Some("User declined".to_owned()),
        );
        assert_eq!(s1, 0);
        assert_eq!(s2, 1);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn audit_log_entries_for_feature() {
        let mut log = AuditLog::new();
        log.record(
            FeatureArea::IndexHealth,
            AutonomyLevel::Observe,
            "action1".to_owned(),
            "j1".to_owned(),
            ActionOutcome::Skipped,
            None,
        );
        log.record(
            FeatureArea::Vacuum,
            AutonomyLevel::Observe,
            "action2".to_owned(),
            "j2".to_owned(),
            ActionOutcome::Skipped,
            None,
        );
        log.record(
            FeatureArea::IndexHealth,
            AutonomyLevel::Observe,
            "action3".to_owned(),
            "j3".to_owned(),
            ActionOutcome::Skipped,
            None,
        );
        let idx_entries = log.entries_for_feature(FeatureArea::IndexHealth);
        assert_eq!(idx_entries.len(), 2);
    }

    #[test]
    fn audit_log_to_json() {
        let mut log = AuditLog::new();
        log.record(
            FeatureArea::Rca,
            AutonomyLevel::Observe,
            "analyze".to_owned(),
            "lock contention".to_owned(),
            ActionOutcome::Success {
                detail: "report generated".to_owned(),
            },
            None,
        );
        let json = log.to_json().expect("should serialize");
        assert!(json.contains("rca"));
        assert!(json.contains("lock contention"));
    }

    #[test]
    fn auditor_approves_valid_proposal() {
        let auditor = Auditor;
        let proposal = ActionProposal {
            feature: FeatureArea::IndexHealth,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Factual,
            finding: "idx_foo is unused for 90 days".to_owned(),
            proposed_action: "DROP INDEX CONCURRENTLY idx_foo".to_owned(),
            expected_outcome: "Free 450MB disk space".to_owned(),
            risk: "Low — index unused".to_owned(),
            created_at: SystemTime::now(),
        };
        let decision = auditor.review(&proposal, AutonomyLevel::Auto);
        assert!(matches!(decision, AuditDecision::Approved { .. }));
    }

    #[test]
    fn auditor_rejects_advisory_at_supervised() {
        let auditor = Auditor;
        let proposal = ActionProposal {
            feature: FeatureArea::ConfigTuning,
            severity: Severity::Info,
            evidence_class: EvidenceClass::Advisory,
            finding: "Consider increasing shared_buffers".to_owned(),
            proposed_action: "ALTER SYSTEM SET shared_buffers = '4GB'".to_owned(),
            expected_outcome: "Better cache hit ratio".to_owned(),
            risk: "Requires restart".to_owned(),
            created_at: SystemTime::now(),
        };
        let decision = auditor.review(&proposal, AutonomyLevel::Supervised);
        assert!(matches!(decision, AuditDecision::Rejected { .. }));
    }

    #[test]
    fn auditor_rejects_empty_action() {
        let auditor = Auditor;
        let proposal = ActionProposal {
            feature: FeatureArea::Vacuum,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Factual,
            finding: "Dead tuples".to_owned(),
            proposed_action: "  ".to_owned(),
            expected_outcome: "Clean up".to_owned(),
            risk: "Low".to_owned(),
            created_at: SystemTime::now(),
        };
        let decision = auditor.review(&proposal, AutonomyLevel::Observe);
        assert!(matches!(decision, AuditDecision::Rejected { .. }));
    }

    #[test]
    fn auditor_rejects_empty_finding() {
        let auditor = Auditor;
        let proposal = ActionProposal {
            feature: FeatureArea::Vacuum,
            severity: Severity::Warning,
            evidence_class: EvidenceClass::Factual,
            finding: String::new(),
            proposed_action: "VACUUM orders".to_owned(),
            expected_outcome: "Clean up".to_owned(),
            risk: "Low".to_owned(),
            created_at: SystemTime::now(),
        };
        let decision = auditor.review(&proposal, AutonomyLevel::Observe);
        assert!(matches!(decision, AuditDecision::Rejected { .. }));
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Critical);
    }

    #[test]
    fn action_outcome_variants() {
        // Verify all variants can be constructed.
        let _ = ActionOutcome::Success {
            detail: "ok".to_owned(),
        };
        let _ = ActionOutcome::Failure {
            error: "failed".to_owned(),
        };
        let _ = ActionOutcome::Vetoed {
            reason: "too risky".to_owned(),
        };
        let _ = ActionOutcome::Skipped;
    }
}
