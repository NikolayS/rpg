//! Connector trait and core types for external system integration (Phase 4).
//!
//! All external connectors (Datadog, pganalyze, `CloudWatch`, etc.)
//! implement the `Connector` trait defined here.

#![allow(dead_code)] // Phase 4 infrastructure — consumers arrive later

use std::collections::HashMap;

use crate::governance::Severity;

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
pub trait Connector: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn capabilities(&self) -> ConnectorCapabilities;
    fn rate_limit_config(&self) -> RateLimitConfig;
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
}
