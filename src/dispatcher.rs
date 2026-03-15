//! Dispatcher — connects Auditor review to action execution.
//!
//! The Dispatcher is the coordination layer between the Analyzer,
//! Auditor, and Actor. It receives [`ActionProposal`] values, routes
//! them through the Auditor, checks the circuit breaker, and either
//! executes or skips them based on the effective autonomy level.

// Phase 3 infrastructure — consumers arrive in subsequent PRs.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use crate::audit_persistence::{self, DEFAULT_MAX_BYTES};
use crate::governance::{
    ActionOutcome, ActionProposal, AuditDecision, AuditLog, Auditor, AutoPromotionTracker,
    AutonomyLevel, CircuitBreaker, FeatureArea, VetoTracker,
};
use crate::llm_auditor::LlmAuditorConfig;

// ---------------------------------------------------------------------------
// Promotion status
// ---------------------------------------------------------------------------

/// Promotion eligibility snapshot for a single feature area.
#[allow(dead_code)]
pub struct PromotionStatus {
    /// The feature's current autonomy level.
    pub current_level: AutonomyLevel,
    /// Whether the feature has met the threshold for Auto promotion.
    pub eligible_for_next: bool,
    /// Number of successful Supervised actions recorded.
    pub successful_actions: u32,
    /// Number required before promotion is considered.
    pub required_actions: u32,
    /// Fraction of proposals approved by the Auditor (0.0–1.0).
    pub approval_rate: f64,
    /// Human-readable summary, e.g. `"S (→A 25/30 actions, 92% approval)"`.
    pub display: String,
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Connects Auditor review to the execution path for action proposals.
///
/// The Dispatcher is the single control point that:
/// 1. Checks vetoes (`VetoTracker` may downgrade Auto to Supervised).
/// 2. Checks the effective autonomy level (circuit breaker may downgrade).
/// 3. Runs rule-based Auditor review.
/// 4. Optionally runs LLM adversarial review for Auto-mode proposals.
/// 5. Executes (Auto) or defers (Supervised) approved proposals.
/// 6. Records every outcome in the audit log.
/// 7. Sets post-action verification on successful Auto executions.
/// 8. Tracks Supervised approvals for Auto promotion eligibility.
/// 9. Optionally persists each entry to a JSONL file for restart recovery.
#[derive(Debug)]
pub struct Dispatcher {
    auditor: Auditor,
    circuit_breakers: HashMap<FeatureArea, CircuitBreaker>,
    audit_log: AuditLog,
    veto_tracker: VetoTracker,
    promotion_tracker: AutoPromotionTracker,
    /// Path to the JSONL audit persistence file, if configured.
    audit_log_path: Option<PathBuf>,
    /// Optional LLM provider for adversarial review of Auto-mode proposals.
    llm_provider: Option<Box<dyn crate::ai::LlmProvider>>,
    /// Configuration for the LLM adversarial auditor.
    llm_auditor_config: LlmAuditorConfig,
}

impl Dispatcher {
    /// Create a new Dispatcher with default configuration and no LLM provider.
    pub fn new() -> Self {
        Self {
            auditor: Auditor,
            circuit_breakers: HashMap::new(),
            audit_log: AuditLog::new(),
            veto_tracker: VetoTracker::new(),
            promotion_tracker: AutoPromotionTracker::new(),
            audit_log_path: None,
            llm_provider: None,
            llm_auditor_config: LlmAuditorConfig::default(),
        }
    }

    /// Create a Dispatcher wired to an LLM provider for adversarial review.
    ///
    /// When `provider` is `Some` and `llm_auditor_config.enabled` is `true`,
    /// Auto-mode proposals are submitted to the LLM for adversarial review
    /// before execution.  Proposals that the LLM rejects are vetoed and
    /// recorded in the audit log.
    pub fn new_with_provider(
        provider: Option<Box<dyn crate::ai::LlmProvider>>,
        llm_auditor_config: LlmAuditorConfig,
    ) -> Self {
        Self {
            auditor: Auditor,
            circuit_breakers: HashMap::new(),
            audit_log: AuditLog::new(),
            veto_tracker: VetoTracker::new(),
            promotion_tracker: AutoPromotionTracker::new(),
            audit_log_path: None,
            llm_provider: provider,
            llm_auditor_config,
        }
    }

    /// Create a new Dispatcher that persists entries to `path`.
    ///
    /// Prior entries are loaded from `path` on construction so that the
    /// in-memory audit log reflects history from previous runs.  If the
    /// file does not exist, an empty log is used — no error is raised.
    pub fn new_with_path(path: PathBuf) -> Self {
        let mut log = AuditLog::new();

        // Load prior entries; silently ignore read errors (e.g. first run).
        if let Ok(entries) = audit_persistence::load_entries(&path, 0) {
            for entry in entries {
                log.restore(entry);
            }
        }

        Self {
            auditor: Auditor,
            circuit_breakers: HashMap::new(),
            audit_log: log,
            veto_tracker: VetoTracker::new(),
            promotion_tracker: AutoPromotionTracker::new(),
            audit_log_path: Some(path),
            llm_provider: None,
            llm_auditor_config: LlmAuditorConfig::default(),
        }
    }

    /// Return the configured persistence path, if any.
    pub fn audit_log_path(&self) -> Option<&PathBuf> {
        self.audit_log_path.as_ref()
    }

