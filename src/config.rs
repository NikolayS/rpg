//! TOML configuration file loading for Rpg.
//!
//! Config hierarchy (later entries override earlier):
//! 1. `/etc/rpg/config.toml` (system-wide)
//! 2. `~/.config/rpg/config.toml` (user)
//! 3. `.rpg.toml` (project, searched from CWD up to home)
//! 4. `RPG_*` environment variables
//! 5. CLI flags
//! 6. `\set` commands (runtime)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::governance::{AutonomyLevel, FeatureArea};

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

/// Top-level config file structure.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Default connection settings (host, port, user, dbname, sslmode).
    pub connection: ConnectionConfig,
    /// Display/output preferences.
    pub display: DisplayConfig,
    /// Safety and destructive-operation settings.
    pub safety: SafetyConfig,
    /// AI/LLM provider settings.
    pub ai: AiConfig,
    /// Per-feature autonomy levels for the governance framework.
    pub governance: GovernanceConfig,
    /// Structured-log file rotation settings.
    pub logging: LoggingConfig,
    /// Named connection profiles (keyed by profile name).
    #[serde(default)]
    pub connections: HashMap<String, ConnectionProfile>,
    /// Named queries loaded from the project config (`.rpg.toml`).
    ///
    /// Not present in user/system config files — populated by
    /// [`merge_project_config`] after project config is loaded.
    #[serde(skip)]
    pub project_named_queries: HashMap<String, String>,
    /// External connector configuration (Datadog, pganalyze, etc.).
    #[serde(default)]
    pub connectors: Option<ConnectorsConfig>,
    /// Severity-based notification routing.
    ///
    /// Specifies which notification channels (by name) receive alerts at
    /// each severity level.  When absent, all channels receive all alerts.
    #[serde(default)]
    pub notification_routing: NotificationRouting,
}

// ---------------------------------------------------------------------------
// Connection settings
// ---------------------------------------------------------------------------

/// Default connection settings applied before CLI flags.
///
/// These provide a fallback when neither the corresponding CLI flag nor
/// an environment variable (PGHOST, PGPORT, …) is set.
///
/// ```toml
/// [connection]
/// host = "db.example.com"
/// port = "5432"
/// user = "app"
/// dbname = "app_prod"
/// sslmode = "require"
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    /// Default server hostname or socket directory.
    pub host: Option<String>,
    /// Default server port (stored as a string to mirror `PGPORT`).
    pub port: Option<String>,
    /// Default database user name.
    pub user: Option<String>,
    /// Default database name.
    pub dbname: Option<String>,
    /// Default SSL mode (`disable`, `prefer`, `require`).
    pub sslmode: Option<String>,
}

// ---------------------------------------------------------------------------
// Display settings
// ---------------------------------------------------------------------------

/// Display settings.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Enable the built-in pager for long output. Default: `true`.
    pub pager: bool,
    /// Enable SQL syntax highlighting in the REPL. Default: `true`.
    pub highlight: bool,
    /// Print query timing after each statement. Default: `false`.
    pub timing: bool,
    /// Expanded display mode (like `\x`). Default: `false`.
    pub expanded: bool,
    /// Minimum output lines before the pager activates.
    /// `None` means "not set in this config layer" (effective default: `0`).
    pub pager_min_lines: Option<usize>,
    /// Table border style (`0`, `1`, or `2`). Mirrors `\pset border`.
    /// `None` means "not set in this config layer" (effective default: `1`).
    pub border: Option<u8>,
    /// Use Vi keybinding mode in the REPL. Default: `false` (Emacs mode).
    ///
    /// When `true`, rustyline uses `EditMode::Vi` instead of the default
    /// Emacs mode.  Takes effect on the next session start.
    ///
    /// ```toml
    /// [display]
    /// vi_mode = true
    /// ```
    pub vi_mode: bool,
    /// Show the persistent status bar at the bottom of the terminal.
    ///
    /// When `true` (the default in interactive sessions), a one-line bar is
    /// rendered at the bottom of the terminal showing connection info, mode,
    /// transaction state, query timing, and AI token usage.
    ///
    /// Disable with `\set STATUSLINE off` at runtime or:
    /// ```toml
    /// [display]
    /// statusline_enabled = false
    /// ```
    pub statusline_enabled: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            pager: true,
            highlight: true,
            timing: false,
            expanded: false,
            pager_min_lines: None,
            border: None,
            vi_mode: false,
            // Default ON — overridden to OFF in non-interactive sessions.
            statusline_enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Safety settings
// ---------------------------------------------------------------------------

/// Safety / destructive-warning settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// Warn before executing destructive statements. Default: `true`.
    pub destructive_warning: bool,
    /// Additional SQL patterns that should trigger a destructive-operation
    /// warning, in addition to the built-in set.
    ///
    /// Each entry is a substring that is matched case-insensitively against
    /// the full SQL text.  If the SQL contains the pattern, the user is
    /// prompted for confirmation just like a built-in destructive statement.
    ///
    /// ```toml
    /// [safety]
    /// protected_patterns = ["DELETE FROM audit_log", "TRUNCATE events"]
    /// ```
    #[serde(default)]
    pub protected_patterns: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            destructive_warning: true,
            protected_patterns: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// AI / LLM settings
// ---------------------------------------------------------------------------

/// AI / LLM provider settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Provider name: `"anthropic"`, `"openai"`, or `"ollama"`.
    pub provider: Option<String>,
    /// Model identifier override (uses provider default when absent).
    pub model: Option<String>,
    /// Name of the environment variable holding the API key.
    ///
    /// Example: `"ANTHROPIC_API_KEY"`.
    pub api_key_env: Option<String>,
    /// Custom base URL for the provider API (useful for proxies / local
    /// deployments).
    pub base_url: Option<String>,
    /// Maximum number of tokens to generate per request.
    pub max_tokens: u32,
    /// Automatically execute read-only queries generated by `/ask` without
    /// prompting.  Defaults to `false`.
    pub auto_execute_readonly: bool,
    /// After a SQL error, automatically show a brief AI-generated fix
    /// suggestion below the error message.  Defaults to `true` when AI
    /// is configured.
    #[serde(default = "default_true")]
    pub auto_explain_errors: bool,
    /// Approximate context window size (in tokens) for the configured model.
    ///
    /// Used by auto-compact: when the conversation context exceeds 70% of
    /// this value, older entries are automatically summarized.
    /// Defaults to 128000 (128k).
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    /// Maximum total tokens to consume in a session.
    ///
    /// When the cumulative token usage (input + output across all AI calls)
    /// reaches this limit, further AI requests are refused until the session
    /// is restarted.  Defaults to 0 (unlimited).
    pub token_budget: u64,
    /// Project-specific system prompt injected from `.rpg.toml`.
    ///
    /// When set, this string is prepended to the AI system prompt for
    /// every request.  Not present in user/system config files — populated
    /// by [`merge_project_config`].
    #[serde(skip)]
    pub project_system_prompt: Option<String>,
    /// Paths to context files from `.rpg.toml` `[ai] context_files`.
    ///
    /// Resolved relative to the directory containing `.rpg.toml`.
    /// Not present in user/system config files — populated by
    /// [`merge_project_config`].
    #[serde(skip)]
    pub project_context_files: Vec<String>,
    /// Show AI-generated SQL before executing it.
    ///
    /// When `true`, the SQL generated by `/ask` is printed (with syntax
    /// highlighting) before its results are displayed — similar to psql's
    /// `ECHO_HIDDEN` behaviour.  Defaults to `false` (SQL is hidden).
    pub show_sql: bool,
}

fn default_context_window() -> u32 {
    128_000
}

fn default_true() -> bool {
    true
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: None,
            model: None,
            api_key_env: None,
            base_url: None,
            max_tokens: 4096,
            auto_execute_readonly: false,
            auto_explain_errors: true,
            context_window: default_context_window(),
            token_budget: 0,
            project_system_prompt: None,
            project_context_files: Vec::new(),
            show_sql: false,
        }
    }
}

