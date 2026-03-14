//! Supabase connector — health checks via the Supabase Management API.
//!
//! ## API endpoints used
//!
//! - `GET /v1/projects/{ref}/health` — project service health, mapped to
//!   alerts (unhealthy services become `Active` alerts).
//! - `GET /v1/projects/{ref}/metrics` — Prometheus-format metrics, parsed
//!   into `Metric` data points.

use std::collections::HashMap;
use std::time::SystemTime;

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    Alert, AlertStatus, BackoffConfig, ConnectorCapabilities, ConnectorError, ConnectorHealth,
    DatabaseId, Metric, RateLimitConfig, TimeWindow,
};
use crate::connectors::Connector;
use crate::governance::Severity;

const DEFAULT_BASE_URL: &str = "https://api.supabase.com";

// ---------------------------------------------------------------------------
// Connector struct
// ---------------------------------------------------------------------------

/// Connector for the Supabase platform.
pub struct SupabaseConnector {
    access_token: String,
    project_ref: Option<String>,
    base_url: String,
    client: reqwest::Client,
}

impl SupabaseConnector {
    /// Create a new `SupabaseConnector` using the default Supabase API base URL.
    pub fn new(access_token: String) -> Self {
        Self {
            access_token,
            project_ref: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Set the Supabase project reference (e.g., `"abcdefghijklmnop"`).
    pub fn with_project_ref(mut self, project_ref: String) -> Self {
        self.project_ref = Some(project_ref);
        self
    }

    /// Override the API base URL (useful for testing with a mock server).
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    /// Map an HTTP status + body to a `ConnectorError`.
    fn map_error(status: u16, body: &str) -> ConnectorError {
        match status {
            401 | 403 => ConnectorError::AuthError(body.to_string()),
            429 => ConnectorError::RateLimited {
                retry_after_ms: None,
            },
            _ => ConnectorError::ApiError {
                status,
                message: body.to_string(),
            },
        }
    }

    /// Return the resolved project ref, or a `ConnectorError` if not set.
    fn require_project_ref(&self) -> Result<&str, ConnectorError> {
        self.project_ref.as_deref().ok_or_else(|| {
            ConnectorError::Other("project_ref is required for this operation".to_string())
        })
    }
}

// ---------------------------------------------------------------------------
// Supabase API response types
// ---------------------------------------------------------------------------

/// A single service entry from `GET /v1/projects/{ref}/health`.
#[derive(Debug, Deserialize)]
struct ServiceHealth {
    name: String,
    healthy: bool,
    status: Option<String>,
}

/// A Prometheus-format metric line parsed from the `/metrics` endpoint.
///
/// Format: `metric_name{label="value",...} value [timestamp_ms]`
struct PrometheusLine {
    name: String,
    labels: HashMap<String, String>,
    value: f64,
}

/// Parse a Prometheus exposition-format text body into individual samples.
///
/// Only handles the simple `name{labels} value` format; TYPE/HELP lines
/// and histogram/summary suffixes are skipped.
fn parse_prometheus(body: &str) -> Vec<PrometheusLine> {
    let mut out = Vec::new();

    for raw in body.lines() {
        let line = raw.trim();
        // Skip comments and blank lines.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split off optional trailing timestamp: `name{...} value [ts]`
        // We only care about name+labels+value.
        let (metric_part, _ts) = match line.rfind(' ') {
            Some(pos) => {
                let tail = &line[pos + 1..];
                // If tail parses as a number it could be the value OR a
                // Unix-ms timestamp — we always want the value column, so
                // split differently: find the last field that looks like a
                // timestamp (> 1e12) vs value.
                if tail.parse::<f64>().is_ok_and(|v| v > 1e12) {
                    // Tail is a timestamp; value is the previous field.
                    (&line[..pos], Some(tail))
                } else {
                    (line, None)
                }
            }
            None => (line, None),
        };

        // Split at the last space to separate `name{labels}` from `value`.
        let (name_labels, value_str) = match metric_part.rfind(' ') {
            Some(pos) => (&metric_part[..pos], &metric_part[pos + 1..]),
            None => continue,
        };

        let value: f64 = match value_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Parse `metric_name` or `metric_name{k="v",...}`.
        let (name, labels) = if let Some(brace) = name_labels.find('{') {
            let metric_name = &name_labels[..brace];
            let label_str = name_labels
                .get(brace + 1..name_labels.len().saturating_sub(1))
                .unwrap_or("");
            let labels = parse_prometheus_labels(label_str);
            (metric_name.to_string(), labels)
        } else {
            (name_labels.to_string(), HashMap::new())
        };

        out.push(PrometheusLine {
            name,
            labels,
            value,
        });
    }

    out
}

/// Parse the label set inside `{k="v", k2="v2"}` (the braces are already
/// stripped by the caller).
fn parse_prometheus_labels(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some(eq) = part.find('=') {
            let key = part[..eq].trim().to_string();
            let raw_val = part[eq + 1..].trim();
            // Strip surrounding quotes.
            let val = raw_val
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(raw_val)
                .to_string();
            map.insert(key, val);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Connector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for SupabaseConnector {
    fn id(&self) -> &'static str {
        "supabase"
    }

    fn name(&self) -> &'static str {
        "Supabase"
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
            requests_per_second: 1.0,
            requests_per_minute: None,
            max_concurrent: 2,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Check connectivity via `GET {base_url}/v1/projects`.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let url = format!("{}/v1/projects", self.base_url);
        let start = SystemTime::now();

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let elapsed_ms = start
            .elapsed()
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !(200..300).contains(&(status as usize)) {
            return Err(Self::map_error(status, &body));
        }

        Ok(ConnectorHealth {
            connected: true,
            message: None,
            latency_ms: Some(elapsed_ms),
        })
    }

    /// Fetch Prometheus-format metrics from
    /// `GET /v1/projects/{ref}/metrics`.
    ///
    /// Each parsed sample line is converted into a [`Metric`] whose `name`
    /// is the Prometheus metric name, `value` the sample value, and `tags`
    /// the Prometheus label set.
    ///
    /// Returns an empty `Vec` when `project_ref` is not set.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let Some(project_ref) = self.project_ref.as_deref() else {
            crate::logging::debug(
                "supabase",
                "fetch_metrics: project_ref not set, returning empty",
            );
            return Ok(vec![]);
        };

        let start_secs = window
            .start
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let end_secs = window
            .end
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let url = format!("{}/v1/projects/{}/metrics", self.base_url, project_ref);

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .query(&[
                ("start", start_secs.to_string()),
                ("end", end_secs.to_string()),
            ])
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !(200..300).contains(&(status as usize)) {
            return Err(Self::map_error(status, &body));
        }

        let now = SystemTime::now();
        let source = self.id().to_string();

        let metrics = parse_prometheus(&body)
            .into_iter()
            .map(|line| {
                let mut tags = line.labels;
                tags.insert("database".to_string(), database.clone());
                Metric {
                    name: line.name,
                    value: line.value,
                    unit: None,
                    timestamp: now,
                    tags,
                    source: source.clone(),
                }
            })
            .collect();

        Ok(metrics)
    }

