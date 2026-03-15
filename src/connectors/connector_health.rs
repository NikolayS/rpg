//! Connector health dashboard — check all registered connectors and surface
//! the results as structured data for `--report` output.

use tokio::time::timeout;

use crate::connectors::{ConnectorHealth, ConnectorRegistry};

/// Timeout applied to each individual `health_check()` call.
const HEALTH_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of a single connector health probe.
pub struct ConnectorHealthResult {
    pub id: String,
    pub name: String,
    pub health: Result<ConnectorHealth, String>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run `health_check()` on every connector in `registry`, applying a 5 s
/// per-connector timeout.  Never panics; errors are captured in the `health`
/// field.
pub async fn check_all_connectors(registry: &ConnectorRegistry) -> Vec<ConnectorHealthResult> {
    let mut results = Vec::new();

    for connector in registry.list() {
        let id = connector.id().to_owned();
        let name = connector.name().to_owned();

        let health = match timeout(HEALTH_CHECK_TIMEOUT, connector.health_check()).await {
            Ok(Ok(h)) => Ok(h),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_elapsed) => Err("health check timed out after 5s".to_owned()),
        };

        results.push(ConnectorHealthResult { id, name, health });
    }

    results
}

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

/// Format a single `ConnectorHealthResult` as a text line suitable for the
/// `=== Connectors ===` report section.
pub fn format_text_line(result: &ConnectorHealthResult) -> String {
    match &result.health {
        Ok(h) if h.connected => {
            if let Some(ms) = h.latency_ms {
                format!("{}: connected (latency: {ms}ms)", result.id)
            } else {
                format!("{}: connected", result.id)
            }
        }
        Ok(h) => {
            let msg = h.message.as_deref().unwrap_or("disconnected");
            format!("{}: disconnected — {msg}", result.id)
        }
        Err(e) => format!("{}: error — {e}", result.id),
    }
}

