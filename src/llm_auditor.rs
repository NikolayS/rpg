//! LLM adversarial review for Auto-mode governance actions.
//!
//! Provides a secondary safety layer that sends high-risk action proposals to
//! an LLM for adversarial review before execution.  The LLM acts as a
//! skeptical auditor looking for reasons to reject rather than approve.
//!
//! This module is a stub: the `review_proposal` function returns a
//! pass-through approval until an LLM provider is wired in (Phase 3).

#![allow(dead_code)]

use serde::Deserialize;

use crate::governance::ActionProposal;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for the LLM adversarial auditor.
///
/// ```toml
/// [governance.llm_auditor]
/// enabled = true
/// timeout_ms = 5000
/// min_severity = "critical"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmAuditorConfig {
    /// Whether LLM adversarial review is active.  Default: `false`.
    pub enabled: bool,
    /// Maximum milliseconds to wait for the LLM response.  Default: `5000`.
    pub timeout_ms: u64,
    /// Minimum severity level that triggers an LLM review.
    ///
    /// Accepted values (case-insensitive): `"info"`, `"warning"`,
    /// `"critical"`.  Default: `"critical"`.
    pub min_severity: String,
}

impl Default for LlmAuditorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_ms: 5000,
            min_severity: "critical".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// Review result
// ---------------------------------------------------------------------------

