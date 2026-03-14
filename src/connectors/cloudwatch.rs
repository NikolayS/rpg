//! AWS `CloudWatch` connector (Phase 4).
//!
//! Fetches metrics and alarms from AWS `CloudWatch` using raw HTTP.
//! AWS Signature V4 signing is required for real API calls — the
//! HTTP request structure is built here but signing is stubbed until
//! a lightweight `SigV4` implementation is wired in.

#![allow(dead_code)] // Phase 4 infrastructure — consumers arrive later

use async_trait::async_trait;

use super::{
    Alert, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError, ConnectorHealth,
    DatabaseId, Metric, RateLimitConfig, TimeWindow,
};

// ---------------------------------------------------------------------------
// CloudWatchConnector
// ---------------------------------------------------------------------------

/// Connector for AWS `CloudWatch` metrics and alarms.
///
/// Communicates with the `CloudWatch` Monitoring API endpoint:
/// `https://monitoring.<region>.amazonaws.com`
///
/// Authentication uses AWS access key credentials; temporary credentials
/// (e.g. from IAM role assumption) are supported via the optional
/// session token.
pub struct CloudWatchConnector {
    region: String,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    db_instance_id: Option<String>,
}

impl CloudWatchConnector {
    /// Create a new connector with long-term IAM credentials.
    pub fn new(region: String, access_key_id: String, secret_access_key: String) -> Self {
        Self {
            region,
            access_key_id,
            secret_access_key,
            session_token: None,
            db_instance_id: None,
        }
    }

    /// Attach a session token for temporary credentials (STS / IAM role).
    pub fn with_session_token(mut self, token: String) -> Self {
        self.session_token = Some(token);
        self
    }

    /// Scope all metric/alarm queries to a specific RDS DB instance.
    pub fn with_db_instance(mut self, id: String) -> Self {
        self.db_instance_id = Some(id);
        self
    }

    /// Base URL of the `CloudWatch` Monitoring API for this region.
    fn endpoint(&self) -> String {
        format!("https://monitoring.{}.amazonaws.com", self.region)
    }
}

