//! Connector trait and core types for external system integration (Phase 4).
//!
//! All external connectors (Datadog, pganalyze, `CloudWatch`, etc.)
//! implement the `Connector` trait defined here.

#![allow(dead_code)] // Phase 4 infrastructure — consumers arrive later

use std::collections::HashMap;

use async_trait::async_trait;

use crate::governance::Severity;

pub mod cloudwatch;
pub mod datadog;
pub mod pganalyze;
pub mod postgresai;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Unique connector identifier (e.g., "datadog", "pganalyze").
pub type ConnectorId = String;

/// Database identifier for multi-database environments.
pub type DatabaseId = String;

/// Issue identifier returned by issue trackers.
pub type IssueId = String;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConnectorError {
    /// The requested operation is not supported by this connector.
    NotSupported(&'static str),
    /// Authentication failed.
    AuthError(String),
    /// Rate limit exceeded.
    RateLimited { retry_after_ms: Option<u64> },
    /// Network or HTTP error.
    NetworkError(String),
    /// API returned an error.
    ApiError { status: u16, message: String },
    /// Other error.
    Other(String),
}

impl std::fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSupported(op) => write!(f, "operation not supported: {op}"),
            Self::AuthError(msg) => write!(f, "auth error: {msg}"),
            Self::RateLimited { retry_after_ms } => {
                write!(f, "rate limited")?;
                if let Some(ms) = retry_after_ms {
                    write!(f, " (retry after {ms}ms)")?;
                }
                Ok(())
            }
            Self::NetworkError(msg) => write!(f, "network error: {msg}"),
            Self::ApiError { status, message } => write!(f, "API error {status}: {message}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ConnectorError {}

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A single metric data point from an external source.
#[derive(Debug, Clone)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: Option<String>,
    pub timestamp: std::time::SystemTime,
    pub tags: HashMap<String, String>,
    pub source: ConnectorId,
}

/// An alert from an external monitoring system.
#[derive(Debug, Clone)]
pub struct Alert {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub status: AlertStatus,
    pub source: ConnectorId,
    pub database: Option<DatabaseId>,
    pub created_at: std::time::SystemTime,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertStatus {
    Active,
    Acknowledged,
    Resolved,
}

/// Request to create an issue in an external tracker.
#[derive(Debug, Clone)]
pub struct IssueRequest {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Update to an existing issue.
#[derive(Debug, Clone)]
pub struct IssueUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: Option<String>,
    pub labels: Option<Vec<String>>,
}

/// Health status of a connector.
#[derive(Debug, Clone)]
pub struct ConnectorHealth {
    pub connected: bool,
    pub message: Option<String>,
    pub latency_ms: Option<u64>,
}

/// Capabilities advertised by a connector.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct ConnectorCapabilities {
    pub can_fetch_metrics: bool,
    pub can_fetch_alerts: bool,
    pub can_create_issues: bool,
    pub can_update_issues: bool,
    pub can_receive_webhooks: bool,
    pub supports_pagination: bool,
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub requests_per_second: f64,
    pub requests_per_minute: Option<u32>,
    pub max_concurrent: u32,
    pub backoff: BackoffConfig,
    pub respect_retry_after: bool,
}

#[derive(Debug, Clone)]
pub struct BackoffConfig {
    pub initial_delay_ms: u64,
    pub multiplier: f64,
    pub max_delay_ms: u64,
    pub jitter: bool,
    pub max_retries: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay_ms: 1000,
            multiplier: 2.0,
            max_delay_ms: 60_000,
            jitter: true,
            max_retries: 5,
        }
    }
}

// ---------------------------------------------------------------------------
// Connector trait
// ---------------------------------------------------------------------------

/// Time window for metric queries.
#[derive(Debug, Clone)]
pub struct TimeWindow {
    pub start: std::time::SystemTime,
    pub end: std::time::SystemTime,
}