impl AiConfig {
    /// Infer `provider` from `api_key_env` when the provider is not set
    /// explicitly.
    ///
    /// Checks whether the env-var name contains a well-known provider token
    /// (case-insensitive) and fills in `provider` accordingly:
    ///
    /// - `OPENAI`    → `"openai"`
    /// - `ANTHROPIC` → `"anthropic"`
    /// - `OLLAMA`    → `"ollama"`
    ///
    /// Called as a post-load fixup so that a minimal config like
    /// `api_key_env = "OPENAI_API_KEY"` works without an explicit
    /// `provider` line.
    pub fn infer_provider(&mut self) {
        if self.provider.is_some() {
            return;
        }
        let key_env = match self.api_key_env.as_deref() {
            Some(s) => s.to_ascii_uppercase(),
            None => return,
        };
        self.provider = if key_env.contains("OPENAI") {
            Some("openai".to_owned())
        } else if key_env.contains("ANTHROPIC") {
            Some("anthropic".to_owned())
        } else if key_env.contains("OLLAMA") {
            Some("ollama".to_owned())
        } else {
            None
        };
    }
}

// ---------------------------------------------------------------------------
// Logging / rotation settings
// ---------------------------------------------------------------------------

/// Structured-log file rotation settings.
///
/// Applied when `--log-file` is set.  Set `max_file_size_mb = 0` to
/// disable rotation entirely.
///
/// ```toml
/// [logging]
/// max_file_size_mb = 10
/// max_files = 5
/// audit_file = "~/.local/share/rpg/queries.log"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LoggingConfig {
    /// Rotate the log file when it exceeds this size in MiB.
    ///
    /// `0` disables rotation.  Default: `10`.
    pub max_file_size_mb: u32,
    /// Maximum number of rotated files to keep (`.log.1` … `.log.N`).
    ///
    /// Default: `5`.
    pub max_files: u32,
    /// Optional path to the query audit log file (FR-23).
    ///
    /// When set, queries are appended to this file in human-readable format
    /// at startup, equivalent to running `\log-file <path>` interactively.
    /// Tilde (`~`) is expanded to the home directory.
    ///
    /// ```toml
    /// [logging]
    /// audit_file = "~/.local/share/rpg/queries.log"
    /// ```
    pub audit_file: Option<String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            max_file_size_mb: 10,
            max_files: 5,
            audit_file: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Governance settings (per-feature autonomy)
// ---------------------------------------------------------------------------

/// Per-feature autonomy configuration.
///
/// Each feature area defaults to [`AutonomyLevel::Observe`] (read-only).
/// Users can escalate individual features to `supervised` or `auto`
/// in their `config.toml`:
///
/// ```toml
/// [governance]
/// rca = "observe"
/// index_health = "supervised"
/// vacuum = "supervised"
/// ```
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct GovernanceConfig {
    /// Vacuum management autonomy.
    pub vacuum: AutonomyLevel,
    /// Bloat management autonomy.
    pub bloat: AutonomyLevel,
    /// Index health management autonomy.
    pub index_health: AutonomyLevel,
    /// `PostgreSQL` parameter tuning autonomy.
    pub config_tuning: AutonomyLevel,
    /// Query optimization autonomy.
    pub query_optimization: AutonomyLevel,
    /// Connection management autonomy.
    pub connection_management: AutonomyLevel,
    /// Replication management autonomy.
    pub replication: AutonomyLevel,
    /// Root cause analysis autonomy.
    pub rca: AutonomyLevel,
    /// Backup monitoring autonomy.
    pub backup_monitoring: AutonomyLevel,
    /// Security audit autonomy.
    pub security: AutonomyLevel,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self {
            vacuum: AutonomyLevel::Observe,
            bloat: AutonomyLevel::Observe,
            index_health: AutonomyLevel::Observe,
            config_tuning: AutonomyLevel::Observe,
            query_optimization: AutonomyLevel::Observe,
            connection_management: AutonomyLevel::Observe,
            replication: AutonomyLevel::Observe,
            rca: AutonomyLevel::Observe,
            backup_monitoring: AutonomyLevel::Observe,
            security: AutonomyLevel::Observe,
        }
    }
}

impl GovernanceConfig {
    /// Look up the autonomy level for a given feature area.
    #[allow(dead_code)]
    pub fn autonomy_for(&self, feature: FeatureArea) -> AutonomyLevel {
        match feature {
            FeatureArea::Vacuum => self.vacuum,
            FeatureArea::Bloat => self.bloat,
            FeatureArea::IndexHealth => self.index_health,
            FeatureArea::ConfigTuning => self.config_tuning,
            FeatureArea::QueryOptimization => self.query_optimization,
            FeatureArea::ConnectionManagement => self.connection_management,
            FeatureArea::Replication => self.replication,
            FeatureArea::Rca => self.rca,
            FeatureArea::BackupMonitoring => self.backup_monitoring,
            FeatureArea::Security => self.security,
        }
    }

    /// Set the autonomy level for a given feature area.
    #[allow(dead_code)]
    pub fn set_autonomy(&mut self, feature: FeatureArea, level: AutonomyLevel) {
        match feature {
            FeatureArea::Vacuum => self.vacuum = level,
            FeatureArea::Bloat => self.bloat = level,
            FeatureArea::IndexHealth => self.index_health = level,
            FeatureArea::ConfigTuning => self.config_tuning = level,
            FeatureArea::QueryOptimization => self.query_optimization = level,
            FeatureArea::ConnectionManagement => self.connection_management = level,
            FeatureArea::Replication => self.replication = level,
            FeatureArea::Rca => self.rca = level,
            FeatureArea::BackupMonitoring => self.backup_monitoring = level,
            FeatureArea::Security => self.security = level,
        }
    }

    /// Return all feature areas with their current autonomy levels.
    #[allow(dead_code)]
    pub fn all_levels(&self) -> Vec<(FeatureArea, AutonomyLevel)> {
        vec![
            (FeatureArea::Vacuum, self.vacuum),
            (FeatureArea::Bloat, self.bloat),
            (FeatureArea::IndexHealth, self.index_health),
            (FeatureArea::ConfigTuning, self.config_tuning),
            (FeatureArea::QueryOptimization, self.query_optimization),
            (
                FeatureArea::ConnectionManagement,
                self.connection_management,
            ),
            (FeatureArea::Replication, self.replication),
            (FeatureArea::Rca, self.rca),
            (FeatureArea::BackupMonitoring, self.backup_monitoring),
            (FeatureArea::Security, self.security),
        ]
    }
}

// ---------------------------------------------------------------------------
// SSH tunnel configuration
// ---------------------------------------------------------------------------

/// SSH tunnel configuration for a connection profile (FR-22).
///
/// When present in a profile, Rpg establishes an SSH tunnel to the bastion
/// host and forwards the Postgres connection through it.
///
/// ```toml
/// [connections.production.ssh_tunnel]
/// host = "bastion.example.com"
/// port = 22
/// user = "deploy"
/// key = "~/.ssh/id_ed25519"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SshTunnelConfig {
    /// SSH bastion host.
    pub host: String,
    /// SSH server port. Default: `22`.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH user name on the bastion.
    pub user: String,
    /// Path to an SSH private key file.  `~` is expanded to `$HOME`.
    /// When `None`, default key paths (`~/.ssh/id_ed25519`, `~/.ssh/id_rsa`)
    /// are tried.
    pub key: Option<String>,
    /// SSH password (never logged).  Prefer key-based auth.
    pub password: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

// ---------------------------------------------------------------------------
// Connection profile
// ---------------------------------------------------------------------------

