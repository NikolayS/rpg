//! Mock and recording connector implementations for unit tests.
//!
//! Provides `MockConnector` (configurable canned responses),
//! `RecordingConnector` (records all method calls), and small
//! helper functions to build test fixtures with minimal boilerplate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    Alert, AlertStatus, BackoffConfig, ConnectorCapabilities, ConnectorError, ConnectorHealth,
    DatabaseId, IssueId, IssueRequest, IssueUpdate, Metric, RateLimitConfig, TimeWindow,
};
use crate::connectors::Connector;
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// MockConnector
// ---------------------------------------------------------------------------

/// A fully configurable mock `Connector` that returns canned responses.
///
/// Construct with [`MockConnector::new`] and chain the `with_*` builder
/// methods to set the desired responses for each operation.
///
/// # Example
///
/// ```rust,ignore
/// let mock = MockConnector::new("test", "Test")
///     .with_health(Ok(ConnectorHealth {
///         connected: true,
///         message: None,
///         latency_ms: Some(1),
///     }))
///     .with_metrics(vec![test_metric("cpu", 42.0)]);
/// ```
pub struct MockConnector {
    id: String,
    name: String,
    health_response: Result<ConnectorHealth, ConnectorError>,
    metrics_response: Result<Vec<Metric>, ConnectorError>,
    alerts_response: Result<Vec<Alert>, ConnectorError>,
    create_issue_response: Result<IssueId, ConnectorError>,
    update_issue_response: Result<(), ConnectorError>,
}

impl MockConnector {
    /// Create a new `MockConnector` with default OK responses.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            health_response: Ok(ConnectorHealth {
                connected: true,
                message: None,
                latency_ms: Some(1),
            }),
            metrics_response: Ok(vec![]),
            alerts_response: Ok(vec![]),
            create_issue_response: Ok("mock-issue-1".to_string()),
            update_issue_response: Ok(()),
        }
    }

    /// Override the response returned by [`Connector::health_check`].
    pub fn with_health(mut self, response: Result<ConnectorHealth, ConnectorError>) -> Self {
        self.health_response = response;
        self
    }

    /// Override the response returned by [`Connector::fetch_metrics`].
    pub fn with_metrics(mut self, metrics: Vec<Metric>) -> Self {
        self.metrics_response = Ok(metrics);
        self
    }

    /// Override the metrics response with an error.
    pub fn with_metrics_error(mut self, error: ConnectorError) -> Self {
        self.metrics_response = Err(error);
        self
    }

    /// Override the response returned by [`Connector::fetch_alerts`].
    pub fn with_alerts(mut self, alerts: Vec<Alert>) -> Self {
        self.alerts_response = Ok(alerts);
        self
    }

    /// Override the alerts response with an error.
    pub fn with_alerts_error(mut self, error: ConnectorError) -> Self {
        self.alerts_response = Err(error);
        self
    }

    /// Override the response returned by [`Connector::create_issue`].
    pub fn with_create_issue(mut self, response: Result<IssueId, ConnectorError>) -> Self {
        self.create_issue_response = response;
        self
    }

    /// Override the response returned by [`Connector::update_issue`].
    pub fn with_update_issue(mut self, response: Result<(), ConnectorError>) -> Self {
        self.update_issue_response = response;
        self
    }
}

#[async_trait]
impl Connector for MockConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            can_fetch_metrics: self.metrics_response.is_ok(),
            can_fetch_alerts: self.alerts_response.is_ok(),
            can_create_issues: self.create_issue_response.is_ok(),
            can_update_issues: self.update_issue_response.is_ok(),
            can_receive_webhooks: false,
            supports_pagination: false,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: 100.0,
            requests_per_minute: None,
            max_concurrent: 10,
            backoff: BackoffConfig::default(),
            respect_retry_after: false,
        }
    }

    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        match &self.health_response {
            Ok(h) => Ok(h.clone()),
            Err(e) => Err(mirror_error(e)),
        }
    }

    async fn fetch_metrics(
        &self,
        _database: &DatabaseId,
        _window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        match &self.metrics_response {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(mirror_error(e)),
        }
    }

    async fn fetch_alerts(&self, _database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        match &self.alerts_response {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(mirror_error(e)),
        }
    }

    async fn create_issue(&self, _issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        match &self.create_issue_response {
            Ok(id) => Ok(id.clone()),
            Err(e) => Err(mirror_error(e)),
        }
    }

    async fn update_issue(
        &self,
        _id: &IssueId,
        _update: &IssueUpdate,
    ) -> Result<(), ConnectorError> {
        match &self.update_issue_response {
            Ok(()) => Ok(()),
            Err(e) => Err(mirror_error(e)),
        }
    }
}

