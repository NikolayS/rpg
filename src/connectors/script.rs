//! Script connector — invokes an external process with JSON stdin/stdout
//! protocol to integrate arbitrary monitoring sources.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{
    Alert, AlertStatus, BackoffConfig, Connector, ConnectorCapabilities, ConnectorError,
    ConnectorHealth, DatabaseId, Metric, RateLimitConfig, TimeWindow,
};
use crate::governance::Severity;

// ---------------------------------------------------------------------------
// Connector struct
// ---------------------------------------------------------------------------

/// Script connector — invokes an external process with JSON stdin/stdout
/// protocol.
///
/// The command receives a JSON object on stdin and must write a JSON response
/// to stdout before exiting.  A non-zero exit code is treated as an error.
pub struct ScriptConnector {
    connector_id: String,
    connector_name: String,
    /// Command to invoke, e.g. `["python3", "/etc/rpg/connectors/custom.py"]`.
    command: Vec<String>,
    /// Timeout for each script invocation (default: 30 seconds).
    timeout_seconds: u64,
    /// Maximum request rate (default: 1.0 RPS).
    rate_limit_rps: f64,
    /// Whether we have already logged the external-execution warning.
    warned_about_external: bool,
}

impl ScriptConnector {
    /// Create a new `ScriptConnector` with default timeout (30 s) and rate
    /// limit (1.0 RPS).
    pub fn new(id: String, name: String, command: Vec<String>) -> Self {
        Self {
            connector_id: id,
            connector_name: name,
            command,
            timeout_seconds: 30,
            rate_limit_rps: 1.0,
            warned_about_external: false,
        }
    }

    /// Override the per-invocation timeout.
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    /// Override the rate-limit setting.
    pub fn with_rate_limit(mut self, rps: f64) -> Self {
        self.rate_limit_rps = rps;
        self
    }

    // -----------------------------------------------------------------------
    // Core script invocation
    // -----------------------------------------------------------------------

    /// Invoke the configured script, writing `input` as JSON to stdin and
    /// returning the parsed JSON from stdout.
    ///
    /// # Errors
    ///
    /// - [`ConnectorError::Other`] if the command list is empty or the process
    ///   cannot be spawned.
    /// - [`ConnectorError::Other`] if the invocation times out.
    /// - [`ConnectorError::ApiError`] if the process exits with a non-zero
    ///   status code.
    /// - [`ConnectorError::Other`] if stdout is not valid JSON.
    async fn invoke_script(
        &self,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, ConnectorError> {
        let (program, args) = self
            .command
            .split_first()
            .ok_or_else(|| ConnectorError::Other("script command list is empty".to_string()))?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ConnectorError::Other(format!("failed to spawn script process: {e}")))?;

        // Write JSON to stdin.
        let stdin_payload =
            serde_json::to_vec(input).map_err(|e| ConnectorError::Other(e.to_string()))?;

        if let Some(mut child_stdin) = child.stdin.take() {
            child_stdin.write_all(&stdin_payload).await.map_err(|e| {
                ConnectorError::Other(format!("failed to write to script stdin: {e}"))
            })?;
            // Drop child_stdin so the child receives EOF.
        }

        // Wait for the process to exit, honouring the timeout.
        let timeout_dur = Duration::from_secs(self.timeout_seconds);
        let output = tokio::time::timeout(timeout_dur, child.wait_with_output())
            .await
            .map_err(|_| {
                ConnectorError::Other(format!(
                    "script timed out after {} seconds",
                    self.timeout_seconds
                ))
            })?
            .map_err(|e| ConnectorError::Other(format!("script process error: {e}")))?;

        // Non-zero exit code → API-style error carrying stderr.
        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(ConnectorError::ApiError {
                status: u16::try_from(code.unsigned_abs()).unwrap_or(u16::MAX),
                message: if stderr.is_empty() {
                    format!("script exited with code {code}")
                } else {
                    stderr
                },
            });
        }

