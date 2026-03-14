//! TOML configuration file loading for Samo.
//!
//! Config hierarchy (later entries override earlier):
//! 1. `/etc/samo/config.toml` (system-wide)
//! 2. `~/.config/samo/config.toml` (user)
//! 3. `SAMO_*` environment variables
//! 4. CLI flags
//! 5. `\set` commands (runtime)

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
    /// Minimum output lines before the pager activates. Default: `0` (always).
    pub pager_min_lines: usize,
    /// Table border style (`0`, `1`, or `2`). Mirrors `\pset border`. Default: `1`.
    pub border: u8,
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
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            pager: true,
            highlight: true,
            timing: false,
            expanded: false,
            pager_min_lines: 0,
            border: 1,
            vi_mode: false,
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
            show_sql: false,
        }
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
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            max_file_size_mb: 10,
            max_files: 5,
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
// Connection profile
// ---------------------------------------------------------------------------

/// A named connection profile used with `samo @profile` or `\c @profile`.
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
    let system_path = PathBuf::from("/etc/samo/config.toml");
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

    (config, warnings)
}

/// Return the path to the user config file, or `None` if the config
/// directory cannot be determined.
fn user_config_path() -> Option<PathBuf> {
    // Check XDG-style path first (~/.config/samo/config.toml) since that's
    // what our docs and error messages reference.  On macOS `dirs::config_dir`
    // returns ~/Library/Application Support/ which is unexpected for CLI
    // tools, so we prefer the XDG path when it exists.
    if let Some(home) = dirs::home_dir() {
        let xdg_path = home.join(".config").join("samo").join("config.toml");
        if xdg_path.exists() {
            return Some(xdg_path);
        }
    }
    // Fall back to the platform-native config dir.
    dirs::config_dir().map(|d| d.join("samo").join("config.toml"))
}

/// Return a human-readable path string for the user config file (for error
/// messages).  Prefers `~/.config/samo/config.toml` since that's cross-platform.
pub fn user_config_path_display() -> String {
    if let Some(home) = dirs::home_dir() {
        format!("{}/.config/samo/config.toml", home.display())
    } else {
        "~/.config/samo/config.toml".to_owned()
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
            pager_min_lines: if overlay.display.pager_min_lines == 0 {
                base.display.pager_min_lines
            } else {
                overlay.display.pager_min_lines
            },
            border: if overlay.display.border == 1 {
                base.display.border
            } else {
                overlay.display.border
            },
            vi_mode: overlay.display.vi_mode || base.display.vi_mode,
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
        },
        connections: {
            let mut merged = base.connections;
            merged.extend(overlay.connections);
            merged
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
        assert_eq!(cfg.display.pager_min_lines, 0);
        assert_eq!(cfg.display.border, 1);
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
        assert_eq!(cfg.display.pager_min_lines, 40);
        assert_eq!(cfg.display.border, 2);
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
                pager_min_lines: 20,
                border: 0,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            display: DisplayConfig {
                pager_min_lines: 50,
                border: 2,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.display.pager_min_lines, 50);
        assert_eq!(merged.display.border, 2);
    }

    #[test]
    fn merge_display_pager_min_lines_base_preserved_when_overlay_zero() {
        let base = Config {
            display: DisplayConfig {
                pager_min_lines: 30,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        // Overlay has pager_min_lines = 0 (default), so base value is kept.
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert_eq!(merged.display.pager_min_lines, 30);
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
            ai: AiConfig::default(),
            governance: GovernanceConfig::default(),
            logging: LoggingConfig::default(),
            connections: HashMap::new(),
            connection: ConnectionConfig::default(),
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
            ai: AiConfig::default(),
            governance: GovernanceConfig::default(),
            logging: LoggingConfig::default(),
            connections: HashMap::new(),
            connection: ConnectionConfig::default(),
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

    #[test]
    fn merge_ai_overlay_wins() {
        let base = Config {
            ai: AiConfig {
                provider: Some("ollama".to_owned()),
                model: None,
                api_key_env: None,
                base_url: None,
                max_tokens: 2048,
                auto_execute_readonly: false,
                auto_explain_errors: true,
                context_window: 128_000,
                token_budget: 0,
                show_sql: false,
            },
            ..Default::default()
        };
        let overlay = Config {
            ai: AiConfig {
                provider: Some("anthropic".to_owned()),
                model: Some("claude-sonnet-4-6".to_owned()),
                api_key_env: Some("ANTHROPIC_API_KEY".to_owned()),
                base_url: None,
                max_tokens: 4096,
                auto_execute_readonly: false,
                auto_explain_errors: true,
                context_window: 128_000,
                token_budget: 0,
                show_sql: false,
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
                base_url: None,
                max_tokens: 4096,
                auto_execute_readonly: false,
                auto_explain_errors: true,
                context_window: 128_000,
                token_budget: 0,
                show_sql: false,
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
}