/// Reconstruct a `ConnectorError` from a reference so `MockConnector` fields
/// can be `Result<T, ConnectorError>` while still implementing `Clone`-like
/// semantics for the response.
fn mirror_error(e: &ConnectorError) -> ConnectorError {
    match e {
        ConnectorError::NotSupported(op) => ConnectorError::NotSupported(op),
        ConnectorError::AuthError(msg) => ConnectorError::AuthError(msg.clone()),
        ConnectorError::RateLimited { retry_after_ms } => ConnectorError::RateLimited {
            retry_after_ms: *retry_after_ms,
        },
        ConnectorError::NetworkError(msg) => ConnectorError::NetworkError(msg.clone()),
        ConnectorError::ApiError { status, message } => ConnectorError::ApiError {
            status: *status,
            message: message.clone(),
        },
        ConnectorError::Other(msg) => ConnectorError::Other(msg.clone()),
    }
}

// ---------------------------------------------------------------------------
// RecordedCall
// ---------------------------------------------------------------------------

/// A single recorded invocation of a `Connector` method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    HealthCheck,
    FetchMetrics { database: String },
    FetchAlerts { database: String },
    CreateIssue { title: String },
    UpdateIssue { id: String },
}

// ---------------------------------------------------------------------------
// RecordingConnector
// ---------------------------------------------------------------------------

/// A `Connector` wrapper that records every method call before delegating
/// to the inner connector.
///
/// Use [`RecordingConnector::calls`] to inspect the recorded calls after
/// running code under test.
pub struct RecordingConnector<C: Connector> {
    inner: C,
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl<C: Connector> RecordingConnector<C> {
    /// Wrap `inner` with call recording.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Return a snapshot of all calls made so far.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("calls mutex poisoned").clone()
    }

    /// Return a shared handle to the calls list.
    ///
    /// Useful when you need to inspect calls from a different thread.
    #[allow(dead_code)]
    pub fn calls_arc(&self) -> Arc<Mutex<Vec<RecordedCall>>> {
        Arc::clone(&self.calls)
    }

    fn record(&self, call: RecordedCall) {
        self.calls.lock().expect("calls mutex poisoned").push(call);
    }
}

#[async_trait]
impl<C: Connector> Connector for RecordingConnector<C> {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        self.inner.capabilities()
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        self.inner.rate_limit_config()
    }

    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        self.record(RecordedCall::HealthCheck);
        self.inner.health_check().await
    }

    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        self.record(RecordedCall::FetchMetrics {
            database: database.clone(),
        });
        self.inner.fetch_metrics(database, window).await
    }

    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        self.record(RecordedCall::FetchAlerts {
            database: database.clone(),
        });
        self.inner.fetch_alerts(database).await
    }

    async fn create_issue(&self, issue: &IssueRequest) -> Result<IssueId, ConnectorError> {
        self.record(RecordedCall::CreateIssue {
            title: issue.title.clone(),
        });
        self.inner.create_issue(issue).await
    }

    async fn update_issue(&self, id: &IssueId, update: &IssueUpdate) -> Result<(), ConnectorError> {
        self.record(RecordedCall::UpdateIssue { id: id.clone() });
        self.inner.update_issue(id, update).await
    }
}

// ---------------------------------------------------------------------------
// Test-fixture helpers
// ---------------------------------------------------------------------------

/// Build a minimal `Metric` for use in tests.
pub fn test_metric(name: &str, value: f64) -> Metric {
    Metric {
        name: name.to_string(),
        value,
        unit: None,
        timestamp: std::time::SystemTime::UNIX_EPOCH,
        tags: HashMap::new(),
        source: "test".to_string(),
    }
}

/// Build a minimal `Alert` for use in tests.
pub fn test_alert(id: &str, title: &str, severity: Severity) -> Alert {
    Alert {
        id: id.to_string(),
        title: title.to_string(),
        severity,
        status: AlertStatus::Active,
        source: "test".to_string(),
        database: None,
        created_at: std::time::SystemTime::UNIX_EPOCH,
        url: None,
    }
}