        // Parse stdout as JSON.
        serde_json::from_slice(&output.stdout)
            .map_err(|e| ConnectorError::Other(format!("script returned invalid JSON: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Connector trait
// ---------------------------------------------------------------------------

#[async_trait]
impl Connector for ScriptConnector {
    fn id(&self) -> &str {
        &self.connector_id
    }

    fn name(&self) -> &str {
        &self.connector_name
    }

    fn capabilities(&self) -> ConnectorCapabilities {
        // A script connector can implement any capability the author desires.
        ConnectorCapabilities {
            can_fetch_metrics: true,
            can_fetch_alerts: true,
            can_create_issues: true,
            can_update_issues: true,
            can_receive_webhooks: true,
            supports_pagination: true,
        }
    }

    fn rate_limit_config(&self) -> RateLimitConfig {
        RateLimitConfig {
            requests_per_second: self.rate_limit_rps,
            requests_per_minute: None,
            max_concurrent: 1,
            backoff: BackoffConfig::default(),
            respect_retry_after: false,
        }
    }

    /// Run the script with `{"action": "health_check"}` and report whether it
    /// returns a response without error.
    async fn health_check(&self) -> Result<ConnectorHealth, ConnectorError> {
        let input = serde_json::json!({ "action": "health_check" });
        let start = std::time::Instant::now();

        let response = self.invoke_script(&input).await?;

        let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        // The script may return `{"ok": true/false, "message": "..."}`.
        let connected = response
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);

        let message = response
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map(String::from);

        Ok(ConnectorHealth {
            connected,
            message,
            latency_ms: Some(latency_ms),
        })
    }

    /// Run the script with a `fetch_metrics` action and parse the returned
    /// JSON array into [`Metric`] values.
    async fn fetch_metrics(
        &self,
        database: &DatabaseId,
        window: &TimeWindow,
    ) -> Result<Vec<Metric>, ConnectorError> {
        let start_iso = system_time_to_iso8601(window.start);
        let end_iso = system_time_to_iso8601(window.end);

        let input = serde_json::json!({
            "action": "fetch_metrics",
            "database_id": database,
            "window": {
                "start": start_iso,
                "end": end_iso,
            },
        });

        let response = self.invoke_script(&input).await?;
        let items = response.as_array().ok_or_else(|| {
            ConnectorError::Other("fetch_metrics: expected JSON array".to_string())
        })?;

        let mut metrics = Vec::with_capacity(items.len());
        for item in items {
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let value = item
                .get("value")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            let unit = item
                .get("unit")
                .and_then(serde_json::Value::as_str)
                .map(String::from);

            // Timestamp: unix seconds float, else now.
            let timestamp = item
                .get("timestamp")
                .and_then(serde_json::Value::as_f64)
                .map_or_else(SystemTime::now, |secs| {
                    UNIX_EPOCH + Duration::from_secs_f64(secs)
                });

            // Tags: flat string→string map.
            let tags = item
                .get("tags")
                .and_then(serde_json::Value::as_object)
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect::<HashMap<String, String>>()
                })
                .unwrap_or_default();

            metrics.push(Metric {
                name,
                value,
                unit,
                timestamp,
                tags,
                source: self.connector_id.clone(),
            });
        }

        Ok(metrics)
    }

    /// Run the script with a `fetch_alerts` action and parse the returned
    /// JSON array into [`Alert`] values.
    async fn fetch_alerts(&self, database: &DatabaseId) -> Result<Vec<Alert>, ConnectorError> {
        let input = serde_json::json!({
            "action": "fetch_alerts",
            "database_id": database,
        });

        let response = self.invoke_script(&input).await?;
        let items = response.as_array().ok_or_else(|| {
            ConnectorError::Other("fetch_alerts: expected JSON array".to_string())
        })?;

        let mut alerts = Vec::with_capacity(items.len());
        for item in items {
            let id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let title = item
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Alert")
                .to_string();
            let severity = item
                .get("severity")
                .and_then(serde_json::Value::as_str)
                .map_or(Severity::Info, parse_severity);
            let status = item
                .get("status")
                .and_then(serde_json::Value::as_str)
                .map_or(AlertStatus::Active, parse_alert_status);
            let url = item
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            let created_at = item
                .get("created_at")
                .and_then(serde_json::Value::as_f64)
                .map_or_else(SystemTime::now, |secs| {
                    UNIX_EPOCH + Duration::from_secs_f64(secs)
                });

            alerts.push(Alert {
                id,
                title,
                severity,
                status,
                source: self.connector_id.clone(),
                database: Some(database.clone()),
                created_at,
                url,
            });
        }

        Ok(alerts)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn system_time_to_iso8601(ts: SystemTime) -> String {
    let total_secs = ts.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    // Minimal RFC 3339 / ISO 8601 UTC representation without pulling in chrono.
    let hour = total_secs / 3600 % 24;
    let minute = total_secs / 60 % 60;
    let second = total_secs % 60;
    // Days since epoch → calendar date.
    let days = total_secs / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-Unix-epoch to `(year, month, day)`.
///
/// Uses the proleptic Gregorian calendar algorithm (Tomohiko Sakamoto variant).
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm works with i64; days since 1970-01-01.
    // Saturate at i64::MAX for dates far in the future (> ~2.5 × 10^16 years).
    let civil_day = i64::try_from(days)
        .unwrap_or(i64::MAX)
        .saturating_add(719_468);
    let era = if civil_day >= 0 {
        civil_day
    } else {
        civil_day - 146_096
    } / 146_097;
    let day_of_era = civil_day - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year_raw = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day_out = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month_out = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year_out = if month_out <= 2 {
        year_raw + 1
    } else {
        year_raw
    };
    (
        u32::try_from(year_out).unwrap_or(1970),
        u32::try_from(month_out).unwrap_or(1),
        u32::try_from(day_out).unwrap_or(1),
    )
}

fn parse_severity(s: &str) -> Severity {
    match s.to_lowercase().as_str() {
        "critical" => Severity::Critical,
        "warning" | "warn" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn parse_alert_status(s: &str) -> AlertStatus {
    match s.to_lowercase().as_str() {
        "acknowledged" | "ack" => AlertStatus::Acknowledged,
        "resolved" => AlertStatus::Resolved,
        _ => AlertStatus::Active,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{Connector, ConnectorCapabilities, RateLimitConfig};

    fn make_connector() -> ScriptConnector {
        ScriptConnector::new(
            "my-script".to_string(),
            "My Script Connector".to_string(),
            vec![
                "python3".to_string(),
                "/etc/rpg/connectors/custom.py".to_string(),
            ],
        )
    }

    #[test]
    fn new_sets_fields_correctly() {
        let c = make_connector();
        assert_eq!(c.connector_id, "my-script");
        assert_eq!(c.connector_name, "My Script Connector");
        assert_eq!(c.command, vec!["python3", "/etc/rpg/connectors/custom.py"]);
        assert_eq!(c.timeout_seconds, 30);
        assert!((c.rate_limit_rps - 1.0).abs() < f64::EPSILON);
        assert!(!c.warned_about_external);
    }

    #[test]
    fn with_timeout_overrides_default() {
        let c = make_connector().with_timeout(60);
        assert_eq!(c.timeout_seconds, 60);
    }

    #[test]
    fn with_rate_limit_overrides_default() {
        let c = make_connector().with_rate_limit(0.5);
        assert!((c.rate_limit_rps - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn id_returns_connector_id() {
        let c = make_connector();
        assert_eq!(c.id(), "my-script");
    }

    #[test]
    fn name_returns_connector_name() {
        let c = make_connector();
        assert_eq!(c.name(), "My Script Connector");
    }

    #[test]
    fn capabilities_are_all_true() {
        let c = make_connector();
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
        assert!(can_create_issues);
        assert!(can_update_issues);
        assert!(can_receive_webhooks);
        assert!(supports_pagination);
    }

    #[test]
    fn rate_limit_config_reflects_rps() {
        let c = make_connector().with_rate_limit(2.5);
        let RateLimitConfig {
            requests_per_second,
            requests_per_minute,
            max_concurrent,
            respect_retry_after,
            ..
        } = c.rate_limit_config();

        assert!((requests_per_second - 2.5).abs() < f64::EPSILON);
        assert!(requests_per_minute.is_none());
        assert_eq!(max_concurrent, 1);
        assert!(!respect_retry_after);
    }

    #[test]
    fn rate_limit_default_rps() {
        let c = make_connector();
        let cfg = c.rate_limit_config();
        assert!((cfg.requests_per_second - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_timeout_is_thirty_seconds() {
        let c = make_connector();
        assert_eq!(c.timeout_seconds, 30);
    }

    #[test]
    fn custom_timeout_stored_correctly() {
        let c = make_connector().with_timeout(120);
        assert_eq!(c.timeout_seconds, 120);
    }

    #[test]
    fn parse_severity_critical() {
        assert!(matches!(parse_severity("critical"), Severity::Critical));
    }

    #[test]
    fn parse_severity_warning() {
        assert!(matches!(parse_severity("warning"), Severity::Warning));
        assert!(matches!(parse_severity("warn"), Severity::Warning));
    }

    #[test]
    fn parse_severity_unknown_defaults_to_info() {
        assert!(matches!(parse_severity("unknown"), Severity::Info));
    }

    #[test]
    fn parse_alert_status_variants() {
        assert!(matches!(parse_alert_status("active"), AlertStatus::Active));
        assert!(matches!(
            parse_alert_status("acknowledged"),
            AlertStatus::Acknowledged
        ));
        assert!(matches!(
            parse_alert_status("ack"),
            AlertStatus::Acknowledged
        ));
        assert!(matches!(
            parse_alert_status("resolved"),
            AlertStatus::Resolved
        ));
    }

    #[test]
    fn system_time_to_iso8601_epoch() {
        let t = UNIX_EPOCH;
        assert_eq!(system_time_to_iso8601(t), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn system_time_to_iso8601_known_date() {
        // 2024-01-15T12:00:00Z = 1705320000 seconds since epoch.
        let t = UNIX_EPOCH + Duration::from_secs(1_705_320_000);
        assert_eq!(system_time_to_iso8601(t), "2024-01-15T12:00:00Z");
    }
}