// ---------------------------------------------------------------------------
// Connector trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for CloudWatchConnector {
    fn id(&self) -> &'static str {
        "cloudwatch"
    }

    fn name(&self) -> &'static str {
        "AWS CloudWatch"
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
            requests_per_second: 5.0,
            requests_per_minute: Some(300),
            max_concurrent: 5,
            backoff: BackoffConfig::default(),
            respect_retry_after: true,
        }
    }

    /// Validate connectivity.
    ///
    /// `CloudWatch` does not provide a dedicated ping endpoint; credential
    /// validation is deferred to the first real API call.  This method
    /// returns `connected: true` immediately so that the connector can be
    /// registered without performing a network round-trip at startup.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        Ok(ConnectorHealth {
            connected: true,
            message: Some("credential validation deferred to first API call".to_string()),
            latency_ms: None,
        })
    }

    /// Fetch `CloudWatch` metric data points for `database` over `window`.
    ///
    /// Constructs the `GetMetricData` request body (Query API, POST to `/`).
    /// Real execution requires AWS Signature V4 request signing which is not
    /// yet implemented — the method returns an empty `Vec` and logs a debug
    /// message until signing support is wired in.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        // Build the GetMetricData query parameters.
        // These would be POST'd to the endpoint once SigV4 signing is added.
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

        let _request_params = format!(
            "Action=GetMetricData\
             &Version=2010-08-01\
             &MetricDataQueries.member.1.Id=cpu\
             &MetricDataQueries.member.1.MetricStat.Metric.Namespace=AWS/RDS\
             &MetricDataQueries.member.1.MetricStat.Metric.MetricName=CPUUtilization\
             &MetricDataQueries.member.1.MetricStat.Metric.Dimensions.member.1.Name=DBInstanceIdentifier\
             &MetricDataQueries.member.1.MetricStat.Metric.Dimensions.member.1.Value={database}\
             &MetricDataQueries.member.1.MetricStat.Period=60\
             &MetricDataQueries.member.1.MetricStat.Stat=Average\
             &StartTime={start_secs}\
             &EndTime={end_secs}"
        );

        // TODO(#463): sign the request with AWS Signature V4 and execute via
        // reqwest, then parse the XML response into Vec<Metric>.
        crate::logging::debug(
            "cloudwatch",
            &format!(
                "fetch_metrics db={database} endpoint={} \
                 — SigV4 signing not yet implemented",
                self.endpoint()
            ),
        );

        Ok(vec![])
    }

    /// Fetch `CloudWatch` alarms for `database`.
    ///
    /// Constructs the `DescribeAlarms` request body (Query API, POST to `/`).
    /// Real execution requires AWS Signature V4 request signing which is not
    /// yet implemented — the method returns an empty `Vec` and logs a debug
    /// message until signing support is wired in.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        // Build the DescribeAlarms query parameters.
        // These would be POST'd to the endpoint once SigV4 signing is added.
        let _request_params = format!(
            "Action=DescribeAlarms\
             &Version=2010-08-01\
             &AlarmNamePrefix={database}-\
             &StateValue=ALARM"
        );

        // TODO(#463): sign the request with AWS Signature V4 and execute via
        // reqwest, then parse the XML response into Vec<Alert>.
        crate::logging::debug(
            "cloudwatch",
            &format!(
                "fetch_alerts db={database} endpoint={} \
                 — SigV4 signing not yet implemented",
                self.endpoint()
            ),
        );

        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::Connector;

    fn make_connector() -> CloudWatchConnector {
        CloudWatchConnector::new(
            "us-east-1".to_string(),
            "AKIAIOSFODNN7EXAMPLE".to_string(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        )
    }

    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    #[test]
    fn new_stores_credentials() {
        let c = make_connector();
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.access_key_id, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(
            c.secret_access_key,
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
        );
        assert!(c.session_token.is_none());
        assert!(c.db_instance_id.is_none());
    }

    #[test]
    fn with_session_token_sets_token() {
        let c = make_connector().with_session_token("my-session-token".to_string());
        assert_eq!(c.session_token.as_deref(), Some("my-session-token"));
    }

    #[test]
    fn with_db_instance_sets_id() {
        let c = make_connector().with_db_instance("prod-pg-01".to_string());
        assert_eq!(c.db_instance_id.as_deref(), Some("prod-pg-01"));
    }

    #[test]
    fn builder_pattern_chaining() {
        let c = make_connector()
            .with_session_token("tok".to_string())
            .with_db_instance("db-id".to_string());
        assert_eq!(c.session_token.as_deref(), Some("tok"));
        assert_eq!(c.db_instance_id.as_deref(), Some("db-id"));
    }

    // ------------------------------------------------------------------
    // Endpoint
    // ------------------------------------------------------------------

    #[test]
    fn endpoint_us_east_1() {
        let c = make_connector();
        assert_eq!(c.endpoint(), "https://monitoring.us-east-1.amazonaws.com");
    }

    #[test]
    fn endpoint_eu_west_2() {
        let c = CloudWatchConnector::new(
            "eu-west-2".to_string(),
            "key".to_string(),
            "secret".to_string(),
        );
        assert_eq!(c.endpoint(), "https://monitoring.eu-west-2.amazonaws.com");
    }

    // ------------------------------------------------------------------
    // Connector trait — identity
    // ------------------------------------------------------------------

    #[test]
    fn id_is_cloudwatch() {
        assert_eq!(make_connector().id(), "cloudwatch");
    }

    #[test]
    fn name_is_aws_cloudwatch() {
        assert_eq!(make_connector().name(), "AWS CloudWatch");
    }

    // ------------------------------------------------------------------
    // Connector trait — capabilities
    // ------------------------------------------------------------------

    #[test]
    fn capabilities_metrics_and_alerts() {
        let caps = make_connector().capabilities();
        assert!(caps.can_fetch_metrics);
        assert!(caps.can_fetch_alerts);
        assert!(!caps.can_create_issues);
        assert!(!caps.can_update_issues);
        assert!(!caps.can_receive_webhooks);
        assert!(!caps.supports_pagination);
    }

    // ------------------------------------------------------------------
    // Connector trait — rate limiting
    // ------------------------------------------------------------------

    #[test]
    fn rate_limit_config_values() {
        let rl = make_connector().rate_limit_config();
        assert!((rl.requests_per_second - 5.0).abs() < f64::EPSILON);
        assert_eq!(rl.requests_per_minute, Some(300));
        assert_eq!(rl.max_concurrent, 5);
        assert!(rl.respect_retry_after);
    }

    // ------------------------------------------------------------------
    // Connector trait — async methods
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn health_check_returns_connected() {
        let health = make_connector().health_check().await.unwrap();
        assert!(health.connected);
        assert!(health.message.is_some());
    }

    #[tokio::test]
    async fn fetch_metrics_returns_empty_vec() {
        let window = TimeWindow {
            start: std::time::UNIX_EPOCH,
            end: std::time::UNIX_EPOCH + std::time::Duration::from_secs(3600),
        };
        let metrics = make_connector()
            .fetch_metrics(&"test-db".to_string(), &window)
            .await
            .unwrap();
        assert!(metrics.is_empty());
    }

    #[tokio::test]
    async fn fetch_alerts_returns_empty_vec() {
        let alerts = make_connector()
            .fetch_alerts(&"test-db".to_string())
            .await
            .unwrap();
        assert!(alerts.is_empty());
    }
}