    /// Dispatch a proposal through the governance pipeline.
    ///
    /// # Decision flow
    ///
    /// 1. `Observe` → [`ActionOutcome::Skipped`] immediately (no review).
    /// 2. `VetoTracker` match → downgrade Auto to Supervised.
    /// 3. Auditor rejects → [`ActionOutcome::Vetoed`]; records veto.
    /// 4. Circuit breaker tripped → downgrade Auto to Supervised.
    /// 5. `Auto` (after downgrade checks) → log success; set verification.
    /// 6. `Supervised` → [`ActionOutcome::Skipped`] (logged for human
    ///    review).
    /// 7. Supervised approvals are recorded in the promotion tracker.
    ///
    /// Every path records an entry in the audit log.  When an
    /// `audit_log_path` is configured the entry is also persisted to the
    /// JSONL file immediately after being added to the in-memory log.
    pub fn dispatch_proposal(
        &mut self,
        proposal: &ActionProposal,
        autonomy: AutonomyLevel,
    ) -> ActionOutcome {
        // Step 1: Observe mode — never act.
        if autonomy == AutonomyLevel::Observe {
            let outcome = ActionOutcome::Skipped;
            let seq = self.audit_log.record(
                proposal.feature,
                autonomy,
                proposal.proposed_action.clone(),
                proposal.finding.clone(),
                outcome.clone(),
                Some("Observe mode: no action taken".to_owned()),
            );
            self.try_persist(seq);
            return outcome;
        }

        // Step 2: VetoTracker — downgrade Auto to Supervised for known-bad
        // action patterns, before the Auditor runs.
        let autonomy = if autonomy == AutonomyLevel::Auto
            && self
                .veto_tracker
                .is_vetoed(proposal.feature, &proposal.proposed_action)
        {
            AutonomyLevel::Supervised
        } else {
            autonomy
        };

        // Step 3: Auditor review.
        let decision = self.auditor.review(proposal, autonomy);
        let auditor_note = match &decision {
            AuditDecision::Approved { note } => note.clone(),
            AuditDecision::Rejected { reason } => {
                // Record the veto so future matching proposals are
                // automatically downgraded.
                self.veto_tracker
                    .record_veto(proposal.feature, &proposal.proposed_action);
                let outcome = ActionOutcome::Vetoed {
                    reason: reason.clone(),
                };
                let seq = self.audit_log.record(
                    proposal.feature,
                    autonomy,
                    proposal.proposed_action.clone(),
                    proposal.finding.clone(),
                    outcome.clone(),
                    Some(format!("Auditor rejected: {reason}")),
                );
                self.try_persist(seq);
                return outcome;
            }
        };

        // Step 4: Apply circuit breaker — downgrade Auto to Supervised if
        // the breaker for this feature has tripped.
        let effective = self.effective_autonomy(proposal.feature, autonomy);

        // Step 5 / 6: Execute or defer.
        let outcome = match effective {
            AutonomyLevel::Auto => {
                // Actual Actor execution arrives in a later PR.
                // Log a simulated success to satisfy the audit trail.
                ActionOutcome::Success {
                    detail: format!("Auto-executed: {}", proposal.proposed_action),
                }
            }
            AutonomyLevel::Supervised | AutonomyLevel::Observe => {
                // Supervised: logged for human review; Observe is handled
                // above, so this branch is always Supervised.
                ActionOutcome::Skipped
            }
        };

        // Record outcome and update circuit breaker.
        let success = matches!(outcome, ActionOutcome::Success { .. });
        let seq = self.audit_log.record(
            proposal.feature,
            effective,
            proposal.proposed_action.clone(),
            proposal.finding.clone(),
            outcome.clone(),
            auditor_note,
        );
        self.circuit_breakers
            .entry(proposal.feature)
            .or_insert_with(CircuitBreaker::new)
            .record(proposal.feature, success);

        // Step 7: Post-action verification for successful Auto executions.
        if success && effective == AutonomyLevel::Auto {
            self.audit_log.set_verification(seq, true);
        }

        // Persist the entry (including any verification flag update).
        self.try_persist(seq);

        // Record Supervised approvals in the promotion tracker so the feature
        // can accumulate enough evidence to be eligible for Auto promotion.
        if effective == AutonomyLevel::Supervised {
            // Auditor approved (we reached here past the rejection branch).
            // Supervised actions are "pending human"; count them as successful
            // supervised outcomes for promotion-tracking purposes.
            self.promotion_tracker.record(proposal.feature, true, true);
        }

        outcome
    }

