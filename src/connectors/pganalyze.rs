//! pganalyze connector — fetches query statistics and index analysis
//! suggestions from the pganalyze `SaaS` API.

use std::time::SystemTime;

use async_trait::async_trait;

use super::{
    Alert, AlertStatus, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError,
    ConnectorHealth, DatabaseId, Metric, RateLimitConfig, TimeWindow,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_BASE_URL: &str = "https://app.pganalyze.com/api/v2";

// ---------------------------------------------------------------------------
// Struct
// ---------------------------------------------------------------------------

/// Connector for the [pganalyze](https://pganalyze.com) monitoring `SaaS`.
///
/// Fetches query statistics and index analysis suggestions via the
/// pganalyze REST API v2.  Authentication uses a Bearer token supplied
/// as `api_key`.
pub struct PganalyzeConnector {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl PganalyzeConnector {
    /// Create a connector using the default pganalyze API base URL.
    pub fn new(api_key: String) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL.to_string())
    }

    /// Create a connector with a custom base URL (useful for testing or
    /// self-hosted pganalyze installations).
    pub fn with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            base_url,
            client: reqwest::Client::new(),
        }
    }

    /// Build an authenticated GET request for the given path.
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        self.client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Accept", "application/json")
    }
}

// ---------------------------------------------------------------------------
// Connector impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for PganalyzeConnector {
    fn id(&self) -> &'static str {
        "pganalyze"
    }

    fn name(&self) -> &'static str {
        "pganalyze"
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            can_fetch_metrics: true,
            can_fetch_alerts: true,
            can_create_issues: false,
            can_update_issues: false,
            can_receive_webhooks: false,
            supports_pagination: false,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 0.17,
            requests_per_minute: Some(10),
            max_concurrent: 1,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Validate the API key by calling the API root endpoint.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let start = std::time::Instant::now();
        let response = self
            .get("/")
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ConnectorError::AuthError(
                "invalid or missing API key".to_string(),
            ));
        }

        if !status.is_success() {
            return Err(ConnectorError::ApiError {
                status: status.as_u16(),
                message: status.canonical_reason().unwrap_or("unknown").to_string(),
            });
        }

        Ok(ConnectorHealth {
            connected: true,
            message: None,
            latency_ms: Some(latency_ms),
        })
    }

    /// Fetch query statistics for `database` over `window`.
    ///
    /// Calls `GET /query_statistics` with `database_id`, `start`, and
    /// `end` query parameters.  Each returned statistic is mapped to a
    /// [`Metric`] with `name = "query_statistics"`.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let start_secs = window
            .start
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let end_secs = window
            .end
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let response = self
            .get("/query_statistics")
            .query(&[
                ("database_id", database.as_str()),
                ("start", &start_secs.to_string()),
                ("end", &end_secs.to_string()),
            ])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ConnectorError::AuthError(
                "invalid or missing API key".to_string(),
            ));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|secs| secs * 1000);
            return Err(ConnectorError::RateLimited { retry_after_ms });
        }
        if !status.is_success() {
            return Err(ConnectorError::ApiError {
                status: status.as_u16(),
                message: status.canonical_reason().unwrap_or("unknown").to_string(),
            });
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let mut metrics = Vec::new();
        if let Some(stats) = body.as_array() {
            for stat in stats {
                let calls = stat
                    .get("calls")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let mut tags = std::collections::HashMap::new();
                tags.insert("database_id".to_string(), database.clone());
                if let Some(query_id) = stat.get("query_id").and_then(serde_json::Value::as_str) {
                    tags.insert("query_id".to_string(), query_id.to_string());
                }
                metrics.push(Metric {
                    name: "query_statistics".to_string(),
                    value: calls,
                    unit: Some("calls".to_string()),
                    timestamp: window.end,
                    tags,
                    source: self.id().to_string(),
                });
            }
        }

        Ok(metrics)
    }

    /// Fetch index analysis suggestions and alerts for `database`.
    ///
    /// Calls `GET /index_analysis` with a `database_id` query parameter.
    /// Each returned issue is mapped to an [`Alert`].
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let response = self
            .get("/index_analysis")
            .query(&[("database_id", database.as_str())])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ConnectorError::AuthError(
                "invalid or missing API key".to_string(),
            ));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after_ms = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|secs| secs * 1000);
            return Err(ConnectorError::RateLimited { retry_after_ms });
        }
        if !status.is_success() {
            return Err(ConnectorError::ApiError {
                status: status.as_u16(),
                message: status.canonical_reason().unwrap_or("unknown").to_string(),
            });
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let mut alerts = Vec::new();
        if let Some(issues) = body.as_array() {
            for issue in issues {
                let id = issue
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let title = issue
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Index analysis suggestion")
                    .to_string();
                let url = issue
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from);
                alerts.push(Alert {
                    id,
                    title,
                    severity: crate::governance::Severity::Warning,
                    status: AlertStatus::Active,
                    source: self.id().to_string(),
                    database: Some(database.clone()),
                    created_at: SystemTime::now(),
                    url,
                });
            }
        }

        Ok(alerts)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_default_base_url() {
        let connector = PganalyzeConnector::new("test-key".to_string());
        assert_eq!(connector.base_url, DEFAULT_BASE_URL);
        assert_eq!(connector.api_key, "test-key");
    }

    #[test]
    fn with_base_url_overrides_default() {
        let connector = PganalyzeConnector::with_base_url(
            "test-key".to_string(),
            "http://localhost:9000".to_string(),
        );
        assert_eq!(connector.base_url, "http://localhost:9000");
        assert_eq!(connector.api_key, "test-key");
    }

    #[test]
    fn id_returns_pganalyze() {
        let connector = PganalyzeConnector::new("test-key".to_string());
        assert_eq!(connector.id(), "pganalyze");
    }

    #[test]
    fn name_returns_pganalyze() {
        let connector = PganalyzeConnector::new("test-key".to_string());
        assert_eq!(connector.name(), "pganalyze");
    }

    #[test]
    fn capabilities_returns_correct_values() {
        let connector = PganalyzeConnector::new("test-key".to_string());
        let caps = connector.capabilities();
        assert!(caps.can_fetch_metrics);
        assert!(caps.can_fetch_alerts);
        assert!(!caps.can_create_issues);
        assert!(!caps.can_update_issues);
        assert!(!caps.can_receive_webhooks);
        assert!(!caps.supports_pagination);
    }

    #[test]
    fn rate_limit_config_returns_expected_values() {
        let connector = PganalyzeConnector::new("test-key".to_string());
        let rl = connector.rate_limit_config();
        // ~0.17 rps (10 rpm)
        assert!((rl.requests_per_second - 0.17).abs() < f64::EPSILON);
        assert_eq!(rl.requests_per_minute, Some(10));
        assert_eq!(rl.max_concurrent, 1);
        assert!(rl.respect_retry_after);
    }
}