/// Build a minimal `IssueRequest` for use in tests.
pub fn test_issue_request(title: &str) -> IssueRequest {
    IssueRequest {
        title: title.to_string(),
        body: String::new(),
        labels: vec![],
        assignees: vec![],
        metadata: HashMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // MockConnector tests
    // ------------------------------------------------------------------

    #[test]
    fn mock_connector_id_and_name() {
        let mock = MockConnector::new("my-id", "My Name");
        assert_eq!(mock.id(), "my-id");
        assert_eq!(mock.name(), "My Name");
    }

    #[tokio::test]
    async fn mock_connector_default_health_ok() {
        let mock = MockConnector::new("test", "Test");
        let health = mock.health_check().await.unwrap();
        assert!(health.connected);
        assert_eq!(health.latency_ms, Some(1));
    }

    #[tokio::test]
    async fn mock_connector_health_error() {
        let mock = MockConnector::new("test", "Test")
            .with_health(Err(ConnectorError::NetworkError("unreachable".to_string())));
        let result = mock.health_check().await;
        assert!(matches!(result, Err(ConnectorError::NetworkError(_))));
    }

    #[tokio::test]
    async fn mock_connector_with_metrics() {
        let mock = MockConnector::new("test", "Test")
            .with_metrics(vec![test_metric("cpu", 55.0), test_metric("mem", 70.0)]);
        let window = TimeWindow {
            start: std::time::SystemTime::UNIX_EPOCH,
            end: std::time::SystemTime::now(),
        };
        let metrics = mock
            .fetch_metrics(&"db".to_string(), &window)
            .await
            .unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].name, "cpu");
        assert!((metrics[0].value - 55.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn mock_connector_metrics_error() {
        let mock = MockConnector::new("test", "Test")
            .with_metrics_error(ConnectorError::AuthError("bad key".to_string()));
        let window = TimeWindow {
            start: std::time::SystemTime::UNIX_EPOCH,
            end: std::time::SystemTime::now(),
        };
        let result = mock.fetch_metrics(&"db".to_string(), &window).await;
        assert!(matches!(result, Err(ConnectorError::AuthError(_))));
    }

    #[tokio::test]
    async fn mock_connector_with_alerts() {
        let mock = MockConnector::new("test", "Test").with_alerts(vec![test_alert(
            "a-1",
            "High CPU",
            Severity::Warning,
        )]);
        let alerts = mock.fetch_alerts(&"db".to_string()).await.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].id, "a-1");
        assert_eq!(alerts[0].severity, Severity::Warning);
    }

    #[tokio::test]
    async fn mock_connector_alerts_error() {
        let mock =
            MockConnector::new("test", "Test").with_alerts_error(ConnectorError::RateLimited {
                retry_after_ms: Some(500),
            });
        let result = mock.fetch_alerts(&"db".to_string()).await;
        assert!(matches!(
            result,
            Err(ConnectorError::RateLimited {
                retry_after_ms: Some(500)
            })
        ));
    }

    #[tokio::test]
    async fn mock_connector_create_issue_default_ok() {
        let mock = MockConnector::new("test", "Test");
        let req = test_issue_request("Disk filling up");
        let id = mock.create_issue(&req).await.unwrap();
        assert_eq!(id, "mock-issue-1");
    }

    #[tokio::test]
    async fn mock_connector_create_issue_custom() {
        let mock =
            MockConnector::new("test", "Test").with_create_issue(Ok("custom-id-99".to_string()));
        let req = test_issue_request("title");
        let id = mock.create_issue(&req).await.unwrap();
        assert_eq!(id, "custom-id-99");
    }

    #[tokio::test]
    async fn mock_connector_update_issue_ok() {
        let mock = MockConnector::new("test", "Test");
        let update = IssueUpdate {
            title: None,
            body: None,
            status: Some("closed".to_string()),
            labels: None,
        };
        let result = mock.update_issue(&"issue-1".to_string(), &update).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn mock_connector_update_issue_error() {
        let mock =
            MockConnector::new("test", "Test").with_update_issue(Err(ConnectorError::ApiError {
                status: 404,
                message: "not found".to_string(),
            }));
        let update = IssueUpdate {
            title: None,
            body: None,
            status: None,
            labels: None,
        };
        let result = mock.update_issue(&"issue-x".to_string(), &update).await;
        assert!(matches!(
            result,
            Err(ConnectorError::ApiError { status: 404, .. })
        ));
    }

    // ------------------------------------------------------------------
    // RecordingConnector tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn recording_connector_records_health_check() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        recorder.health_check().await.unwrap();
        assert_eq!(recorder.calls(), vec![RecordedCall::HealthCheck]);
    }

    #[tokio::test]
    async fn recording_connector_records_fetch_metrics() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        let window = TimeWindow {
            start: std::time::SystemTime::UNIX_EPOCH,
            end: std::time::SystemTime::now(),
        };
        recorder
            .fetch_metrics(&"mydb".to_string(), &window)
            .await
            .unwrap();
        assert_eq!(
            recorder.calls(),
            vec![RecordedCall::FetchMetrics {
                database: "mydb".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn recording_connector_records_fetch_alerts() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        recorder.fetch_alerts(&"mydb".to_string()).await.unwrap();
        assert_eq!(
            recorder.calls(),
            vec![RecordedCall::FetchAlerts {
                database: "mydb".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn recording_connector_records_create_issue() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        let req = test_issue_request("My Issue");
        recorder.create_issue(&req).await.unwrap();
        assert_eq!(
            recorder.calls(),
            vec![RecordedCall::CreateIssue {
                title: "My Issue".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn recording_connector_records_update_issue() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        let update = IssueUpdate {
            title: None,
            body: None,
            status: None,
            labels: None,
        };
        recorder
            .update_issue(&"iss-42".to_string(), &update)
            .await
            .unwrap();
        assert_eq!(
            recorder.calls(),
            vec![RecordedCall::UpdateIssue {
                id: "iss-42".to_string()
            }]
        );
    }

    #[tokio::test]
    async fn recording_connector_accumulates_multiple_calls() {
        let recorder = RecordingConnector::new(MockConnector::new("rec", "Rec"));
        let window = TimeWindow {
            start: std::time::SystemTime::UNIX_EPOCH,
            end: std::time::SystemTime::now(),
        };
        recorder.health_check().await.unwrap();
        recorder
            .fetch_metrics(&"db1".to_string(), &window)
            .await
            .unwrap();
        recorder.fetch_alerts(&"db1".to_string()).await.unwrap();
        let calls = recorder.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], RecordedCall::HealthCheck);
        assert_eq!(
            calls[1],
            RecordedCall::FetchMetrics {
                database: "db1".to_string()
            }
        );
        assert_eq!(
            calls[2],
            RecordedCall::FetchAlerts {
                database: "db1".to_string()
            }
        );
    }

    #[tokio::test]
    async fn recording_connector_still_returns_inner_result() {
        let inner =
            MockConnector::new("rec", "Rec").with_metrics(vec![test_metric("latency", 12.5)]);
        let recorder = RecordingConnector::new(inner);
        let window = TimeWindow {
            start: std::time::SystemTime::UNIX_EPOCH,
            end: std::time::SystemTime::now(),
        };
        let metrics = recorder
            .fetch_metrics(&"db".to_string(), &window)
            .await
            .unwrap();
        assert_eq!(metrics.len(), 1);
        assert!((metrics[0].value - 12.5).abs() < f64::EPSILON);
    }

    // ------------------------------------------------------------------
    // Helper tests
    // ------------------------------------------------------------------

    #[test]
    fn test_metric_helper() {
        let m = test_metric("connections", 42.0);
        assert_eq!(m.name, "connections");
        assert!((m.value - 42.0).abs() < f64::EPSILON);
        assert_eq!(m.source, "test");
        assert!(m.unit.is_none());
        assert!(m.tags.is_empty());
    }

    #[test]
    fn test_alert_helper() {
        let a = test_alert("alert-1", "Replication Lag", Severity::Critical);
        assert_eq!(a.id, "alert-1");
        assert_eq!(a.title, "Replication Lag");
        assert_eq!(a.severity, Severity::Critical);
        assert_eq!(a.status, AlertStatus::Active);
        assert_eq!(a.source, "test");
        assert!(a.database.is_none());
        assert!(a.url.is_none());
    }

    #[test]
    fn test_issue_request_helper() {
        let req = test_issue_request("Disk filling up");
        assert_eq!(req.title, "Disk filling up");
        assert!(req.body.is_empty());
        assert!(req.labels.is_empty());
        assert!(req.assignees.is_empty());
        assert!(req.metadata.is_empty());
    }
}
