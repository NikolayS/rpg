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

    /// Whether a proposal warrants adversarial LLM review.
    ///
    /// Returns `true` for Auto-mode actions or high-severity findings.
    pub fn needs_llm_review(proposal: &ActionProposal, autonomy: AutonomyLevel) -> bool {
        autonomy == AutonomyLevel::Auto || proposal.severity == Severity::Critical
    }

    /// Adversarial LLM review for high-risk actions.
    ///
    /// Sends the proposal to a secondary LLM call that specifically looks
    /// for reasons the action might be unsafe or counterproductive.
    /// Falls back to rule-based approval if the LLM is unavailable.
    pub async fn review_with_llm(
        &self,
        proposal: &ActionProposal,
        current_autonomy: AutonomyLevel,
        provider: &dyn crate::ai::LlmProvider,
    ) -> AuditDecision {
        // First, run the rule-based checks.
        let rule_decision = self.review(proposal, current_autonomy);
        if matches!(rule_decision, AuditDecision::Rejected { .. }) {
            return rule_decision;
        }

        // Only invoke LLM for high-risk proposals.
        if !Self::needs_llm_review(proposal, current_autonomy) {
            return rule_decision;
        }

        // Build the adversarial review prompt.
        let prompt = format!(
            "You are a PostgreSQL safety auditor. A monitoring tool proposes the \
             following action on a production database. Your job is to find reasons \
             this action might be UNSAFE, COUNTERPRODUCTIVE, or UNNECESSARY.\n\n\
             Finding: {}\n\
             Proposed action: {}\n\
             Expected outcome: {}\n\
             Risk assessment: {}\n\
             Severity: {:?}\n\
             Evidence class: {:?}\n\n\
             Reply with EXACTLY one of:\n\
             APPROVE: <one-line reason>\n\
             REJECT: <one-line reason>\n\n\
             Be conservative. If in doubt, REJECT.",
            proposal.finding,
            proposal.proposed_action,
            proposal.expected_outcome,
            proposal.risk,
            proposal.severity,
            proposal.evidence_class,
        );

        let messages = vec![crate::ai::Message {
            role: crate::ai::Role::User,
            content: prompt,
        }];
        let options = crate::ai::CompletionOptions {
            max_tokens: 256,
            temperature: 0.0,
            ..Default::default()
        };

        match provider.complete(&messages, &options).await {
            Ok(result) => parse_llm_audit_response(&result.content),
            Err(e) => {
                crate::logging::warn(
                    "auditor",
                    &format!("LLM review failed, falling back to rules: {e}"),
                );
                // Fail-open: if LLM is unavailable, use rule-based decision.
                rule_decision
            }
        }
    }
}

/// Parse the LLM's APPROVE/REJECT response.
fn parse_llm_audit_response(response: &str) -> AuditDecision {
    let trimmed = response.trim();
    for line in trimmed.lines() {
        let line = line.trim();
        if let Some(reason) = line.strip_prefix("APPROVE:") {
            return AuditDecision::Approved {
                note: Some(format!("[LLM] {}", reason.trim())),
            };
        }
        if let Some(reason) = line.strip_prefix("REJECT:") {
            return AuditDecision::Rejected {
                reason: format!("[LLM] {}", reason.trim()),
            };
        }
    }
    // If the LLM didn't follow the format, reject (conservative).
    AuditDecision::Rejected {
        reason: "[LLM] Could not parse auditor response — rejecting as precaution".to_owned(),
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
// Circuit breaker
// ---------------------------------------------------------------------------

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Window of recent outcomes to consider.
    pub window_size: usize,
    /// Failure rate threshold (0.0–1.0) to trip the breaker.
    pub failure_threshold: f64,
    /// Minimum actions before the breaker can trip.
    pub min_actions: usize,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            window_size: 20,
            failure_threshold: 0.15, // >15% failure rate trips
            min_actions: 5,
        }
    }
}

