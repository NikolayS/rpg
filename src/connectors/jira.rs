//! Jira connector (Phase 4).
//!
//! Integrates with the Atlassian Jira REST API v3 to fetch
//! database-related issues as alerts and create/update issues.

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
// JiraConnector
// ---------------------------------------------------------------------------

/// Connector for the Atlassian Jira REST API v3.
///
/// Supports creating and updating issues, and fetching database-related
/// issues as alerts via JQL. Does not provide metric data.
pub struct JiraConnector {
    email: String,
    api_token: String,
    base_url: String,
    client: reqwest::Client,
}

impl JiraConnector {
    /// Create a new connector with the given credentials.
    ///
    /// Uses `https://your-domain.atlassian.net` as the default base URL.
    pub fn new(email: String, api_token: String) -> Self {
        Self {
            email,
            api_token,
            base_url: "https://your-domain.atlassian.net".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Override the base URL (e.g. `https://mycompany.atlassian.net`).
    pub fn with_base_url(mut self, url: String) -> Self {
        self.base_url = url;
        self
    }

    /// Build a `reqwest::RequestBuilder` with Basic Auth already set.
    ///
    /// Jira uses HTTP Basic Auth: `email:api_token` base64-encoded.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.client
            .request(method, url)
            .basic_auth(&self.email, Some(&self.api_token))
    }

    /// Map an HTTP status code and body into a `ConnectorError`.
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
// Wire types — minimal shapes expected from the Jira REST API v3
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct JiraSearchResult {
    #[serde(default)]
    issues: Vec<JiraIssue>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct JiraIssue {
    id: String,
    #[serde(rename = "self")]
    self_url: Option<String>,
    fields: JiraIssueFields,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct JiraIssueFields {
    summary: String,
    #[serde(default)]
    priority: Option<JiraPriority>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct JiraPriority {
    #[serde(default)]
    name: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct JiraCreatedIssue {
    id: String,
}

// ---------------------------------------------------------------------------
// Connector impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for JiraConnector {
    fn id(&self) -> &'static str {
        "jira"
    }

    fn name(&self) -> &'static str {
        "Jira"
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

    /// Ping `GET {base_url}/rest/api/3/myself` to verify connectivity.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let start = std::time::Instant::now();
        let resp = self
            .request(reqwest::Method::GET, "/rest/api/3/myself")
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

    /// Jira is an issue tracker, not a metrics source.
    ///
    /// Always returns an empty vec.
    async fn fetch_metrics(
        &self,
        _database: &DatabaseId,
        _window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        Ok(vec![])
    }

    /// Fetch database-related issues as alerts via JQL search.
    ///
    /// Queries for open issues whose summary or description mentions
    /// common database terms (postgresql, postgres, database, pg_).
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let jql = "statusCategory != Done AND \
             (summary ~ \"postgresql\" OR summary ~ \"postgres\" \
             OR summary ~ \"database\" OR summary ~ \"pg_\")"
            .to_string();

        let resp = self
            .request(reqwest::Method::GET, "/rest/api/3/search")
            .query(&[("jql", jql.as_str()), ("maxResults", "50")])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::api_error(status, body));
        }

        let result: JiraSearchResult = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Other(format!("failed to parse search result: {e}")))?;

        let connector_id: ConnectorId = self.id().to_string();
        let alerts = result
            .issues
            .into_iter()
            .map(|issue| {
                let severity = parse_priority(
                    issue
                        .fields
                        .priority
                        .as_ref()
                        .and_then(|p| p.name.as_deref()),
                );
                Alert {
                    id: issue.id,
                    title: issue.fields.summary,
                    severity,
                    status: AlertStatus::Active,
                    source: connector_id.clone(),
                    database: Some(database.clone()),
                    created_at: SystemTime::now()
                        .checked_sub(Duration::from_secs(0))
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    url: issue.self_url,
                }
            })
            .collect();

        Ok(alerts)
    }

    /// Create an issue via `POST {base_url}/rest/api/3/issue`.
    ///
    /// Expects `metadata` to contain `"project_key"` (e.g. `"OPS"`).
    /// Falls back to `"DEFAULT"` if not provided.
    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        let project_key = issue
            .metadata
            .get("project_key")
            .and_then(|v| v.as_str())
            .unwrap_or("DEFAULT");

        let body = serde_json::json!({
            "fields": {
                "project": { "key": project_key },
                "summary": issue.title,
                "description": {
                    "version": 1,
                    "type": "doc",
                    "content": [
                        {
                            "type": "paragraph",
                            "content": [
                                { "type": "text", "text": issue.body }
                            ]
                        }
                    ]
                },
                "issuetype": { "name": "Task" }
            }
        });

