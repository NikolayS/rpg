//! GitHub Issues connector (Phase 4).
//!
//! Integrates with the GitHub REST API to fetch open issues as alerts
//! and create/update issues in a GitHub repository.

#![allow(dead_code)] // Phase 4 infrastructure — consumers arrive later

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
// GitHubConnector
// ---------------------------------------------------------------------------

/// Connector for the GitHub Issues API.
///
/// Supports creating and updating issues, and fetching open issues as
/// alerts. Does not provide metric data.
pub struct GitHubConnector {
    token: String,
    owner: String,
    repo: String,
    base_url: String,
    client: reqwest::Client,
}

impl GitHubConnector {
    /// Create a new connector for the given repository.
    ///
    /// Uses `https://api.github.com` as the default base URL.
    pub fn new(token: String, owner: String, repo: String) -> Self {
        Self {
            token,
            owner,
            repo,
            base_url: "https://api.github.com".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Override the base URL (useful for GitHub Enterprise instances).
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    /// Build a `reqwest::RequestBuilder` with auth and required headers set.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "rpg")
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
// Wire types — minimal shapes expected from the GitHub API
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ApiIssueCreated {
    number: u64,
}

// ---------------------------------------------------------------------------
// Connector impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for GitHubConnector {
    fn id(&self) -> &'static str {
        "github"
    }

    fn name(&self) -> &'static str {
        "GitHub Issues"
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
            requests_per_minute: Some(30),
            max_concurrent: 2,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Ping `GET {base_url}/user` to verify the token is valid.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let start = std::time::Instant::now();
        let resp = self
            .request(reqwest::Method::GET, "/user")
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

    /// GitHub Issues focuses on issues, not time-series metrics.
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
    /// `GET {base_url}/repos/{owner}/{repo}/issues?state=open`.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let path = format!("/repos/{}/{}/issues", self.owner, self.repo);
        let resp = self
            .request(reqwest::Method::GET, &path)
            .query(&[("state", "open")])
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
                let label_names: Vec<String> = issue.labels.into_iter().map(|l| l.name).collect();
                let severity = severity_from_labels(&label_names);
                Alert {
                    id: issue.number.to_string(),
                    title: issue.title,
                    severity,
                    status: AlertStatus::Active,
                    source: connector_id.clone(),
                    database: Some(database.clone()),
                    created_at: SystemTime::now(),
                    url: issue.html_url,
                }
            })
            .collect();

        Ok(alerts)
    }

    /// Create an issue via
    /// `POST {base_url}/repos/{owner}/{repo}/issues`.
    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        let body = serde_json::json!({
            "title": issue.title,
            "body": issue.body,
            "labels": issue.labels,
            "assignees": issue.assignees,
        });

        let path = format!("/repos/{}/{}/issues", self.owner, self.repo);
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

        Ok(created.number.to_string())
    }

    /// Update an existing issue via
    /// `PATCH {base_url}/repos/{owner}/{repo}/issues/{number}`.
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
                "body".to_string(),
                serde_json::Value::String(description.clone()),
            );
        }
        if let Some(ref status) = update.status {
            // GitHub uses "state" with values "open" / "closed".
            let state = match status.as_str() {
                "closed" | "close" => "closed",
                _ => "open",
            };
            body.insert(
                "state".to_string(),
                serde_json::Value::String(state.to_string()),
            );
        }
        if let Some(ref labels) = update.labels {
            body.insert(
                "labels".to_string(),
                serde_json::Value::Array(
                    labels
                        .iter()
                        .map(|l| serde_json::Value::String(l.clone()))
                        .collect(),
                ),
            );
        }

        let path = format!("/repos/{}/{}/issues/{id}", self.owner, self.repo);
        let resp = self
            .request(reqwest::Method::PATCH, &path)
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

/// Derive severity from GitHub issue labels.
///
/// Checks for conventional label names: contains "critical", "info"
/// (case-insensitive). Falls back to `Warning` when no matching label
/// is found.
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
        let c = GitHubConnector::new("tok".to_string(), "acme".to_string(), "myrepo".to_string());
        assert_eq!(c.token, "tok");
        assert_eq!(c.owner, "acme");
        assert_eq!(c.repo, "myrepo");
        assert_eq!(c.base_url, "https://api.github.com");
    }

    #[test]
    fn with_base_url_overrides_default() {
        let c = GitHubConnector::new("tok".to_string(), "acme".to_string(), "myrepo".to_string())
            .with_base_url("https://github.example.com/api/v3".to_string());
        assert_eq!(c.base_url, "https://github.example.com/api/v3");
    }

    #[test]
    fn builder_is_chainable() {
        let c = GitHubConnector::new("tok".to_string(), "acme".to_string(), "myrepo".to_string())
            .with_base_url("https://ghe.internal".to_string());
        assert_eq!(c.owner, "acme");
        assert_eq!(c.repo, "myrepo");
        assert_eq!(c.base_url, "https://ghe.internal");
    }

    // ------------------------------------------------------------------
    // Identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_github() {
        let c = GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string());
        assert_eq!(c.id(), "github");
    }

    #[test]
    fn name_is_github_issues() {
        let c = GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string());
        assert_eq!(c.name(), "GitHub Issues");
    }

    // ------------------------------------------------------------------
    // Capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_issue_support() {
        let caps =
            GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string()).capabilities();
        assert!(caps.can_create_issues, "must support creating issues");
        assert!(caps.can_update_issues, "must support updating issues");
        assert!(caps.can_fetch_alerts, "must support fetching alerts");
    }

    #[test]
    fn capabilities_no_metrics() {
        let caps =
            GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string()).capabilities();
        assert!(!caps.can_fetch_metrics, "should not report metric support");
    }

    #[test]
    fn capabilities_no_webhooks() {
        let caps =
            GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string()).capabilities();
        assert!(!caps.can_receive_webhooks);
    }

    // ------------------------------------------------------------------
    // Rate limit config
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl = GitHubConnector::new("t".to_string(), "o".to_string(), "r".to_string())
            .rate_limit_config();
        assert!(
            (rl.requests_per_second - 0.5).abs() < f64::EPSILON,
            "expected 0.5 rps"
        );
        assert_eq!(rl.requests_per_minute, Some(30));
        assert_eq!(rl.max_concurrent, 2);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // severity_from_labels
    // ------------------------------------------------------------------

    #[test]
    fn severity_critical_label() {
        let labels = vec!["severity:critical".to_string()];
        assert!(matches!(severity_from_labels(&labels), Severity::Critical));
    }

    #[test]
    fn severity_info_label() {
        let labels = vec!["severity:info".to_string()];
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
        let err = GitHubConnector::api_error(status, "unauthorized".to_string());
        assert!(matches!(err, ConnectorError::AuthError(_)));
    }

    #[test]
    fn api_error_403_is_auth_error() {
        let status = reqwest::StatusCode::FORBIDDEN;
        let err = GitHubConnector::api_error(status, "forbidden".to_string());
        assert!(matches!(err, ConnectorError::AuthError(_)));
    }

    #[test]
    fn api_error_429_is_rate_limited() {
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;
        let err = GitHubConnector::api_error(status, "rate limited".to_string());
        assert!(matches!(err, ConnectorError::RateLimited { .. }));
    }

    #[test]
    fn api_error_500_is_api_error() {
        let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
        let err = GitHubConnector::api_error(status, "server error".to_string());
        assert!(matches!(err, ConnectorError::ApiError { status: 500, .. }));
    }
}