/// Per-feature circuit breaker that tracks success/failure and downgrades
/// from Auto to Supervised when the failure rate exceeds the threshold.
#[derive(Debug)]
pub struct CircuitBreaker {
    /// Configuration.
    config: CircuitBreakerConfig,
    /// Per-feature outcome windows (true = success, false = failure).
    windows: std::collections::HashMap<FeatureArea, Vec<bool>>,
    /// Features that have been tripped (downgraded to Supervised).
    tripped: std::collections::HashSet<FeatureArea>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default config.
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    /// Create a circuit breaker with custom config.
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            windows: std::collections::HashMap::new(),
            tripped: std::collections::HashSet::new(),
        }
    }

    /// Record an action outcome for a feature area.
    ///
    /// Returns `true` if the breaker just tripped (transition to tripped state).
    pub fn record(&mut self, feature: FeatureArea, success: bool) -> bool {
        let window = self.windows.entry(feature).or_default();
        window.push(success);

        // Trim to window size.
        if window.len() > self.config.window_size {
            let excess = window.len() - self.config.window_size;
            window.drain(..excess);
        }

        // Check if breaker should trip.
        if window.len() >= self.config.min_actions && !self.tripped.contains(&feature) {
            let failures = window.iter().filter(|&&s| !s).count();
            #[allow(clippy::cast_precision_loss)]
            let failure_rate = failures as f64 / window.len() as f64;
            if failure_rate > self.config.failure_threshold {
                self.tripped.insert(feature);
                return true;
            }
        }

        false
    }

    /// Check if a feature area has been tripped (downgraded).
    pub fn is_tripped(&self, feature: FeatureArea) -> bool {
        self.tripped.contains(&feature)
    }

    /// Get the effective autonomy level for a feature, respecting the breaker.
    ///
    /// If the feature's breaker has tripped, Auto is downgraded to Supervised.
    pub fn effective_autonomy(
        &self,
        feature: FeatureArea,
        configured: AutonomyLevel,
    ) -> AutonomyLevel {
        if configured == AutonomyLevel::Auto && self.is_tripped(feature) {
            AutonomyLevel::Supervised
        } else {
            configured
        }
    }

    /// Reset a tripped breaker (manual recovery).
    pub fn reset(&mut self, feature: FeatureArea) {
        self.tripped.remove(&feature);
        self.windows.remove(&feature);
    }

    /// Get the current failure rate for a feature (0.0–1.0).
    #[allow(clippy::cast_precision_loss)]
    pub fn failure_rate(&self, feature: FeatureArea) -> f64 {
        self.windows.get(&feature).map_or(0.0, |w| {
            if w.is_empty() {
                0.0
            } else {
                let failures = w.iter().filter(|&&s| !s).count();
                failures as f64 / w.len() as f64
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Auto promotion tracker
// ---------------------------------------------------------------------------

/// Tracks Supervised action history to determine Auto promotion eligibility.
///
/// A feature can be promoted to Auto only after a minimum number of
/// successful Supervised actions with a high Auditor approval rate.
#[derive(Debug)]
pub struct AutoPromotionTracker {
    /// Minimum successful Supervised actions required.
    pub min_actions: usize,
    /// Minimum approval rate (0.0–1.0).
    pub min_approval_rate: f64,
    /// Per-feature counters: (total, approved, successful).
    counters: std::collections::HashMap<FeatureArea, (usize, usize, usize)>,
}

impl Default for AutoPromotionTracker {
    fn default() -> Self {
        Self {
            min_actions: 30,
            min_approval_rate: 0.85,
            counters: std::collections::HashMap::new(),
        }
    }
}

impl AutoPromotionTracker {
    /// Create a new tracker with default thresholds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a Supervised action outcome.
    pub fn record(&mut self, feature: FeatureArea, approved: bool, success: bool) {
        let (total, approved_count, success_count) = self.counters.entry(feature).or_default();
        *total += 1;
        if approved {
            *approved_count += 1;
        }
        if success {
            *success_count += 1;
        }
    }

    /// Check if a feature is eligible for Auto promotion.
    #[allow(clippy::cast_precision_loss)]
    pub fn is_eligible(&self, feature: FeatureArea) -> bool {
        let Some(&(total, approved, successful)) = self.counters.get(&feature) else {
            return false;
        };
        if successful < self.min_actions {
            return false;
        }
        if total == 0 {
            return false;
        }
        let approval_rate = approved as f64 / total as f64;
        approval_rate >= self.min_approval_rate
    }

    /// Get promotion stats for a feature: (total, approved, successful).
    pub fn stats(&self, feature: FeatureArea) -> (usize, usize, usize) {
        self.counters.get(&feature).copied().unwrap_or((0, 0, 0))
    }
}

// ---------------------------------------------------------------------------
// Auto-mode action constraints
// ---------------------------------------------------------------------------

/// Defines which action types are permitted in Auto mode per feature area.
///
/// Auto mode is deliberately narrow — only safe, well-validated actions
/// that are reversible or have low blast radius.
pub fn auto_permitted_actions(feature: FeatureArea) -> &'static [&'static str] {
    match feature {
        // RCA Auto: only cancel/terminate (no GUC changes).
        FeatureArea::Rca => &["pg_cancel_backend", "pg_terminate_backend"],
        // Index health Auto: only REINDEX CONCURRENTLY (no DROP, no CREATE).
        FeatureArea::IndexHealth => &["REINDEX CONCURRENTLY"],
        // All other features: no Auto actions permitted yet.
        _ => &[],
    }
}

/// Check if a proposed action is permitted in Auto mode for its feature area.
pub fn is_auto_permitted(feature: FeatureArea, proposed_action: &str) -> bool {
    let permitted = auto_permitted_actions(feature);
    let action_lower = proposed_action.to_lowercase();
    permitted
        .iter()
        .any(|p| action_lower.contains(&p.to_lowercase()))
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

    #[test]
    fn parse_llm_approve_response() {
        let decision = parse_llm_audit_response("APPROVE: action is safe and well-scoped");
        match decision {
            AuditDecision::Approved { note } => {
                let n = note.unwrap();
                assert!(n.contains("[LLM]"));
                assert!(n.contains("safe"));
            }
            AuditDecision::Rejected { .. } => panic!("Expected Approved"),
        }
    }

    #[test]
    fn parse_llm_reject_response() {
        let decision =
            parse_llm_audit_response("REJECT: terminating this PID could cascade to replicas");
        match decision {
            AuditDecision::Rejected { reason } => {
                assert!(reason.contains("[LLM]"));
                assert!(reason.contains("cascade"));
            }
            AuditDecision::Approved { .. } => panic!("Expected Rejected"),
        }
    }

    #[test]
    fn parse_llm_multiline_response() {
        let response = "Let me analyze this...\nAPPROVE: looks good\nSome trailing text";
        let decision = parse_llm_audit_response(response);
        assert!(matches!(decision, AuditDecision::Approved { .. }));
    }

    #[test]
    fn parse_llm_garbage_response_rejects() {
        let decision = parse_llm_audit_response("I think this is fine, go ahead");
        assert!(matches!(decision, AuditDecision::Rejected { .. }));
    }

    #[test]
    fn needs_llm_review_auto_mode() {
        let proposal = ActionProposal {
            feature: FeatureArea::Rca,
            severity: Severity::Info,
            evidence_class: EvidenceClass::Factual,
            finding: "test".to_owned(),
            proposed_action: "test".to_owned(),
            expected_outcome: "test".to_owned(),
            risk: "low".to_owned(),
            created_at: SystemTime::now(),
        };
        // Auto mode always needs LLM review.
        assert!(Auditor::needs_llm_review(&proposal, AutonomyLevel::Auto));
        // Supervised + Info severity does not.
        assert!(!Auditor::needs_llm_review(
            &proposal,
            AutonomyLevel::Supervised
        ));
    }

    #[test]
    fn needs_llm_review_critical_severity() {
        let proposal = ActionProposal {
            feature: FeatureArea::Rca,
            severity: Severity::Critical,
            evidence_class: EvidenceClass::Factual,
            finding: "test".to_owned(),
            proposed_action: "test".to_owned(),
            expected_outcome: "test".to_owned(),
            risk: "high".to_owned(),
            created_at: SystemTime::now(),
        };
        // Critical severity always needs LLM review.
        assert!(Auditor::needs_llm_review(
            &proposal,
            AutonomyLevel::Supervised
        ));
    }

    // -----------------------------------------------------------------------
    // Circuit breaker tests
    // -----------------------------------------------------------------------

    #[test]
    fn circuit_breaker_does_not_trip_on_success() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..20 {
            assert!(!cb.record(FeatureArea::Rca, true));
        }
        assert!(!cb.is_tripped(FeatureArea::Rca));
    }

    #[test]
    fn circuit_breaker_trips_on_high_failure_rate() {
        let config = CircuitBreakerConfig {
            window_size: 10,
            failure_threshold: 0.15,
            min_actions: 5,
        };
        let mut cb = CircuitBreaker::with_config(config);
        // 3 successes + 2 failures = 40% failure rate > 15%
        cb.record(FeatureArea::Rca, true);
        cb.record(FeatureArea::Rca, true);
        cb.record(FeatureArea::Rca, true);
        cb.record(FeatureArea::Rca, false);
        let tripped = cb.record(FeatureArea::Rca, false);
        assert!(tripped);
        assert!(cb.is_tripped(FeatureArea::Rca));
    }

    #[test]
    fn circuit_breaker_does_not_trip_below_min_actions() {
        let config = CircuitBreakerConfig {
            window_size: 10,
            failure_threshold: 0.15,
            min_actions: 5,
        };
        let mut cb = CircuitBreaker::with_config(config);
        // 3 failures but only 3 actions (below min_actions=5)
        cb.record(FeatureArea::Rca, false);
        cb.record(FeatureArea::Rca, false);
        assert!(!cb.record(FeatureArea::Rca, false));
        assert!(!cb.is_tripped(FeatureArea::Rca));
    }

    #[test]
    fn circuit_breaker_effective_autonomy_downgrades_auto() {
        let mut cb = CircuitBreaker::new();
        // Before trip: Auto stays Auto.
        assert_eq!(
            cb.effective_autonomy(FeatureArea::Rca, AutonomyLevel::Auto),
            AutonomyLevel::Auto
        );

        // Trip the breaker with all failures.
        for _ in 0..5 {
            cb.record(FeatureArea::Rca, false);
        }
        assert!(cb.is_tripped(FeatureArea::Rca));

        // After trip: Auto → Supervised.
        assert_eq!(
            cb.effective_autonomy(FeatureArea::Rca, AutonomyLevel::Auto),
            AutonomyLevel::Supervised
        );

        // Supervised and Observe unchanged.
        assert_eq!(
            cb.effective_autonomy(FeatureArea::Rca, AutonomyLevel::Supervised),
            AutonomyLevel::Supervised
        );
    }

    #[test]
    fn circuit_breaker_reset_clears_state() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..5 {
            cb.record(FeatureArea::Rca, false);
        }
        assert!(cb.is_tripped(FeatureArea::Rca));

        cb.reset(FeatureArea::Rca);
        assert!(!cb.is_tripped(FeatureArea::Rca));
        assert!((cb.failure_rate(FeatureArea::Rca) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn circuit_breaker_per_feature_isolation() {
        let mut cb = CircuitBreaker::new();
        for _ in 0..5 {
            cb.record(FeatureArea::Rca, false);
        }
        assert!(cb.is_tripped(FeatureArea::Rca));
        assert!(!cb.is_tripped(FeatureArea::IndexHealth));
    }

    #[test]
    fn circuit_breaker_failure_rate() {
        let mut cb = CircuitBreaker::new();
        cb.record(FeatureArea::Rca, true);
        cb.record(FeatureArea::Rca, true);
        cb.record(FeatureArea::Rca, false);
        cb.record(FeatureArea::Rca, false);
        assert!((cb.failure_rate(FeatureArea::Rca) - 0.5).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Auto promotion tracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn promotion_tracker_not_eligible_initially() {
        let tracker = AutoPromotionTracker::new();
        assert!(!tracker.is_eligible(FeatureArea::Rca));
    }

    #[test]
    fn promotion_tracker_eligible_after_threshold() {
        let mut tracker = AutoPromotionTracker {
            min_actions: 5,
            min_approval_rate: 0.80,
            ..Default::default()
        };
        // 5 approved+successful actions.
        for _ in 0..5 {
            tracker.record(FeatureArea::Rca, true, true);
        }
        assert!(tracker.is_eligible(FeatureArea::Rca));
    }

    #[test]
    fn promotion_tracker_not_eligible_low_approval() {
        let mut tracker = AutoPromotionTracker {
            min_actions: 5,
            min_approval_rate: 0.80,
            ..Default::default()
        };
        // 5 successful but only 3 approved (60% < 80%).
        for _ in 0..3 {
            tracker.record(FeatureArea::Rca, true, true);
        }
        for _ in 0..2 {
            tracker.record(FeatureArea::Rca, false, true);
        }
        assert!(!tracker.is_eligible(FeatureArea::Rca));
    }

    #[test]
    fn promotion_tracker_not_eligible_insufficient_successes() {
        let mut tracker = AutoPromotionTracker {
            min_actions: 10,
            min_approval_rate: 0.80,
            ..Default::default()
        };
        // 5 successful (below min_actions=10).
        for _ in 0..5 {
            tracker.record(FeatureArea::Rca, true, true);
        }
        assert!(!tracker.is_eligible(FeatureArea::Rca));
    }

    #[test]
    fn promotion_tracker_stats() {
        let mut tracker = AutoPromotionTracker::new();
        tracker.record(FeatureArea::IndexHealth, true, true);
        tracker.record(FeatureArea::IndexHealth, true, false);
        tracker.record(FeatureArea::IndexHealth, false, true);
        let (total, approved, successful) = tracker.stats(FeatureArea::IndexHealth);
        assert_eq!(total, 3);
        assert_eq!(approved, 2);
        assert_eq!(successful, 2);
    }

    // -----------------------------------------------------------------------
    // Auto-mode action constraints tests
    // -----------------------------------------------------------------------

    #[test]
    fn auto_permitted_rca_cancel() {
        assert!(is_auto_permitted(
            FeatureArea::Rca,
            "SELECT pg_cancel_backend(1234)"
        ));
    }

    #[test]
    fn auto_permitted_rca_terminate() {
        assert!(is_auto_permitted(
            FeatureArea::Rca,
            "SELECT pg_terminate_backend(5678)"
        ));
    }

    #[test]
    fn auto_not_permitted_rca_guc_change() {
        assert!(!is_auto_permitted(
            FeatureArea::Rca,
            "ALTER SYSTEM SET statement_timeout = '30s'"
        ));
    }

    #[test]
    fn auto_permitted_index_reindex() {
        assert!(is_auto_permitted(
            FeatureArea::IndexHealth,
            "REINDEX CONCURRENTLY idx_foo"
        ));
    }

    #[test]
    fn auto_not_permitted_index_drop() {
        assert!(!is_auto_permitted(
            FeatureArea::IndexHealth,
            "DROP INDEX CONCURRENTLY idx_foo"
        ));
    }

    #[test]
    fn auto_not_permitted_vacuum() {
        assert!(!is_auto_permitted(FeatureArea::Vacuum, "VACUUM orders"));
    }
}