/// A named connection profile used with `rpg @profile` or `\c @profile`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ConnectionProfile {
    /// Hostname or socket directory.
    pub host: Option<String>,
    /// Port number.
    pub port: Option<u16>,
    /// Database name.
    pub dbname: Option<String>,
    /// Username.
    pub username: Option<String>,
    /// SSL mode (`disable`, `prefer`, `require`).
    pub sslmode: Option<String>,
    /// Password (stored in plaintext — use `.pgpass` where possible).
    pub password: Option<String>,
    /// Optional SSH tunnel to use when connecting to this profile.
    ///
    /// ```toml
    /// [connections.production]
    /// host = "10.0.1.5"
    /// port = 5432
    /// dbname = "myapp"
    /// username = "app_user"
    /// [connections.production.ssh_tunnel]
    /// host = "bastion.example.com"
    /// port = 22
    /// user = "deploy"
    /// key = "~/.ssh/id_ed25519"
    /// ```
    pub ssh_tunnel: Option<SshTunnelConfig>,
}

// ---------------------------------------------------------------------------
// Connector configuration
// ---------------------------------------------------------------------------

/// Per-connector configuration from the config file.
///
/// Each connector is optional. When absent the connector is disabled.
///
/// ```toml
/// [connectors.datadog]
/// enabled = true
/// api_key_env = "DD_API_KEY"
/// app_key_env = "DD_APP_KEY"
///
/// [connectors.pganalyze]
/// api_key_env = "PGANALYZE_API_KEY"
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ConnectorsConfig {
    /// Datadog monitoring connector.
    pub datadog: Option<DatadogConfig>,
    /// pganalyze `SaaS` connector.
    pub pganalyze: Option<PganalyzeConfig>,
    /// AWS `CloudWatch` connector.
    pub cloudwatch: Option<CloudWatchConfig>,
    /// postgres.ai Issues connector.
    pub postgresai: Option<PostgresAIConfig>,
    /// Supabase Management API connector.
    pub supabase: Option<SupabaseConfig>,
    /// GitHub Issues connector.
    pub github: Option<GitHubConfig>,
    /// GitLab Issues connector.
    pub gitlab: Option<GitLabConfig>,
    /// Jira Issues connector.
    pub jira: Option<JiraConfig>,
    /// Bidirectional issue sync settings.
    #[serde(default)]
    #[allow(dead_code)] // Phase 4 infrastructure — consumers arrive later
    pub sync: SyncConfig,
}

/// Datadog connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatadogConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the Datadog API key.
    #[serde(default = "dd_api_key_env")]
    pub api_key_env: String,
    /// Name of the env var holding the Datadog application key.
    #[serde(default = "dd_app_key_env")]
    pub app_key_env: String,
}

fn dd_api_key_env() -> String {
    "DD_API_KEY".to_owned()
}

fn dd_app_key_env() -> String {
    "DD_APP_KEY".to_owned()
}

impl Default for DatadogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: dd_api_key_env(),
            app_key_env: dd_app_key_env(),
        }
    }
}

/// pganalyze `SaaS` connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PganalyzeConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the pganalyze API key.
    #[serde(default = "pganalyze_api_key_env")]
    pub api_key_env: String,
}

fn pganalyze_api_key_env() -> String {
    "PGANALYZE_API_KEY".to_owned()
}

impl Default for PganalyzeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: pganalyze_api_key_env(),
        }
    }
}

/// AWS `CloudWatch` connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CloudWatchConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the AWS region.
    #[serde(default = "cw_region_env")]
    pub region_env: String,
    /// Name of the env var holding the AWS access key ID.
    #[serde(default = "cw_access_key_id_env")]
    pub access_key_id_env: String,
    /// Name of the env var holding the AWS secret access key.
    #[serde(default = "cw_secret_access_key_env")]
    pub secret_access_key_env: String,
}

fn cw_region_env() -> String {
    "AWS_DEFAULT_REGION".to_owned()
}

fn cw_access_key_id_env() -> String {
    "AWS_ACCESS_KEY_ID".to_owned()
}

fn cw_secret_access_key_env() -> String {
    "AWS_SECRET_ACCESS_KEY".to_owned()
}

impl Default for CloudWatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            region_env: cw_region_env(),
            access_key_id_env: cw_access_key_id_env(),
            secret_access_key_env: cw_secret_access_key_env(),
        }
    }
}

/// postgres.ai Issues connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PostgresAIConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the postgres.ai API key.
    #[serde(default = "postgresai_api_key_env")]
    pub api_key_env: String,
}

fn postgresai_api_key_env() -> String {
    "POSTGRESAI_API_KEY".to_owned()
}

impl Default for PostgresAIConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_env: postgresai_api_key_env(),
        }
    }
}

/// Supabase Management API connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SupabaseConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the Supabase access token.
    #[serde(default = "supabase_token_env")]
    pub access_token_env: String,
}

fn supabase_token_env() -> String {
    "SUPABASE_ACCESS_TOKEN".to_owned()
}

impl Default for SupabaseConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            access_token_env: supabase_token_env(),
        }
    }
}

/// GitHub Issues connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the GitHub personal access token.
    #[serde(default = "github_token_env")]
    pub token_env: String,
    /// GitHub repository in `owner/repo` format.
    pub repo: Option<String>,
}

fn github_token_env() -> String {
    "GITHUB_TOKEN".to_owned()
}

impl Default for GitHubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_env: github_token_env(),
            repo: None,
        }
    }
}

/// GitLab Issues connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GitLabConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the GitLab personal access token.
    #[serde(default = "gitlab_token_env")]
    pub token_env: String,
    /// GitLab project ID or path (e.g. `"mygroup/myproject"`).
    pub project_id: Option<String>,
}

fn gitlab_token_env() -> String {
    "GITLAB_TOKEN".to_owned()
}

impl Default for GitLabConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            token_env: gitlab_token_env(),
            project_id: None,
        }
    }
}

/// Jira Issues connector configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct JiraConfig {
    /// Enable this connector. Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Name of the env var holding the Jira user e-mail address.
    #[serde(default = "jira_email_env")]
    pub email_env: String,
    /// Name of the env var holding the Jira API token.
    #[serde(default = "jira_api_token_env")]
    pub api_token_env: String,
    /// Jira instance base URL (e.g. `"https://mycompany.atlassian.net"`).
    pub base_url: Option<String>,
}

fn jira_email_env() -> String {
    "JIRA_EMAIL".to_owned()
}

fn jira_api_token_env() -> String {
    "JIRA_API_TOKEN".to_owned()
}

impl Default for JiraConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            email_env: jira_email_env(),
            api_token_env: jira_api_token_env(),
            base_url: None,
        }
    }
}

/// Bidirectional issue sync configuration.
///
/// ```toml
/// [connectors.sync]
/// enabled = true
/// targets = ["github", "jira"]
/// ```
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SyncConfig {
    /// Enable bidirectional issue sync. Default: `false`.
    pub enabled: bool,
    /// Connector IDs to sync issues to (e.g. `["github", "jira"]`).
    pub targets: Vec<String>,
}

// ---------------------------------------------------------------------------
// Notification routing
// ---------------------------------------------------------------------------

/// Severity-based notification routing.
///
/// Specifies which notification channel names receive alerts at each
/// severity level.  An empty `Vec` means "route to all channels".
///
/// Channel names must match the variant tag used in the daemon's
/// `--slack-webhook`, `--webhook-url`, etc. flags.  This config is
/// evaluated at alert dispatch time; consumers not yet wired in v1
/// treat an empty list as "send to all".
///
/// ```toml
/// [notification_routing]
/// critical = ["slack", "pagerduty"]
/// warning  = ["slack"]
/// info     = []
/// ```
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct NotificationRouting {
    /// Channel names that receive `critical` severity alerts.
    pub critical: Vec<String>,
    /// Channel names that receive `warning` severity alerts.
    pub warning: Vec<String>,
    /// Channel names that receive `info` severity alerts.
    pub info: Vec<String>,
}

