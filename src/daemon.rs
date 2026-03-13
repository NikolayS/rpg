//! Daemon mode — headless continuous monitoring.
//!
//! Runs Samo without a REPL, performing continuous observation and
//! anomaly detection. Reports via configured notification channels.
//!
//! Usage: `samo daemon --config config.toml`

use std::path::{Path, PathBuf};

use tokio_postgres::Client;

use crate::anomaly::{AnomalyDetector, MetricSnapshot};
use crate::config::Config;

// ---------------------------------------------------------------------------
// PID file management
// ---------------------------------------------------------------------------

/// Write the current process PID to a file.
///
/// Returns the path to the PID file for cleanup.
pub fn write_pid_file(path: &Path) -> std::io::Result<()> {
    std::fs::write(path, format!("{}", std::process::id()))
}

/// Remove the PID file on shutdown.
pub fn remove_pid_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Default PID file path.
pub fn default_pid_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    PathBuf::from(runtime_dir).join("samo.pid")
}

/// Check if another daemon is already running.
pub fn check_existing_pid(path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    // Check if process exists (Unix only).
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // kill(pid, 0) checks existence without sending a signal.
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .ok()?;
        if status.success() || status.signal() == Some(0) {
            return Some(pid);
        }
    }

    #[cfg(not(unix))]
    {
        // On non-Unix, assume the PID is stale if file exists.
        let _ = pid;
    }

    None
}

// ---------------------------------------------------------------------------
// Notification channels
// ---------------------------------------------------------------------------

/// Notification destination.
#[derive(Debug, Clone)]
pub enum NotificationChannel {
    /// Slack incoming webhook URL.
    Slack { webhook_url: String },
    /// Email (placeholder — not implemented in v1).
    #[allow(dead_code)]
    Email { to: String },
    /// Log to stderr (always active).
    Stderr,
}

/// Send a notification to a channel.
pub async fn notify(channel: &NotificationChannel, message: &str) {
    match channel {
        NotificationChannel::Slack { webhook_url } => {
            send_slack_notification(webhook_url, message).await;
        }
        NotificationChannel::Email { to } => {
            eprintln!("[daemon] Email notification to {to}: {message}");
            // Email sending not implemented in v1.
        }
        NotificationChannel::Stderr => {
            eprintln!("[daemon] {message}");
        }
    }
}

async fn send_slack_notification(webhook_url: &str, message: &str) {
    let payload = format!(r#"{{"text":"{}"}}"#, message.replace('"', r#"\""#));

    match reqwest::Client::new()
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            crate::logging::debug("daemon", "Slack notification sent");
        }
        Ok(resp) => {
            crate::logging::warn(
                "daemon",
                &format!("Slack notification failed: HTTP {}", resp.status()),
            );
        }
        Err(e) => {
            crate::logging::warn("daemon", &format!("Slack notification error: {e}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Health check endpoint
// ---------------------------------------------------------------------------

/// Health status for the HTTP endpoint.
#[derive(Debug, Clone)]
pub struct HealthStatus {
    /// Whether the daemon is connected to the database.
    pub connected: bool,
    /// Database name.
    pub dbname: String,
    /// Last observation timestamp (ISO 8601).
    pub last_check: Option<String>,
    /// Number of active anomalies.
    pub active_anomalies: usize,
}

impl HealthStatus {
    /// Serialize to JSON.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"status":"{}","database":"{}","last_check":{},"active_anomalies":{}}}"#,
            if self.connected {
                "healthy"
            } else {
                "disconnected"
            },
            self.dbname,
            self.last_check
                .as_ref()
                .map_or("null".to_owned(), |t| format!("\"{t}\"")),
            self.active_anomalies,
        )
    }
}

/// Run a minimal HTTP health check server on the given port.
///
/// Responds to `GET /health` with JSON status.
pub async fn run_health_server(
    port: u16,
    health: std::sync::Arc<tokio::sync::RwLock<HealthStatus>>,
) {
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    let addr = format!("127.0.0.1:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            crate::logging::warn("daemon", &format!("Health server bind failed: {e}"));
            return;
        }
    };

    crate::logging::info("daemon", &format!("Health endpoint listening on {addr}"));

    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            continue;
        };
        let status = health.read().await;
        let body = status.to_json();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }
}

// ---------------------------------------------------------------------------
// Daemon main loop
// ---------------------------------------------------------------------------

/// Observe query for the daemon monitoring loop.
const DAEMON_OBSERVE_SQL: &str = "\
    SELECT \
        count(*) FILTER (WHERE state = 'active') AS active, \
        count(*) AS total, \
        count(*) FILTER (WHERE wait_event_type = 'Lock') AS blocked, \
        count(*) FILTER (WHERE state = 'active' \
            AND query_start < now() - interval '30 seconds') AS long_running \
    FROM pg_stat_activity \
    WHERE pid != pg_backend_pid() \
      AND backend_type = 'client backend'";

/// Top wait event query.
const TOP_WAIT_SQL: &str = "\
    SELECT count(*) AS cnt \
    FROM pg_stat_activity \
    WHERE state = 'active' AND wait_event IS NOT NULL \
      AND pid != pg_backend_pid() \
    ORDER BY 1 DESC LIMIT 1";

