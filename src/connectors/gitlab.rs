//! GitLab Issues connector (Phase 4).
//!
//! Integrates with the GitLab API to fetch open issues as alerts
//! and create/update issues in a GitLab project.

use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    Alert, AlertStatus, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError,
    ConnectorHealth, ConnectorId, DatabaseId, IssueId, IssueRequest, IssueUpdate, Metric,
    RateLimitConfig, TimeWindow,
};
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// GitLabConnector
// ---------------------------------------------------------------------------

/// Connector for the GitLab Issues API.
///
/// Supports creating and updating issues, and fetching open issues as
/// alerts. Does not provide metric data.
#[allow(dead_code)]
pub struct GitLabConnector {
    token: String,
    project_id: String,
    base_url: String,
    client: reqwest::Client,
}

impl GitLabConnector {
    /// Create a new connector for the given project.
    ///
    /// Uses `https://gitlab.com` as the default base URL.
    pub fn new(token: String, project_id: String) -> Self {
        Self {
            token,
            project_id,
            base_url: "https://gitlab.com".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Override the base URL (useful for self-hosted GitLab instances).
    #[allow(dead_code)]
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    /// Build a `reqwest::RequestBuilder` with the auth header already set.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.client
            .request(method, url)
            .header("PRIVATE-TOKEN", &self.token)
    }

    /// Map an HTTP status code + body into a `ConnectorError`.
    fn api_error(status: reqwest::StatusCode, message: String) -> ConnectorError {
        match status.as_u16() {
            401 | 403 => ConnectorError::AuthError(message),
            429 => ConnectorError::RateLimited {
                retry_after_ms: None,
            },
            _ => ConnectorError::ApiError {
                status: status.as_u16(),
                message,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Wire types — minimal shapes expected from the GitLab API
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ApiIssue {
    iid: u64,
    title: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    web_url: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ApiIssueCreated {
    iid: u64,
}

// ---------------------------------------------------------------------------
// Connector impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for GitLabConnector {
    fn id(&self) -> &'static str {
        "gitlab"
    }

    fn name(&self) -> &'static str {
        "GitLab Issues"
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            can_fetch_metrics: false,
            can_fetch_alerts: true,
            can_create_issues: true,
            can_update_issues: true,
            can_receive_webhooks: false,
            supports_pagination: false,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 0.5,
            requests_per_minute: None,
            max_concurrent: 2,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Ping `GET {base_url}/api/v4/user` to verify the token is valid.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let start = std::time::Instant::now();
        let resp = self
            .request(reqwest::Method::GET, "/api/v4/user")
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        if resp.status().is_success() {
            Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(latency_ms),
            })
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(Self::api_error(status, body))
        }
    }

    /// GitLab focuses on issues, not time-series metrics.
    ///
    /// Always returns an empty vec.
    async fn fetch_metrics(
        &self,
        _database: &DatabaseId,
        _window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        Ok(vec![])
    }

    /// Fetch open issues as alerts via
    /// `GET {base_url}/api/v4/projects/{project_id}/issues?state=opened`.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let path = format!("/api/v4/projects/{}/issues", self.project_id);
        let resp = self
            .request(reqwest::Method::GET, &path)
            .query(&[("state", "opened")])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::api_error(status, body));
        }

        let issues: Vec<ApiIssue> = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Other(format!("failed to parse issues: {e}")))?;

        let connector_id: ConnectorId = self.id().to_string();
        let alerts = issues
            .into_iter()
            .map(|issue| {
                let severity = severity_from_labels(&issue.labels);
                Alert {
                    id: issue.iid.to_string(),
                    title: issue.title,
                    severity,
                    status: AlertStatus::Active,
                    source: connector_id.clone(),
                    database: Some(database.clone()),
                    created_at: SystemTime::now(),
                    url: issue.web_url,
                }
            })
            .collect();

        Ok(alerts)
    }

    /// Create an issue via
    /// `POST {base_url}/api/v4/projects/{project_id}/issues`.
    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        let body = serde_json::json!({
            "title": issue.title,
            "description": issue.body,
            "labels": issue.labels.join(","),
            "assignee_ids": issue.assignees,
        });

        let path = format!("/api/v4/projects/{}/issues", self.project_id);
        let resp = self
            .request(reqwest::Method::POST, &path)
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            return Err(Self::api_error(status, msg));
        }

        let created: ApiIssueCreated = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Other(format!("failed to parse created issue: {e}")))?;

        Ok(created.iid.to_string())
    }

    /// Update an existing issue via
    /// `PUT {base_url}/api/v4/projects/{project_id}/issues/{iid}`.
    async fn update_issue(&self, id: &IssueId, update: &IssueUpdate) -> Result<(), ConnectorError> {
        let mut body = serde_json::Map::new();
        if let Some(ref title) = update.title {
            body.insert(
                "title".to_string(),
                serde_json::Value::String(title.clone()),
            );
        }
        if let Some(ref description) = update.body {
            body.insert(
                "description".to_string(),
                serde_json::Value::String(description.clone()),
            );
        }
        if let Some(ref status) = update.status {
            // GitLab uses "state_event" with values "close" / "reopen".
            let state_event = match status.as_str() {
                "closed" | "close" => "close",
                _ => "reopen",
            };
            body.insert(
                "state_event".to_string(),
                serde_json::Value::String(state_event.to_string()),
            );
        }
        if let Some(ref labels) = update.labels {
            body.insert(
                "labels".to_string(),
                serde_json::Value::String(labels.join(",")),
            );
        }

        let path = format!("/api/v4/projects/{}/issues/{id}", self.project_id);
        let resp = self
            .request(reqwest::Method::PUT, &path)
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            Err(Self::api_error(status, msg))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derive severity from GitLab issue labels.
///
/// Checks for conventional label prefixes: `severity::critical`,
/// `severity::warning`, `severity::info` (case-insensitive).
/// Falls back to `Warning` when no matching label is found.
#[allow(dead_code)]
fn severity_from_labels(labels: &[String]) -> Severity {
    for label in labels {
        let lower = label.to_lowercase();
        if lower.contains("critical") {
            return Severity::Critical;
        }
        if lower.contains("info") {
            return Severity::Info;
        }
    }
    Severity::Warning
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Constructor and builder
    // ------------------------------------------------------------------

    #[test]
    fn new_sets_defaults() {
        let c = GitLabConnector::new("tok".to_string(), "42".to_string());
        assert_eq!(c.token, "tok");
        assert_eq!(c.project_id, "42");
        assert_eq!(c.base_url, "https://gitlab.com");
    }

    #[test]
    fn with_base_url_overrides_default() {
        let c = GitLabConnector::new("tok".to_string(), "42".to_string())
            .with_base_url("https://gitlab.example.com".to_string());
        assert_eq!(c.base_url, "https://gitlab.example.com");
    }

    #[test]
    fn builder_is_chainable() {
        let c = GitLabConnector::new("tok".to_string(), "123".to_string())
            .with_base_url("https://gl.internal".to_string());
        assert_eq!(c.project_id, "123");
        assert_eq!(c.base_url, "https://gl.internal");
    }

    // ------------------------------------------------------------------
    // Identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_gitlab() {
        let c = GitLabConnector::new("t".to_string(), "1".to_string());
        assert_eq!(c.id(), "gitlab");
    }

    #[test]
    fn name_is_gitlab_issues() {
        let c = GitLabConnector::new("t".to_string(), "1".to_string());
        assert_eq!(c.name(), "GitLab Issues");
    }

    // ------------------------------------------------------------------
    // Capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_issue_support() {
        let caps = GitLabConnector::new("t".to_string(), "1".to_string()).capabilities();
        assert!(caps.can_create_issues, "must support creating issues");
        assert!(caps.can_update_issues, "must support updating issues");
        assert!(caps.can_fetch_alerts, "must support fetching alerts");
    }

    #[test]
    fn capabilities_no_metrics() {
        let caps = GitLabConnector::new("t".to_string(), "1".to_string()).capabilities();
        assert!(!caps.can_fetch_metrics, "should not report metric support");
    }

    #[test]
    fn capabilities_no_webhooks() {
        let caps = GitLabConnector::new("t".to_string(), "1".to_string()).capabilities();
        assert!(!caps.can_receive_webhooks);
    }

    // ------------------------------------------------------------------
    // Rate limit config
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl = GitLabConnector::new("t".to_string(), "1".to_string()).rate_limit_config();
        assert!(
            (rl.requests_per_second - 0.5).abs() < f64::EPSILON,
            "expected 0.5 rps"
        );
        assert_eq!(rl.max_concurrent, 2);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // severity_from_labels
    // ------------------------------------------------------------------

    #[test]
    fn severity_critical_label() {
        let labels = vec!["severity::critical".to_string()];
        assert!(matches!(severity_from_labels(&labels), Severity::Critical));
    }

    #[test]
    fn severity_info_label() {
        let labels = vec!["severity::info".to_string()];
        assert!(matches!(severity_from_labels(&labels), Severity::Info));
    }

    #[test]
    fn severity_default_warning() {
        let labels = vec!["bug".to_string(), "backend".to_string()];
        assert!(matches!(severity_from_labels(&labels), Severity::Warning));
    }

    #[test]
    fn severity_empty_labels() {
        assert!(matches!(severity_from_labels(&[]), Severity::Warning));
    }

    #[test]
    fn severity_critical_case_insensitive() {
        let labels = vec!["CRITICAL".to_string()];
        assert!(matches!(severity_from_labels(&labels), Severity::Critical));
    }

    // ------------------------------------------------------------------
    // api_error mapping
    // ------------------------------------------------------------------

    #[test]
    fn api_error_401_is_auth_error() {
        let status = reqwest::StatusCode::UNAUTHORIZED;
        let err = GitLabConnector::api_error(status, "unauthorized".to_string());
        assert!(matches!(err, ConnectorError::AuthError(_)));
    }

    #[test]
    fn api_error_403_is_auth_error() {
        let status = reqwest::StatusCode::FORBIDDEN;
        let err = GitLabConnector::api_error(status, "forbidden".to_string());
        assert!(matches!(err, ConnectorError::AuthError(_)));
    }

    #[test]
    fn api_error_429_is_rate_limited() {
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;
        let err = GitLabConnector::api_error(status, "rate limited".to_string());
        assert!(matches!(err, ConnectorError::RateLimited { .. }));
    }

    #[test]
    fn api_error_500_is_api_error() {
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        let err = GitLabConnector::api_error(status, "server error".to_string());
        assert!(matches!(err, ConnectorError::ApiError { status: 500, .. }));
    }
}
