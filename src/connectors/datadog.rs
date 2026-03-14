//! Datadog connector — fetches metrics and alerts via the Datadog HTTP API.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;

use super::{
    Alert, AlertStatus, BackoffConfig, ConnectorCapabilities, ConnectorError, ConnectorHealth,
    ConnectorId, DatabaseId, Metric, RateLimitConfig, TimeWindow,
};
use crate::connectors::Connector;
use crate::governance::Severity;

const DEFAULT_BASE_URL: &str = "https://api.datadoghq.com";

// ---------------------------------------------------------------------------
// Connector struct
// ---------------------------------------------------------------------------

/// Connector for the Datadog monitoring platform.
pub struct DatadogConnector {
    api_key: String,
    app_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl DatadogConnector {
    /// Create a new `DatadogConnector` using the default Datadog API base URL.
    pub fn new(api_key: String, application_key: String) -> Self {
        Self::with_base_url(api_key, application_key, DEFAULT_BASE_URL.to_string())
    }

    /// Create a new `DatadogConnector` with a custom base URL.
    ///
    /// Useful for testing with a mock server or for EU-region endpoints.
    pub fn with_base_url(api_key: String, application_key: String, base_url: String) -> Self {
        Self {
            api_key,
            app_key: application_key,
            base_url,
            client: reqwest::Client::new(),
        }
    }

    /// Build a `reqwest::RequestBuilder` with Datadog auth headers pre-set.
    fn authenticated_get(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header("DD-API-KEY", &self.api_key)
            .header("DD-APPLICATION-KEY", &self.app_key)
    }

    /// Build an authenticated POST request builder.
    fn authenticated_post(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .post(url)
            .header("DD-API-KEY", &self.api_key)
            .header("DD-APPLICATION-KEY", &self.app_key)
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
}

// ---------------------------------------------------------------------------
// Datadog API response types
// ---------------------------------------------------------------------------

/// Top-level response from `GET /api/v1/validate`.
#[derive(Debug, Deserialize)]
struct ValidateResponse {
    valid: bool,
}

/// Top-level response from `POST /api/v1/query`.
#[derive(Debug, Deserialize)]
struct MetricsQueryResponse {
    series: Option<Vec<MetricSeries>>,
}

#[derive(Debug, Deserialize)]
struct MetricSeries {
    metric: String,
    unit: Option<Vec<Option<MetricUnit>>>,
    pointlist: Vec<[f64; 2]>,
    tag_set: Option<Vec<String>>,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MetricUnit {
    name: Option<String>,
}

/// Top-level response from `GET /api/v1/monitor`.
type MonitorsResponse = Vec<MonitorEntry>;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MonitorEntry {
    id: u64,
    name: String,
    overall_state: String,
    tags: Option<Vec<String>>,
    created: Option<String>,
    deleted: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn unix_to_system_time(secs: f64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs_f64(secs)
}

fn parse_iso8601(s: &str) -> SystemTime {
    // Best-effort parse; we avoid pulling in `chrono` just for this connector.
    // A full implementation would parse RFC 3339 here.
    let _ = s;
    SystemTime::now()
}

fn monitor_state_to_alert_status(state: &str) -> AlertStatus {
    match state.to_lowercase().as_str() {
        "silenced" => AlertStatus::Acknowledged,
        "ok" | "resolved" | "no data" => AlertStatus::Resolved,
        _ => AlertStatus::Active,
    }
}

fn monitor_state_to_severity(state: &str) -> Severity {
    match state.to_lowercase().as_str() {
        "alert" => Severity::Critical,
        "warn" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn tags_vec_to_map(tags: &[String]) -> HashMap<String, String> {
    tags.iter()
        .filter_map(|t| {
            let mut parts = t.splitn(2, ':');
            let key = parts.next()?.to_string();
            let val = parts.next().unwrap_or("").to_string();
            Some((key, val))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Connector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for DatadogConnector {
    fn id(&self) -> &'static str {
        "datadog"
    }

    fn name(&self) -> &'static str {
        "Datadog"
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
            requests_per_second: 0.5,
            requests_per_minute: Some(30),
            max_concurrent: 2,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Validate API credentials via `GET /api/v1/validate`.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let url = format!("{}/api/v1/validate", self.base_url);
        let start = SystemTime::now();

        let response = self
            .authenticated_get(&url)
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

        let parsed: ValidateResponse =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Other(e.to_string()))?;

        if parsed.valid {
            Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(elapsed_ms),
            })
        } else {
            Ok(ConnectorHealth {
                connected: false,
                message: Some("credentials are not valid".to_string()),
                latency_ms: Some(elapsed_ms),
            })
        }
    }

    /// Query metrics via `POST /api/v1/query`.
    ///
    /// Builds a generic query for the given database over the time window and
    /// converts each point in each returned series into a [`Metric`].
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let url = format!("{}/api/v1/query", self.base_url);

        let from = window
            .start
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let to = window
            .end
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Generic per-db query; callers can refine this later.
        let query = format!("avg:postgresql.*{{db:{database}}}");

        let response = self
            .authenticated_post(&url)
            .form(&[
                ("from", from.to_string()),
                ("to", to.to_string()),
                ("query", query),
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

        let parsed: MetricsQueryResponse =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Other(e.to_string()))?;

        let source: ConnectorId = self.id().to_string();
        let mut metrics = Vec::new();

        for series in parsed.series.unwrap_or_default() {
            // Derive a unit string from the first unit entry, if any.
            let unit = series
                .unit
                .as_deref()
                .and_then(|units| units.first())
                .and_then(|u| u.as_ref())
                .and_then(|u| u.name.clone());

            // Build tags: merge tag_set plus scope.
            let mut tags: HashMap<String, String> =
                tags_vec_to_map(series.tag_set.as_deref().unwrap_or(&[]));
            if let Some(scope) = &series.scope {
                tags.insert("scope".to_string(), scope.clone());
            }

            for point in &series.pointlist {
                let timestamp = unix_to_system_time(point[0] / 1000.0);
                metrics.push(Metric {
                    name: series.metric.clone(),
                    value: point[1],
                    unit: unit.clone(),
                    timestamp,
                    tags: tags.clone(),
                    source: source.clone(),
                });
            }
        }

        Ok(metrics)
    }

    /// Fetch monitors filtered by database tag via `GET /api/v1/monitor`.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let url = format!("{}/api/v1/monitor", self.base_url);

        let response = self
            .authenticated_get(&url)
            .query(&[("tags", format!("db:{database}"))])
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

        let monitors: MonitorsResponse =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Other(e.to_string()))?;

        let source: ConnectorId = self.id().to_string();
        let mut alerts = Vec::new();

        for monitor in monitors {
            // Skip deleted monitors.
            if monitor.deleted.is_some() {
                continue;
            }

            let alert_status = monitor_state_to_alert_status(&monitor.overall_state);
            let severity = monitor_state_to_severity(&monitor.overall_state);
            let created_at = monitor.created.as_deref().map_or(UNIX_EPOCH, parse_iso8601);

            alerts.push(Alert {
                id: monitor.id.to_string(),
                title: monitor.name,
                severity,
                status: alert_status,
                source: source.clone(),
                database: Some(database.clone()),
                created_at,
                url: None,
            });
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
    use crate::connectors::{Connector, ConnectorCapabilities, RateLimitConfig};

    #[test]
    fn new_sets_default_base_url() {
        let c = DatadogConnector::new("key".to_string(), "app".to_string());
        assert_eq!(c.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn with_base_url_overrides_base_url() {
        let custom = "https://api.datadoghq.eu";
        let c = DatadogConnector::with_base_url(
            "key".to_string(),
            "app".to_string(),
            custom.to_string(),
        );
        assert_eq!(c.base_url, custom);
    }

    #[test]
    fn id_returns_datadog() {
        let c = DatadogConnector::new("k".to_string(), "a".to_string());
        assert_eq!(c.id(), "datadog");
    }

    #[test]
    fn name_returns_datadog() {
        let c = DatadogConnector::new("k".to_string(), "a".to_string());
        assert_eq!(c.name(), "Datadog");
    }

    #[test]
    fn capabilities_are_correct() {
        let c = DatadogConnector::new("k".to_string(), "a".to_string());
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
        let c = DatadogConnector::new("k".to_string(), "a".to_string());
        let RateLimitConfig {
            requests_per_second,
            requests_per_minute,
            max_concurrent,
            ..
        } = c.rate_limit_config();

        assert!((requests_per_second - 0.5).abs() < f64::EPSILON);
        assert_eq!(requests_per_minute, Some(30));
        assert_eq!(max_concurrent, 2);
    }
}