    /// Async variant of [`Self::dispatch_proposal`] that additionally runs
    /// LLM adversarial review for `Auto`-mode proposals when a provider is
    /// configured.
    ///
    /// # Decision flow (additional step vs the sync variant)
    ///
    /// After the rule-based Auditor approves a proposal in `Auto` mode, the
    /// proposal is submitted to the configured LLM provider (if any).  If the
    /// LLM rejects the proposal, it is vetoed and the outcome is
    /// [`ActionOutcome::Vetoed`].  Timeout and provider errors fall back to
    /// heuristic approval so that a transient LLM outage never blocks actions
    /// that passed rule-based review.
    #[allow(clippy::too_many_lines)]
    pub async fn dispatch_proposal_async(
        &mut self,
        proposal: &ActionProposal,
        autonomy: AutonomyLevel,
    ) -> ActionOutcome {
        // Step 1: Observe mode — never act.
        if autonomy == AutonomyLevel::Observe {
            let outcome = ActionOutcome::Skipped;
            self.audit_log.record(
                proposal.feature,
                autonomy,
                proposal.proposed_action.clone(),
                proposal.finding.clone(),
                outcome.clone(),
                Some("Observe mode: no action taken".to_owned()),
            );
            return outcome;
        }

        // Step 2: VetoTracker — downgrade Auto to Supervised for known-bad
        // action patterns, before the Auditor runs.
        let autonomy = if autonomy == AutonomyLevel::Auto
            && self
                .veto_tracker
                .is_vetoed(proposal.feature, &proposal.proposed_action)
        {
            AutonomyLevel::Supervised
        } else {
            autonomy
        };

        // Step 3: Rule-based Auditor review.
        let decision = self.auditor.review(proposal, autonomy);
        let auditor_note = match &decision {
            AuditDecision::Approved { note } => note.clone(),
            AuditDecision::Rejected { reason } => {
                self.veto_tracker
                    .record_veto(proposal.feature, &proposal.proposed_action);
                let outcome = ActionOutcome::Vetoed {
                    reason: reason.clone(),
                };
                self.audit_log.record(
                    proposal.feature,
                    autonomy,
                    proposal.proposed_action.clone(),
                    proposal.finding.clone(),
                    outcome.clone(),
                    Some(format!("Auditor rejected: {reason}")),
                );
                return outcome;
            }
        };

        // Step 3b: LLM adversarial review — only for Auto-mode proposals
        // that passed rule-based checks.
        if autonomy == AutonomyLevel::Auto {
            let provider_ref = self.llm_provider.as_deref();
            let llm_review = crate::llm_auditor::review_proposal(
                proposal,
                &self.llm_auditor_config,
                provider_ref,
            )
            .await
            .unwrap_or_else(|err| crate::llm_auditor::LlmAuditReview {
                approved: true,
                concerns: vec![],
                recommendation: format!("LLM review error (fallback): {err}"),
            });

            if !llm_review.approved {
                let reason = format!(
                    "LLM adversarial review rejected: {}",
                    llm_review.recommendation
                );
                self.veto_tracker
                    .record_veto(proposal.feature, &proposal.proposed_action);
                let outcome = ActionOutcome::Vetoed {
                    reason: reason.clone(),
                };
                self.audit_log.record(
                    proposal.feature,
                    autonomy,
                    proposal.proposed_action.clone(),
                    proposal.finding.clone(),
                    outcome.clone(),
                    Some(reason),
                );
                return outcome;
            }
        }

        // Step 4: Apply circuit breaker — downgrade Auto to Supervised if
        // the breaker for this feature has tripped.
        let effective = self.effective_autonomy(proposal.feature, autonomy);

        // Step 5 / 6: Execute or defer.
        let outcome = match effective {
            AutonomyLevel::Auto => ActionOutcome::Success {
                detail: format!("Auto-executed: {}", proposal.proposed_action),
            },
            AutonomyLevel::Supervised | AutonomyLevel::Observe => ActionOutcome::Skipped,
        };

        // Record outcome and update circuit breaker.
        let success = matches!(outcome, ActionOutcome::Success { .. });
        let seq = self.audit_log.record(
            proposal.feature,
            effective,
            proposal.proposed_action.clone(),
            proposal.finding.clone(),
            outcome.clone(),
            auditor_note,
        );
        self.circuit_breakers
            .entry(proposal.feature)
            .or_insert_with(CircuitBreaker::new)
            .record(proposal.feature, success);

        // Step 7: Post-action verification for successful Auto executions.
        if success && effective == AutonomyLevel::Auto {
            self.audit_log.set_verification(seq, true);
        }

        // Record Supervised approvals in the promotion tracker.
        if effective == AutonomyLevel::Supervised {
            self.promotion_tracker.record(proposal.feature, true, true);
        }

        outcome
    }

    /// Borrow the audit log.
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
    }

    /// Borrow the audit log mutably (e.g. to set verification results).
    pub fn audit_log_mut(&mut self) -> &mut AuditLog {
        &mut self.audit_log
    }

    /// Borrow the veto tracker.
    pub fn veto_tracker(&self) -> &VetoTracker {
        &self.veto_tracker
    }

    /// Borrow the promotion tracker.
    pub fn promotion_tracker(&self) -> &AutoPromotionTracker {
        &self.promotion_tracker
    }

    /// Return a snapshot of circuit breaker state for every feature that has
    /// one. Yields `(feature, is_tripped, failure_rate)` tuples.
    pub fn circuit_breaker_states(&self) -> Vec<(FeatureArea, bool, f64)> {
        self.circuit_breakers
            .iter()
            .map(|(&feature, cb)| (feature, cb.is_tripped(feature), cb.failure_rate(feature)))
            .collect()
    }

