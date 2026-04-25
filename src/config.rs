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
    /// pgMustard EXPLAIN visualiser settings.
    pub pgmustard: PgMustardConfig,
    /// Structured-log file rotation settings.
    pub logging: LoggingConfig,
    /// Active Session History (`/ash`) settings.
    pub ash: AshConfig,
    /// Named connection profiles (keyed by profile name).
    #[serde(default)]
    pub connections: HashMap<String, ConnectionProfile>,
    /// Named queries loaded from the project config (`.rpg.toml`).
    ///
    /// Not present in user/system config files — populated by
    /// [`merge_project_config`] after project config is loaded.
    #[serde(skip)]
    pub project_named_queries: HashMap<String, String>,
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
    /// Enable the pgcli-style visual dropdown completion menu.
    ///
    /// **Experimental** — disabled by default.  When `false` (the default),
    /// Tab completion still works (longest-common-prefix / cycling via
    /// rustyline) but no visual overlay is shown.  Set to `true` to opt in:
    ///
    /// ```toml
    /// [display]
    /// dropdown_completion = true
    /// ```
    ///
    /// Can also be enabled via the `RPG_DROPDOWN_COMPLETION=1` environment
    /// variable.
    pub dropdown_completion: bool,
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
            // Experimental — disabled by default.
            dropdown_completion: false,
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
    /// Timeout in seconds for LLM HTTP requests.
    ///
    /// When the AI provider does not respond within this many seconds the
    /// request is cancelled and an error is shown.  Defaults to `30`.
    ///
    /// ```toml
    /// [ai]
    /// timeout = 60
    /// ```
    #[serde(default = "default_ai_timeout")]
    pub timeout: u64,
}

fn default_context_window() -> u32 {
    128_000
}

fn default_ai_timeout() -> u64 {
    30
}

