//! Config-driven HTTP JSON connector.
//!
//! Implements [`Connector`] for any HTTP API that returns JSON, driven
//! entirely by configuration — no hand-written per-service code needed.

use std::collections::HashMap;
use std::time::SystemTime;

use async_trait::async_trait;

use super::{
    Alert, AlertStatus, BackoffConfig, ConnectorCapabilities, ConnectorError, ConnectorHealth,
    DatabaseId, Metric, RateLimitConfig, TimeWindow,
};
use crate::connectors::Connector;
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// Auth configuration
// ---------------------------------------------------------------------------

/// Authentication strategy for an [`HttpJsonConnector`].
#[derive(Debug, Clone)]
pub enum HttpJsonAuth {
    /// `Authorization: Bearer <token>`
    Bearer { token: String },
    /// Arbitrary header key/value pair (e.g. `X-Api-Key: <value>`).
    ApiKey { header: String, value: String },
    /// HTTP Basic authentication.
    Basic { username: String, password: String },
    /// No authentication.
    None,
}

// ---------------------------------------------------------------------------
// Metric mapping
// ---------------------------------------------------------------------------

/// Describes how to extract a single metric value from a JSON response.
#[derive(Debug, Clone)]
pub struct MetricMapping {
    /// Dot-notation path into the JSON object, e.g. `"data.cpu_percent"`.
    pub json_path: String,
    /// Name that will be set on the resulting [`Metric`].
    pub metric_name: String,
    /// Optional unit string (e.g. `"percent"`, `"ms"`).
    pub unit: Option<String>,
}

// ---------------------------------------------------------------------------
// Connector struct
// ---------------------------------------------------------------------------

/// Config-driven HTTP JSON connector.
///
/// Construct via [`HttpJsonConnectorBuilder`].
pub struct HttpJsonConnector {
    connector_id: String,
    connector_name: String,
    base_url: String,
    auth: HttpJsonAuth,
    /// Relative path appended to `base_url` for metric fetches, e.g. `"/metrics"`.
    metrics_endpoint: Option<String>,
    /// Relative path appended to `base_url` for alert fetches, e.g. `"/alerts"`.
    alerts_endpoint: Option<String>,
    metric_mappings: Vec<MetricMapping>,
    rate_limit_rps: f64,
    client: reqwest::Client,
}