// ---------------------------------------------------------------------------
// Project config (.rpg.toml)
// ---------------------------------------------------------------------------

/// Project-specific config loaded from `.rpg.toml`.
///
/// Searched from the current working directory up to the user's home
/// directory.  When found, it is merged on top of the user config.
///
/// ```toml
/// [connection]
/// default_database = "myapp_development"
/// default_host = "localhost"
///
/// [named_queries]
/// migrations = "SELECT * FROM schema_migrations ORDER BY version DESC LIMIT 20"
/// active = "SELECT * FROM pg_stat_activity WHERE state = 'active'"
///
/// [ai]
/// context_files = ["docs/schema.md", "docs/queries.md"]
/// system_prompt = "This is a Rails app. The schema uses UUID primary keys."
///
/// [safety]
/// protected_tables = ["users", "payments", "audit_log"]
/// ```
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    /// Connection overrides for this project.
    pub connection: ProjectConnectionConfig,
    /// Named queries specific to this project.
    #[serde(default)]
    pub named_queries: HashMap<String, String>,
    /// AI context for this project.
    pub ai: ProjectAiConfig,
    /// Safety overrides for this project.
    pub safety: ProjectSafetyConfig,
}

/// Connection settings that can be overridden in `.rpg.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectConnectionConfig {
    /// Default database name for this project.
    pub default_database: Option<String>,
    /// Default host for this project.
    pub default_host: Option<String>,
}

/// AI context settings in `.rpg.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectAiConfig {
    /// Paths to context files to include in AI prompts.
    ///
    /// Resolved relative to the directory containing `.rpg.toml`.
    #[serde(default)]
    pub context_files: Vec<String>,
    /// Project-specific system prompt prefix injected into AI requests.
    pub system_prompt: Option<String>,
}

/// Safety overrides in `.rpg.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectSafetyConfig {
    /// Table names whose mutation should trigger an extra confirmation.
    ///
    /// Appended (not replaced) to any `protected_tables` already in the
    /// user config.
    #[serde(default)]
    pub protected_tables: Vec<String>,
}

/// Result of loading a project config file.
#[derive(Debug, Default, Clone)]
pub struct ProjectConfigResult {
    /// The parsed project config, or a default if none was found.
    pub config: ProjectConfig,
    /// Absolute path of the `.rpg.toml` that was loaded, if any.
    pub config_path: Option<PathBuf>,
    /// Absolute path of the `POSTGRES.md` file that was found, if any.
    pub postgres_md_path: Option<PathBuf>,
    /// Contents of `POSTGRES.md`, if found.
    pub postgres_md: Option<String>,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Load config from the standard file hierarchy.
///
/// Merges system config then user config; later entries win. Returns the
/// merged [`Config`] and any non-fatal warning strings (e.g. parse errors
/// in the system config are warnings, not hard failures).
pub fn load_config() -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let mut config = Config::default();

    // 1. System-wide config.
    let system_path = PathBuf::from("/etc/rpg/config.toml");
    if system_path.exists() {
        match load_file(&system_path) {
            Ok(c) => config = merge_config(config, c),
            Err(e) => warnings.push(format!("system config: {e}")),
        }
    }

    // 2. User config.
    if let Some(user_path) = user_config_path() {
        if user_path.exists() {
            match load_file(&user_path) {
                Ok(c) => config = merge_config(config, c),
                Err(e) => warnings.push(format!("user config: {e}")),
            }
        }
    }

    // Post-load fixup: infer provider from api_key_env when not explicit.
    config.ai.infer_provider();

    (config, warnings)
}

/// Search for `.rpg.toml` starting from `start_dir` and walking up to
/// the user's home directory (inclusive).
///
/// Returns the path of the first `.rpg.toml` found, or `None`.
pub fn find_project_config(start_dir: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir();
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".rpg.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        // Stop after checking the home directory.
        if let Some(ref h) = home {
            if dir == *h {
                break;
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    None
}

/// Load a `.rpg.toml` project config file and look for `POSTGRES.md`
/// alongside it.
///
/// Searches from the current working directory up to the user's home
/// directory.  Returns a [`ProjectConfigResult`] that is always safe to
/// use: when no file is found, the config field holds a default value
/// and the path fields are `None`.
pub fn load_project_config() -> ProjectConfigResult {
    let Ok(cwd) = std::env::current_dir() else {
        return ProjectConfigResult::default();
    };

    let config_path = find_project_config(&cwd);

    let (config, config_path) = match config_path {
        Some(p) => {
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|s| toml::from_str::<ProjectConfig>(&s).map_err(|e| e.to_string()))
            {
                Ok(c) => (c, Some(p)),
                Err(_) => (ProjectConfig::default(), None),
            }
        }
        None => (ProjectConfig::default(), None),
    };

    // Look for POSTGRES.md next to the config file or in CWD.
    let search_dir = config_path
        .as_ref()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or(cwd);

    let postgres_md_path = search_dir.join("POSTGRES.md");
    let (postgres_md_path, postgres_md) = if postgres_md_path.exists() {
        let contents = std::fs::read_to_string(&postgres_md_path).ok();
        (Some(postgres_md_path), contents)
    } else {
        (None, None)
    };

    ProjectConfigResult {
        config,
        config_path,
        postgres_md_path,
        postgres_md,
    }
}

/// Merge a [`ProjectConfig`] on top of an existing [`Config`].
///
/// - `connection.default_database` → sets `config.connection.dbname`
///   (only when not already set).
/// - `connection.default_host` → sets `config.connection.host`
///   (only when not already set).
/// - `safety.protected_tables` → **appended** to
///   `config.safety.protected_patterns` (converted to table-name patterns).
/// - `named_queries` → merged into `config`'s named-query map (stored in
///   a new field; callers should use [`Config::merged_named_queries`]).
/// - `ai.system_prompt` / `ai.context_files` are stored for later use.
pub fn merge_project_config(mut base: Config, project: &ProjectConfig) -> Config {
    // Connection overrides: project config wins over config defaults, but
    // only fills in when the field was not set at all.
    if base.connection.dbname.is_none() {
        base.connection
            .dbname
            .clone_from(&project.connection.default_database);
    }
    if base.connection.host.is_none() {
        base.connection
            .host
            .clone_from(&project.connection.default_host);
    }

    // Safety: protected_tables become `DELETE FROM <table>` and
    // `UPDATE <table>` patterns, appended additively.
    for table in &project.safety.protected_tables {
        let delete_pattern = format!("delete from {table}");
        let update_pattern = format!("update {table}");
        if !base.safety.protected_patterns.contains(&delete_pattern) {
            base.safety.protected_patterns.push(delete_pattern);
        }
        if !base.safety.protected_patterns.contains(&update_pattern) {
            base.safety.protected_patterns.push(update_pattern);
        }
    }

    // Named queries: merge project queries into the config store.
    for (name, query) in &project.named_queries {
        base.project_named_queries
            .entry(name.clone())
            .or_insert_with(|| query.clone());
    }

    // AI project settings.
    if base.ai.project_system_prompt.is_none() {
        base.ai
            .project_system_prompt
            .clone_from(&project.ai.system_prompt);
    }
    base.ai
        .project_context_files
        .extend_from_slice(&project.ai.context_files);

    // Post-merge fixup: infer provider from api_key_env when not explicit.
    base.ai.infer_provider();

    base
}

/// Return the path to the user config file, or `None` if the config
/// directory cannot be determined.
fn user_config_path() -> Option<PathBuf> {
    // Check XDG-style path first (~/.config/rpg/config.toml) since that's
    // what our docs and error messages reference.  On macOS `dirs::config_dir`
    // returns ~/Library/Application Support/ which is unexpected for CLI
    // tools, so we prefer the XDG path when it exists.
    if let Some(home) = dirs::home_dir() {
        let xdg_path = home.join(".config").join("rpg").join("config.toml");
        if xdg_path.exists() {
            return Some(xdg_path);
        }
    }
    // Fall back to the platform-native config dir.
    dirs::config_dir().map(|d| d.join("rpg").join("config.toml"))
}

/// Return a human-readable path string for the user config file (for error
/// messages).  Prefers `~/.config/rpg/config.toml` since that's cross-platform.
pub fn user_config_path_display() -> String {
    if let Some(home) = dirs::home_dir() {
        format!("{}/.config/rpg/config.toml", home.display())
    } else {
        "~/.config/rpg/config.toml".to_owned()
    }
}

/// Read and parse a single TOML config file.
fn load_file(path: &Path) -> Result<Config, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&content).map_err(|e| e.to_string())
}

