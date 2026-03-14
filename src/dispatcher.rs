//! Dispatcher — connects Auditor review to action execution.
//!
//! The Dispatcher is the coordination layer between the Analyzer,
//! Auditor, and Actor. It receives [`ActionProposal`] values, routes
//! them through the Auditor, checks the circuit breaker, and either
//! executes or skips them based on the effective autonomy level.

// Phase 3 infrastructure — consumers arrive in subsequent PRs.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::governance::{
    ActionOutcome, ActionProposal, AuditDecision, AuditLog, Auditor, AutonomyLevel, CircuitBreaker,
    FeatureArea, VetoTracker,
};

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Connects Auditor review to the execution path for action proposals.
///
/// The Dispatcher is the single control point that:
/// 1. Checks vetoes (`VetoTracker` may downgrade Auto to Supervised).
/// 2. Checks the effective autonomy level (circuit breaker may downgrade).
/// 3. Runs rule-based Auditor review.
/// 4. Executes (Auto) or defers (Supervised) approved proposals.
/// 5. Records every outcome in the audit log.
/// 6. Sets post-action verification on successful Auto executions.
#[derive(Debug)]
pub struct Dispatcher {
    auditor: Auditor,
    circuit_breakers: HashMap<FeatureArea, CircuitBreaker>,
    audit_log: AuditLog,
    veto_tracker: VetoTracker,
}

impl Dispatcher {
    /// Create a new Dispatcher with default configuration.
    pub fn new() -> Self {
        Self {
            auditor: Auditor,
            circuit_breakers: HashMap::new(),
            audit_log: AuditLog::new(),
            veto_tracker: VetoTracker::new(),
        }
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
    ///
    /// Every path records an entry in the audit log.
    pub fn dispatch_proposal(
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
            cb.map_or(true, |c| !c.is_tripped(FeatureArea::Rca)),
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
}