impl HttpJsonConnector {
    /// Return the fully-qualified URL for a given endpoint path.
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), path)
    }

    /// Apply the configured [`HttpJsonAuth`] to a request builder.
    fn apply_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            HttpJsonAuth::Bearer { token } => {
                builder.header("Authorization", format!("Bearer {token}"))
            }
            HttpJsonAuth::ApiKey { header, value } => builder.header(header.as_str(), value),
            HttpJsonAuth::Basic { username, password } => {
                builder.basic_auth(username, Some(password))
            }
            HttpJsonAuth::None => builder,
        }
    }

    /// Issue an authenticated GET and return the parsed JSON body.
    async fn get_json(&self, url: &str) -> Result<serde_json::Value, ConnectorError> {
        let builder = self.apply_auth(self.client.get(url));

        let response = builder
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !(200..300).contains(&(status as usize)) {
            return Err(match status {
                401 | 403 => ConnectorError::AuthError(body),
                429 => ConnectorError::RateLimited {
                    retry_after_ms: None,
                },
                _ => ConnectorError::ApiError {
                    status,
                    message: body,
                },
            });
        }

        serde_json::from_str(&body).map_err(|e| ConnectorError::Other(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Connector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for HttpJsonConnector {
    fn id(&self) -> &str {
        &self.connector_id
    }

    fn name(&self) -> &str {
        &self.connector_name
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            can_fetch_metrics: self.metrics_endpoint.is_some() && !self.metric_mappings.is_empty(),
            can_fetch_alerts: self.alerts_endpoint.is_some(),
            can_create_issues: false,
            can_update_issues: false,
            can_receive_webhooks: false,
            supports_pagination: false,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: self.rate_limit_rps,
            requests_per_minute: None,
            max_concurrent: 4,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Health check: a GET to `base_url` that must return 2xx.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let url = self.base_url.clone();
        let start = SystemTime::now();

        let builder = self.apply_auth(self.client.get(&url));
        let response = builder
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let elapsed_ms = start
            .elapsed()
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let status = response.status().as_u16();

        if (200..300).contains(&(status as usize)) {
            Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(elapsed_ms),
            })
        } else {
            Ok(ConnectorHealth {
                connected: false,
                message: Some(format!("HTTP {status}")),
                latency_ms: Some(elapsed_ms),
            })
        }
    }

    /// Fetch metrics by GET-ing `metrics_endpoint` and applying each mapping.
    ///
    /// The `database` and `window` parameters are accepted for interface
    /// compatibility; this connector fetches whatever the endpoint returns
    /// regardless of filters.
    async fn fetch_metrics(
        &self,
        _database: &DatabaseId,
        _window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let path = match &self.metrics_endpoint {
            Some(p) => p.clone(),
            None => {
                return Err(ConnectorError::NotSupported("fetch_metrics"));
            }
        };

        let json = self.get_json(&self.url(&path)).await?;
        let now = SystemTime::now();

        let mut metrics = Vec::new();
        for mapping in &self.metric_mappings {
            if let Some(value) = extract_json_value(&json, &mapping.json_path) {
                metrics.push(Metric {
                    name: mapping.metric_name.clone(),
                    value,
                    unit: mapping.unit.clone(),
                    timestamp: now,
                    tags: HashMap::new(),
                    source: self.connector_id.clone(),
                });
            }
        }

        Ok(metrics)
    }

    /// Fetch alerts by GET-ing `alerts_endpoint`.
    ///
    /// The response is expected to be a JSON array of objects, each with at
    /// least an `"id"` and `"title"` field.  All other fields are optional and
    /// fall back to sensible defaults.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let path = match &self.alerts_endpoint {
            Some(p) => p.clone(),
            None => {
                return Err(ConnectorError::NotSupported("fetch_alerts"));
            }
        };

        let json = self.get_json(&self.url(&path)).await?;
        let now = SystemTime::now();

        let entries = match json.as_array() {
            Some(arr) => arr.clone(),
            None => {
                return Err(ConnectorError::Other(
                    "alerts endpoint did not return a JSON array".to_string(),
                ));
            }
        };

        let mut alerts = Vec::new();
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(no title)")
                .to_string();

            let severity = entry
                .get("severity")
                .and_then(|v| v.as_str())
                .map_or(Severity::Info, parse_severity);

            let status = entry
                .get("status")
                .and_then(|v| v.as_str())
                .map_or(AlertStatus::Active, parse_alert_status);

            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);

            alerts.push(Alert {
                id,
                title,
                severity,
                status,
                source: self.connector_id.clone(),
                database: Some(database.clone()),
                created_at: now,
                url,
            });
        }

        Ok(alerts)
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Extract a numeric value from `json` using a dot-notation `path`.
///
/// For example, `"data.cpu_percent"` navigates `json["data"]["cpu_percent"]`
/// and returns the value as `f64` if it is a JSON number.
pub fn extract_json_value(json: &serde_json::Value, path: &str) -> Option<f64> {
    let mut current = json;
    for key in path.split('.') {
        current = current.get(key)?;
    }
    current.as_f64()
}

