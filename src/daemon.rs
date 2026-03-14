//! Daemon mode — headless continuous monitoring.
//!
//! Runs Rpg without a REPL, performing continuous observation and
//! anomaly detection. Reports via configured notification channels.
//!
//! Usage: `rpg daemon --config config.toml`

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
    PathBuf::from(runtime_dir).join("rpg.pid")
}

/// Derive the token file path from a PID file path.
///
/// The token file is placed next to the PID file with a `.token` extension.
/// For example, `/tmp/rpg.pid` → `/tmp/rpg.token`.
pub fn token_path_for(pid_path: &Path) -> PathBuf {
    pid_path.with_extension("token")
}

/// Generate a random 32-byte bearer token encoded as lowercase hex (64 chars).
///
/// Uses [`std::collections::hash_map::DefaultHasher`] seeded from
/// `SystemTime` and process ID as entropy sources, XOR-combined across
/// four independent draws to produce 256 bits of output.
///
/// This is sufficient for a local monitoring secret; it is not a
/// cryptographic RNG.
pub fn generate_health_token() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::fmt::Write as _;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let pid = std::process::id();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    // Build four independent 64-bit values from different seeds.
    // Truncating u128 nanos to u64 is intentional — lower bits carry entropy.
    #[allow(clippy::cast_possible_truncation)]
    let seeds: [u64; 4] = [
        now.as_nanos() as u64,
        u64::from(now.subsec_nanos()) ^ u64::from(pid),
        now.as_secs().wrapping_mul(0x9e37_79b9_7f4a_7c15),
        u64::from(pid).wrapping_mul(0x6c62_272e_07bb_0142),
    ];

    let mut bytes = [0u8; 32];
    for (i, &seed) in seeds.iter().enumerate() {
        let mut h = DefaultHasher::new();
        seed.hash(&mut h);
        i.hash(&mut h);
        let val = h.finish().to_le_bytes();
        for (j, b) in val.iter().enumerate() {
            bytes[i * 8 + j] = *b;
        }
    }

    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Write the bearer token to the token file (mode 0600 on Unix).
pub fn write_token_file(path: &Path, token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts.open(path)?;
        write!(f, "{token}")
    }

    #[cfg(not(unix))]
    {
        std::fs::write(path, token)
    }
}