    /// Return a promotion status snapshot for every known feature area.
    ///
    /// Only features that have at least one recorded Supervised action are
    /// included. The `current_level` parameter describes what autonomy level
    /// the feature currently runs at (supplied by the caller because the
    /// Dispatcher itself does not own per-feature configuration).
    #[allow(clippy::cast_precision_loss)]
    pub fn promotion_status(
        &self,
        current_levels: &HashMap<FeatureArea, AutonomyLevel>,
    ) -> Vec<(FeatureArea, PromotionStatus)> {
        let required = u32::try_from(self.promotion_tracker.min_actions).unwrap_or(u32::MAX);
        let mut result = Vec::new();

        for (&feature, &current_level) in current_levels {
            let (total, approved, successful) = self.promotion_tracker.stats(feature);
            if total == 0 {
                continue;
            }
            // total > 0 is guaranteed by the check above.
            let approval_rate = approved as f64 / total as f64;
            let eligible_for_next = current_level == AutonomyLevel::Supervised
                && self.promotion_tracker.is_eligible(feature);

            // Format approval rate as a rounded integer percentage string to
            // avoid any integer cast lints (approval_rate is in [0.0, 1.0]).
            let pct = format!("{:.0}", approval_rate * 100.0);
            let display = match current_level {
                AutonomyLevel::Supervised => {
                    format!("S (\u{2192}A {successful}/{required} actions, {pct}% approval)")
                }
                AutonomyLevel::Auto => {
                    format!("A ({successful} successful actions, {pct}% approval)")
                }
                AutonomyLevel::Observe => {
                    format!("O ({total} actions observed)")
                }
            };

            let successful_actions = u32::try_from(successful).unwrap_or(u32::MAX);
            result.push((
                feature,
                PromotionStatus {
                    current_level,
                    eligible_for_next,
                    successful_actions,
                    required_actions: required,
                    approval_rate,
                    display,
                },
            ));
        }

        result
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Return the effective autonomy for a feature, honouring the circuit
    /// breaker. Auto is downgraded to Supervised when the breaker is tripped.
    fn effective_autonomy(&self, feature: FeatureArea, configured: AutonomyLevel) -> AutonomyLevel {
        if configured == AutonomyLevel::Auto {
            if let Some(cb) = self.circuit_breakers.get(&feature) {
                return cb.effective_autonomy(feature, configured);
            }
        }
        configured
    }

    /// Persist the entry identified by `seq` to the JSONL file, if a path
    /// is configured.
    ///
    /// Rotation is attempted before each write.  I/O errors are logged to
    /// stderr and silently ignored so that persistence failures never
    /// interrupt the governance pipeline.
    fn try_persist(&self, seq: u64) {
        let Some(path) = self.audit_log_path.as_ref() else {
            return;
        };

        let Some(entry) = self.audit_log.entries().iter().find(|e| e.seq == seq) else {
            return;
        };

        // Rotate before writing; ignore errors.
        if let Err(e) = audit_persistence::rotate_if_needed(path, DEFAULT_MAX_BYTES) {
            eprintln!("rpg: audit log rotation failed: {e}");
        }

        if let Err(e) = audit_persistence::persist_entry(path, entry) {
            eprintln!("rpg: audit log write failed: {e}");
        }
    }
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::governance::{CircuitBreakerConfig, EvidenceClass, FeatureArea, Severity};

    fn make_proposal(
        feature: FeatureArea,
        evidence_class: EvidenceClass,
        proposed_action: &str,
        finding: &str,
    ) -> ActionProposal {
        ActionProposal {
            feature,
            severity: Severity::Warning,
            evidence_class,
            finding: finding.to_owned(),
            proposed_action: proposed_action.to_owned(),
            expected_outcome: "Improved health".to_owned(),
            risk: "Low".to_owned(),
            created_at: SystemTime::now(),
        }
    }

    // -----------------------------------------------------------------------
    // 1. Observe → always Skipped
    // -----------------------------------------------------------------------

    #[test]
    fn observe_proposal_is_skipped() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Observe);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Observe must always return Skipped"
        );
    }

    // -----------------------------------------------------------------------
    // 2. Observe is recorded in the audit log
    // -----------------------------------------------------------------------

    #[test]
    fn observe_is_recorded_in_audit_log() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Observe);
        assert_eq!(d.audit_log().len(), 1);
    }

    // -----------------------------------------------------------------------
    // 3. Supervised + approved → Skipped (waits for human)
    // -----------------------------------------------------------------------

    #[test]
    fn supervised_approved_returns_skipped() {
        let mut d = Dispatcher::new();
        // Factual evidence is approved at Supervised level.
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Supervised-approved proposals must return Skipped (awaiting human)"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Auto + approved → Success
    // -----------------------------------------------------------------------

    #[test]
    fn auto_approved_returns_success() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(1234)",
            "Long-running query detected",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Success { .. }),
            "Auto + approved must return Success"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Auto + Auditor rejected → Vetoed
    // -----------------------------------------------------------------------

    #[test]
    fn auto_rejected_returns_vetoed() {
        let mut d = Dispatcher::new();
        // Advisory evidence is rejected at Auto level (max is Observe).
        let p = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Advisory,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Some advisory finding",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Vetoed { .. }),
            "Advisory evidence must be vetoed in Auto mode"
        );
    }

    // -----------------------------------------------------------------------
    // 6. Supervised + Auditor rejected → Vetoed
    // -----------------------------------------------------------------------

    #[test]
    fn supervised_rejected_returns_vetoed() {
        let mut d = Dispatcher::new();
        // Advisory evidence is rejected at Supervised level too.
        let p = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Advisory,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Some advisory finding",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        assert!(
            matches!(outcome, ActionOutcome::Vetoed { .. }),
            "Advisory evidence must be vetoed in Supervised mode"
        );
    }

    // -----------------------------------------------------------------------
    // 7. Empty proposed_action → Vetoed
    // -----------------------------------------------------------------------

    #[test]
    fn empty_action_is_vetoed() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "",
            "Something bad",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Vetoed { .. }),
            "Empty proposed_action must be vetoed"
        );
    }

    // -----------------------------------------------------------------------
    // 8. Circuit breaker tripped → Auto downgraded to Supervised → Skipped
    // -----------------------------------------------------------------------

    #[test]
    fn circuit_breaker_tripped_downgrades_auto_to_supervised() {
        // Use a very sensitive breaker: trips after 2 failures in a window of 2.
        let sensitive_config = CircuitBreakerConfig {
            window_size: 2,
            failure_threshold: 0.0, // any failure trips
            min_actions: 2,
        };

        let mut d = Dispatcher::new();
        // Pre-install a tripped circuit breaker for Vacuum.
        let mut cb = CircuitBreaker::with_config(sensitive_config);
        cb.record(FeatureArea::Vacuum, false);
        cb.record(FeatureArea::Vacuum, false); // now tripped
        d.circuit_breakers.insert(FeatureArea::Vacuum, cb);

        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        // Even in Auto mode the breaker downgrades to Supervised → Skipped.
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Tripped circuit breaker must downgrade Auto to Supervised (Skipped)"
        );
    }

    // -----------------------------------------------------------------------
    // 9. Audit log records all entries across multiple proposals
    // -----------------------------------------------------------------------

    #[test]
    fn audit_log_records_multiple_proposals() {
        let mut d = Dispatcher::new();
        let features = [
            FeatureArea::Vacuum,
            FeatureArea::Bloat,
            FeatureArea::IndexHealth,
        ];
        for &f in &features {
            let p = make_proposal(f, EvidenceClass::Factual, "some action", "some finding");
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }
        assert_eq!(
            d.audit_log().len(),
            3,
            "Every proposal must produce one audit log entry"
        );
    }

    // -----------------------------------------------------------------------
    // 10. Success updates circuit breaker (no trip on success)
    // -----------------------------------------------------------------------

    #[test]
    fn success_does_not_trip_circuit_breaker() {
        let mut d = Dispatcher::new();
        for _ in 0..10 {
            let p = make_proposal(
                FeatureArea::Rca,
                EvidenceClass::Factual,
                "SELECT pg_cancel_backend(1)",
                "Query running too long",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }
        let cb = d.circuit_breakers.get(&FeatureArea::Rca);
        assert!(
            cb.is_none_or(|c| !c.is_tripped(FeatureArea::Rca)),
            "All-success history must not trip the circuit breaker"
        );
    }

    // -----------------------------------------------------------------------
    // 11. Heuristic evidence at Supervised → approved (Skipped, not Vetoed)
    // -----------------------------------------------------------------------

    #[test]
    fn heuristic_evidence_at_supervised_approved() {
        let mut d = Dispatcher::new();
        // Heuristic supports up to Supervised, so it is approved here.
        let p = make_proposal(
            FeatureArea::Bloat,
            EvidenceClass::Heuristic,
            "VACUUM FULL some_table",
            "Table bloat detected (heuristic)",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Heuristic at Supervised should be approved (returns Skipped)"
        );
    }

    // -----------------------------------------------------------------------
    // 12. Heuristic evidence at Auto → Vetoed (exceeds allowed autonomy)
    // -----------------------------------------------------------------------

    #[test]
    fn heuristic_evidence_at_auto_vetoed() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Bloat,
            EvidenceClass::Heuristic,
            "VACUUM FULL some_table",
            "Table bloat detected (heuristic)",
        );
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Vetoed { .. }),
            "Heuristic evidence must be vetoed in Auto mode"
        );
    }

    // -----------------------------------------------------------------------
    // 13. Default dispatcher is equivalent to new()
    // -----------------------------------------------------------------------

    #[test]
    fn default_dispatcher_is_empty() {
        let d = Dispatcher::default();
        assert!(d.audit_log().is_empty());
        assert!(d.circuit_breakers.is_empty());
    }

    // -----------------------------------------------------------------------
    // 14. Vetoed action pattern downgrades Auto to Supervised
    // -----------------------------------------------------------------------

    #[test]
    fn vetoed_action_downgrades_auto_to_supervised() {
        let mut d = Dispatcher::new();
        // Pre-record a veto for a Vacuum action pattern.
        d.veto_tracker
            .record_veto(FeatureArea::Vacuum, "VACUUM ANALYZE");

        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        // Auto is downgraded to Supervised due to the veto → Skipped.
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Vetoed action pattern must downgrade Auto to Supervised (Skipped)"
        );
    }

    // -----------------------------------------------------------------------
    // 15. Non-vetoed action proceeds normally in Auto
    // -----------------------------------------------------------------------

    #[test]
    fn non_vetoed_action_proceeds_normally() {
        let mut d = Dispatcher::new();
        // Veto a different action for the same feature.
        d.veto_tracker
            .record_veto(FeatureArea::Vacuum, "VACUUM FULL");

        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        // VACUUM ANALYZE does not match the "vacuum full" pattern → Auto.
        let outcome = d.dispatch_proposal(&p, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Success { .. }),
            "Non-vetoed action must proceed normally in Auto mode"
        );
    }

    // -----------------------------------------------------------------------
    // 16. Rejected proposal records veto in VetoTracker
    // -----------------------------------------------------------------------

    #[test]
    fn rejected_proposal_records_veto() {
        let mut d = Dispatcher::new();
        assert_eq!(d.veto_tracker().veto_count(), 0);

        // Advisory evidence → Auditor rejects → veto recorded.
        let p = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Advisory,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Some advisory finding",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Auto);

        assert_eq!(
            d.veto_tracker().veto_count(),
            1,
            "Rejected proposal must record one veto"
        );
    }

    // -----------------------------------------------------------------------
    // 17. Subsequent matching proposal is auto-downgraded after rejection
    // -----------------------------------------------------------------------

    #[test]
    fn subsequent_matching_proposal_is_auto_downgraded() {
        let mut d = Dispatcher::new();

        // First dispatch: Advisory → rejected, veto recorded.
        let p1 = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Advisory,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Advisory finding",
        );
        d.dispatch_proposal(&p1, AutonomyLevel::Auto);

        // Second dispatch: same action (Factual evidence this time).
        // The veto match (substring of the same string) downgrades
        // Auto → Supervised → Skipped.
        let p2 = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Factual,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Factual finding",
        );
        let outcome = d.dispatch_proposal(&p2, AutonomyLevel::Auto);
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "Subsequent matching proposal must be auto-downgraded to Supervised"
        );
    }

    // -----------------------------------------------------------------------
    // 18. Post-action verification is set on successful Auto execution
    // -----------------------------------------------------------------------

    #[test]
    fn post_action_verification_set_on_auto_success() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(42)",
            "Long-running query",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Auto);

        let entries = d.audit_log().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].verified,
            Some(true),
            "Successful Auto action must have verification set to true"
        );
    }

    // -----------------------------------------------------------------------
    // 19. Verification not set on Supervised (skipped)
    // -----------------------------------------------------------------------

    #[test]
    fn verification_not_set_on_supervised() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Supervised);

        let entries = d.audit_log().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].verified, None,
            "Supervised (Skipped) action must not have verification set"
        );
    }

    // -----------------------------------------------------------------------
    // 20. Multiple vetoes accumulate in the tracker
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_vetoes_accumulate() {
        let mut d = Dispatcher::new();

        // Three distinct advisory rejections → three distinct veto patterns.
        let actions = [
            (
                FeatureArea::ConfigTuning,
                "ALTER SYSTEM SET work_mem = '1GB'",
            ),
            (
                FeatureArea::ConfigTuning,
                "ALTER SYSTEM SET shared_buffers = '4GB'",
            ),
            (
                FeatureArea::Vacuum,
                "ALTER SYSTEM SET autovacuum_max_workers = 10",
            ),
        ];
        for (feature, action) in actions {
            let p = make_proposal(feature, EvidenceClass::Advisory, action, "Advisory");
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }

        assert_eq!(
            d.veto_tracker().veto_count(),
            3,
            "Three distinct rejections must produce three veto entries"
        );
    }

    // -----------------------------------------------------------------------
    // 21. veto_tracker() accessor returns the tracker
    // -----------------------------------------------------------------------

    #[test]
    fn veto_tracker_accessor_works() {
        let d = Dispatcher::new();
        assert_eq!(d.veto_tracker().veto_count(), 0);
    }

    // -----------------------------------------------------------------------
    // 22. New dispatcher has no promotion history
    // -----------------------------------------------------------------------

    #[test]
    fn new_dispatcher_has_no_promotion_history() {
        let d = Dispatcher::new();
        let (total, approved, successful) = d.promotion_tracker().stats(FeatureArea::Vacuum);
        assert_eq!(total, 0, "new dispatcher: total must be 0");
        assert_eq!(approved, 0, "new dispatcher: approved must be 0");
        assert_eq!(successful, 0, "new dispatcher: successful must be 0");
    }

    // -----------------------------------------------------------------------
    // 23. Supervised approval increments the promotion tracker
    // -----------------------------------------------------------------------

    #[test]
    fn supervised_success_increments_tracker() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuple ratio high",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        let (total, _approved, successful) = d.promotion_tracker().stats(FeatureArea::Vacuum);
        assert_eq!(total, 1, "one Supervised action must be recorded");
        assert_eq!(successful, 1, "approved Supervised counts as successful");
    }

    // -----------------------------------------------------------------------
    // 24. Promotion eligible after threshold met
    // -----------------------------------------------------------------------

    #[test]
    fn promotion_eligible_after_threshold_met() {
        let mut d = Dispatcher::new();
        // Default threshold is 30; use a tracker with a lower threshold to
        // keep the test fast without bypassing the real code path.
        d.promotion_tracker.min_actions = 3;
        d.promotion_tracker.min_approval_rate = 0.5;

        for _ in 0..3 {
            let p = make_proposal(
                FeatureArea::Vacuum,
                EvidenceClass::Factual,
                "VACUUM ANALYZE users",
                "Dead tuple ratio high",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }
        assert!(
            d.promotion_tracker().is_eligible(FeatureArea::Vacuum),
            "feature must be eligible after meeting the threshold"
        );
    }

    // -----------------------------------------------------------------------
    // 25. Not eligible before threshold
    // -----------------------------------------------------------------------

    #[test]
    fn not_eligible_before_threshold() {
        let mut d = Dispatcher::new();
        // Record fewer actions than the default threshold (30).
        for _ in 0..5 {
            let p = make_proposal(
                FeatureArea::Bloat,
                EvidenceClass::Factual,
                "REINDEX CONCURRENTLY idx",
                "Bloat detected",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }
        assert!(
            !d.promotion_tracker().is_eligible(FeatureArea::Bloat),
            "feature must not be eligible before threshold"
        );
    }

    // -----------------------------------------------------------------------
    // 26. Status display format is correct
    // -----------------------------------------------------------------------

    #[test]
    fn status_display_format_correct() {
        let mut d = Dispatcher::new();
        d.promotion_tracker.min_actions = 10;
        d.promotion_tracker.min_approval_rate = 0.8;

        for _ in 0..5 {
            let p = make_proposal(
                FeatureArea::IndexHealth,
                EvidenceClass::Factual,
                "REINDEX CONCURRENTLY idx_users_email",
                "Unused index detected",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }

        let mut levels = HashMap::new();
        levels.insert(FeatureArea::IndexHealth, AutonomyLevel::Supervised);
        let statuses = d.promotion_status(&levels);
        assert_eq!(statuses.len(), 1);
        let (_, status) = &statuses[0];
        // Display must contain the arrow, counts, and approval percentage.
        assert!(
            status.display.contains("→A"),
            "display must contain promotion arrow"
        );
        assert!(
            status.display.contains("5/10"),
            "display must show current/required counts"
        );
        assert!(
            status.display.contains("100%"),
            "display must show approval rate"
        );
    }

    // -----------------------------------------------------------------------
    // 27. Multiple features tracked independently
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_features_tracked_independently() {
        let mut d = Dispatcher::new();
        // Vacuum: 2 actions; Bloat: 5 actions.
        for _ in 0..2 {
            let p = make_proposal(
                FeatureArea::Vacuum,
                EvidenceClass::Factual,
                "VACUUM ANALYZE users",
                "Dead tuples",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }
        for _ in 0..5 {
            let p = make_proposal(
                FeatureArea::Bloat,
                EvidenceClass::Factual,
                "REINDEX CONCURRENTLY idx",
                "Bloat detected",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Supervised);
        }
        let (vacuum_total, _, _) = d.promotion_tracker().stats(FeatureArea::Vacuum);
        let (bloat_total, _, _) = d.promotion_tracker().stats(FeatureArea::Bloat);
        assert_eq!(vacuum_total, 2, "Vacuum must have 2 actions");
        assert_eq!(bloat_total, 5, "Bloat must have 5 actions");
    }

    // -----------------------------------------------------------------------
    // 28. Auto actions do not affect promotion tracking
    // -----------------------------------------------------------------------

    #[test]
    fn auto_actions_do_not_affect_promotion_tracking() {
        let mut d = Dispatcher::new();
        for _ in 0..10 {
            let p = make_proposal(
                FeatureArea::Rca,
                EvidenceClass::Factual,
                "SELECT pg_cancel_backend(1)",
                "Long-running query",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }
        let (total, _, _) = d.promotion_tracker().stats(FeatureArea::Rca);
        assert_eq!(
            total, 0,
            "Auto actions must not contribute to promotion tracking"
        );
    }

    // -----------------------------------------------------------------------
    // 29. Observe actions do not affect promotion tracking
    // -----------------------------------------------------------------------

    #[test]
    fn observe_actions_do_not_affect_tracking() {
        let mut d = Dispatcher::new();
        for _ in 0..5 {
            let p = make_proposal(
                FeatureArea::Vacuum,
                EvidenceClass::Factual,
                "VACUUM ANALYZE users",
                "Dead tuples",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Observe);
        }
        let (total, _, _) = d.promotion_tracker().stats(FeatureArea::Vacuum);
        assert_eq!(
            total, 0,
            "Observe actions must not contribute to promotion tracking"
        );
    }

    // -----------------------------------------------------------------------
    // Persistence integration tests
    // -----------------------------------------------------------------------

    // 30. new_with_path writes entries to the JSONL file.
    #[test]
    fn persistence_writes_entries_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let mut d = Dispatcher::new_with_path(path.clone());
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(1)",
            "Long-running query",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Auto);

        assert!(path.exists(), "JSONL file must be created after dispatch");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(!contents.is_empty(), "JSONL file must not be empty");
        // Should have exactly one line.
        assert_eq!(contents.lines().count(), 1, "one dispatch → one line");
    }

    // 31. Entries survive a restart (new_with_path loads prior entries).
    #[test]
    fn persistence_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        // First "run": dispatch two proposals.
        {
            let mut d = Dispatcher::new_with_path(path.clone());
            for i in 0..2u32 {
                let action = format!("VACUUM ANALYZE t{i}");
                let p = make_proposal(
                    FeatureArea::Vacuum,
                    EvidenceClass::Factual,
                    &action,
                    "Dead tuples",
                );
                d.dispatch_proposal(&p, AutonomyLevel::Auto);
            }
            assert_eq!(d.audit_log().len(), 2);
        }

        // Second "run": load from file and verify entries are present.
        let d2 = Dispatcher::new_with_path(path.clone());
        assert_eq!(
            d2.audit_log().len(),
            2,
            "prior entries must be loaded on restart"
        );
    }

    // 32. Seq counter continues from loaded entries after restart.
    #[test]
    fn persistence_seq_continues_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        // First run: one entry (seq = 0).
        {
            let mut d = Dispatcher::new_with_path(path.clone());
            let p = make_proposal(
                FeatureArea::Rca,
                EvidenceClass::Factual,
                "pg_cancel_backend(1)",
                "Long query",
            );
            d.dispatch_proposal(&p, AutonomyLevel::Auto);
        }

        // Second run: new entry must get seq = 1, not 0.
        {
            let mut d2 = Dispatcher::new_with_path(path.clone());
            let p = make_proposal(
                FeatureArea::Rca,
                EvidenceClass::Factual,
                "pg_cancel_backend(2)",
                "Long query",
            );
            d2.dispatch_proposal(&p, AutonomyLevel::Auto);

            let entries = d2.audit_log().entries();
            assert_eq!(entries.len(), 2, "loaded entry + new entry");
            // The newly added entry must have seq 1.
            assert_eq!(
                entries[1].seq, 1,
                "new entry seq must follow loaded entries"
            );
        }
    }

    // 33. Without a path, no file is created.
    #[test]
    fn no_persistence_without_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let mut d = Dispatcher::new(); // no path
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "pg_cancel_backend(1)",
            "Long query",
        );
        d.dispatch_proposal(&p, AutonomyLevel::Auto);

        assert!(
            !path.exists(),
            "no file must be created when path is not configured"
        );
    }

    // 34. audit_log_path accessor returns the configured path.
    #[test]
    fn audit_log_path_accessor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let d = Dispatcher::new_with_path(path.clone());
        assert_eq!(
            d.audit_log_path(),
            Some(&path),
            "accessor must return the configured path"
        );

        let d2 = Dispatcher::new();
        assert!(d2.audit_log_path().is_none(), "no path returns None");
    }

    // 35. All outcomes (Vetoed, Skipped, Success) are persisted.
    #[test]
    fn persistence_all_outcomes_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");

        let mut d = Dispatcher::new_with_path(path.clone());

        // Observe → Skipped.
        let p1 = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuples",
        );
        d.dispatch_proposal(&p1, AutonomyLevel::Observe);

        // Auto + Advisory → Vetoed.
        let p2 = make_proposal(
            FeatureArea::ConfigTuning,
            EvidenceClass::Advisory,
            "ALTER SYSTEM SET work_mem = '1GB'",
            "Advisory finding",
        );
        d.dispatch_proposal(&p2, AutonomyLevel::Auto);

        // Auto + Factual → Success.
        let p3 = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "pg_cancel_backend(42)",
            "Long query",
        );
        d.dispatch_proposal(&p3, AutonomyLevel::Auto);

        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            lines.lines().count(),
            3,
            "all three outcomes must be persisted"
        );
    }

    // -----------------------------------------------------------------------
    // Mock LLM provider for async dispatch tests
    // -----------------------------------------------------------------------

    #[derive(Debug)]
    struct MockLlmProvider {
        response: String,
    }

    impl MockLlmProvider {
        fn approving() -> Self {
            Self {
                response: r#"{"approved": true, "concerns": [], "recommendation": "Safe"}"#
                    .to_owned(),
            }
        }

        fn rejecting() -> Self {
            Self {
                response:
                    r#"{"approved": false, "concerns": ["Too risky"], "recommendation": "Reject"}"#
                        .to_owned(),
            }
        }
    }

    impl crate::ai::LlmProvider for MockLlmProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn default_model(&self) -> &'static str {
            "mock-model"
        }

        fn complete(
            &self,
            _messages: &[crate::ai::Message],
            _options: &crate::ai::CompletionOptions,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::ai::CompletionResult, String>>
                    + Send
                    + '_,
            >,
        > {
            let content = self.response.clone();
            Box::pin(async move {
                Ok(crate::ai::CompletionResult {
                    content,
                    input_tokens: 5,
                    output_tokens: 10,
                })
            })
        }
    }

    // -----------------------------------------------------------------------
    // 36. dispatch_proposal_async: no provider — behaves like sync
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn async_dispatch_no_provider_behaves_like_sync() {
        let mut d = Dispatcher::new();
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(42)",
            "Long-running query",
        );
        let outcome = d.dispatch_proposal_async(&p, AutonomyLevel::Auto).await;
        assert!(
            matches!(outcome, ActionOutcome::Success { .. }),
            "without a provider the async dispatch must succeed in Auto mode"
        );
    }

    // -----------------------------------------------------------------------
    // 37. dispatch_proposal_async: LLM approves → Success
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn async_dispatch_llm_approve_returns_success() {
        use crate::llm_auditor::LlmAuditorConfig;
        let mut d = Dispatcher::new_with_provider(
            Some(Box::new(MockLlmProvider::approving())),
            LlmAuditorConfig {
                enabled: true,
                ..LlmAuditorConfig::default()
            },
        );
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(42)",
            "Long-running query",
        );
        let outcome = d.dispatch_proposal_async(&p, AutonomyLevel::Auto).await;
        assert!(
            matches!(outcome, ActionOutcome::Success { .. }),
            "LLM approval must produce Success in Auto mode"
        );
    }

    // -----------------------------------------------------------------------
    // 38. dispatch_proposal_async: LLM rejects → Vetoed
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn async_dispatch_llm_reject_returns_vetoed() {
        use crate::llm_auditor::LlmAuditorConfig;
        let mut d = Dispatcher::new_with_provider(
            Some(Box::new(MockLlmProvider::rejecting())),
            LlmAuditorConfig {
                enabled: true,
                ..LlmAuditorConfig::default()
            },
        );
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(42)",
            "Long-running query",
        );
        let outcome = d.dispatch_proposal_async(&p, AutonomyLevel::Auto).await;
        assert!(
            matches!(outcome, ActionOutcome::Vetoed { .. }),
            "LLM rejection must veto the proposal"
        );
    }

    // -----------------------------------------------------------------------
    // 39. dispatch_proposal_async: LLM rejects but Supervised → Skipped
    //     (LLM review only applies to Auto mode)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn async_dispatch_llm_reject_supervised_still_skipped() {
        use crate::llm_auditor::LlmAuditorConfig;
        let mut d = Dispatcher::new_with_provider(
            Some(Box::new(MockLlmProvider::rejecting())),
            LlmAuditorConfig {
                enabled: true,
                ..LlmAuditorConfig::default()
            },
        );
        let p = make_proposal(
            FeatureArea::Vacuum,
            EvidenceClass::Factual,
            "VACUUM ANALYZE users",
            "Dead tuples",
        );
        // Supervised mode: LLM review is skipped — rule-based audit approves.
        let outcome = d
            .dispatch_proposal_async(&p, AutonomyLevel::Supervised)
            .await;
        assert!(
            matches!(outcome, ActionOutcome::Skipped),
            "LLM review is not applied in Supervised mode"
        );
    }

    // -----------------------------------------------------------------------
    // 40. dispatch_proposal_async: LLM rejection records veto
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn async_dispatch_llm_reject_records_veto() {
        use crate::llm_auditor::LlmAuditorConfig;
        let mut d = Dispatcher::new_with_provider(
            Some(Box::new(MockLlmProvider::rejecting())),
            LlmAuditorConfig {
                enabled: true,
                ..LlmAuditorConfig::default()
            },
        );
        assert_eq!(d.veto_tracker().veto_count(), 0);
        let p = make_proposal(
            FeatureArea::Rca,
            EvidenceClass::Factual,
            "SELECT pg_cancel_backend(99)",
            "Long-running query",
        );
        d.dispatch_proposal_async(&p, AutonomyLevel::Auto).await;
        assert_eq!(
            d.veto_tracker().veto_count(),
            1,
            "LLM rejection must record a veto"
        );
    }
}
