//! `PostgresAI` Issues connector (Phase 4).
//!
//! Integrates with the postgres.ai API to fetch open issues as alerts
//! and create/update issues in the `PostgresAI` tracker.

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    Alert, AlertStatus, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError,
    ConnectorHealth, ConnectorId, DatabaseId, IssueId, IssueRequest, IssueUpdate, Metric,
    RateLimitConfig, TimeWindow,
};
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// PostgresAIConnector
// ---------------------------------------------------------------------------

/// Connector for the postgres.ai Issues API.
///
/// Supports creating and updating issues, and fetching open issues as
/// alerts. Does not provide metric data.
#[allow(dead_code)]
pub struct PostgresAIConnector {
    api_key: String,
    org_id: Option<String>,
    project_id: Option<String>,
    base_url: String,
    client: reqwest::Client,
}

impl PostgresAIConnector {
    /// Create a new connector with the given API key.
    ///
    /// Uses `https://postgres.ai/api` as the default base URL.
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            org_id: None,
            project_id: None,
            base_url: "https://postgres.ai/api".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Set the organisation ID scope for API requests.
    #[allow(dead_code)]
    pub fn with_org(mut self, org_id: String) -> Self {
        self.org_id = Some(org_id);
        self
    }

    /// Set the project ID scope for API requests.
    #[allow(dead_code)]
    pub fn with_project(mut self, project_id: String) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Override the base URL (useful for testing against staging).
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
            .header("Authorization", format!("Bearer {}", self.api_key))
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
// Wire types — minimal shapes expected from the postgres.ai API
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ApiIssue {
    id: String,
    title: String,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    database_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ApiIssueList {
    #[serde(default)]
    issues: Vec<ApiIssue>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ApiIssueCreated {
    id: String,
}

// ---------------------------------------------------------------------------
// Connector impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for PostgresAIConnector {
    fn id(&self) -> &'static str {
        "postgresai"
    }

    fn name(&self) -> &'static str {
        "PostgresAI"
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
            requests_per_second: 1.0,
            requests_per_minute: None,
            max_concurrent: 2,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Ping `GET {base_url}/health` and report connectivity.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let start = std::time::Instant::now();
        let resp = self
            .request(reqwest::Method::GET, "/health")
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

    /// `PostgresAI` focuses on issues, not time-series metrics.
    ///
    /// Always returns an empty vec.
    async fn fetch_metrics(
        &self,
        _database: &DatabaseId,
        _window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        Ok(vec![])
    }

    /// Fetch open issues as alerts via `GET {base_url}/issues?status=open`.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let resp = self
            .request(reqwest::Method::GET, "/issues")
            .query(&[("status", "open")])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::api_error(status, body));
        }

        let list: ApiIssueList = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Other(format!("failed to parse issues: {e}")))?;

        let connector_id: ConnectorId = self.id().to_string();
        let alerts = list
            .issues
            .into_iter()
            .map(|issue| {
                let severity = parse_severity(issue.severity.as_deref());
                // Filter by database if the issue carries a database_id.
                // Issues without a database_id are returned for all databases.
                let database_field = issue
                    .database_id
                    .filter(|db| !db.is_empty())
                    .or_else(|| Some(database.clone()));
                Alert {
                    id: issue.id,
                    title: issue.title,
                    severity,
                    status: AlertStatus::Active,
                    source: connector_id.clone(),
                    database: database_field,
                    created_at: SystemTime::now()
                        .checked_sub(Duration::from_secs(0))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    url: issue.url,
                }
            })
            .collect();

        Ok(alerts)
    }

    /// Create an issue via `POST {base_url}/issues`.
    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        let body = serde_json::json!({
            "title": issue.title,
            "body": issue.body,
            "labels": issue.labels,
            "assignees": issue.assignees,
            "metadata": issue.metadata,
        });

        let resp = self
            .request(reqwest::Method::POST, "/issues")
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

        Ok(created.id)
    }

    /// Update an existing issue via `PATCH {base_url}/issues/{id}`.
    async fn update_issue(&self, id: &IssueId, update: &IssueUpdate) -> Result<(), ConnectorError> {
        let mut body = serde_json::Map::new();
        if let Some(ref title) = update.title {
            body.insert(
                "title".to_string(),
                serde_json::Value::String(title.clone()),
            );
        }
        if let Some(ref b) = update.body {
            body.insert("body".to_string(), serde_json::Value::String(b.clone()));
        }
        if let Some(ref status) = update.status {
            body.insert(
                "status".to_string(),
                serde_json::Value::String(status.clone()),
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

        let path = format!("/issues/{id}");
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

#[allow(dead_code)]
fn parse_severity(s: Option<&str>) -> Severity {
    match s {
        Some("critical") => Severity::Critical,
        Some("info") => Severity::Info,
        _ => Severity::Warning,
    }
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
        let c = PostgresAIConnector::new("key123".to_string());
        assert_eq!(c.api_key, "key123");
        assert_eq!(c.base_url, "https://postgres.ai/api");
        assert!(c.org_id.is_none());
        assert!(c.project_id.is_none());
    }

    #[test]
    fn with_org_sets_org_id() {
        let c = PostgresAIConnector::new("k".to_string()).with_org("org-42".to_string());
        assert_eq!(c.org_id.as_deref(), Some("org-42"));
    }

    #[test]
    fn with_project_sets_project_id() {
        let c = PostgresAIConnector::new("k".to_string()).with_project("proj-7".to_string());
        assert_eq!(c.project_id.as_deref(), Some("proj-7"));
    }

    #[test]
    fn with_base_url_overrides_default() {
        let c = PostgresAIConnector::new("k".to_string())
            .with_base_url("http://localhost:8080".to_string());
        assert_eq!(c.base_url, "http://localhost:8080");
    }

    #[test]
    fn builder_is_chainable() {
        let c = PostgresAIConnector::new("key".to_string())
            .with_org("org-1".to_string())
            .with_project("proj-1".to_string())
            .with_base_url("https://staging.postgres.ai/api".to_string());
        assert_eq!(c.org_id.as_deref(), Some("org-1"));
        assert_eq!(c.project_id.as_deref(), Some("proj-1"));
        assert_eq!(c.base_url, "https://staging.postgres.ai/api");
    }

    // ------------------------------------------------------------------
    // Identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_postgresai() {
        let c = PostgresAIConnector::new("k".to_string());
        assert_eq!(c.id(), "postgresai");
    }

    #[test]
    fn name_is_postgresai_display() {
        let c = PostgresAIConnector::new("k".to_string());
        assert_eq!(c.name(), "PostgresAI");
    }

    // ------------------------------------------------------------------
    // Capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_issue_support() {
        let caps = PostgresAIConnector::new("k".to_string()).capabilities();
        assert!(caps.can_create_issues, "must support creating issues");
        assert!(caps.can_update_issues, "must support updating issues");
        assert!(caps.can_fetch_alerts, "must support fetching alerts");
    }

    #[test]
    fn capabilities_no_metrics() {
        let caps = PostgresAIConnector::new("k".to_string()).capabilities();
        assert!(!caps.can_fetch_metrics, "should not report metric support");
    }

    #[test]
    fn capabilities_no_webhooks() {
        let caps = PostgresAIConnector::new("k".to_string()).capabilities();
        assert!(!caps.can_receive_webhooks);
    }

    // ------------------------------------------------------------------
    // Rate limit config
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl = PostgresAIConnector::new("k".to_string()).rate_limit_config();
        assert!(
            (rl.requests_per_second - 1.0).abs() < f64::EPSILON,
            "expected 1.0 rps"
        );
        assert_eq!(rl.max_concurrent, 2);
        assert!(rl.respect_retry_after);
    }
}