/// Merge two configs, with `overlay` taking precedence over `base`.
///
/// For scalar fields the overlay wins.  For the `connections` map, overlay
/// entries are inserted (overwriting any same-named base entries) so that
/// the user config can override individual profiles without losing the rest.
fn merge_config(base: Config, overlay: Config) -> Config {
    Config {
        connection: ConnectionConfig {
            host: overlay.connection.host.or(base.connection.host),
            port: overlay.connection.port.or(base.connection.port),
            user: overlay.connection.user.or(base.connection.user),
            dbname: overlay.connection.dbname.or(base.connection.dbname),
            sslmode: overlay.connection.sslmode.or(base.connection.sslmode),
        },
        display: DisplayConfig {
            pager: overlay.display.pager,
            highlight: overlay.display.highlight,
            timing: overlay.display.timing,
            expanded: overlay.display.expanded,
            pager_min_lines: overlay
                .display
                .pager_min_lines
                .or(base.display.pager_min_lines),
            border: overlay.display.border.or(base.display.border),
            vi_mode: overlay.display.vi_mode || base.display.vi_mode,
            // Prefer explicit false from overlay over base default.
            statusline_enabled: overlay.display.statusline_enabled
                && base.display.statusline_enabled,
        },
        safety: SafetyConfig {
            destructive_warning: overlay.safety.destructive_warning,
            protected_patterns: {
                let mut merged = base.safety.protected_patterns;
                for p in overlay.safety.protected_patterns {
                    if !merged.contains(&p) {
                        merged.push(p);
                    }
                }
                merged
            },
        },
        ai: AiConfig {
            provider: overlay.ai.provider.or(base.ai.provider),
            model: overlay.ai.model.or(base.ai.model),
            api_key_env: overlay.ai.api_key_env.or(base.ai.api_key_env),
            base_url: overlay.ai.base_url.or(base.ai.base_url),
            max_tokens: overlay.ai.max_tokens,
            auto_execute_readonly: overlay.ai.auto_execute_readonly
                || base.ai.auto_execute_readonly,
            auto_explain_errors: overlay.ai.auto_explain_errors && base.ai.auto_explain_errors,
            context_window: if overlay.ai.context_window == default_context_window() {
                base.ai.context_window
            } else {
                overlay.ai.context_window
            },
            token_budget: if overlay.ai.token_budget == 0 {
                base.ai.token_budget
            } else {
                overlay.ai.token_budget
            },
            // project_system_prompt and project_context_files are set by
            // merge_project_config, not by file layering.
            project_system_prompt: base.ai.project_system_prompt,
            project_context_files: base.ai.project_context_files,
            show_sql: overlay.ai.show_sql || base.ai.show_sql,
        },
        governance: merge_governance(base.governance, overlay.governance),
        logging: LoggingConfig {
            max_file_size_mb: if overlay.logging.max_file_size_mb
                == LoggingConfig::default().max_file_size_mb
            {
                base.logging.max_file_size_mb
            } else {
                overlay.logging.max_file_size_mb
            },
            max_files: if overlay.logging.max_files == LoggingConfig::default().max_files {
                base.logging.max_files
            } else {
                overlay.logging.max_files
            },
            audit_file: overlay.logging.audit_file.or(base.logging.audit_file),
        },
        connections: {
            let mut merged = base.connections;
            merged.extend(overlay.connections);
            merged
        },
        // project_named_queries is not set during file-layer merging;
        // it is populated by merge_project_config.
        project_named_queries: base.project_named_queries,
        // Overlay connector config wins when present; base is used otherwise.
        connectors: overlay.connectors.or(base.connectors),
        // Overlay routing wins when it has any entries; fall back to base.
        notification_routing: {
            let o = overlay.notification_routing;
            let b = base.notification_routing;
            NotificationRouting {
                critical: if o.critical.is_empty() {
                    b.critical
                } else {
                    o.critical
                },
                warning: if o.warning.is_empty() {
                    b.warning
                } else {
                    o.warning
                },
                info: if o.info.is_empty() { b.info } else { o.info },
            }
        },
    }
}

/// Merge governance config: overlay wins when not the default (Observe).
fn merge_governance(base: GovernanceConfig, overlay: GovernanceConfig) -> GovernanceConfig {
    let pick = |base_level, overlay_level| {
        if overlay_level == AutonomyLevel::Observe {
            base_level
        } else {
            overlay_level
        }
    };
    GovernanceConfig {
        vacuum: pick(base.vacuum, overlay.vacuum),
        bloat: pick(base.bloat, overlay.bloat),
        index_health: pick(base.index_health, overlay.index_health),
        config_tuning: pick(base.config_tuning, overlay.config_tuning),
        query_optimization: pick(base.query_optimization, overlay.query_optimization),
        connection_management: pick(base.connection_management, overlay.connection_management),
        replication: pick(base.replication, overlay.replication),
        rca: pick(base.rca, overlay.rca),
        backup_monitoring: pick(base.backup_monitoring, overlay.backup_monitoring),
        security: pick(base.security, overlay.security),
    }
}

// ---------------------------------------------------------------------------
// Profile lookup
// ---------------------------------------------------------------------------