/// The structured verdict returned by the LLM adversarial auditor.
#[derive(Debug, Clone)]
pub struct LlmAuditReview {
    /// `true` if the LLM considers the action safe to proceed.
    pub approved: bool,
    /// Zero or more specific concerns identified by the LLM.
    pub concerns: Vec<String>,
    /// A brief free-text recommendation from the LLM.
    pub recommendation: String,
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

/// Build the adversarial review prompt for a given proposal.
///
/// The prompt instructs the LLM to act as a skeptical `PostgreSQL` safety
/// auditor and respond with a JSON object.
pub fn build_adversarial_prompt(proposal: &ActionProposal) -> String {
    format!(
        "You are a PostgreSQL safety auditor. Review this proposed autonomous \
         action and identify any risks:\n\n\
         Feature: {feature}\n\
         Action: {action}\n\
         Finding: {finding}\n\
         Severity: {severity:?}\n\
         Evidence: {evidence:?}\n\n\
         List any concerns about:\n\
         1. Could this action cause data loss?\n\
         2. Could this cause downtime or performance degradation?\n\
         3. Are there edge cases where this action is inappropriate?\n\
         4. Is the evidence sufficient for autonomous execution?\n\n\
         Respond with JSON: \
         {{\"approved\": true/false, \"concerns\": [...], \
         \"recommendation\": \"...\"}}",
        feature = proposal.feature.label(),
        action = proposal.proposed_action,
        finding = proposal.finding,
        severity = proposal.severity,
        evidence = proposal.evidence_class,
    )
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Internal serde target for the LLM JSON response.
#[derive(Debug, Deserialize)]
struct RawReview {
    approved: bool,
    #[serde(default)]
    concerns: Vec<String>,
    #[serde(default)]
    recommendation: String,
}

/// Parse the LLM's response text into an [`LlmAuditReview`].
///
/// Tries JSON first.  If that fails, falls back to a heuristic: if the
/// response text contains the word "reject" or "unsafe" (case-insensitive)
/// the proposal is not approved.
///
/// Always succeeds — returns `Ok` in all cases so callers can use `?`
/// uniformly when a real error path is added later.
#[allow(clippy::unnecessary_wraps)]
pub fn parse_review_response(response: &str) -> Result<LlmAuditReview, String> {
    // Attempt JSON parse — try to find the first `{` in case the LLM
    // prefixed its answer with prose.
    let json_start = response.find('{');
    let json_end = response.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end >= start {
            let candidate = &response[start..=end];
            if let Ok(raw) = serde_json::from_str::<RawReview>(candidate) {
                return Ok(LlmAuditReview {
                    approved: raw.approved,
                    concerns: raw.concerns,
                    recommendation: raw.recommendation,
                });
            }
        }
    }

    // Heuristic fallback.
    let lower = response.to_lowercase();
    let approved = !lower.contains("reject") && !lower.contains("unsafe");
    Ok(LlmAuditReview {
        approved,
        concerns: vec![],
        recommendation: format!(
            "Non-JSON response from LLM — heuristic decision: {}",
            if approved { "approved" } else { "rejected" }
        ),
    })
}

// ---------------------------------------------------------------------------
// Review function (stub)
// ---------------------------------------------------------------------------

/// Submit a proposal for adversarial LLM review.
///
/// This is currently a stub that returns an unconditional approval with a
/// note that no LLM provider is configured.  The actual provider call will
/// be wired in Phase 3 when the AI subsystem is available to the governance
/// framework.
// `async` is intentional: the real implementation will await an LLM call.
#[allow(clippy::unused_async)]
pub async fn review_proposal(
    _proposal: &ActionProposal,
    _config: &LlmAuditorConfig,
) -> Result<LlmAuditReview, String> {
    Ok(LlmAuditReview {
        approved: true,
        concerns: vec![],
        recommendation: "No LLM provider configured".to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::governance::{ActionProposal, EvidenceClass, FeatureArea, Severity};

    fn sample_proposal() -> ActionProposal {
        ActionProposal {
            feature: FeatureArea::Vacuum,
            severity: Severity::Critical,
            evidence_class: EvidenceClass::Factual,
            finding: "Dead tuple ratio exceeds 50%".to_owned(),
            proposed_action: "VACUUM ANALYZE public.orders".to_owned(),
            expected_outcome: "Dead tuples reclaimed, autovacuum reset".to_owned(),
            risk: "Low — VACUUM does not lock the table".to_owned(),
            created_at: SystemTime::now(),
        }
    }

    // --- LlmAuditorConfig defaults ---

    #[test]
    fn config_default_enabled_is_false() {
        let cfg = LlmAuditorConfig::default();
        assert!(!cfg.enabled);
    }

    #[test]
    fn config_default_timeout_ms_is_5000() {
        let cfg = LlmAuditorConfig::default();
        assert_eq!(cfg.timeout_ms, 5000);
    }

    #[test]
    fn config_default_min_severity_is_critical() {
        let cfg = LlmAuditorConfig::default();
        assert_eq!(cfg.min_severity, "critical");
    }

    #[test]
    fn config_deserialize_from_toml_overrides_defaults() {
        let raw = r#"
            enabled = true
            timeout_ms = 3000
            min_severity = "warning"
        "#;
        let cfg: LlmAuditorConfig = toml::from_str(raw).expect("valid TOML");
        assert!(cfg.enabled);
        assert_eq!(cfg.timeout_ms, 3000);
        assert_eq!(cfg.min_severity, "warning");
    }

    // --- build_adversarial_prompt ---

    #[test]
    fn prompt_contains_feature_label() {
        let p = sample_proposal();
        let prompt = build_adversarial_prompt(&p);
        assert!(
            prompt.contains("vacuum"),
            "expected feature label in prompt"
        );
    }

    #[test]
    fn prompt_contains_proposed_action() {
        let p = sample_proposal();
        let prompt = build_adversarial_prompt(&p);
        assert!(prompt.contains("VACUUM ANALYZE public.orders"));
    }

    #[test]
    fn prompt_contains_finding() {
        let p = sample_proposal();
        let prompt = build_adversarial_prompt(&p);
        assert!(prompt.contains("Dead tuple ratio exceeds 50%"));
    }

    #[test]
    fn prompt_contains_json_instruction() {
        let p = sample_proposal();
        let prompt = build_adversarial_prompt(&p);
        assert!(
            prompt.contains("\"approved\""),
            "prompt should include JSON schema hint"
        );
    }

    // --- parse_review_response ---

    #[test]
    fn parse_valid_approved_json() {
        let resp = r#"{"approved": true, "concerns": [], "recommendation": "Safe to proceed"}"#;
        let review = parse_review_response(resp).expect("parse ok");
        assert!(review.approved);
        assert!(review.concerns.is_empty());
        assert_eq!(review.recommendation, "Safe to proceed");
    }

    #[test]
    fn parse_valid_rejected_json_with_concerns() {
        let resp = r#"{
            "approved": false,
            "concerns": ["Could lock the table", "Timing is risky"],
            "recommendation": "Defer to maintenance window"
        }"#;
        let review = parse_review_response(resp).expect("parse ok");
        assert!(!review.approved);
        assert_eq!(review.concerns.len(), 2);
        assert!(review.recommendation.contains("maintenance"));
    }

    #[test]
    fn parse_json_embedded_in_prose() {
        let resp = "Sure, here is my review:\n{\"approved\": true, \"concerns\": [], \
             \"recommendation\": \"Looks good\"}";
        let review = parse_review_response(resp).expect("parse ok");
        assert!(review.approved);
    }

    #[test]
    fn parse_heuristic_fallback_reject_keyword() {
        let resp = "I would reject this because it is unsafe.";
        let review = parse_review_response(resp).expect("parse ok");
        assert!(
            !review.approved,
            "heuristic should reject when 'reject' keyword is present"
        );
    }

    #[test]
    fn parse_heuristic_fallback_unsafe_keyword() {
        let resp = "This operation is unsafe for production.";
        let review = parse_review_response(resp).expect("parse ok");
        assert!(!review.approved);
    }

    #[test]
    fn parse_heuristic_fallback_approve_neutral_text() {
        let resp = "The proposed action appears reasonable.";
        let review = parse_review_response(resp).expect("parse ok");
        assert!(
            review.approved,
            "neutral text with no reject/unsafe keywords should approve"
        );
    }

    // --- review_proposal stub ---

    #[tokio::test]
    async fn review_proposal_stub_returns_approved() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig::default();
        let review = review_proposal(&p, &cfg).await.expect("no error");
        assert!(review.approved);
        assert!(review.concerns.is_empty());
        assert!(review.recommendation.contains("No LLM provider"));
    }
}