fn parse_severity(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "critical" | "error" | "high" => Severity::Critical,
        "warning" | "warn" | "medium" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn parse_alert_status(s: &str) -> AlertStatus {
    match s.to_lowercase().as_str() {
        "acknowledged" | "ack" => AlertStatus::Acknowledged,
        "resolved" | "ok" | "closed" => AlertStatus::Resolved,
        _ => AlertStatus::Active,
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`HttpJsonConnector`].
///
/// # Example
///
/// ```rust
/// # use rpg::connectors::http_json::{HttpJsonConnectorBuilder, HttpJsonAuth, MetricMapping};
/// let connector = HttpJsonConnectorBuilder::new("my-api", "My API", "https://api.example.com")
///     .auth(HttpJsonAuth::Bearer { token: "secret".to_string() })
///     .metrics_endpoint("/v1/metrics")
///     .add_metric_mapping(MetricMapping {
///         json_path: "data.cpu_percent".to_string(),
///         metric_name: "cpu_percent".to_string(),
///         unit: Some("percent".to_string()),
///     })
///     .rate_limit_rps(10.0)
///     .build();
/// ```
pub struct HttpJsonConnectorBuilder {
    connector_id: String,
    connector_name: String,
    base_url: String,
    auth: HttpJsonAuth,
    metrics_endpoint: Option<String>,
    alerts_endpoint: Option<String>,
    metric_mappings: Vec<MetricMapping>,
    rate_limit_rps: f64,
}

impl HttpJsonConnectorBuilder {
    /// Start a new builder.  `id` is the unique connector identifier.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            connector_id: id.into(),
            connector_name: name.into(),
            base_url: base_url.into(),
            auth: HttpJsonAuth::None,
            metrics_endpoint: None,
            alerts_endpoint: None,
            metric_mappings: Vec::new(),
            rate_limit_rps: 10.0,
        }
    }

    /// Set the authentication strategy.
    pub fn auth(mut self, auth: HttpJsonAuth) -> Self {
        self.auth = auth;
        self
    }

    /// Set the relative path for the metrics endpoint (e.g. `"/metrics"`).
    pub fn metrics_endpoint(mut self, path: impl Into<String>) -> Self {
        self.metrics_endpoint = Some(path.into());
        self
    }

    /// Set the relative path for the alerts endpoint (e.g. `"/alerts"`).
    pub fn alerts_endpoint(mut self, path: impl Into<String>) -> Self {
        self.alerts_endpoint = Some(path.into());
        self
    }

    /// Append a single [`MetricMapping`].
    pub fn add_metric_mapping(mut self, mapping: MetricMapping) -> Self {
        self.metric_mappings.push(mapping);
        self
    }

    /// Set all metric mappings at once, replacing any previously added.
    pub fn metric_mappings(mut self, mappings: Vec<MetricMapping>) -> Self {
        self.metric_mappings = mappings;
        self
    }

    /// Set the target requests-per-second for rate limiting.
    pub fn rate_limit_rps(mut self, rps: f64) -> Self {
        self.rate_limit_rps = rps;
        self
    }

    /// Consume the builder and produce an [`HttpJsonConnector`].
    pub fn build(self) -> HttpJsonConnector {
        HttpJsonConnector {
            connector_id: self.connector_id,
            connector_name: self.connector_name,
            base_url: self.base_url,
            auth: self.auth,
            metrics_endpoint: self.metrics_endpoint,
            alerts_endpoint: self.alerts_endpoint,
            metric_mappings: self.metric_mappings,
            rate_limit_rps: self.rate_limit_rps,
            client: reqwest::Client::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Connector;

    // ------------------------------------------------------------------
    // extract_json_value
    // ------------------------------------------------------------------

    #[test]
    fn extract_top_level_number() {
        let json = serde_json::json!({ "cpu": 42.5 });
        assert_eq!(extract_json_value(&json, "cpu"), Some(42.5));
    }

    #[test]
    fn extract_nested_number() {
        let json = serde_json::json!({
            "data": { "cpu_percent": 78.3 }
        });
        assert_eq!(extract_json_value(&json, "data.cpu_percent"), Some(78.3));
    }

    #[test]
    fn extract_deeply_nested() {
        let json = serde_json::json!({
            "a": { "b": { "c": 3.14 } }
        });
        assert_eq!(extract_json_value(&json, "a.b.c"), Some(3.14));
    }

    #[test]
    fn extract_missing_key_returns_none() {
        let json = serde_json::json!({ "cpu": 1.0 });
        assert!(extract_json_value(&json, "missing").is_none());
    }

    #[test]
    fn extract_missing_nested_key_returns_none() {
        let json = serde_json::json!({ "data": {} });
        assert!(extract_json_value(&json, "data.cpu_percent").is_none());
    }

    #[test]
    fn extract_non_numeric_returns_none() {
        let json = serde_json::json!({ "status": "ok" });
        assert!(extract_json_value(&json, "status").is_none());
    }

    #[test]
    fn extract_integer_coerced_to_f64() {
        let json = serde_json::json!({ "count": 100 });
        assert_eq!(extract_json_value(&json, "count"), Some(100.0));
    }

    // ------------------------------------------------------------------
    // Builder pattern
    // ------------------------------------------------------------------

    #[test]
    fn builder_sets_id_and_name() {
        let c = HttpJsonConnectorBuilder::new("my-api", "My API", "https://example.com").build();
        assert_eq!(c.id(), "my-api");
        assert_eq!(c.name(), "My API");
    }

    #[test]
    fn builder_sets_base_url() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://api.example.com").build();
        assert_eq!(c.base_url, "https://api.example.com");
    }

    #[test]
    fn builder_default_rate_limit() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com").build();
        assert!((c.rate_limit_rps - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn builder_custom_rate_limit() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .rate_limit_rps(5.0)
            .build();
        assert!((c.rate_limit_rps - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn builder_metric_mappings() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .metrics_endpoint("/metrics")
            .add_metric_mapping(MetricMapping {
                json_path: "data.cpu".to_string(),
                metric_name: "cpu".to_string(),
                unit: Some("percent".to_string()),
            })
            .build();
        assert_eq!(c.metric_mappings.len(), 1);
        assert_eq!(c.metric_mappings[0].json_path, "data.cpu");
    }

    #[test]
    fn builder_replace_all_metric_mappings() {
        let mappings = vec![
            MetricMapping {
                json_path: "a".to_string(),
                metric_name: "a".to_string(),
                unit: None,
            },
            MetricMapping {
                json_path: "b".to_string(),
                metric_name: "b".to_string(),
                unit: None,
            },
        ];
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .metric_mappings(mappings)
            .build();
        assert_eq!(c.metric_mappings.len(), 2);
    }

    // ------------------------------------------------------------------
    // Capabilities based on configured endpoints
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_no_endpoints() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com").build();
        let caps = c.capabilities();
        assert!(!caps.can_fetch_metrics);
        assert!(!caps.can_fetch_alerts);
        assert!(!caps.can_create_issues);
        assert!(!caps.can_update_issues);
        assert!(!caps.can_receive_webhooks);
        assert!(!caps.supports_pagination);
    }

    #[test]
    fn capabilities_metrics_endpoint_without_mappings() {
        // An endpoint without mappings should NOT advertise can_fetch_metrics.
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .metrics_endpoint("/metrics")
            .build();
        assert!(!c.capabilities().can_fetch_metrics);
    }

    #[test]
    fn capabilities_metrics_endpoint_with_mappings() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .metrics_endpoint("/metrics")
            .add_metric_mapping(MetricMapping {
                json_path: "cpu".to_string(),
                metric_name: "cpu".to_string(),
                unit: None,
            })
            .build();
        assert!(c.capabilities().can_fetch_metrics);
        assert!(!c.capabilities().can_fetch_alerts);
    }

    #[test]
    fn capabilities_alerts_endpoint() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .alerts_endpoint("/alerts")
            .build();
        assert!(!c.capabilities().can_fetch_metrics);
        assert!(c.capabilities().can_fetch_alerts);
    }

    #[test]
    fn capabilities_both_endpoints() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .metrics_endpoint("/metrics")
            .add_metric_mapping(MetricMapping {
                json_path: "v".to_string(),
                metric_name: "v".to_string(),
                unit: None,
            })
            .alerts_endpoint("/alerts")
            .build();
        let caps = c.capabilities();
        assert!(caps.can_fetch_metrics);
        assert!(caps.can_fetch_alerts);
    }

    // ------------------------------------------------------------------
    // Rate limit from config
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_reflects_builder() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .rate_limit_rps(2.5)
            .build();
        let rl = c.rate_limit_config();
        assert!((rl.requests_per_second - 2.5).abs() < f64::EPSILON);
        assert_eq!(rl.max_concurrent, 4);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // Auth header construction (via apply_auth / url helpers)
    // ------------------------------------------------------------------

    #[test]
    fn url_helper_strips_trailing_slash() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com/").build();
        assert_eq!(c.url("/metrics"), "https://example.com/metrics");
    }

    #[test]
    fn url_helper_no_double_slash() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com").build();
        assert_eq!(c.url("/v1/data"), "https://example.com/v1/data");
    }

    /// Smoke-test: verify that Bearer auth is accepted by the builder without panicking.
    #[test]
    fn bearer_auth_round_trip() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .auth(HttpJsonAuth::Bearer {
                token: "tok123".to_string(),
            })
            .build();
        assert!(matches!(c.auth, HttpJsonAuth::Bearer { .. }));
    }

    #[test]
    fn api_key_auth_round_trip() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .auth(HttpJsonAuth::ApiKey {
                header: "X-Api-Key".to_string(),
                value: "secret".to_string(),
            })
            .build();
        assert!(matches!(c.auth, HttpJsonAuth::ApiKey { .. }));
    }

    #[test]
    fn basic_auth_round_trip() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .auth(HttpJsonAuth::Basic {
                username: "admin".to_string(),
                password: "pass".to_string(),
            })
            .build();
        assert!(matches!(c.auth, HttpJsonAuth::Basic { .. }));
    }

    #[test]
    fn no_auth_round_trip() {
        let c = HttpJsonConnectorBuilder::new("x", "X", "https://example.com")
            .auth(HttpJsonAuth::None)
            .build();
        assert!(matches!(c.auth, HttpJsonAuth::None));
    }
}