/// Common abstraction for all external connectors.
///
/// Concrete implementations (Datadog, pganalyze, `CloudWatch`, etc.)
/// will be added in Phase 4.
#[async_trait]
pub trait Connector: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn capabilities(&self) -> ConnectorCapabilities;
    fn rate_limit_config(&self) -> RateLimitConfig;

    /// Check whether the connector can reach the external service.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError>;

    /// Fetch metric data points for a database over the given window.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError>;

    /// Fetch active alerts for a database.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError>;

    /// Create an issue in the external tracker.
    ///
    /// Returns [`ConnectorError::NotSupported`] by default; connectors that
    /// support issue creation should override this.
    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        let _ = issue;
        Err(ConnectorError::NotSupported("create_issue"))
    }

    /// Update an existing issue in the external tracker.
    ///
    /// Returns [`ConnectorError::NotSupported`] by default; connectors that
    /// support issue updates should override this.
    async fn update_issue(&self, id: &IssueId, update: &IssueUpdate) -> Result<(), ConnectorError> {
        let _ = (id, update);
        Err(ConnectorError::NotSupported("update_issue"))
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Runtime registry of registered connectors.
pub struct ConnectorRegistry {
    connectors: Vec<Box<dyn Connector>>,
}

impl ConnectorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            connectors: Vec::new(),
        }
    }

    /// Register a connector.
    pub fn register(&mut self, connector: Box<dyn Connector>) {
        self.connectors.push(connector);
    }

    /// Look up a connector by its [`Connector::id`].
    pub fn get(&self, id: &str) -> Option<&dyn Connector> {
        self.connectors
            .iter()
            .find(|c| c.id() == id)
            .map(std::convert::AsRef::as_ref)
    }

    /// Return all registered connectors in registration order.
    pub fn list(&self) -> &[Box<dyn Connector>] {
        &self.connectors
    }
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_config_defaults() {
        let cfg = BackoffConfig::default();
        assert_eq!(cfg.initial_delay_ms, 1000);
        assert_eq!(cfg.multiplier, 2.0);
        assert_eq!(cfg.max_delay_ms, 60_000);
        assert!(cfg.jitter);
        assert_eq!(cfg.max_retries, 5);
    }

    #[test]
    fn connector_error_display_not_supported() {
        let err = ConnectorError::NotSupported("fetch_metrics");
        assert_eq!(err.to_string(), "operation not supported: fetch_metrics");
    }

    #[test]
    fn connector_error_display_auth_error() {
        let err = ConnectorError::AuthError("invalid API key".to_string());
        assert_eq!(err.to_string(), "auth error: invalid API key");
    }

    #[test]
    fn connector_error_display_rate_limited_no_retry() {
        let err = ConnectorError::RateLimited {
            retry_after_ms: None,
        };
        assert_eq!(err.to_string(), "rate limited");
    }

    #[test]
    fn connector_error_display_rate_limited_with_retry() {
        let err = ConnectorError::RateLimited {
            retry_after_ms: Some(5000),
        };
        assert_eq!(err.to_string(), "rate limited (retry after 5000ms)");
    }

    #[test]
    fn connector_error_display_api_error() {
        let err = ConnectorError::ApiError {
            status: 429,
            message: "Too Many Requests".to_string(),
        };
        assert_eq!(err.to_string(), "API error 429: Too Many Requests");
    }

    #[test]
    fn connector_error_display_network_error() {
        let err = ConnectorError::NetworkError("connection refused".to_string());
        assert_eq!(err.to_string(), "network error: connection refused");
    }

    #[test]
    fn connector_error_display_other() {
        let err = ConnectorError::Other("something went wrong".to_string());
        assert_eq!(err.to_string(), "something went wrong");
    }

    // ------------------------------------------------------------------
    // ConnectorRegistry tests
    // ------------------------------------------------------------------

    struct StubConnector {
        id: &'static str,
        name: &'static str,
    }

    #[async_trait]
    impl Connector for StubConnector {
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
                respect_retry_after: true,
            }
        }

        async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
            Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(1),
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

    #[test]
    fn registry_register_and_list() {
        let mut registry = ConnectorRegistry::new();
        assert!(registry.list().is_empty());

        registry.register(Box::new(StubConnector {
            id: "stub-a",
            name: "Stub A",
        }));
        registry.register(Box::new(StubConnector {
            id: "stub-b",
            name: "Stub B",
        }));

        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn registry_get_existing() {
        let mut registry = ConnectorRegistry::new();
        registry.register(Box::new(StubConnector {
            id: "stub-a",
            name: "Stub A",
        }));

        let found = registry.get("stub-a");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id(), "stub-a");
        assert_eq!(found.unwrap().name(), "Stub A");
    }

    #[test]
    fn registry_get_missing() {
        let registry = ConnectorRegistry::new();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_default_is_empty() {
        let registry = ConnectorRegistry::default();
        assert!(registry.list().is_empty());
    }

    #[tokio::test]
    async fn default_create_issue_returns_not_supported() {
        let connector = StubConnector {
            id: "stub",
            name: "Stub",
        };
        let req = IssueRequest {
            title: "test".to_string(),
            body: "body".to_string(),
            labels: vec![],
            assignees: vec![],
            metadata: HashMap::new(),
        };
        let result = connector.create_issue(&req).await;
        assert!(matches!(
            result,
            Err(ConnectorError::NotSupported("create_issue"))
        ));
    }

    #[tokio::test]
    async fn default_update_issue_returns_not_supported() {
        let connector = StubConnector {
            id: "stub",
            name: "Stub",
        };
        let id = "issue-1".to_string();
        let update = IssueUpdate {
            title: None,
            body: None,
            status: None,
            labels: None,
        };
        let result = connector.update_issue(&id, &update).await;
        assert!(matches!(
            result,
            Err(ConnectorError::NotSupported("update_issue"))
        ));
    }
}