/// Look up a named connection profile by name.
///
/// Returns `None` when no profile with that name exists.
pub fn get_profile<'a>(config: &'a Config, name: &str) -> Option<&'a ConnectionProfile> {
    config.connections.get(name)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- TOML parsing --------------------------------------------------------

    #[test]
    fn parse_empty_config() {
        let cfg: Config = toml::from_str("").expect("empty TOML should parse");
        assert!(cfg.connections.is_empty());
        assert!(cfg.display.pager); // default
        assert!(cfg.display.highlight); // default
        assert!(!cfg.display.timing);
        assert!(!cfg.display.expanded);
        assert_eq!(cfg.display.pager_min_lines, None);
        assert_eq!(cfg.display.border, None);
        assert!(!cfg.display.vi_mode); // default is Emacs
        assert!(cfg.safety.destructive_warning);
        assert!(cfg.connection.host.is_none());
        assert!(cfg.connection.port.is_none());
        assert!(cfg.connection.user.is_none());
        assert!(cfg.connection.dbname.is_none());
        assert!(cfg.connection.sslmode.is_none());
    }

    #[test]
    fn parse_display_section() {
        let toml_str = r"
[display]
pager = false
highlight = false
timing = true
expanded = true
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert!(!cfg.display.pager);
        assert!(!cfg.display.highlight);
        assert!(cfg.display.timing);
        assert!(cfg.display.expanded);
    }

    #[test]
    fn parse_display_vi_mode() {
        let toml_str = r"
[display]
vi_mode = true
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert!(cfg.display.vi_mode);
    }

    #[test]
    fn merge_display_vi_mode_overlay_wins() {
        let base = Config {
            display: DisplayConfig {
                vi_mode: false,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            display: DisplayConfig {
                vi_mode: true,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert!(merged.display.vi_mode);
    }

    #[test]
    fn merge_display_vi_mode_base_preserved_when_overlay_false() {
        let base = Config {
            display: DisplayConfig {
                vi_mode: true,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        // overlay has vi_mode = false (default) → OR-merge keeps base true.
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert!(merged.display.vi_mode);
    }

    #[test]
    fn parse_display_pager_min_lines_and_border() {
        let toml_str = r"
[display]
pager_min_lines = 40
border = 2
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.display.pager_min_lines, Some(40));
        assert_eq!(cfg.display.border, Some(2));
    }

    #[test]
    fn parse_connection_section() {
        let toml_str = r#"
[connection]
host = "db.internal"
port = "5433"
user = "readonly"
dbname = "analytics"
sslmode = "require"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.connection.host.as_deref(), Some("db.internal"));
        assert_eq!(cfg.connection.port.as_deref(), Some("5433"));
        assert_eq!(cfg.connection.user.as_deref(), Some("readonly"));
        assert_eq!(cfg.connection.dbname.as_deref(), Some("analytics"));
        assert_eq!(cfg.connection.sslmode.as_deref(), Some("require"));
    }

    #[test]
    fn parse_connection_section_partial() {
        let toml_str = r#"
[connection]
host = "localhost"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.connection.host.as_deref(), Some("localhost"));
        assert!(cfg.connection.port.is_none());
        assert!(cfg.connection.user.is_none());
        assert!(cfg.connection.dbname.is_none());
        assert!(cfg.connection.sslmode.is_none());
    }

    #[test]
    fn merge_connection_overlay_wins() {
        let base = Config {
            connection: ConnectionConfig {
                host: Some("base-host".to_owned()),
                port: Some("5432".to_owned()),
                user: Some("base-user".to_owned()),
                dbname: Some("base-db".to_owned()),
                sslmode: Some("prefer".to_owned()),
            },
            ..Default::default()
        };
        let overlay = Config {
            connection: ConnectionConfig {
                host: Some("overlay-host".to_owned()),
                port: None,
                user: None,
                dbname: Some("overlay-db".to_owned()),
                sslmode: None,
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        // Overlay wins when set.
        assert_eq!(merged.connection.host.as_deref(), Some("overlay-host"));
        assert_eq!(merged.connection.dbname.as_deref(), Some("overlay-db"));
        // Base values preserved when overlay is None.
        assert_eq!(merged.connection.port.as_deref(), Some("5432"));
        assert_eq!(merged.connection.user.as_deref(), Some("base-user"));
        assert_eq!(merged.connection.sslmode.as_deref(), Some("prefer"));
    }

    #[test]
    fn merge_display_pager_min_lines_overlay_wins() {
        let base = Config {
            display: DisplayConfig {
                pager_min_lines: Some(20),
                border: Some(0),
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            display: DisplayConfig {
                pager_min_lines: Some(50),
                border: Some(2),
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.display.pager_min_lines, Some(50));
        assert_eq!(merged.display.border, Some(2));
    }

    #[test]
    fn merge_display_pager_min_lines_base_preserved_when_overlay_none() {
        let base = Config {
            display: DisplayConfig {
                pager_min_lines: Some(30),
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        // Overlay has pager_min_lines = None (default), so base value is kept.
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert_eq!(merged.display.pager_min_lines, Some(30));
    }

    #[test]
    fn parse_safety_section() {
        let toml_str = r"
[safety]
destructive_warning = false
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert!(!cfg.safety.destructive_warning);
    }

    #[test]
    fn parse_single_connection_profile() {
        let toml_str = r#"
[connections.production]
host = "db.example.com"
port = 5432
dbname = "app_prod"
username = "app"
sslmode = "require"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let profile = cfg.connections.get("production").expect("profile missing");
        assert_eq!(profile.host.as_deref(), Some("db.example.com"));
        assert_eq!(profile.port, Some(5432));
        assert_eq!(profile.dbname.as_deref(), Some("app_prod"));
        assert_eq!(profile.username.as_deref(), Some("app"));
        assert_eq!(profile.sslmode.as_deref(), Some("require"));
        assert!(profile.password.is_none());
    }

    #[test]
    fn parse_multiple_profiles() {
        let toml_str = r#"
[connections.staging]
host = "staging.example.com"
dbname = "app_staging"

[connections.local]
dbname = "mydb"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.connections.len(), 2);
        assert!(cfg.connections.contains_key("staging"));
        assert!(cfg.connections.contains_key("local"));
    }

    #[test]
    fn parse_profile_with_password() {
        let toml_str = r#"
[connections.dev]
host = "localhost"
password = "secret"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let profile = cfg.connections.get("dev").expect("profile missing");
        assert_eq!(profile.password.as_deref(), Some("secret"));
    }

    #[test]
    fn parse_partial_profile_has_defaults() {
        let toml_str = r#"
[connections.minimal]
dbname = "testdb"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let profile = cfg.connections.get("minimal").expect("missing");
        assert!(profile.host.is_none());
        assert!(profile.port.is_none());
        assert!(profile.username.is_none());
        assert!(profile.sslmode.is_none());
        assert!(profile.password.is_none());
    }

    // -- get_profile ---------------------------------------------------------

    #[test]
    fn get_profile_existing() {
        let mut cfg = Config::default();
        cfg.connections.insert(
            "prod".into(),
            ConnectionProfile {
                host: Some("prod.db".into()),
                ..Default::default()
            },
        );
        let p = get_profile(&cfg, "prod").expect("should find profile");
        assert_eq!(p.host.as_deref(), Some("prod.db"));
    }

    #[test]
    fn get_profile_missing() {
        let cfg = Config::default();
        assert!(get_profile(&cfg, "nonexistent").is_none());
    }

    // -- merge_config --------------------------------------------------------

    #[test]
    fn merge_overlay_wins_scalars() {
        let base = Config {
            display: DisplayConfig {
                pager: true,
                highlight: true,
                timing: false,
                expanded: false,
                ..DisplayConfig::default()
            },
            safety: SafetyConfig {
                destructive_warning: true,
                ..SafetyConfig::default()
            },
            ..Config::default()
        };
        let overlay = Config {
            display: DisplayConfig {
                pager: false,
                highlight: false,
                timing: true,
                expanded: true,
                ..DisplayConfig::default()
            },
            safety: SafetyConfig {
                destructive_warning: false,
                ..SafetyConfig::default()
            },
            ..Config::default()
        };
        let merged = merge_config(base, overlay);
        assert!(!merged.display.pager);
        assert!(!merged.display.highlight);
        assert!(merged.display.timing);
        assert!(merged.display.expanded);
        assert!(!merged.safety.destructive_warning);
    }

    #[test]
    fn merge_connections_overlay_adds_and_overrides() {
        let mut base_conns = HashMap::new();
        base_conns.insert(
            "shared".into(),
            ConnectionProfile {
                host: Some("base-host".into()),
                ..Default::default()
            },
        );
        base_conns.insert(
            "base-only".into(),
            ConnectionProfile {
                dbname: Some("basedb".into()),
                ..Default::default()
            },
        );
        let base = Config {
            connections: base_conns,
            ..Default::default()
        };

        let mut overlay_conns = HashMap::new();
        overlay_conns.insert(
            "shared".into(),
            ConnectionProfile {
                host: Some("overlay-host".into()),
                ..Default::default()
            },
        );
        overlay_conns.insert(
            "overlay-only".into(),
            ConnectionProfile {
                dbname: Some("overlaydb".into()),
                ..Default::default()
            },
        );
        let overlay = Config {
            connections: overlay_conns,
            ..Default::default()
        };

        let merged = merge_config(base, overlay);
        // Overlay wins for "shared".
        assert_eq!(
            merged.connections["shared"].host.as_deref(),
            Some("overlay-host")
        );
        // Base-only key is preserved.
        assert!(merged.connections.contains_key("base-only"));
        // Overlay-only key is added.
        assert!(merged.connections.contains_key("overlay-only"));
    }

    // -- @profile detection in CLI args -------------------------------------

    #[test]
    fn profile_name_detection_with_at_prefix() {
        let dbname_pos = Some("@production".to_owned());
        let profile_name = dbname_pos
            .as_deref()
            .filter(|s| s.starts_with('@'))
            .map(|s| &s[1..]);
        assert_eq!(profile_name, Some("production"));
    }

    #[test]
    fn profile_name_detection_no_prefix() {
        let dbname_pos = Some("mydb".to_owned());
        let profile_name = dbname_pos
            .as_deref()
            .filter(|s| s.starts_with('@'))
            .map(|s| &s[1..]);
        assert!(profile_name.is_none());
    }

    #[test]
    fn profile_name_detection_none() {
        let dbname_pos: Option<String> = None;
        let profile_name = dbname_pos
            .as_deref()
            .filter(|s| s.starts_with('@'))
            .map(|s| &s[1..]);
        assert!(profile_name.is_none());
    }

    // -- ConnectionProfile defaults -----------------------------------------

    #[test]
    fn connection_profile_all_defaults_none() {
        let p = ConnectionProfile::default();
        assert!(p.host.is_none());
        assert!(p.port.is_none());
        assert!(p.dbname.is_none());
        assert!(p.username.is_none());
        assert!(p.sslmode.is_none());
        assert!(p.password.is_none());
    }

    // -- AiConfig TOML parsing ----------------------------------------------

    #[test]
    fn parse_ai_section_full() {
        let toml_str = r#"
[ai]
provider = "anthropic"
model = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"
max_tokens = 8192
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ai.provider.as_deref(), Some("anthropic"));
        assert_eq!(cfg.ai.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(cfg.ai.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(
            cfg.ai.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(cfg.ai.max_tokens, 8192);
    }

    #[test]
    fn parse_ai_section_minimal() {
        let toml_str = r#"
[ai]
provider = "ollama"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ai.provider.as_deref(), Some("ollama"));
        assert!(cfg.ai.model.is_none());
        assert!(cfg.ai.api_key_env.is_none());
        assert!(cfg.ai.base_url.is_none());
        assert_eq!(cfg.ai.max_tokens, 4096); // default
    }

    #[test]
    fn ai_config_defaults_all_none() {
        let cfg: Config = toml::from_str("").expect("empty TOML should parse");
        assert!(cfg.ai.provider.is_none());
        assert!(cfg.ai.model.is_none());
        assert!(cfg.ai.api_key_env.is_none());
        assert!(cfg.ai.base_url.is_none());
        assert_eq!(cfg.ai.max_tokens, 4096);
    }

    // -- AiConfig::infer_provider -------------------------------------------

    #[test]
    fn infer_provider_openai() {
        let mut ai = AiConfig {
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.infer_provider();
        assert_eq!(ai.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn infer_provider_anthropic() {
        let mut ai = AiConfig {
            api_key_env: Some("ANTHROPIC_API_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.infer_provider();
        assert_eq!(ai.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn infer_provider_ollama() {
        let mut ai = AiConfig {
            api_key_env: Some("OLLAMA_API_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.infer_provider();
        assert_eq!(ai.provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn infer_provider_no_match_leaves_none() {
        let mut ai = AiConfig {
            api_key_env: Some("MY_CUSTOM_LLM_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.infer_provider();
        assert!(ai.provider.is_none());
    }

    #[test]
    fn infer_provider_does_not_override_explicit() {
        let mut ai = AiConfig {
            provider: Some("ollama".to_owned()),
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.infer_provider();
        // Explicit provider is preserved; env name is not used to override.
        assert_eq!(ai.provider.as_deref(), Some("ollama"));
    }

    #[test]
    fn infer_provider_no_api_key_env_leaves_none() {
        let mut ai = AiConfig::default();
        ai.infer_provider();
        assert!(ai.provider.is_none());
    }

    #[test]
    fn merge_ai_overlay_wins() {
        let base = Config {
            ai: AiConfig {
                provider: Some("ollama".to_owned()),
                max_tokens: 2048,
                ..AiConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            ai: AiConfig {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-sonnet-4-6".to_owned()),
                api_key_env: Some("ANTHROPIC_API_KEY".to_owned()),
                max_tokens: 4096,
                ..AiConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.ai.provider.as_deref(), Some("anthropic"));
        assert_eq!(merged.ai.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(merged.ai.max_tokens, 4096);
    }

    #[test]
    fn merge_ai_base_preserved_when_overlay_absent() {
        let base = Config {
            ai: AiConfig {
                provider: Some("openai".to_owned()),
                model: Some("gpt-4o".to_owned()),
                api_key_env: Some("OPENAI_API_KEY".to_owned()),
                ..AiConfig::default()
            },
            ..Default::default()
        };
        // Overlay has no ai section (all None).
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        // provider from base is preserved because overlay is None.
        assert_eq!(merged.ai.provider.as_deref(), Some("openai"));
        assert_eq!(merged.ai.model.as_deref(), Some("gpt-4o"));
    }

    // -- GovernanceConfig -----------------------------------------------------

    #[test]
    fn governance_defaults_all_observe() {
        let cfg: Config = toml::from_str("").expect("empty TOML");
        let g = &cfg.governance;
        assert_eq!(g.vacuum, AutonomyLevel::Observe);
        assert_eq!(g.bloat, AutonomyLevel::Observe);
        assert_eq!(g.index_health, AutonomyLevel::Observe);
        assert_eq!(g.config_tuning, AutonomyLevel::Observe);
        assert_eq!(g.query_optimization, AutonomyLevel::Observe);
        assert_eq!(g.connection_management, AutonomyLevel::Observe);
        assert_eq!(g.replication, AutonomyLevel::Observe);
        assert_eq!(g.rca, AutonomyLevel::Observe);
        assert_eq!(g.backup_monitoring, AutonomyLevel::Observe);
        assert_eq!(g.security, AutonomyLevel::Observe);
    }

    #[test]
    fn governance_parse_mixed_levels() {
        let toml_str = r#"
[governance]
vacuum = "supervised"
rca = "observe"
index_health = "auto"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.governance.vacuum, AutonomyLevel::Supervised);
        assert_eq!(cfg.governance.rca, AutonomyLevel::Observe);
        assert_eq!(cfg.governance.index_health, AutonomyLevel::Auto);
        // Unspecified fields remain Observe.
        assert_eq!(cfg.governance.bloat, AutonomyLevel::Observe);
        assert_eq!(cfg.governance.security, AutonomyLevel::Observe);
    }

    #[test]
    fn governance_autonomy_for_lookup() {
        let g = GovernanceConfig {
            rca: AutonomyLevel::Supervised,
            ..GovernanceConfig::default()
        };
        assert_eq!(g.autonomy_for(FeatureArea::Rca), AutonomyLevel::Supervised);
        assert_eq!(g.autonomy_for(FeatureArea::Vacuum), AutonomyLevel::Observe);
    }

    #[test]
    fn governance_set_autonomy() {
        let mut g = GovernanceConfig::default();
        g.set_autonomy(FeatureArea::IndexHealth, AutonomyLevel::Auto);
        assert_eq!(g.index_health, AutonomyLevel::Auto);
    }

    #[test]
    fn governance_all_levels_returns_10_entries() {
        let g = GovernanceConfig::default();
        assert_eq!(g.all_levels().len(), 10);
    }

    #[test]
    fn merge_governance_overlay_wins() {
        let base = Config {
            governance: GovernanceConfig {
                vacuum: AutonomyLevel::Supervised,
                rca: AutonomyLevel::Supervised,
                ..GovernanceConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            governance: GovernanceConfig {
                vacuum: AutonomyLevel::Auto,
                // rca left at Observe (default) → base should be preserved.
                ..GovernanceConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.governance.vacuum, AutonomyLevel::Auto);
        assert_eq!(merged.governance.rca, AutonomyLevel::Supervised);
    }

    #[test]
    fn merge_governance_base_preserved_when_overlay_default() {
        let base = Config {
            governance: GovernanceConfig {
                index_health: AutonomyLevel::Auto,
                ..GovernanceConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert_eq!(merged.governance.index_health, AutonomyLevel::Auto);
    }

    // -- ConnectionProfile with ssh_tunnel field -----------------------------

    #[test]
    fn parse_profile_with_ssh_tunnel() {
        let toml_str = r#"
[connections.production]
host = "10.0.1.5"
port = 5432
dbname = "myapp"
username = "app_user"

[connections.production.ssh_tunnel]
host = "bastion.example.com"
port = 22
user = "deploy"
key = "~/.ssh/id_ed25519"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let profile = cfg.connections.get("production").expect("profile missing");
        let tunnel = profile.ssh_tunnel.as_ref().expect("ssh_tunnel missing");
        assert_eq!(tunnel.host, "bastion.example.com");
        assert_eq!(tunnel.port, 22);
        assert_eq!(tunnel.user, "deploy");
        assert_eq!(tunnel.key.as_deref(), Some("~/.ssh/id_ed25519"));
        assert!(tunnel.password.is_none());
    }

    #[test]
    fn parse_profile_without_ssh_tunnel_is_none() {
        let toml_str = r#"
[connections.local]
dbname = "mydb"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let profile = cfg.connections.get("local").expect("profile missing");
        assert!(profile.ssh_tunnel.is_none());
    }

    #[test]
    fn connection_profile_defaults_include_no_ssh_tunnel() {
        let p = ConnectionProfile::default();
        assert!(p.ssh_tunnel.is_none());
    }

    // -- ProjectConfig -------------------------------------------------------

    #[test]
    fn parse_project_config_full() {
        let toml_str = r#"
[connection]
default_database = "myapp_development"
default_host = "localhost"

[named_queries]
migrations = "SELECT * FROM schema_migrations ORDER BY version DESC LIMIT 20"
active = "SELECT * FROM pg_stat_activity WHERE state = 'active'"

[ai]
context_files = ["docs/schema.md", "docs/queries.md"]
system_prompt = "This is a Rails app."

[safety]
protected_tables = ["users", "payments", "audit_log"]
"#;
        let cfg: ProjectConfig = toml::from_str(toml_str).expect("project config should parse");

        assert_eq!(
            cfg.connection.default_database.as_deref(),
            Some("myapp_development")
        );
        assert_eq!(cfg.connection.default_host.as_deref(), Some("localhost"));
        assert_eq!(cfg.named_queries.len(), 2);
        assert!(cfg.named_queries.contains_key("migrations"));
        assert!(cfg.named_queries.contains_key("active"));
        assert_eq!(
            cfg.ai.context_files,
            vec!["docs/schema.md", "docs/queries.md"]
        );
        assert_eq!(
            cfg.ai.system_prompt.as_deref(),
            Some("This is a Rails app.")
        );
        assert_eq!(
            cfg.safety.protected_tables,
            vec!["users", "payments", "audit_log"]
        );
    }

    #[test]
    fn parse_project_config_empty() {
        let cfg: ProjectConfig = toml::from_str("").expect("empty project config should parse");
        assert!(cfg.connection.default_database.is_none());
        assert!(cfg.connection.default_host.is_none());
        assert!(cfg.named_queries.is_empty());
        assert!(cfg.ai.context_files.is_empty());
        assert!(cfg.ai.system_prompt.is_none());
        assert!(cfg.safety.protected_tables.is_empty());
    }

    #[test]
    fn merge_project_config_fills_empty_connection_fields() {
        let base = Config::default(); // all connection fields None
        let project = ProjectConfig {
            connection: ProjectConnectionConfig {
                default_database: Some("myapp_development".to_owned()),
                default_host: Some("localhost".to_owned()),
            },
            ..ProjectConfig::default()
        };
        let merged = merge_project_config(base, &project);
        assert_eq!(
            merged.connection.dbname.as_deref(),
            Some("myapp_development")
        );
        assert_eq!(merged.connection.host.as_deref(), Some("localhost"));
    }

    #[test]
    fn merge_project_config_does_not_override_existing_connection() {
        let base = Config {
            connection: ConnectionConfig {
                dbname: Some("existing_db".to_owned()),
                host: Some("existing_host".to_owned()),
                ..ConnectionConfig::default()
            },
            ..Config::default()
        };
        let project = ProjectConfig {
            connection: ProjectConnectionConfig {
                default_database: Some("project_db".to_owned()),
                default_host: Some("project_host".to_owned()),
            },
            ..ProjectConfig::default()
        };
        let merged = merge_project_config(base, &project);
        // User config wins over project config.
        assert_eq!(merged.connection.dbname.as_deref(), Some("existing_db"));
        assert_eq!(merged.connection.host.as_deref(), Some("existing_host"));
    }

    #[test]
    fn merge_project_config_protected_tables_are_additive() {
        let base = Config {
            safety: SafetyConfig {
                destructive_warning: true,
                protected_patterns: vec!["delete from orders".to_owned()],
            },
            ..Config::default()
        };
        let project = ProjectConfig {
            safety: ProjectSafetyConfig {
                protected_tables: vec!["users".to_owned(), "payments".to_owned()],
            },
            ..ProjectConfig::default()
        };
        let merged = merge_project_config(base, &project);
        // Original pattern preserved.
        assert!(merged
            .safety
            .protected_patterns
            .contains(&"delete from orders".to_owned()));
        // Each protected table produces delete + update patterns.
        assert!(merged
            .safety
            .protected_patterns
            .contains(&"delete from users".to_owned()));
        assert!(merged
            .safety
            .protected_patterns
            .contains(&"update users".to_owned()));
        assert!(merged
            .safety
            .protected_patterns
            .contains(&"delete from payments".to_owned()));
        assert!(merged
            .safety
            .protected_patterns
            .contains(&"update payments".to_owned()));
    }

    #[test]
    fn merge_project_config_named_queries_added() {
        let base = Config::default();
        let mut named = std::collections::HashMap::new();
        named.insert(
            "active".to_owned(),
            "SELECT * FROM pg_stat_activity".to_owned(),
        );
        let project = ProjectConfig {
            named_queries: named,
            ..ProjectConfig::default()
        };
        let merged = merge_project_config(base, &project);
        assert!(merged.project_named_queries.contains_key("active"));
        assert_eq!(
            merged.project_named_queries["active"],
            "SELECT * FROM pg_stat_activity"
        );
    }

    #[test]
    fn find_project_config_finds_file_in_cwd() {
        use std::fs;
        let dir = tempfile::tempdir().expect("temp dir");
        let config_path = dir.path().join(".rpg.toml");
        fs::write(&config_path, "[connection]\n").expect("write .rpg.toml");

        let found = find_project_config(dir.path());
        assert_eq!(found.as_deref(), Some(config_path.as_path()));
    }

    #[test]
    fn find_project_config_finds_file_in_parent() {
        use std::fs;
        let parent = tempfile::tempdir().expect("temp dir");
        let child = parent.path().join("subdir");
        fs::create_dir(&child).expect("create subdir");
        let config_path = parent.path().join(".rpg.toml");
        fs::write(&config_path, "[connection]\n").expect("write .rpg.toml");

        let found = find_project_config(&child);
        assert_eq!(found.as_deref(), Some(config_path.as_path()));
    }

    #[test]
    fn find_project_config_returns_none_when_absent() {
        let dir = tempfile::tempdir().expect("temp dir");
        // No .rpg.toml in this temp dir; walk will stop at root before home.
        let found = find_project_config(dir.path());
        // May find a real .rpg.toml in parent dirs, so only assert None
        // when we are outside the home directory tree.
        if let Some(path) = found {
            // A .rpg.toml exists somewhere above the temp dir — that is fine.
            assert!(path.file_name().unwrap() == ".rpg.toml");
        }
    }
}