        let resp = self
            .request(reqwest::Method::POST, "/rest/api/3/issue")
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let msg = resp.text().await.unwrap_or_default();
            return Err(Self::api_error(status, msg));
        }

        let created: JiraCreatedIssue = resp
            .json()
            .await
            .map_err(|e| ConnectorError::Other(format!("failed to parse created issue: {e}")))?;

        Ok(created.id)
    }

    /// Update an existing issue via `PUT {base_url}/rest/api/3/issue/{id}`.
    async fn update_issue(&self, id: &IssueId, update: &IssueUpdate) -> Result<(), ConnectorError> {
        let mut fields = serde_json::Map::new();

        if let Some(ref title) = update.title {
            fields.insert(
                "summary".to_string(),
                serde_json::Value::String(title.clone()),
            );
        }

        if let Some(ref body_text) = update.body {
            let description = serde_json::json!({
                "version": 1,
                "type": "doc",
                "content": [
                    {
                        "type": "paragraph",
                        "content": [
                            { "type": "text", "text": body_text }
                        ]
                    }
                ]
            });
            fields.insert("description".to_string(), description);
        }

        let body = serde_json::json!({ "fields": fields });
        let path = format!("/rest/api/3/issue/{id}");

        let resp = self
            .request(reqwest::Method::PUT, &path)
            .json(&body)
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

/// Map Jira priority names to `Severity`.
#[allow(dead_code)]
fn parse_priority(priority: Option<&str>) -> Severity {
    match priority {
        Some("Highest" | "Critical") => Severity::Critical,
        Some("Low" | "Lowest") => Severity::Info,
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
        let c = JiraConnector::new("user@example.com".to_string(), "token123".to_string());
        assert_eq!(c.email, "user@example.com");
        assert_eq!(c.api_token, "token123");
        assert_eq!(c.base_url, "https://your-domain.atlassian.net");
    }

    #[test]
    fn with_base_url_overrides_default() {
        let c = JiraConnector::new("u@example.com".to_string(), "t".to_string())
            .with_base_url("https://mycompany.atlassian.net".to_string());
        assert_eq!(c.base_url, "https://mycompany.atlassian.net");
    }

    // ------------------------------------------------------------------
    // Identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_jira() {
        let c = JiraConnector::new("u@example.com".to_string(), "t".to_string());
        assert_eq!(c.id(), "jira");
    }

    #[test]
    fn name_is_jira() {
        let c = JiraConnector::new("u@example.com".to_string(), "t".to_string());
        assert_eq!(c.name(), "Jira");
    }

    // ------------------------------------------------------------------
    // Capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_issue_support() {
        let caps = JiraConnector::new("u@example.com".to_string(), "t".to_string()).capabilities();
        assert!(caps.can_create_issues, "must support creating issues");
        assert!(caps.can_update_issues, "must support updating issues");
        assert!(caps.can_fetch_alerts, "must support fetching alerts");
    }

    #[test]
    fn capabilities_no_metrics() {
        let caps = JiraConnector::new("u@example.com".to_string(), "t".to_string()).capabilities();
        assert!(!caps.can_fetch_metrics, "should not report metric support");
    }

    #[test]
    fn capabilities_no_webhooks() {
        let caps = JiraConnector::new("u@example.com".to_string(), "t".to_string()).capabilities();
        assert!(!caps.can_receive_webhooks);
    }

    // ------------------------------------------------------------------
    // Rate limit config
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl =
            JiraConnector::new("u@example.com".to_string(), "t".to_string()).rate_limit_config();
        assert!(
            (rl.requests_per_second - 0.5).abs() < f64::EPSILON,
            "expected 0.5 rps"
        );
        assert_eq!(rl.max_concurrent, 2);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // Priority parsing
    // ------------------------------------------------------------------

    #[test]
    fn parse_priority_critical() {
        assert!(matches!(
            parse_priority(Some("Highest")),
            Severity::Critical
        ));
        assert!(matches!(
            parse_priority(Some("Critical")),
            Severity::Critical
        ));
    }

    #[test]
    fn parse_priority_info() {
        assert!(matches!(parse_priority(Some("Low")), Severity::Info));
        assert!(matches!(parse_priority(Some("Lowest")), Severity::Info));
    }

    #[test]
    fn parse_priority_warning_default() {
        assert!(matches!(parse_priority(Some("Medium")), Severity::Warning));
        assert!(matches!(parse_priority(Some("High")), Severity::Warning));
        assert!(matches!(parse_priority(None), Severity::Warning));
    }
}