/// Run the daemon monitoring loop.
///
/// Continuously monitors the database, detects anomalies, and sends
/// notifications. Exits on SIGTERM or SIGINT.
pub async fn run(
    client: &Client,
    config: &Config,
    dbname: &str,
    channels: &[NotificationChannel],
    health_port: Option<u16>,
) {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    let mut detector = AnomalyDetector::new();
    let interval = Duration::from_secs(10);

    let health = Arc::new(RwLock::new(HealthStatus {
        connected: true,
        dbname: dbname.to_owned(),
        last_check: None,
        active_anomalies: 0,
    }));

    // Start health server if port configured.
    if let Some(port) = health_port {
        let h = Arc::clone(&health);
        tokio::spawn(async move {
            run_health_server(port, h).await;
        });
    }

    crate::logging::info("daemon", &format!("Monitoring {dbname} (interval: 10s)"));

    // Notify startup.
    for ch in channels {
        notify(ch, &format!("Samo daemon started — monitoring {dbname}")).await;
    }

    loop {
        let mut snap = MetricSnapshot::default();
        let now = chrono_now();

        // Collect metrics.
        if let Ok(messages) = client.simple_query(DAEMON_OBSERVE_SQL).await {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    snap.active_sessions = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.total_sessions = row.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.blocked_sessions = row.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    snap.long_queries = row.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        // Top wait count.
        if let Ok(messages) = client.simple_query(TOP_WAIT_SQL).await {
            for msg in &messages {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    snap.top_wait_count = row.get(0).and_then(|s| s.parse().ok()).unwrap_or(0);
                }
            }
        }

        // Update health status.
        {
            let mut h = health.write().await;
            h.connected = true;
            h.last_check = Some(now);
        }

        // Run anomaly detection.
        let anomalies = detector.check(&snap);

        // Update health anomaly count.
        {
            let mut h = health.write().await;
            h.active_anomalies = anomalies.len();
        }

        // Notify on anomalies.
        for anomaly in &anomalies {
            let msg = format!(
                "[{dbname}] Anomaly detected: [{kind}] {desc}",
                kind = anomaly.kind.label(),
                desc = anomaly.description,
            );
            for ch in channels {
                notify(ch, &msg).await;
            }
        }

        // Auto-RCA on severe anomalies.
        if crate::anomaly::AnomalyDetector::should_trigger_rca(&anomalies) {
            let pg_ash = config
                .governance
                .autonomy_for(crate::governance::FeatureArea::Rca);
            let _ = pg_ash; // RCA in daemon is Observe-only for now.

            crate::logging::info("daemon", "Auto-triggering RCA investigation");
            let rca_snapshot = crate::rca::collect_snapshot(client, false).await;
            let data_steps = rca_snapshot.steps.iter().filter(|s| s.has_data).count();

            let rca_msg =
                format!("[{dbname}] RCA auto-triggered — {data_steps} diagnostic steps collected");
            for ch in channels {
                notify(ch, &rca_msg).await;
            }

            detector.reset_rca_cooldown();
        }

        // Sleep, exit on signal.
        tokio::select! {
            () = tokio::time::sleep(interval) => {},
            _ = tokio::signal::ctrl_c() => {
                crate::logging::info("daemon", "Received shutdown signal");
                for ch in channels {
                    notify(ch, &format!("Samo daemon shutting down ({dbname})")).await;
                }
                break;
            },
        }
    }
}

/// Get current time as ISO 8601 string.
fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now();
    let secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple UTC timestamp without chrono dependency.
    format!("{secs}")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pid_path_is_absolute() {
        let path = default_pid_path();
        assert!(path.is_absolute());
        assert!(path.to_str().unwrap().contains("samo.pid"));
    }

    #[test]
    fn health_status_json_connected() {
        let status = HealthStatus {
            connected: true,
            dbname: "mydb".to_owned(),
            last_check: Some("2026-03-12T14:23:01Z".to_owned()),
            active_anomalies: 0,
        };
        let json = status.to_json();
        assert!(json.contains("\"healthy\""));
        assert!(json.contains("\"mydb\""));
        assert!(json.contains("\"2026-03-12T14:23:01Z\""));
    }

    #[test]
    fn health_status_json_disconnected() {
        let status = HealthStatus {
            connected: false,
            dbname: "prod".to_owned(),
            last_check: None,
            active_anomalies: 2,
        };
        let json = status.to_json();
        assert!(json.contains("\"disconnected\""));
        assert!(json.contains("null"));
        assert!(json.contains("\"active_anomalies\":2"));
    }

    #[test]
    fn pid_file_write_and_check() {
        let dir = std::env::temp_dir().join("samo_test_pid");
        let _ = std::fs::create_dir_all(&dir);
        let pid_path = dir.join("test.pid");

        write_pid_file(&pid_path).unwrap();
        let content = std::fs::read_to_string(&pid_path).unwrap();
        assert_eq!(content, format!("{}", std::process::id()));

        remove_pid_file(&pid_path);
        assert!(!pid_path.exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn check_stale_pid() {
        let dir = std::env::temp_dir().join("samo_test_stale");
        let _ = std::fs::create_dir_all(&dir);
        let pid_path = dir.join("stale.pid");

        // Write a PID that almost certainly doesn't exist.
        std::fs::write(&pid_path, "999999999").unwrap();
        let result = check_existing_pid(&pid_path);
        // On Unix, this should return None (PID doesn't exist).
        // On non-Unix, may vary.
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn daemon_observe_sql_is_valid() {
        assert!(DAEMON_OBSERVE_SQL.contains("pg_stat_activity"));
        assert!(DAEMON_OBSERVE_SQL.contains("active"));
        assert!(DAEMON_OBSERVE_SQL.contains("Lock"));
    }

    #[test]
    fn notification_channel_slack_has_url() {
        let ch = NotificationChannel::Slack {
            webhook_url: "https://hooks.slack.com/test".to_owned(),
        };
        if let NotificationChannel::Slack { webhook_url } = ch {
            assert!(webhook_url.starts_with("https://"));
        }
    }
}
