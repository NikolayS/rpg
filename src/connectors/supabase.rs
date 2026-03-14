//! Supabase connector — health checks via the Supabase Management API.

use std::time::SystemTime;

use async_trait::async_trait;

use super::{
    Alert, BackoffConfig, ConnectorCapabilities, ConnectorError, ConnectorHealth, DatabaseId,
    Metric, RateLimitConfig, TimeWindow,
};
use crate::connectors::Connector;

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

    /// Fetch metrics for a database.
    ///
    /// Not yet implemented — returns an empty list.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
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
        crate::logging::debug(
            "supabase",
            &format!(
                "fetch_metrics db={database} window={start_secs}..{end_secs} \
                 (not yet implemented)",
            ),
        );
        Ok(vec![])
    }

    /// Fetch alerts for a database.
    ///
    /// Not yet implemented — returns an empty list.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        crate::logging::debug(
            "supabase",
            &format!("fetch_alerts db={database} (not yet implemented)"),
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
    use crate::connectors::{Connector, ConnectorCapabilities, RateLimitConfig};

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

    #[tokio::test]
    async fn fetch_metrics_returns_empty() {
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

    #[tokio::test]
    async fn fetch_alerts_returns_empty() {
        let c = SupabaseConnector::new("t".to_string());
        let result = c.fetch_alerts(&"mydb".to_string()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