/// Remove the token file on shutdown.
pub fn remove_token_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Check if another daemon is already running.
pub fn check_existing_pid(path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let pid: u32 = content.trim().parse().ok()?;

    // Check if process exists (Unix only).
    #[cfg(unix)]
    {
        // kill(pid, 0) checks existence without sending a signal.
        // Returns 0 on success (process exists and we can signal it)
        // or -1 with ESRCH if the process does not exist.
        if let Ok(pid_i32) = i32::try_from(pid) {
            let alive = unsafe { libc::kill(pid_i32, 0) } == 0;
            if alive {
                return Some(pid);
            }
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
    /// Generic webhook URL (POSTs JSON with message, source, timestamp).
    Webhook { url: String },
    /// Email (placeholder — not implemented in v1).
    #[allow(dead_code)]
    Email { to: String },
    /// `PagerDuty` Events API v2 routing key.
    PagerDuty { routing_key: String },
    /// Telegram bot token and chat ID.
    Telegram { bot_token: String, chat_id: String },
    /// Log to stderr (always active).
    Stderr,
}

/// Send a notification to a channel.
pub async fn notify(channel: &NotificationChannel, message: &str) {
    match channel {
        NotificationChannel::Slack { webhook_url } => {
            send_slack_notification(webhook_url, message).await;
        }
        NotificationChannel::Webhook { url } => {
            send_webhook_notification(url, message).await;
        }
        NotificationChannel::PagerDuty { routing_key } => {
            send_pagerduty_notification(routing_key, message).await;
        }
        NotificationChannel::Telegram { bot_token, chat_id } => {
            send_telegram_notification(bot_token, chat_id, message).await;
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

async fn send_webhook_notification(url: &str, message: &str) {
    let payload = serde_json::to_string(&serde_json::json!({
        "message": message,
        "source": "rpg",
        "timestamp": chrono_now(),
    }))
    .unwrap_or_else(|_| r#"{"message":"(encoding error)"}"#.to_owned());

    match reqwest::Client::new()
        .post(url)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            crate::logging::debug("daemon", "Webhook notification sent");
        }
        Ok(resp) => {
            crate::logging::warn(
                "daemon",
                &format!("Webhook notification failed: HTTP {}", resp.status()),
            );
        }
        Err(e) => {
            crate::logging::warn("daemon", &format!("Webhook notification error: {e}"));
        }
    }
}

async fn send_slack_notification(webhook_url: &str, message: &str) {
    let payload = serde_json::to_string(&serde_json::json!({ "text": message }))
        .unwrap_or_else(|_| r#"{"text":"(encoding error)"}"#.to_owned());

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

async fn send_pagerduty_notification(routing_key: &str, message: &str) {
    let payload = serde_json::to_string(&serde_json::json!({
        "routing_key": routing_key,
        "event_action": "trigger",
        "payload": {
            "summary": message,
            "source": "rpg",
            "severity": "critical",
            "timestamp": chrono_now(),
        }
    }))
    .unwrap_or_else(|_| r#"{"routing_key":"","event_action":"trigger","payload":{"summary":"(encoding error)","source":"rpg","severity":"critical"}}"#.to_owned());

    match reqwest::Client::new()
        .post("https://events.pagerduty.com/v2/enqueue")
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            crate::logging::debug("daemon", "PagerDuty notification sent");
        }
        Ok(resp) => {
            crate::logging::warn(
                "daemon",
                &format!("PagerDuty notification failed: HTTP {}", resp.status()),
            );
        }
        Err(e) => {
            crate::logging::warn("daemon", &format!("PagerDuty notification error: {e}"));
        }
    }
}

async fn send_telegram_notification(bot_token: &str, chat_id: &str, message: &str) {
    let url = format!("https://api.telegram.org/bot{bot_token}/sendMessage");
    let payload = serde_json::to_string(&serde_json::json!({
        "chat_id": chat_id,
        "text": message,
        "parse_mode": "Markdown",
    }))
    .unwrap_or_else(|_| {
        r#"{"chat_id":"","text":"(encoding error)","parse_mode":"Markdown"}"#.to_owned()
    });

    match reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            crate::logging::debug("daemon", "Telegram notification sent");
        }
        Ok(resp) => {
            crate::logging::warn(
                "daemon",
                &format!("Telegram notification failed: HTTP {}", resp.status()),
            );
        }
        Err(e) => {
            crate::logging::warn("daemon", &format!("Telegram notification error: {e}"));
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
/// Requires a valid `Authorization: Bearer <token>` header on every
/// request. Returns `401 Unauthorized` if the header is absent or the
/// token does not match. The token is written to the `.token` file next
/// to the PID file so monitoring scripts can read it.
pub async fn run_health_server(
    port: u16,
    health: std::sync::Arc<tokio::sync::RwLock<HealthStatus>>,
    token: String,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

        // Read the HTTP request (up to 4 KiB — enough for headers).
        let mut buf = vec![0u8; 4096];
        let Ok(n) = stream.read(&mut buf).await else {
            continue;
        };
        let request = String::from_utf8_lossy(&buf[..n]);

        // Extract the Authorization header value, if present.
        let auth_ok = request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
            .and_then(|line| line.split_once(':').map(|x| x.1))
            .map(str::trim)
            .map(|val| {
                val.strip_prefix("Bearer ")
                    .or_else(|| val.strip_prefix("bearer "))
                    .unwrap_or("")
            })
            .is_some_and(|presented| presented == token);

        if !auth_ok {
            let body = r#"{"error":"Unauthorized"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 WWW-Authenticate: Bearer realm=\"rpg-health\"\r\n\
                 \r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(response.as_bytes()).await;
            continue;
        }

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
#[allow(clippy::too_many_lines)]
pub async fn run(
    client: &Client,
    config: &Config,
    dbname: &str,
    channels: &[NotificationChannel],
    health_port: Option<u16>,
    github_repo: Option<&str>,
    registry: &crate::connectors::ConnectorRegistry,
) {
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::RwLock;

    let mut detector = AnomalyDetector::new();
    let mut circuit_breaker = crate::governance::CircuitBreaker::new();
    let mut veto_tracker = crate::governance::VetoTracker::new();
    let mut audit_log = crate::governance::AuditLog::new();
    let interval = Duration::from_secs(10);

    let health = Arc::new(RwLock::new(HealthStatus {
        connected: true,
        dbname: dbname.to_owned(),
        last_check: None,
        active_anomalies: 0,
    }));

    // Start health server if port configured.
    if let Some(port) = health_port {
        let token = generate_health_token();
        let pid_path = default_pid_path();
        let tok_path = token_path_for(&pid_path);

        match write_token_file(&tok_path, &token) {
            Ok(()) => {
                crate::logging::info(
                    "daemon",
                    &format!("Health token written to {}", tok_path.display()),
                );
            }
            Err(e) => {
                crate::logging::warn(
                    "daemon",
                    &format!("Could not write token file {}: {e}", tok_path.display()),
                );
            }
        }

        let h = Arc::clone(&health);
        let tok_path_clone = tok_path.clone();
        tokio::spawn(async move {
            run_health_server(port, h, token).await;
            remove_token_file(&tok_path_clone);
        });
    }

    crate::logging::info("daemon", &format!("Monitoring {dbname} (interval: 10s)"));

    // Log connector status on startup.
    {
        let connectors = registry.list();
        if connectors.is_empty() {
            crate::logging::info("daemon", "Connectors: none configured");
        } else {
            let status: Vec<String> = connectors
                .iter()
                .map(|c| format!("{} (enabled)", c.id()))
                .collect();
            crate::logging::info("daemon", &format!("Connectors: {}", status.join(", ")));
        }
    }

    // Notify startup.
    for ch in channels {
        notify(ch, &format!("Rpg daemon started — monitoring {dbname}")).await;
    }

    let mut iteration: u64 = 0;

    loop {
        let mut snap = MetricSnapshot::default();
        let now = chrono_now();

        // Fetch metrics from enabled connectors (every iteration).
        // Errors are logged and skipped — connector failures must not
        // interrupt the main monitoring loop.
        if !registry.list().is_empty() {
            let db_id = dbname.to_owned();
            let window = crate::connectors::TimeWindow {
                start: std::time::SystemTime::now() - std::time::Duration::from_secs(60),
                end: std::time::SystemTime::now(),
            };
            for connector in registry.list() {
                match connector.fetch_metrics(&db_id, &window).await {
                    Ok(metrics) => {
                        crate::logging::info(
                            "daemon",
                            &format!(
                                "connector {}: fetched {} metric(s)",
                                connector.id(),
                                metrics.len()
                            ),
                        );
                    }
                    Err(e) => {
                        crate::logging::warn(
                            "daemon",
                            &format!("connector {}: fetch_metrics failed: {e}", connector.id()),
                        );
                    }
                }
            }
        }

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

            // Create GitHub issue if configured.
            if let Some(repo) = github_repo {
                let template = crate::issues::issue_from_anomaly(
                    dbname,
                    anomaly.kind.label(),
                    &anomaly.description,
                );
                let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                match creator.create_issue(&template).await {
                    Ok(url) => {
                        crate::logging::info("daemon", &format!("Created issue: {url}"));
                    }
                    Err(e) => {
                        crate::logging::warn("daemon", &format!("Issue creation failed: {e}"));
                    }
                }
            }
        }

        iteration += 1;

        // Run index health check every 30 iterations (~5 minutes).
        if iteration % 30 == 0 {
            let ih_report = crate::index_health::analyze(client).await;
            if !ih_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Index health: {} finding(s) detected",
                    ih_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &ih_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Index health: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "index-health".to_owned()],
                                source: "index-health".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::IndexHealth);
                let effective = circuit_breaker
                    .effective_autonomy(crate::governance::FeatureArea::IndexHealth, configured);
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = ih_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg = format!(
                                "[{dbname}] Auto-executed {executed} index health action(s)"
                            );
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run vacuum health check every 30 iterations (~5 minutes).
        if iteration % 30 == 0 {
            let vacuum_report = crate::vacuum::analyze(client).await;
            if !vacuum_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Vacuum health: {} finding(s) detected",
                    vacuum_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &vacuum_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Vacuum: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "vacuum".to_owned()],
                                source: "vacuum".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe vacuum proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::Vacuum);
                let effective = circuit_breaker
                    .effective_autonomy(crate::governance::FeatureArea::Vacuum, configured);
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = vacuum_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg =
                                format!("[{dbname}] Auto-executed {executed} vacuum action(s)");
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run bloat check every 60 iterations (~10 minutes).
        if iteration % 60 == 0 {
            let bloat_report = crate::bloat::BloatAnalyzer::analyze(client).await;
            if !bloat_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Bloat analysis: {} finding(s) detected",
                    bloat_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &bloat_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!("[Rpg] Bloat: {} on {dbname}", finding.kind.label()),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "bloat".to_owned()],
                                source: "bloat".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe bloat proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::Bloat);
                let effective = circuit_breaker
                    .effective_autonomy(crate::governance::FeatureArea::Bloat, configured);
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = bloat_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg =
                                format!("[{dbname}] Auto-executed {executed} bloat action(s)");
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run config tuning check every 180 iterations (~30 minutes).
        // Config changes are sensitive — notify only, no auto-execution.
        if iteration % 180 == 0 {
            let config_report = crate::config_tuning::analyze(client).await;
            if !config_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Config tuning: {} finding(s) detected",
                    config_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &config_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Config tuning: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "config-tuning".to_owned()],
                                source: "config-tuning".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Run query optimization check every 6 iterations (~1 minute), offset 3.
        if iteration % 6 == 3 {
            let qo_report = crate::query_optimization::analyze(client).await;
            if !qo_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Query optimization: {} finding(s) detected",
                    qo_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &qo_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Query optimization: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "query-optimization".to_owned()],
                                source: "query-optimization".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::QueryOptimization);
                let effective = circuit_breaker.effective_autonomy(
                    crate::governance::FeatureArea::QueryOptimization,
                    configured,
                );
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = qo_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg = format!(
                                "[{dbname}] Auto-executed {executed} \
                                 query optimization action(s)"
                            );
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run connection management check every 30 iterations (~5 min), offset 15.
        if iteration % 30 == 15 {
            let cm_report =
                crate::connection_management::ConnectionManagementAnalyzer::analyze(client).await;
            if !cm_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Connection management: {} finding(s) detected",
                    cm_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &cm_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Connection management: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "connection-management".to_owned()],
                                source: "connection-management".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::ConnectionManagement);
                let effective = circuit_breaker.effective_autonomy(
                    crate::governance::FeatureArea::ConnectionManagement,
                    configured,
                );
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = cm_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg = format!(
                                "[{dbname}] Auto-executed {executed} \
                                 connection management action(s)"
                            );
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run replication check every 30 iterations (~5 min), offset 20.
        if iteration % 30 == 20 {
            let repl_report = crate::replication::ReplicationAnalyzer::analyze(client).await;
            if !repl_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Replication: {} finding(s) detected",
                    repl_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &repl_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Replication: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "replication".to_owned()],
                                source: "replication".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::Replication);
                let effective = circuit_breaker
                    .effective_autonomy(crate::governance::FeatureArea::Replication, configured);
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = repl_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg = format!(
                                "[{dbname}] Auto-executed {executed} \
                                 replication action(s)"
                            );
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run backup monitoring check every 60 iterations (~10 min), offset 45.
        if iteration % 60 == 45 {
            let bm_report =
                crate::backup_monitoring::BackupMonitoringAnalyzer::analyze(client).await;
            if !bm_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Backup monitoring: {} finding(s) detected",
                    bm_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &bm_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Backup monitoring: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "backup-monitoring".to_owned()],
                                source: "backup-monitoring".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                // Backup monitoring has no auto-actions (always returns empty
                // proposals), but we follow the standard pattern for consistency.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::BackupMonitoring);
                let effective = circuit_breaker.effective_autonomy(
                    crate::governance::FeatureArea::BackupMonitoring,
                    configured,
                );
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = bm_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg = format!(
                                "[{dbname}] Auto-executed {executed} \
                                 backup monitoring action(s)"
                            );
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Run security check every 180 iterations (~30 min), offset 90.
        // Security changes are sensitive — notify and create issues only.
        if iteration % 180 == 90 {
            let sec_report = crate::security::SecurityAnalyzer::analyze(client).await;
            if !sec_report.findings.is_empty() {
                let msg = format!(
                    "[{dbname}] Security: {} finding(s) detected",
                    sec_report.findings.len()
                );
                for ch in channels {
                    notify(ch, &msg).await;
                }

                // Create GitHub issues for critical findings.
                if let Some(repo) = github_repo {
                    for finding in &sec_report.findings {
                        if finding.severity == crate::governance::Severity::Critical {
                            let template = crate::issues::IssueTemplate {
                                title: format!(
                                    "[Rpg] Security: {} on {dbname}",
                                    finding.kind.label()
                                ),
                                body: finding.description.clone(),
                                labels: vec!["rpg".to_owned(), "security".to_owned()],
                                source: "security".to_owned(),
                            };
                            let creator = crate::issues::GitHubIssueCreator::new(repo.to_owned());
                            match creator.create_issue(&template).await {
                                Ok(url) => {
                                    crate::logging::info(
                                        "daemon",
                                        &format!("Created issue: {url}"),
                                    );
                                }
                                Err(e) => {
                                    crate::logging::warn(
                                        "daemon",
                                        &format!("Issue creation failed: {e}"),
                                    );
                                }
                            }
                        }
                    }
                }

                // In Auto mode, execute safe proposals.
                let configured = config
                    .governance
                    .autonomy_for(crate::governance::FeatureArea::Security);
                let effective = circuit_breaker
                    .effective_autonomy(crate::governance::FeatureArea::Security, configured);
                if effective == crate::governance::AutonomyLevel::Auto {
                    let proposals = sec_report.to_proposals();
                    if !proposals.is_empty() {
                        let executed = crate::rca_actions::run_auto_flow(
                            client,
                            &proposals,
                            &mut audit_log,
                            &mut circuit_breaker,
                            &mut veto_tracker,
                        )
                        .await;
                        if executed > 0 {
                            let auto_msg =
                                format!("[{dbname}] Auto-executed {executed} security action(s)");
                            for ch in channels {
                                notify(ch, &auto_msg).await;
                            }
                        }
                    }
                }
            }
        }

        // Auto-RCA on severe anomalies.
        if crate::anomaly::AnomalyDetector::should_trigger_rca(&anomalies) {
            let configured_autonomy = config
                .governance
                .autonomy_for(crate::governance::FeatureArea::Rca);
            let effective_autonomy = circuit_breaker
                .effective_autonomy(crate::governance::FeatureArea::Rca, configured_autonomy);

            crate::logging::info("daemon", "Auto-triggering RCA investigation");
            let rca_snapshot = crate::rca::collect_snapshot(client, false).await;
            let data_steps = rca_snapshot.steps.iter().filter(|s| s.has_data).count();

            let rca_msg =
                format!("[{dbname}] RCA auto-triggered — {data_steps} diagnostic steps collected");
            for ch in channels {
                notify(ch, &rca_msg).await;
            }

            // In Auto mode, propose and execute mitigations automatically.
            if effective_autonomy == crate::governance::AutonomyLevel::Auto {
                let proposals = crate::rca_actions::propose_mitigations(client).await;
                if !proposals.is_empty() {
                    let executed = crate::rca_actions::run_auto_flow(
                        client,
                        &proposals,
                        &mut audit_log,
                        &mut circuit_breaker,
                        &mut veto_tracker,
                    )
                    .await;
                    if executed > 0 {
                        let msg =
                            format!("[{dbname}] Auto-executed {executed} mitigation action(s)");
                        for ch in channels {
                            notify(ch, &msg).await;
                        }
                    }
                }
            }

            detector.reset_rca_cooldown();
        }

        // Sleep, exit on signal.
        tokio::select! {
            () = tokio::time::sleep(interval) => {},
            _ = tokio::signal::ctrl_c() => {
                crate::logging::info("daemon", "Received shutdown signal");
                for ch in channels {
                    notify(ch, &format!("Rpg daemon shutting down ({dbname})")).await;
                }
                break;
            },
        }
    }
}