    /// Fetch service-health alerts from
    /// `GET /v1/projects/{ref}/health`.
    ///
    /// Each service that is not `healthy` is emitted as an `Active`
    /// [`Alert`].  When all services are healthy the returned `Vec` is
    /// empty.
    ///
    /// Returns a `ConnectorError::Other` when `project_ref` is not set.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let project_ref = self.require_project_ref()?;

        let url = format!("{}/v1/projects/{}/health", self.base_url, project_ref);

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ConnectorError::NetworkError(e.to_string()))?;

        if !(200..300).contains(&(status as usize)) {
            return Err(Self::map_error(status, &body));
        }

        let services: Vec<ServiceHealth> = serde_json::from_str(&body)
            .map_err(|e| ConnectorError::Other(format!("failed to parse health response: {e}")))?;

        let source = self.id().to_string();
        let now = SystemTime::now();

        let alerts = services
            .into_iter()
            .filter(|s| !s.healthy)
            .map(|s| {
                let status_label = s.status.as_deref().unwrap_or("unhealthy").to_string();
                Alert {
                    id: format!("supabase-{project_ref}-{}", s.name),
                    title: format!("Supabase service '{}' is {}", s.name, status_label),
                    severity: Severity::Warning,
                    status: AlertStatus::Active,
                    source: source.clone(),
                    database: Some(database.clone()),
                    created_at: now,
                    url: None,
                }
            })
            .collect();

        Ok(alerts)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{Connector, ConnectorCapabilities, RateLimitConfig};

    // -----------------------------------------------------------------------
    // Constructor / builder tests
    // -----------------------------------------------------------------------

    #[test]
    fn new_sets_default_base_url() {
        let c = SupabaseConnector::new("token".to_string());
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn new_has_no_project_ref() {
        let c = SupabaseConnector::new("token".to_string());
        assert!(c.project_ref.is_none());
    }

    #[test]
    fn with_project_ref_sets_project_ref() {
        let c = SupabaseConnector::new("token".to_string())
            .with_project_ref("abcdefghijklmnop".to_string());
        assert_eq!(c.project_ref.as_deref(), Some("abcdefghijklmnop"));
    }

    #[test]
    fn with_base_url_overrides_base_url() {
        let custom = "http://localhost:8080";
        let c = SupabaseConnector::new("token".to_string()).with_base_url(custom.to_string());
        assert_eq!(c.base_url, custom);
    }

    #[test]
    fn builder_chain() {
        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("proj".to_string())
            .with_base_url("http://mock".to_string());
        assert_eq!(c.project_ref.as_deref(), Some("proj"));
        assert_eq!(c.base_url, "http://mock");
    }

    #[test]
    fn id_returns_supabase() {
        let c = SupabaseConnector::new("t".to_string());
        assert_eq!(c.id(), "supabase");
    }

    #[test]
    fn name_returns_supabase() {
        let c = SupabaseConnector::new("t".to_string());
        assert_eq!(c.name(), "Supabase");
    }

    #[test]
    fn capabilities_are_correct() {
        let c = SupabaseConnector::new("t".to_string());
        let ConnectorCapabilities {
            can_fetch_metrics,
            can_fetch_alerts,
            can_create_issues,
            can_update_issues,
            can_receive_webhooks,
            supports_pagination,
        } = c.capabilities();

        assert!(can_fetch_metrics);
        assert!(can_fetch_alerts);
        assert!(!can_create_issues);
        assert!(!can_update_issues);
        assert!(!can_receive_webhooks);
        assert!(!supports_pagination);
    }

    #[test]
    fn rate_limit_config_is_correct() {
        let c = SupabaseConnector::new("t".to_string());
        let RateLimitConfig {
            requests_per_second,
            requests_per_minute,
            max_concurrent,
            ..
        } = c.rate_limit_config();

        assert!((requests_per_second - 1.0).abs() < f64::EPSILON);
        assert!(requests_per_minute.is_none());
        assert_eq!(max_concurrent, 2);
    }

    // -----------------------------------------------------------------------
    // require_project_ref
    // -----------------------------------------------------------------------

    #[test]
    fn require_project_ref_ok_when_set() {
        let c = SupabaseConnector::new("t".to_string()).with_project_ref("myref".to_string());
        assert!(c.require_project_ref().is_ok());
        assert_eq!(c.require_project_ref().unwrap(), "myref");
    }

    #[test]
    fn require_project_ref_err_when_unset() {
        let c = SupabaseConnector::new("t".to_string());
        assert!(matches!(
            c.require_project_ref(),
            Err(ConnectorError::Other(_))
        ));
    }

    // -----------------------------------------------------------------------
    // Prometheus parser unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_prometheus_skips_comments_and_blanks() {
        let body = "# HELP pg_up whether postgres is up\n\
                    # TYPE pg_up gauge\n\
                    \n\
                    pg_up 1";
        let lines = parse_prometheus(body);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].name, "pg_up");
        assert!((lines[0].value - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_prometheus_parses_labels() {
        let body = r#"pg_stat_table_n_live_tup{schemaname="public",relname="users"} 42"#;
        let lines = parse_prometheus(body);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].name, "pg_stat_table_n_live_tup");
        assert!((lines[0].value - 42.0).abs() < f64::EPSILON);
        assert_eq!(
            lines[0].labels.get("schemaname").map(String::as_str),
            Some("public")
        );
        assert_eq!(
            lines[0].labels.get("relname").map(String::as_str),
            Some("users")
        );
    }

    #[test]
    fn parse_prometheus_handles_no_labels() {
        let body = "process_resident_memory_bytes 12345678";
        let lines = parse_prometheus(body);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].name, "process_resident_memory_bytes");
        assert!((lines[0].value - 12_345_678.0).abs() < f64::EPSILON);
        assert!(lines[0].labels.is_empty());
    }

    #[test]
    fn parse_prometheus_multiple_lines() {
        let body = "cpu_usage 0.5\nmemory_bytes 1024\ndisk_read_bytes 2048";
        let lines = parse_prometheus(body);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn parse_prometheus_ignores_unparseable_value() {
        let body = "bad_metric NaN_not_a_real_float\ngood_metric 1.0";
        let lines = parse_prometheus(body);
        // NaN is technically parseable in Rust; "NaN_not_a_real_float" is not
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].name, "good_metric");
    }

    // -----------------------------------------------------------------------
    // fetch_alerts — without project_ref
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_alerts_returns_err_when_no_project_ref() {
        let c = SupabaseConnector::new("token".to_string());
        let result = c.fetch_alerts(&"mydb".to_string()).await;
        assert!(matches!(result, Err(ConnectorError::Other(_))));
    }

    // -----------------------------------------------------------------------
    // fetch_metrics — without project_ref returns empty
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_metrics_returns_empty_when_no_project_ref() {
        use std::time::UNIX_EPOCH;
        let c = SupabaseConnector::new("t".to_string());
        let window = TimeWindow {
            start: UNIX_EPOCH,
            end: SystemTime::now(),
        };
        let result = c.fetch_metrics(&"mydb".to_string(), &window).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // fetch_alerts — mock HTTP responses via wiremock
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_alerts_all_healthy_returns_empty() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/testref/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "db", "healthy": true},
                {"name": "api", "healthy": true},
                {"name": "storage", "healthy": true}
            ])))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("testref".to_string())
            .with_base_url(server.uri());

        let alerts = c.fetch_alerts(&"mydb".to_string()).await.unwrap();
        assert!(alerts.is_empty());
    }

    #[tokio::test]
    async fn fetch_alerts_unhealthy_service_produces_alert() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/testref/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "db", "healthy": true},
                {"name": "realtime", "healthy": false, "status": "degraded"}
            ])))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("testref".to_string())
            .with_base_url(server.uri());

        let alerts = c.fetch_alerts(&"mydb".to_string()).await.unwrap();
        assert_eq!(alerts.len(), 1);
        let a = &alerts[0];
        assert_eq!(a.id, "supabase-testref-realtime");
        assert!(a.title.contains("realtime"));
        assert!(a.title.contains("degraded"));
        assert_eq!(a.severity, Severity::Warning);
        assert_eq!(a.status, AlertStatus::Active);
        assert_eq!(a.source, "supabase");
        assert_eq!(a.database.as_deref(), Some("mydb"));
    }

    #[tokio::test]
    async fn fetch_alerts_multiple_unhealthy_services() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref123/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "db", "healthy": false, "status": "offline"},
                {"name": "api", "healthy": false, "status": "starting"},
                {"name": "storage", "healthy": true}
            ])))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref123".to_string())
            .with_base_url(server.uri());

        let alerts = c.fetch_alerts(&"db1".to_string()).await.unwrap();
        assert_eq!(alerts.len(), 2);
        let ids: Vec<&str> = alerts.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"supabase-ref123-db"));
        assert!(ids.contains(&"supabase-ref123-api"));
    }

    #[tokio::test]
    async fn fetch_alerts_unhealthy_without_status_field() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"name": "auth", "healthy": false}])),
            )
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let alerts = c.fetch_alerts(&"db".to_string()).await.unwrap();
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].title.contains("unhealthy"));
    }

    #[tokio::test]
    async fn fetch_alerts_returns_auth_error_on_401() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/health"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("bad-tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let result = c.fetch_alerts(&"db".to_string()).await;
        assert!(matches!(result, Err(ConnectorError::AuthError(_))));
    }

    #[tokio::test]
    async fn fetch_alerts_returns_rate_limited_on_429() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/health"))
            .respond_with(ResponseTemplate::new(429).set_body_string("Too Many Requests"))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let result = c.fetch_alerts(&"db".to_string()).await;
        assert!(matches!(result, Err(ConnectorError::RateLimited { .. })));
    }

    #[tokio::test]
    async fn fetch_alerts_returns_api_error_on_500() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/health"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let result = c.fetch_alerts(&"db".to_string()).await;
        assert!(matches!(
            result,
            Err(ConnectorError::ApiError { status: 500, .. })
        ));
    }

    // -----------------------------------------------------------------------
    // fetch_metrics — mock HTTP responses via wiremock
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_metrics_parses_prometheus_body() {
        use std::time::UNIX_EPOCH;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let prometheus_body = "# HELP pg_up Postgres up\n\
                               # TYPE pg_up gauge\n\
                               pg_up 1\n\
                               pg_stat_database_numbackends{datname=\"mydb\"} 5\n";

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(prometheus_body))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let window = TimeWindow {
            start: UNIX_EPOCH,
            end: SystemTime::now(),
        };
        let metrics = c.fetch_metrics(&"mydb".to_string(), &window).await.unwrap();
        assert_eq!(metrics.len(), 2);

        let pg_up = metrics.iter().find(|m| m.name == "pg_up").unwrap();
        assert!((pg_up.value - 1.0).abs() < f64::EPSILON);
        assert_eq!(pg_up.source, "supabase");
        assert_eq!(pg_up.tags.get("database").map(String::as_str), Some("mydb"));

        let backends = metrics
            .iter()
            .find(|m| m.name == "pg_stat_database_numbackends")
            .unwrap();
        assert!((backends.value - 5.0).abs() < f64::EPSILON);
        assert_eq!(
            backends.tags.get("datname").map(String::as_str),
            Some("mydb")
        );
    }

    #[tokio::test]
    async fn fetch_metrics_returns_auth_error_on_401() {
        use std::time::UNIX_EPOCH;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/metrics"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("bad-tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let window = TimeWindow {
            start: UNIX_EPOCH,
            end: SystemTime::now(),
        };
        let result = c.fetch_metrics(&"db".to_string(), &window).await;
        assert!(matches!(result, Err(ConnectorError::AuthError(_))));
    }

    #[tokio::test]
    async fn fetch_metrics_empty_body_returns_empty_vec() {
        use std::time::UNIX_EPOCH;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/projects/ref/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;

        let c = SupabaseConnector::new("tok".to_string())
            .with_project_ref("ref".to_string())
            .with_base_url(server.uri());

        let window = TimeWindow {
            start: UNIX_EPOCH,
            end: SystemTime::now(),
        };
        let metrics = c.fetch_metrics(&"db".to_string(), &window).await.unwrap();
        assert!(metrics.is_empty());
    }
}
