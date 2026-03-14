//! LLM adversarial review for Auto-mode governance actions.
//!
//! Provides a secondary safety layer that sends high-risk action proposals to
//! an LLM for adversarial review before execution.  The LLM acts as a
//! skeptical auditor looking for reasons to reject rather than approve.

use std::time::Duration;

use serde::Deserialize;

use crate::ai::{CompletionOptions, LlmProvider, Message, Role};
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
    #[allow(dead_code)]
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
// Review function
// ---------------------------------------------------------------------------

/// Submit a proposal for adversarial LLM review.
///
/// When a provider is supplied and the auditor config has `enabled = true`,
/// the proposal is sent to the LLM using [`build_adversarial_prompt`] and the
/// response is parsed with [`parse_review_response`].  The call is wrapped in
/// a timeout derived from [`LlmAuditorConfig::timeout_ms`].
///
/// Falls back to heuristic approval (no-provider path) in three cases:
/// - `provider` is `None`
/// - `config.enabled` is `false`
/// - The LLM call times out or returns an error
pub async fn review_proposal(
    proposal: &ActionProposal,
    config: &LlmAuditorConfig,
    provider: Option<&dyn LlmProvider>,
) -> Result<LlmAuditReview, String> {
    let Some(provider) = provider else {
        return Ok(LlmAuditReview {
            approved: true,
            concerns: vec![],
            recommendation: "No LLM provider configured".to_owned(),
        });
    };

    if !config.enabled {
        return Ok(LlmAuditReview {
            approved: true,
            concerns: vec![],
            recommendation: "LLM auditor disabled".to_owned(),
        });
    }

    let prompt = build_adversarial_prompt(proposal);
    let messages = vec![Message {
        role: Role::User,
        content: prompt,
    }];
    let options = CompletionOptions {
        model: String::new(),
        max_tokens: 512,
        temperature: 0.0,
    };

    let timeout = Duration::from_millis(config.timeout_ms);
    let call = provider.complete(&messages, &options);

    match tokio::time::timeout(timeout, call).await {
        Ok(Ok(result)) => parse_review_response(&result.content),
        Ok(Err(err)) => {
            // LLM returned an error — fall back to heuristic approval so
            // that a transient provider outage does not block all actions.
            Ok(LlmAuditReview {
                approved: true,
                concerns: vec![],
                recommendation: format!("LLM error (heuristic fallback): {err}"),
            })
        }
        Err(_elapsed) => Ok(LlmAuditReview {
            approved: true,
            concerns: vec![],
            recommendation: format!(
                "LLM timed out after {}ms (heuristic fallback)",
                config.timeout_ms
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::time::SystemTime;

    use super::*;
    use crate::ai::{CompletionResult, LlmProvider};
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

    // -----------------------------------------------------------------------
    // Mock provider that returns a fixed response
    // -----------------------------------------------------------------------

    #[derive(Debug)]
    struct MockProvider {
        response: String,
    }

    impl MockProvider {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
            }
        }
    }

    impl LlmProvider for MockProvider {
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
        ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>>
        {
            let content = self.response.clone();
            Box::pin(async move {
                Ok(CompletionResult {
                    content,
                    input_tokens: 10,
                    output_tokens: 20,
                })
            })
        }
    }

    // -----------------------------------------------------------------------
    // Mock provider that always returns an error
    // -----------------------------------------------------------------------

    #[derive(Debug)]
    struct ErrorProvider;

    impl LlmProvider for ErrorProvider {
        fn name(&self) -> &'static str {
            "error-mock"
        }

        fn default_model(&self) -> &'static str {
            "error-model"
        }

        fn complete(
            &self,
            _messages: &[crate::ai::Message],
            _options: &crate::ai::CompletionOptions,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>>
        {
            Box::pin(async move { Err("simulated provider error".to_owned()) })
        }
    }

    // -----------------------------------------------------------------------
    // Mock provider that simulates a slow response (for timeout testing)
    // -----------------------------------------------------------------------

    #[derive(Debug)]
    struct SlowProvider;

    impl LlmProvider for SlowProvider {
        fn name(&self) -> &'static str {
            "slow-mock"
        }

        fn default_model(&self) -> &'static str {
            "slow-model"
        }

        fn complete(
            &self,
            _messages: &[crate::ai::Message],
            _options: &crate::ai::CompletionOptions,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<CompletionResult, String>> + Send + '_>>
        {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(CompletionResult {
                    content: "late response".to_owned(),
                    input_tokens: 0,
                    output_tokens: 0,
                })
            })
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

    // --- review_proposal: no provider ---

    #[tokio::test]
    async fn review_proposal_no_provider_returns_approved() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig::default();
        let review = review_proposal(&p, &cfg, None).await.expect("no error");
        assert!(review.approved);
        assert!(review.concerns.is_empty());
        assert!(review.recommendation.contains("No LLM provider"));
    }

    // --- review_proposal: disabled config ---

    #[tokio::test]
    async fn review_proposal_disabled_config_returns_approved() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: false,
            ..LlmAuditorConfig::default()
        };
        let provider = MockProvider::new(
            r#"{"approved": false, "concerns": ["risky"], "recommendation": "reject"}"#,
        );
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(
            review.approved,
            "disabled auditor must approve without calling the provider"
        );
        assert!(review.recommendation.contains("disabled"));
    }

    // --- review_proposal: enabled, provider returns approve ---

    #[tokio::test]
    async fn review_proposal_with_provider_approve() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: true,
            ..LlmAuditorConfig::default()
        };
        let provider = MockProvider::new(
            r#"{"approved": true, "concerns": [], "recommendation": "Looks safe"}"#,
        );
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(review.approved);
        assert_eq!(review.recommendation, "Looks safe");
    }

    // --- review_proposal: enabled, provider returns reject ---

    #[tokio::test]
    async fn review_proposal_with_provider_reject() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: true,
            ..LlmAuditorConfig::default()
        };
        let provider = MockProvider::new(
            r#"{"approved": false, "concerns": ["Could cause downtime"], "recommendation": "Defer"}"#,
        );
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(!review.approved);
        assert_eq!(review.concerns.len(), 1);
        assert_eq!(review.recommendation, "Defer");
    }

    // --- review_proposal: provider returns heuristic-parsed prose ---

    #[tokio::test]
    async fn review_proposal_provider_prose_reject_keyword() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: true,
            ..LlmAuditorConfig::default()
        };
        let provider = MockProvider::new("I would reject this action due to risk.");
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(
            !review.approved,
            "heuristic parse must reject when 'reject' keyword present"
        );
    }

    // --- review_proposal: provider error falls back to approval ---

    #[tokio::test]
    async fn review_proposal_provider_error_fallback_approves() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: true,
            ..LlmAuditorConfig::default()
        };
        let provider = ErrorProvider;
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(
            review.approved,
            "provider error must fall back to heuristic approval"
        );
        assert!(review.recommendation.contains("LLM error"));
    }

    // --- review_proposal: provider timeout falls back to approval ---

    #[tokio::test]
    async fn review_proposal_provider_timeout_fallback_approves() {
        let p = sample_proposal();
        let cfg = LlmAuditorConfig {
            enabled: true,
            timeout_ms: 1, // 1ms timeout — SlowProvider sleeps 60s
            ..LlmAuditorConfig::default()
        };
        let provider = SlowProvider;
        let review = review_proposal(&p, &cfg, Some(&provider))
            .await
            .expect("no error");
        assert!(
            review.approved,
            "timeout must fall back to heuristic approval"
        );
        assert!(review.recommendation.contains("timed out"));
    }
}