fn default_ssh_connect_timeout() -> u64 {
    15
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
            timeout: default_ai_timeout(),
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

    /// Auto-detect provider from well-known environment variables when
    /// neither `api_key_env` nor `provider` has been set explicitly.
    ///
    /// Probes environment variables in priority order:
    ///
    /// 1. `ANTHROPIC_API_KEY` → provider `"anthropic"`
    /// 2. `OPENAI_API_KEY`    → provider `"openai"`
    /// 3. `OLLAMA_API_KEY`    → provider `"ollama"`
    ///
    /// Stops at the first non-empty variable found.  Called after
    /// [`infer_provider`] so that an explicit `api_key_env` config value
    /// always takes precedence.
    pub fn auto_detect_provider(&mut self) {
        const CANDIDATES: &[(&str, &str)] = &[
            ("ANTHROPIC_API_KEY", "anthropic"),
            ("OPENAI_API_KEY", "openai"),
            ("OLLAMA_API_KEY", "ollama"),
        ];
        // Only probe when the config carries no explicit settings.
        if self.api_key_env.is_some() || self.provider.is_some() {
            return;
        }
        for (env_var, provider_name) in CANDIDATES {
            match std::env::var(env_var) {
                Ok(val) if !val.is_empty() => {
                    self.api_key_env = Some((*env_var).to_owned());
                    self.provider = Some((*provider_name).to_owned());
                    return;
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// pgMustard settings
// ---------------------------------------------------------------------------

/// pgMustard EXPLAIN visualiser settings.
///
/// pgMustard requires an API key to upload plans.  Set the name of the
/// environment variable that holds the key:
///
/// ```toml
/// [pgmustard]
/// api_key_env = "PGMUSTARD_API_KEY"
/// ```
///
/// When `api_key_env` is not set in the config file, the implementation
/// falls back to reading `PGMUSTARD_API_KEY` directly from the environment.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct PgMustardConfig {
    /// Name of the environment variable holding the pgMustard API key.
    ///
    /// Example: `"PGMUSTARD_API_KEY"`.
    ///
    /// When absent the code looks up `PGMUSTARD_API_KEY` directly.
    pub api_key_env: Option<String>,
}

impl PgMustardConfig {
    /// Resolve the API key.
    ///
    /// Reads from the env-var named by `api_key_env`, falling back to the
    /// default `PGMUSTARD_API_KEY` variable when `api_key_env` is not set.
    ///
    /// Returns `None` when the variable is unset or empty.
    pub fn resolve_api_key(&self) -> Option<String> {
        let var_name = self.api_key_env.as_deref().unwrap_or("PGMUSTARD_API_KEY");
        match std::env::var(var_name) {
            Ok(val) if !val.is_empty() => Some(val),
            _ => None,
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
// ASH (Active Session History) settings
// ---------------------------------------------------------------------------

/// Active Session History (`/ash`) settings.
///
/// Controls the timeout for both the live sample query and the history query
/// used by the `/ash` TUI sampler.
///
/// ```toml
/// [ash]
/// sample_timeout_ms = 500  # 0 = disabled
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct AshConfig {
    /// Maximum time in milliseconds allowed for a single `/ash` sample query
    /// (`pg_stat_activity` or `ash.wait_timeline`).
    ///
    /// Matches the same guard used by `pg_ash`'s per-second `pg_cron` job —
    /// fail fast rather than block the TUI.
    ///
    /// Set to `0` to disable the timeout entirely (for power users on very
    /// large clusters where the default 500 ms is too tight).
    ///
    /// Default: `500`.
    pub sample_timeout_ms: Option<u64>,
}

const DEFAULT_ASH_SAMPLE_TIMEOUT_MS: u64 = 500;

impl AshConfig {
    /// Resolved timeout value: returns the configured value or the default
    /// (500 ms) when not explicitly set.
    pub fn sample_timeout_ms(&self) -> u64 {
        self.sample_timeout_ms
            .unwrap_or(DEFAULT_ASH_SAMPLE_TIMEOUT_MS)
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
/// strict_host_key_checking = true
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SshTunnelConfig {
    /// SSH bastion host.
    pub host: String,
    /// SSH server port. Default: `22`.
    pub port: u16,
    /// SSH user name on the bastion.
    pub user: String,
    /// Path to an SSH private key file.  `~` is expanded to `$HOME`.
    /// When `None`, default key paths (`~/.ssh/id_ed25519`, `~/.ssh/id_rsa`)
    /// are tried.
    pub key: Option<String>,
    /// SSH password (never logged).  Prefer key-based auth.
    pub password: Option<String>,
    /// Enforce strict host key checking against `~/.ssh/known_hosts`.
    ///
    /// When `true` (default): unknown hosts are rejected; key mismatches
    /// are hard errors.  When `false`: unknown hosts are accepted on first
    /// use (TOFU) and recorded in `known_hosts`; key mismatches emit a
    /// warning but still fail the connection.
    pub strict_host_key_checking: bool,
    /// Timeout in seconds for establishing the SSH connection.
    ///
    /// If the SSH handshake does not complete within this many seconds the
    /// attempt is cancelled.  Defaults to `15`.
    ///
    /// ```toml
    /// [connections.production.ssh_tunnel]
    /// connect_timeout = 30
    /// ```
    #[serde(default = "default_ssh_connect_timeout")]
    pub connect_timeout: u64,
}

impl Default for SshTunnelConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            key: None,
            password: None,
            strict_host_key_checking: true,
            connect_timeout: default_ssh_connect_timeout(),
        }
    }
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
#[cfg(not(target_arch = "wasm32"))]
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

    // Post-load fixup: infer provider from api_key_env when not explicit,
    // then fall back to probing well-known env vars (zero-config case).
    config.ai.infer_provider();
    config.ai.auto_detect_provider();

    // Apply RPG_* environment variable overrides.
    // RPG_DROPDOWN_COMPLETION=1 enables the experimental dropdown menu.
    if std::env::var("RPG_DROPDOWN_COMPLETION").as_deref() == Ok("1") {
        config.display.dropdown_completion = true;
    }

    (config, warnings)
}

/// WASM stub: no filesystem, return defaults.
#[cfg(target_arch = "wasm32")]
pub fn load_config() -> (Config, Vec<String>) {
    let mut config = Config::default();
    config.ai.infer_provider();
    config.ai.auto_detect_provider();
    (config, Vec::new())
}

/// Search for `.rpg.toml` starting from `start_dir` and walking up to
/// the user's home directory (inclusive).
///
/// Returns the path of the first `.rpg.toml` found, or `None`.
#[cfg(not(target_arch = "wasm32"))]
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

/// WASM stub: no filesystem, no project config.
#[cfg(target_arch = "wasm32")]
pub fn find_project_config(_start_dir: &Path) -> Option<PathBuf> {
    None
}

/// Load a `.rpg.toml` project config file and look for `POSTGRES.md`
/// alongside it.
///
/// Searches from the current working directory up to the user's home
/// directory.  Returns a [`ProjectConfigResult`] that is always safe to
/// use: when no file is found, the config field holds a default value
/// and the path fields are `None`.
#[cfg(not(target_arch = "wasm32"))]
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

/// WASM stub: no filesystem, return defaults.
#[cfg(target_arch = "wasm32")]
pub fn load_project_config() -> ProjectConfigResult {
    ProjectConfigResult::default()
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

    // Post-merge fixup: infer provider from api_key_env when not explicit,
    // then fall back to probing well-known env vars (zero-config case).
    base.ai.infer_provider();
    base.ai.auto_detect_provider();

    base
}

/// Return the path to the user config file, or `None` if the config
/// directory cannot be determined.
#[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(home) = dirs::home_dir() {
        return format!("{}/.config/rpg/config.toml", home.display());
    }
    "~/.config/rpg/config.toml".to_owned()
}

/// Read and parse a single TOML config file.
#[cfg(not(target_arch = "wasm32"))]
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
            // Opt-in: either layer enabling it is sufficient.
            dropdown_completion: overlay.display.dropdown_completion
                || base.display.dropdown_completion,
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
            timeout: if overlay.ai.timeout == default_ai_timeout() {
                base.ai.timeout
            } else {
                overlay.ai.timeout
            },
        },
        pgmustard: PgMustardConfig {
            api_key_env: overlay.pgmustard.api_key_env.or(base.pgmustard.api_key_env),
        },
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
        ash: AshConfig {
            sample_timeout_ms: overlay.ash.sample_timeout_ms.or(base.ash.sample_timeout_ms),
        },
        connections: {
            let mut merged = base.connections;
            merged.extend(overlay.connections);
            merged
        },
        // project_named_queries is not set during file-layer merging;
        // it is populated by merge_project_config.
        project_named_queries: base.project_named_queries,
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
    use std::sync::Mutex;

    // Serializes tests that mutate process-wide environment variables so that
    // parallel test threads do not interfere with each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
                                       // Dropdown is experimental — off by default.
        assert!(!cfg.display.dropdown_completion);
        assert!(cfg.safety.destructive_warning);
        assert!(cfg.connection.host.is_none());
        assert!(cfg.connection.port.is_none());
        assert!(cfg.connection.user.is_none());
        assert!(cfg.connection.dbname.is_none());
        assert!(cfg.connection.sslmode.is_none());
    }

    #[test]
    fn parse_display_dropdown_completion() {
        let toml_str = r"
[display]
dropdown_completion = true
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert!(cfg.display.dropdown_completion);
    }

    #[test]
    fn merge_display_dropdown_completion_either_layer_wins() {
        // Overlay enabling it wins.
        let base = Config {
            display: DisplayConfig {
                dropdown_completion: false,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            display: DisplayConfig {
                dropdown_completion: true,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert!(merged.display.dropdown_completion);

        // Base enabling it also wins (OR semantics).
        let base2 = Config {
            display: DisplayConfig {
                dropdown_completion: true,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let overlay2 = Config {
            display: DisplayConfig {
                dropdown_completion: false,
                ..DisplayConfig::default()
            },
            ..Default::default()
        };
        let merged2 = merge_config(base2, overlay2);
        assert!(merged2.display.dropdown_completion);
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

    // -- AiConfig::auto_detect_provider -------------------------------------

    #[test]
    fn auto_detect_finds_anthropic_key() {
        let _lock = ENV_LOCK.lock().unwrap();
        // Ensure OPENAI and OLLAMA are absent so only ANTHROPIC is visible.
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("OLLAMA_API_KEY");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        let mut ai = AiConfig::default();
        ai.auto_detect_provider();
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert_eq!(ai.provider.as_deref(), Some("anthropic"));
        assert_eq!(ai.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn auto_detect_anthropic_before_openai() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        std::env::set_var("OPENAI_API_KEY", "sk-openai-test");
        let mut ai = AiConfig::default();
        ai.auto_detect_provider();
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        // Anthropic is checked first and wins.
        assert_eq!(ai.provider.as_deref(), Some("anthropic"));
        assert_eq!(ai.api_key_env.as_deref(), Some("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn auto_detect_skipped_when_api_key_env_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        let mut ai = AiConfig {
            api_key_env: Some("OPENAI_API_KEY".to_owned()),
            ..AiConfig::default()
        };
        ai.auto_detect_provider();
        std::env::remove_var("ANTHROPIC_API_KEY");
        // Explicit api_key_env takes precedence; auto-detect must not fire.
        assert!(ai.provider.is_none());
        assert_eq!(ai.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn auto_detect_skipped_when_provider_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        let mut ai = AiConfig {
            provider: Some("ollama".to_owned()),
            ..AiConfig::default()
        };
        ai.auto_detect_provider();
        std::env::remove_var("ANTHROPIC_API_KEY");
        // Explicit provider takes precedence; auto-detect must not fire.
        assert_eq!(ai.provider.as_deref(), Some("ollama"));
        assert!(ai.api_key_env.is_none());
    }

    #[test]
    fn auto_detect_no_env_vars_leaves_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("OLLAMA_API_KEY");
        let mut ai = AiConfig::default();
        ai.auto_detect_provider();
        assert!(ai.provider.is_none());
        assert!(ai.api_key_env.is_none());
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

    // -- Project config trust check (#824 H5) --------------------------------
    //
    // A world-writable `.rpg.toml` lets any local user inject settings such
    // as `PROMPT1` that flow through backtick-shell expansion in the REPL
    // (`src/repl/mod.rs:282`).  rpg must therefore refuse to load project
    // configs that fail the same ownership/mode check pgpass uses
    // (`src/connection.rs:1566–1582`).
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn load_project_config_skips_world_writable_file() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let config_path = dir.path().join(".rpg.toml");
        // Recognisable named_query — if the loader trusted this file the
        // entry would appear in the merged Config.
        fs::write(
            &config_path,
            "[named_queries]\nh5_canary = \"select 'pwn'\"\n",
        )
        .expect("write .rpg.toml");
        // chmod 0666 — world-writable, the exact case pgpass rejects.
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o666))
            .expect("chmod .rpg.toml");

        // load_project_config() reads CWD — point it at our temp dir for the
        // duration of this serial test.
        let prev_cwd = std::env::current_dir().expect("get cwd");
        std::env::set_current_dir(dir.path()).expect("set cwd to temp dir");
        let result = load_project_config();
        std::env::set_current_dir(&prev_cwd).expect("restore cwd");

        assert!(
            result.config_path.is_none(),
            "world-writable .rpg.toml must not be loaded; got {:?}",
            result.config_path
        );
        assert!(
            !result.config.named_queries.contains_key("h5_canary"),
            "world-writable .rpg.toml contents must not be merged into config"
        );
    }

    // -- AI timeout ----------------------------------------------------------

    #[test]
    fn ai_timeout_default_is_30() {
        let cfg = AiConfig::default();
        assert_eq!(cfg.timeout, 30, "AI timeout should default to 30 seconds");
    }

    #[test]
    fn parse_ai_timeout() {
        let toml_str = r"
[ai]
timeout = 60
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ai.timeout, 60);
    }

    #[test]
    fn merge_ai_timeout_overlay_wins_when_non_default() {
        let base = Config {
            ai: AiConfig {
                timeout: 10,
                ..AiConfig::default()
            },
            ..Default::default()
        };
        let overlay = Config {
            ai: AiConfig {
                timeout: 90,
                ..AiConfig::default()
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.ai.timeout, 90);
    }

    #[test]
    fn merge_ai_timeout_base_preserved_when_overlay_is_default() {
        let base = Config {
            ai: AiConfig {
                timeout: 10,
                ..AiConfig::default()
            },
            ..Default::default()
        };
        // overlay has timeout = 30 (default)
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert_eq!(merged.ai.timeout, 10);
    }

    // -- SSH connect_timeout -------------------------------------------------

    #[test]
    fn ssh_connect_timeout_default_is_15() {
        let cfg = SshTunnelConfig::default();
        assert_eq!(
            cfg.connect_timeout, 15,
            "SSH connect_timeout should default to 15 seconds"
        );
    }

    #[test]
    fn parse_ssh_connect_timeout() {
        let toml_str = r#"
[connections.prod.ssh_tunnel]
host = "bastion.example.com"
port = 22
user = "deploy"
connect_timeout = 30
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        let tunnel = cfg
            .connections
            .get("prod")
            .and_then(|p| p.ssh_tunnel.as_ref())
            .expect("prod ssh_tunnel should be present");
        assert_eq!(tunnel.connect_timeout, 30);
    }

    // -- PgMustardConfig -----------------------------------------------------

    #[test]
    fn pgmustard_default_has_no_api_key_env() {
        let cfg = PgMustardConfig::default();
        assert!(cfg.api_key_env.is_none());
    }

    #[test]
    fn pgmustard_resolve_api_key_from_default_env_var() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("PGMUSTARD_API_KEY", "test-key-123");
        let cfg = PgMustardConfig::default();
        let key = cfg.resolve_api_key();
        std::env::remove_var("PGMUSTARD_API_KEY");
        assert_eq!(key, Some("test-key-123".to_owned()));
    }

    #[test]
    fn pgmustard_resolve_api_key_from_custom_env_var() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("MY_MUSTARD_KEY", "custom-key-456");
        let cfg = PgMustardConfig {
            api_key_env: Some("MY_MUSTARD_KEY".to_owned()),
        };
        let key = cfg.resolve_api_key();
        std::env::remove_var("MY_MUSTARD_KEY");
        assert_eq!(key, Some("custom-key-456".to_owned()));
    }

    #[test]
    fn pgmustard_resolve_api_key_missing_returns_none() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("PGMUSTARD_API_KEY");
        let cfg = PgMustardConfig::default();
        assert!(cfg.resolve_api_key().is_none());
    }

    #[test]
    fn parse_pgmustard_config_from_toml() {
        let toml_str = r#"
[pgmustard]
api_key_env = "PGMUSTARD_API_KEY"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(
            cfg.pgmustard.api_key_env,
            Some("PGMUSTARD_API_KEY".to_owned())
        );
    }

    // -- AshConfig --------------------------------------------------------------

    #[test]
    fn ash_config_default_is_none() {
        // Default is None so that config merging can distinguish "absent" from
        // "explicitly set to 500".  The actual default of 500 is applied at the
        // usage site via unwrap_or(DEFAULT_ASH_SAMPLE_TIMEOUT_MS).
        let cfg = AshConfig::default();
        assert_eq!(cfg.sample_timeout_ms, None);
    }

    #[test]
    fn ash_config_parse_explicit_value() {
        let toml_str = r"
[ash]
sample_timeout_ms = 200
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms, Some(200));
    }

    #[test]
    fn ash_config_parse_explicit_default_value() {
        // Explicitly setting 500 (the default) must be preserved as Some(500).
        let toml_str = r"
[ash]
sample_timeout_ms = 500
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms, Some(500));
    }

    #[test]
    fn ash_config_parse_zero_disables_timeout() {
        let toml_str = r"
[ash]
sample_timeout_ms = 0
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms, Some(0));
    }

    #[test]
    fn ash_config_absent_section_is_none() {
        let toml_str = "";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms, None);
    }

    #[test]
    fn ash_config_empty_section_is_none() {
        let toml_str = r"
[ash]
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms, None);
    }

    #[test]
    fn ash_default_timeout_accessor() {
        let cfg: Config = toml::from_str("").expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms(), 500);
    }

    #[test]
    fn ash_disabled_timeout_accessor() {
        let toml_str = r"
[ash]
sample_timeout_ms = 0
";
        let cfg: Config = toml::from_str(toml_str).expect("should parse");
        assert_eq!(cfg.ash.sample_timeout_ms(), 0);
    }

    // -- AshConfig merge --------------------------------------------------------

    #[test]
    fn merge_ash_overlay_none_preserves_base() {
        let base = Config {
            ash: AshConfig {
                sample_timeout_ms: Some(0),
            },
            ..Default::default()
        };
        let overlay = Config {
            ash: AshConfig {
                sample_timeout_ms: None,
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(
            merged.ash.sample_timeout_ms,
            Some(0),
            "base value should be preserved when overlay is absent"
        );
    }

    #[test]
    fn merge_ash_overlay_explicit_wins() {
        let base = Config {
            ash: AshConfig {
                sample_timeout_ms: Some(0),
            },
            ..Default::default()
        };
        let overlay = Config {
            ash: AshConfig {
                sample_timeout_ms: Some(500),
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(
            merged.ash.sample_timeout_ms,
            Some(500),
            "overlay explicit value should win even when it equals the old default"
        );
    }

    #[test]
    fn merge_ash_both_none_stays_none() {
        let base = Config::default();
        let overlay = Config::default();
        let merged = merge_config(base, overlay);
        assert_eq!(merged.ash.sample_timeout_ms, None);
    }

    #[test]
    fn merge_ash_base_some_overlay_different_some() {
        let base = Config {
            ash: AshConfig {
                sample_timeout_ms: Some(300),
            },
            ..Default::default()
        };
        let overlay = Config {
            ash: AshConfig {
                sample_timeout_ms: Some(1000),
            },
            ..Default::default()
        };
        let merged = merge_config(base, overlay);
        assert_eq!(merged.ash.sample_timeout_ms, Some(1000));
    }

    #[test]
    fn merge_ash_overlay_unset_preserves_base_via_toml() {
        let base: Config = toml::from_str(
            r"
[ash]
sample_timeout_ms = 0
",
        )
        .expect("parse base");
        let overlay: Config = toml::from_str("").expect("parse overlay");
        let merged = merge_config(base, overlay);
        assert_eq!(
            merged.ash.sample_timeout_ms(),
            0,
            "unset overlay must preserve base value"
        );
    }
}