/// Produce a `serde_json::Value` for one connector result for the JSON report.
pub fn format_json_entry(result: &ConnectorHealthResult) -> serde_json::Value {
    match &result.health {
        Ok(h) if h.connected => serde_json::json!({
            "id":       result.id,
            "name":     result.name,
            "status":   "connected",
            "latency_ms": h.latency_ms,
            "message":  h.message,
        }),
        Ok(h) => serde_json::json!({
            "id":       result.id,
            "name":     result.name,
            "status":   "disconnected",
            "latency_ms": h.latency_ms,
            "message":  h.message,
        }),
        Err(e) => serde_json::json!({
            "id":     result.id,
            "name":   result.name,
            "status": "error",
            "error":  e,
        }),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{
        Alert, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError, DatabaseId, Metric,
        RateLimitConfig, TimeWindow,
    };
    use async_trait::async_trait;

    // ------------------------------------------------------------------
    // Stub connectors
    // ------------------------------------------------------------------

    struct OkConnector {
        id: &'static str,
        name: &'static str,
        latency_ms: Option<u64>,
    }

    #[async_trait]
    impl Connector for OkConnector {
        fn id(&self) -> &str {
            self.id
        }

        fn name(&self) -> &str {
            self.name
        }

        fn capabilities(&self) -> ConnectorCapabilities {
            ConnectorCapabilities {
                can_fetch_metrics: false,
                can_fetch_alerts: false,
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
                max_concurrent: 1,
                backoff: BackoffConfig::default(),
                respect_retry_after: false,
            }
        }

        async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
            Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: self.latency_ms,
            })
        }

        async fn fetch_metrics(
            &self,
            _database: &DatabaseId,
            _window: &TimeWindow,
        ) -> Result<Vec<Metric>, ConnectorError> {
            Ok(vec![])
        }

        async fn fetch_alerts(&self, _database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
            Ok(vec![])
        }
    }

    struct ErrConnector;

    #[async_trait]
    impl Connector for ErrConnector {
        fn id(&self) -> &'static str {
            "failing"
        }

        fn name(&self) -> &'static str {
            "Failing Connector"
        }

        fn capabilities(&self) -> ConnectorCapabilities {
            ConnectorCapabilities {
                can_fetch_metrics: false,
                can_fetch_alerts: false,
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
                max_concurrent: 1,
                backoff: BackoffConfig::default(),
                respect_retry_after: false,
            }
        }

        async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
            Err(ConnectorError::AuthError("invalid API key".to_owned()))
        }

        async fn fetch_metrics(
            &self,
            _database: &DatabaseId,
            _window: &TimeWindow,
        ) -> Result<Vec<Metric>, ConnectorError> {
            Ok(vec![])
        }

        async fn fetch_alerts(&self, _database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
            Ok(vec![])
        }
    }

    // ------------------------------------------------------------------
    // check_all_connectors tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn empty_registry_returns_empty_results() {
        let registry = ConnectorRegistry::new();
        let results = check_all_connectors(&registry).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn healthy_connector_captured_correctly() {
        let mut registry = ConnectorRegistry::new();
        registry.register(Box::new(OkConnector {
            id: "datadog",
            name: "Datadog",
            latency_ms: Some(42),
        }));

        let results = check_all_connectors(&registry).await;
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.id, "datadog");
        assert_eq!(r.name, "Datadog");
        let health = r.health.as_ref().unwrap();
        assert!(health.connected);
        assert_eq!(health.latency_ms, Some(42));
    }

    #[tokio::test]
    async fn error_connector_captured_as_err() {
        let mut registry = ConnectorRegistry::new();
        registry.register(Box::new(ErrConnector));

        let results = check_all_connectors(&registry).await;
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.id, "failing");
        assert!(r.health.is_err());
        assert!(r.health.as_ref().unwrap_err().contains("invalid API key"));
    }

    #[tokio::test]
    async fn multiple_connectors_all_checked() {
        let mut registry = ConnectorRegistry::new();
        registry.register(Box::new(OkConnector {
            id: "datadog",
            name: "Datadog",
            latency_ms: Some(10),
        }));
        registry.register(Box::new(ErrConnector));
        registry.register(Box::new(OkConnector {
            id: "pganalyze",
            name: "pganalyze",
            latency_ms: None,
        }));

        let results = check_all_connectors(&registry).await;
        assert_eq!(results.len(), 3);
        assert!(results[0].health.is_ok());
        assert!(results[1].health.is_err());
        assert!(results[2].health.is_ok());
    }

    // ------------------------------------------------------------------
    // format_text_line tests
    // ------------------------------------------------------------------

    #[test]
    fn text_line_connected_with_latency() {
        let result = ConnectorHealthResult {
            id: "datadog".to_owned(),
            name: "Datadog".to_owned(),
            health: Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(45),
            }),
        };
        assert_eq!(
            format_text_line(&result),
            "datadog: connected (latency: 45ms)"
        );
    }

    #[test]
    fn text_line_connected_no_latency() {
        let result = ConnectorHealthResult {
            id: "pganalyze".to_owned(),
            name: "pganalyze".to_owned(),
            health: Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: None,
            }),
        };
        assert_eq!(format_text_line(&result), "pganalyze: connected");
    }

    #[test]
    fn text_line_disconnected_with_message() {
        let result = ConnectorHealthResult {
            id: "cloudwatch".to_owned(),
            name: "CloudWatch".to_owned(),
            health: Ok(ConnectorHealth {
                connected: false,
                message: Some("region not set".to_owned()),
                latency_ms: None,
            }),
        };
        assert_eq!(
            format_text_line(&result),
            "cloudwatch: disconnected — region not set"
        );
    }

    #[test]
    fn text_line_disconnected_no_message() {
        let result = ConnectorHealthResult {
            id: "cloudwatch".to_owned(),
            name: "CloudWatch".to_owned(),
            health: Ok(ConnectorHealth {
                connected: false,
                message: None,
                latency_ms: None,
            }),
        };
        assert_eq!(
            format_text_line(&result),
            "cloudwatch: disconnected — disconnected"
        );
    }

    #[test]
    fn text_line_error() {
        let result = ConnectorHealthResult {
            id: "pganalyze".to_owned(),
            name: "pganalyze".to_owned(),
            health: Err("auth error: invalid API key".to_owned()),
        };
        assert_eq!(
            format_text_line(&result),
            "pganalyze: error — auth error: invalid API key"
        );
    }

    // ------------------------------------------------------------------
    // format_json_entry tests
    // ------------------------------------------------------------------

    #[test]
    fn json_entry_connected() {
        let result = ConnectorHealthResult {
            id: "datadog".to_owned(),
            name: "Datadog".to_owned(),
            health: Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(45),
            }),
        };
        let v = format_json_entry(&result);
        assert_eq!(v["id"], "datadog");
        assert_eq!(v["status"], "connected");
        assert_eq!(v["latency_ms"], 45);
    }

    #[test]
    fn json_entry_error() {
        let result = ConnectorHealthResult {
            id: "pganalyze".to_owned(),
            name: "pganalyze".to_owned(),
            health: Err("auth error: invalid API key".to_owned()),
        };
        let v = format_json_entry(&result);
        assert_eq!(v["id"], "pganalyze");
        assert_eq!(v["status"], "error");
        assert_eq!(v["error"], "auth error: invalid API key");
    }

    #[test]
    fn json_entry_disconnected() {
        let result = ConnectorHealthResult {
            id: "cloudwatch".to_owned(),
            name: "CloudWatch".to_owned(),
            health: Ok(ConnectorHealth {
                connected: false,
                message: Some("not configured".to_owned()),
                latency_ms: None,
            }),
        };
        let v = format_json_entry(&result);
        assert_eq!(v["id"], "cloudwatch");
        assert_eq!(v["status"], "disconnected");
        assert_eq!(v["message"], "not configured");
    }
}