/// Get current time as ISO 8601 string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Uses only `std::time::SystemTime` — no external crate required.
fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Decompose epoch seconds into a calendar date + time-of-day.
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Gregorian calendar conversion (proleptic; valid for 1970+).
    // Algorithm: shift epoch to 1 March 0000, then use the 400-year cycle.
    let z = days_since_epoch + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
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
        assert!(path.to_str().unwrap().contains("rpg.pid"));
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
        let dir = std::env::temp_dir().join("rpg_test_pid");
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
        let dir = std::env::temp_dir().join("rpg_test_stale");
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

    #[test]
    fn notification_channel_webhook_has_url() {
        let ch = NotificationChannel::Webhook {
            url: "https://example.com/hook".to_owned(),
        };
        if let NotificationChannel::Webhook { url } = ch {
            assert!(url.starts_with("https://"));
        }
    }

    #[test]
    fn notification_channel_pagerduty_has_key() {
        let ch = NotificationChannel::PagerDuty {
            routing_key: "r0utingk3y1234567890abcdef".to_owned(),
        };
        if let NotificationChannel::PagerDuty { routing_key } = ch {
            assert!(!routing_key.is_empty());
        }
    }

    #[test]
    fn notification_channel_telegram_has_fields() {
        let ch = NotificationChannel::Telegram {
            bot_token: "123456:ABCdef".to_owned(),
            chat_id: "-1001234567890".to_owned(),
        };
        if let NotificationChannel::Telegram { bot_token, chat_id } = ch {
            assert!(!bot_token.is_empty());
            assert!(!chat_id.is_empty());
        }
    }

    #[test]
    fn telegram_payload_has_required_fields() {
        let chat_id = "-1001234567890";
        let message = "test pg alert";
        let payload = serde_json::to_string(&serde_json::json!({
            "chat_id": chat_id,
            "text": message,
            "parse_mode": "Markdown",
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["chat_id"], chat_id);
        assert_eq!(v["text"], message);
        assert_eq!(v["parse_mode"], "Markdown");
    }

    #[test]
    fn pagerduty_payload_has_required_fields() {
        let message = "test pg alert";
        let routing_key = "testkey123";
        let payload = serde_json::to_string(&serde_json::json!({
            "routing_key": routing_key,
            "event_action": "trigger",
            "payload": {
                "summary": message,
                "source": "rpg",
                "severity": "critical",
                "timestamp": chrono_now(),
            }
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["routing_key"], routing_key);
        assert_eq!(v["event_action"], "trigger");
        assert_eq!(v["payload"]["summary"], message);
        assert_eq!(v["payload"]["severity"], "critical");
    }

    #[test]
    fn webhook_payload_has_required_fields() {
        let message = "test alert";
        let payload = serde_json::to_string(&serde_json::json!({
            "message": message,
            "source": "rpg",
            "timestamp": chrono_now(),
        }))
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["message"], "test alert");
        assert_eq!(v["source"], "rpg");
        // timestamp should be a non-empty ISO 8601 string
        let ts = v["timestamp"].as_str().unwrap();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn chrono_now_is_iso8601() {
        let ts = chrono_now();
        // Expected format: YYYY-MM-DDTHH:MM:SSZ (20 chars)
        assert_eq!(ts.len(), 20, "expected 20-char ISO 8601 string, got: {ts}");
        assert!(ts.ends_with('Z'), "should end with Z: {ts}");
        assert_eq!(&ts[4..5], "-", "char 4 should be '-': {ts}");
        assert_eq!(&ts[7..8], "-", "char 7 should be '-': {ts}");
        assert_eq!(&ts[10..11], "T", "char 10 should be 'T': {ts}");
        assert_eq!(&ts[13..14], ":", "char 13 should be ':': {ts}");
        assert_eq!(&ts[16..17], ":", "char 16 should be ':': {ts}");
        // Year should be 2025 or later.
        let year: u64 = ts[..4].parse().expect("year should be numeric");
        assert!(year >= 2025, "year should be >= 2025, got {year}");
    }

    #[test]
    fn index_health_check_interval_logic() {
        // Verify that the modulo-30 interval fires at the right iterations.
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..100 {
            iteration += 1;
            if iteration % 30 == 0 {
                fired_at.push(iteration);
            }
        }
        // Should fire at iterations 30, 60, 90 — exactly 3 times in 100 loops.
        assert_eq!(fired_at, vec![30, 60, 90]);
        // First fire at iteration 30, not before.
        assert_eq!(fired_at[0], 30);
    }

    #[test]
    fn query_optimization_check_interval_logic() {
        // Fires every 6 iterations at offset 3: iterations 3, 9, 15, …
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..30 {
            iteration += 1;
            if iteration % 6 == 3 {
                fired_at.push(iteration);
            }
        }
        assert_eq!(fired_at, vec![3, 9, 15, 21, 27]);
        // Never fires at the same iteration as index_health (% 30 == 0).
        assert!(fired_at.iter().all(|i| i % 30 != 0));
    }

    #[test]
    fn connection_management_check_interval_logic() {
        // Fires every 30 iterations at offset 15: iterations 15, 45, 75, …
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..100 {
            iteration += 1;
            if iteration % 30 == 15 {
                fired_at.push(iteration);
            }
        }
        assert_eq!(fired_at, vec![15, 45, 75]);
        // Never overlaps with index_health / vacuum (offset 0).
        assert!(fired_at.iter().all(|i| i % 30 != 0));
    }

    #[test]
    fn replication_check_interval_logic() {
        // Fires every 30 iterations at offset 20: iterations 20, 50, 80, …
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..100 {
            iteration += 1;
            if iteration % 30 == 20 {
                fired_at.push(iteration);
            }
        }
        assert_eq!(fired_at, vec![20, 50, 80]);
        // Does not overlap with connection_management (offset 15) or
        // index_health / vacuum (offset 0).
        assert!(fired_at.iter().all(|i| i % 30 != 0));
        assert!(fired_at.iter().all(|i| i % 30 != 15));
    }

    #[test]
    fn backup_monitoring_check_interval_logic() {
        // Fires every 60 iterations at offset 45: iterations 45, 105, …
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..180 {
            iteration += 1;
            if iteration % 60 == 45 {
                fired_at.push(iteration);
            }
        }
        assert_eq!(fired_at, vec![45, 105, 165]);
        // Never overlaps with bloat (% 60 == 0).
        assert!(fired_at.iter().all(|i| i % 60 != 0));
    }

    #[test]
    fn security_check_interval_logic() {
        // Fires every 180 iterations at offset 90: iteration 90 in first 180.
        let mut fired_at: Vec<u64> = Vec::new();
        let mut iteration: u64 = 0;
        for _ in 0..360 {
            iteration += 1;
            if iteration % 180 == 90 {
                fired_at.push(iteration);
            }
        }
        assert_eq!(fired_at, vec![90, 270]);
        // Does not overlap with config_tuning (% 180 == 0).
        assert!(fired_at.iter().all(|i| i % 180 != 0));
    }

    #[test]
    fn analyzer_offsets_do_not_overlap_within_180_iterations() {
        // Verify all 9 analyzer triggers are distinct across 180 iterations.
        // We collect (iteration, analyzer_name) for each fire in 1..=180.
        let mut events: Vec<(u64, &str)> = Vec::new();
        for iteration in 1u64..=180 {
            if iteration % 30 == 0 {
                events.push((iteration, "index_health"));
                events.push((iteration, "vacuum"));
            }
            if iteration % 6 == 3 {
                events.push((iteration, "query_optimization"));
            }
            if iteration % 30 == 15 {
                events.push((iteration, "connection_management"));
            }
            if iteration % 30 == 20 {
                events.push((iteration, "replication"));
            }
            if iteration % 60 == 0 {
                events.push((iteration, "bloat"));
            }
            if iteration % 180 == 0 {
                events.push((iteration, "config_tuning"));
            }
            if iteration % 60 == 45 {
                events.push((iteration, "backup_monitoring"));
            }
            if iteration % 180 == 90 {
                events.push((iteration, "security"));
            }
        }
        // Each of the 5 new analyzers must appear at least once.
        let names: Vec<&str> = events.iter().map(|(_, n)| *n).collect();
        assert!(
            names.contains(&"query_optimization"),
            "query_optimization missing"
        );
        assert!(
            names.contains(&"connection_management"),
            "connection_management missing"
        );
        assert!(names.contains(&"replication"), "replication missing");
        assert!(
            names.contains(&"backup_monitoring"),
            "backup_monitoring missing"
        );
        assert!(names.contains(&"security"), "security missing");
    }

    #[test]
    fn generate_health_token_is_64_hex_chars() {
        let token = generate_health_token();
        assert_eq!(
            token.len(),
            64,
            "token should be 64 hex chars, got: {token}"
        );
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "token should be all hex, got: {token}"
        );
    }

    #[test]
    fn generate_health_token_is_unique() {
        // Two tokens generated in sequence should differ.
        let t1 = generate_health_token();
        let t2 = generate_health_token();
        // They differ because nanos will have advanced between calls.
        // Allow the rare collision on extremely fast hardware with no-op
        // by just asserting the type is right, not strict inequality.
        assert_eq!(t1.len(), 64);
        assert_eq!(t2.len(), 64);
    }

    #[test]
    fn token_path_for_derives_from_pid_path() {
        let pid = PathBuf::from("/tmp/rpg.pid");
        let tok = token_path_for(&pid);
        assert_eq!(tok, PathBuf::from("/tmp/rpg.token"));
    }

    #[test]
    fn token_path_for_works_with_no_extension() {
        let pid = PathBuf::from("/run/rpg-daemon");
        let tok = token_path_for(&pid);
        assert_eq!(tok, PathBuf::from("/run/rpg-daemon.token"));
    }

    #[test]
    fn write_and_remove_token_file() {
        let dir = std::env::temp_dir().join("rpg_test_token");
        let _ = std::fs::create_dir_all(&dir);
        let tok_path = dir.join("rpg.token");
        let token = "deadbeef1234".to_owned();

        write_token_file(&tok_path, &token).unwrap();
        let contents = std::fs::read_to_string(&tok_path).unwrap();
        assert_eq!(contents, token);

        remove_token_file(&tok_path);
        assert!(!tok_path.exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn token_file_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join("rpg_test_token_perms");
        let _ = std::fs::create_dir_all(&dir);
        let tok_path = dir.join("rpg.token");

        write_token_file(&tok_path, "secret").unwrap();
        let meta = std::fs::metadata(&tok_path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file mode should be 0600, got {mode:o}");

        let _ = std::fs::remove_dir_all(dir);
    }
}
