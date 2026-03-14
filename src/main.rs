//! Rpg — self-driving Postgres agent and psql-compatible terminal.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! rpg-specific options, then dispatches to the appropriate subsystem.

use clap::Parser;

// Core modules.
mod actor;
mod ai;
mod capabilities;
mod compat;
mod complete;
mod conditional;
mod config;
mod connection;
mod copy;
mod crosstab;
mod daemon;
mod dba;
mod describe;
mod governance;
mod highlight;
mod init;
mod io;
mod large_object;
mod logging;
mod markdown;
mod metacmd;
mod named;
mod observe;
mod output;
mod pager;
mod pattern;
mod query;
mod rca;
mod repl;
mod safety;
mod session;
mod session_store;
mod setup;
mod ssh_tunnel;
mod statusline;
mod vars;

// Phase 2/3 infrastructure — compiled but not yet wired into the main
// dispatch loop. Each module suppresses dead_code at the item level.
mod aaa_commands;
mod alert_delivery;
mod anomaly;
mod backup_monitoring;
mod bloat;
mod check;
mod config_tuning;
mod connection_management;
mod connector_health;
mod connector_setup;
mod connectors;
mod dispatcher;
mod health_check_commands;
mod health_checks;
mod index_health;
mod issues;
mod llm_auditor;
mod query_optimization;
mod rca_actions;
mod replication;
mod report;
mod security;
mod update;
mod vacuum;
mod verification;

/// Build-time git commit hash injected by `build.rs`.
const GIT_HASH: &str = env!("RPG_GIT_HASH");

/// Build-time date (UTC, `YYYY-MM-DD`) injected by `build.rs`.
const BUILD_DATE: &str = env!("RPG_BUILD_DATE");

/// One-line version string: `rpg 0.2.0 (abc1234, built 2026-03-13)`.
///
/// Exposed as `pub` so that meta-command handlers can print it without
/// duplicating the formatting logic.
pub fn version_string() -> &'static str {
    // Leak is fine: called at most a handful of times, lives for the
    // process lifetime.
    Box::leak(
        format!(
            "rpg {} ({}, built {})",
            env!("CARGO_PKG_VERSION"),
            GIT_HASH,
            BUILD_DATE,
        )
        .into_boxed_str(),
    )
}

// ---------------------------------------------------------------------------
// Autonomy levels (rpg-specific)
// ---------------------------------------------------------------------------

/// Autonomy level for the agent subsystem.
///
// TODO: Support per-feature granular syntax like `vacuum:auto,index_health:auto`
// (SPEC section 8.6). The current `ValueEnum` handles global level only.
#[derive(Clone, Debug, Default, clap::ValueEnum)]
enum Autonomy {
    /// Read-only: observe, diagnose, report. Zero writes.
    #[default]
    Observe,
    /// Propose actions, human confirms before execution.
    Supervised,
    /// Act autonomously within policy and DB permissions.
    Auto,
}

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// Assemble the clap version string: delegates to [`version_string`].
fn long_version() -> &'static str {
    version_string()
}

/// Rpg — self-driving Postgres agent and psql-compatible terminal.
///
/// A psql-compatible interface with built-in AI and autonomous
/// database health management.
#[derive(Parser, Debug)]
#[command(
    name = "rpg",
    version = long_version(),
    about = "Self-driving Postgres agent and psql-compatible terminal",
    long_about = None,
    // Disable auto-generated -h so we can use it for --host (psql compat).
    disable_help_flag = true,
)]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// Print help information.
    #[arg(long, action = clap::ArgAction::Help)]
    help: Option<bool>,

    // -- Positional arguments (psql-compatible order) -----------------------
    // Named flags (-d, -U, -h, -p) override positionals when both are given.
    /// Database name to connect to.
    #[arg(value_name = "DBNAME")]
    dbname_pos: Option<String>,

    /// Username (positional).
    #[arg(value_name = "USER")]
    user_pos: Option<String>,

    /// Hostname (positional).
    #[arg(value_name = "HOST")]
    host_pos: Option<String>,

    /// Port (positional).
    #[arg(value_name = "PORT")]
    port_pos: Option<String>,

    // -- Connection flags ---------------------------------------------------
    /// Database server host or socket directory.
    #[arg(short = 'h', long)]
    host: Option<String>,

    /// Database server port number.
    #[arg(short = 'p', long)]
    port: Option<u16>,

    /// Database user name.
    #[arg(short = 'U', long)]
    username: Option<String>,

    /// Database name.
    #[arg(short = 'd', long)]
    dbname: Option<String>,

    /// SSL mode (disable, allow, prefer, require, verify-ca, verify-full).
    #[arg(long, value_name = "SSLMODE")]
    sslmode: Option<String>,

    /// Force password prompt.
    #[arg(short = 'W', long)]
    password: bool,

    /// Never prompt for password.
    #[arg(short = 'w', long = "no-password")]
    no_password: bool,

    /// SSH tunnel in `user@host:port` format (port defaults to 22).
    ///
    /// Establishes an SSH tunnel through the specified bastion host and
    /// routes the Postgres connection through it automatically.
    ///
    /// Example: `--ssh-tunnel deploy@bastion.example.com:22`
    #[arg(long, value_name = "USER@HOST:PORT")]
    ssh_tunnel: Option<String>,

    // -- Psql scripting flags -----------------------------------------------
    /// Set psql variable (can be specified multiple times).
    #[arg(short = 'v', long = "variable", value_name = "NAME=VALUE")]
    variable: Vec<String>,

    // -- Common psql flags --------------------------------------------------
    /// Run a command (SQL or backslash) and exit. May be given multiple
    /// times; commands are executed in order, like psql.
    #[arg(short = 'c', long, action = clap::ArgAction::Append)]
    command: Vec<String>,

    /// Execute commands from file, then exit.
    #[arg(short = 'f', long)]
    file: Option<String>,

    /// Do not read startup file (~/.psqlrc / ~/.rpgrc).
    #[arg(short = 'X', long = "no-psqlrc")]
    no_psqlrc: bool,

    /// Unaligned table output mode.
    #[arg(short = 'A', long = "no-align")]
    no_align: bool,

    /// Print rows only (tuples only).
    #[arg(short = 't', long = "tuples-only")]
    tuples_only: bool,

    /// Expanded table output mode (like `\x`).
    #[arg(short = 'x', long = "expanded")]
    expanded: bool,

    /// Set printing option (like `\pset`). Can be specified multiple times.
    #[arg(short = 'P', long, value_name = "VAR[=ARG]")]
    pset: Vec<String>,

    /// Send query results to file (or pipe).
    #[arg(short = 'o', long)]
    output: Option<String>,

    /// Field separator for unaligned output.
    #[arg(short = 'F', long = "field-separator", value_name = "SEP")]
    field_separator: Option<String>,

    /// Record separator for unaligned output.
    #[arg(short = 'R', long = "record-separator", value_name = "SEP")]
    record_separator: Option<String>,

    /// Log all query output to file.
    #[arg(short = 'L', long = "log-queries", value_name = "FILE")]
    log_queries: Option<String>,

    /// Disable readline (no line editing).
    #[arg(short = 'n', long = "no-readline")]
    no_readline: bool,

    /// Single-step mode: confirm each command before execution.
    #[arg(short = 's', long = "single-step")]
    single_step: bool,

    /// Use NUL as field separator (unaligned output).
    #[arg(short = 'z', long = "field-separator-zero")]
    field_separator_zero: bool,

    /// Use NUL as record separator (unaligned output).
    #[arg(short = '0', long = "record-separator-zero")]
    record_separator_zero: bool,

    /// Echo queries that rpg generates internally.
    #[arg(short = 'E', long = "echo-hidden")]
    echo_hidden: bool,

    /// Echo all input from script.
    #[arg(short = 'e', long = "echo-queries")]
    echo_queries: bool,

    /// Echo failed commands' error messages.
    #[arg(short = 'b', long = "echo-errors")]
    echo_errors: bool,

    /// Run in quiet mode (suppress informational messages).
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Single-line mode: newline terminates a SQL command.
    #[arg(short = 'S', long = "single-line")]
    single_line: bool,

    /// Single-transaction mode: wrap all commands in BEGIN/COMMIT.
    #[arg(short = '1', long = "single-transaction")]
    single_transaction: bool,

    /// Force interactive mode even when input is not a terminal.
    #[arg(short = 'i', long)]
    interactive: bool,

    /// CSV output format.
    #[arg(long)]
    csv: bool,

    /// JSON output format.
    #[arg(long)]
    json: bool,

    /// Enable debug output.
    #[arg(short = 'D', long)]
    debug: bool,

    // -- Rpg-specific flags ------------------------------------------------
    /// Show psql compatibility report and exit.
    #[arg(long)]
    compat: bool,

    /// Disable syntax highlighting in the interactive REPL.
    #[arg(long)]
    no_highlight: bool,

    /// Enable text-to-SQL mode: translate natural language to SQL.
    #[arg(long)]
    text2sql: bool,

    /// Show query execution plan before running.
    #[arg(long)]
    plan: bool,

    /// Skip confirmation prompts for AI-generated queries (use with care).
    #[arg(long)]
    yolo: bool,

    /// Bypass autonomy level checks in YOLO mode (dangerous).
    ///
    /// When combined with `--yolo`, write queries are auto-executed
    /// regardless of the configured autonomy level. Use only when you
    /// fully understand the consequences.
    #[arg(long)]
    i_know_what_im_doing: bool,

    /// Launch in observe mode. Optionally accepts a duration (e.g. `30m`, `2h`).
    /// With no value: observe indefinitely. With a value: observe then exit.
    #[arg(long, value_name = "DURATION", default_missing_value = "", num_args = 0..=1)]
    observe: Option<String>,

    /// Set agent autonomy level.
    #[arg(long, value_enum, default_value_t = Autonomy::Observe)]
    autonomy: Autonomy,

    /// Run health check, exit with code reflecting severity (FR-13).
    #[arg(long)]
    check: bool,

    /// Generate a full diagnostic report. Optionally specify format (text, json).
    #[arg(long, value_name = "FORMAT", default_missing_value = "text", num_args = 0..=1)]
    report: Option<String>,

    /// Write structured logs to this file (FR-14).
    #[arg(long, value_name = "FILE")]
    log_file: Option<String>,

    /// Set log verbosity level (error, warn, info, debug, trace) (FR-14).
    #[arg(long, value_name = "LEVEL")]
    log_level: Option<String>,

    /// Generate `rpg_ops` wrapper SQL and exit. Specify PG version (e.g. 14, 16).
    #[arg(long, value_name = "PG_VERSION", default_missing_value = "16", num_args = 0..=1)]
    generate_wrappers: Option<String>,

    /// Run in daemon mode (headless continuous monitoring).
    #[arg(long)]
    daemon: bool,

    /// Port for HTTP health check endpoint in daemon mode.
    #[arg(long, value_name = "PORT")]
    health_port: Option<u16>,

    /// Slack webhook URL for daemon notifications.
    #[arg(long, value_name = "URL")]
    slack_webhook: Option<String>,

    /// Generic webhook URL for daemon notifications (POSTs JSON).
    #[arg(long, value_name = "URL")]
    webhook_url: Option<String>,

    /// HMAC-SHA256 signing secret for the generic webhook.
    ///
    /// When set, each webhook POST includes an `X-Rpg-Signature-256` header
    /// with the hex-encoded HMAC-SHA256 signature of the request body.
    #[arg(long, value_name = "SECRET")]
    webhook_secret: Option<String>,

    /// `PagerDuty` Events API v2 routing key for daemon notifications.
    #[arg(long, value_name = "KEY")]
    pagerduty_key: Option<String>,

    /// Telegram bot token for daemon notifications.
    #[arg(long, value_name = "TOKEN")]
    telegram_bot_token: Option<String>,

    /// Telegram chat ID for daemon notifications.
    #[arg(long, value_name = "ID")]
    telegram_chat_id: Option<String>,

    /// Path to PID file for daemon mode.
    #[arg(long, value_name = "PATH")]
    pid_file: Option<String>,

    /// GitHub repository (owner/repo) for creating issues from findings.
    #[arg(long, value_name = "OWNER/REPO")]
    github_repo: Option<String>,

    /// Check for a newer version of rpg, download and replace the binary,
    /// then exit. No database connection is required.
    #[arg(long)]
    update: bool,

    /// Check for a newer version of rpg and print the result, then exit.
    /// Does not download or replace the binary.
    #[arg(long)]
    update_check: bool,
}

impl Cli {
    /// Convert CLI flags into connection-layer options.
    fn conn_opts(&self) -> connection::CliConnOpts {
        connection::CliConnOpts {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            dbname: self.dbname.clone(),
            dbname_pos: self.dbname_pos.clone(),
            user_pos: self.user_pos.clone(),
            host_pos: self.host_pos.clone(),
            port_pos: self.port_pos.clone(),
            force_password: self.password,
            no_password: self.no_password,
            sslmode: self.sslmode.clone(),
            ssh_tunnel: self.ssh_tunnel.as_deref().and_then(|s| {
                ssh_tunnel::SshTunnelSpec::parse(s).map(config::SshTunnelConfig::from)
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// CLI pset helper
// ---------------------------------------------------------------------------

/// Apply a single `-P VAR[=ARG]` option to the initial `PsetConfig`.
fn apply_cli_pset(pset: &mut output::PsetConfig, arg: &str) {
    let (option, value) = if let Some((k, v)) = arg.split_once('=') {
        (k, Some(v))
    } else {
        (arg, None)
    };

    match option {
        "format" => {
            pset.format = match value.unwrap_or("") {
                "aligned" => output::OutputFormat::Aligned,
                "unaligned" => output::OutputFormat::Unaligned,
                "csv" => output::OutputFormat::Csv,
                "json" => output::OutputFormat::Json,
                "html" => output::OutputFormat::Html,
                "wrapped" => output::OutputFormat::Wrapped,
                other => {
                    eprintln!("rpg: invalid value for -P format: \"{other}\"");
                    std::process::exit(2);
                }
            };
        }
        "border" => {
            if let Some(v) = value.and_then(|s| s.parse::<u8>().ok()) {
                pset.border = v.min(2);
            }
        }
        "null" => {
            value.unwrap_or("").clone_into(&mut pset.null_display);
        }
        "fieldsep" => {
            value.unwrap_or("|").clone_into(&mut pset.field_sep);
        }
        "tuples_only" | "t" => {
            pset.tuples_only = matches!(value, Some("on" | "true" | "1"));
        }
        "footer" => {
            pset.footer = !matches!(value, Some("off" | "false" | "0"));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Settings construction helpers
// ---------------------------------------------------------------------------

/// Open the `-L` log file for append, exiting on failure.
fn open_log_file(path: &str) -> Box<dyn std::io::Write> {
    use std::fs::OpenOptions;
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => Box::new(f),
        Err(e) => {
            eprintln!("rpg: -L: could not open \"{path}\": {e}");
            std::process::exit(2);
        }
    }
}

/// Build a [`repl::ReplSettings`] from the parsed CLI flags and loaded config.
///
/// Config values set defaults; CLI flags take precedence and override them.
/// Exits the process (code 2) if file-opening operations fail.
fn build_settings(
    cli: &Cli,
    cfg: &config::Config,
    project: &config::ProjectConfigResult,
) -> repl::ReplSettings {
    // Build PsetConfig from CLI flags.
    let mut pset = output::PsetConfig::default();
    if cli.csv {
        pset.format = output::OutputFormat::Csv;
    } else if cli.json {
        pset.format = output::OutputFormat::Json;
    } else if cli.no_align {
        pset.format = output::OutputFormat::Unaligned;
    }
    if cli.tuples_only {
        pset.tuples_only = true;
    }
    if cli.expanded {
        pset.expanded = output::ExpandedMode::On;
    }
    if cli.field_separator_zero {
        "\0".clone_into(&mut pset.field_sep);
    } else if let Some(ref sep) = cli.field_separator {
        sep.clone_into(&mut pset.field_sep);
    }
    if cli.record_separator_zero {
        "\0".clone_into(&mut pset.record_sep);
    } else if let Some(ref sep) = cli.record_separator {
        sep.clone_into(&mut pset.record_sep);
    }
    for pset_arg in &cli.pset {
        apply_cli_pset(&mut pset, pset_arg);
    }

    // Build variable store; apply -v NAME=VALUE assignments.
    let mut vars = vars::Variables::new();
    for assignment in &cli.variable {
        if let Some((name, val)) = assignment.split_once('=') {
            vars.set(name, val);
        } else {
            eprintln!("rpg: -v requires name=value");
        }
    }

    // -o / --output: redirect query output to file.
    let output_target = cli
        .output
        .as_deref()
        .map(|path| match io::open_output(Some(path)) {
            Ok(w) => w.expect("open_output with Some path returns Some"),
            Err(e) => {
                eprintln!("rpg: {e}");
                std::process::exit(2);
            }
        });

    // -L / --log-queries: open log file.
    let log_file: Option<Box<dyn std::io::Write>> = cli.log_queries.as_deref().map(open_log_file);

    // Apply config display defaults; explicit CLI flags take precedence.
    //
    // `--no-highlight` always wins over config.highlight (it is a bool flag,
    // so we cannot distinguish "not provided" from "false"). For pager and
    // timing the config default applies when the corresponding CLI override
    // has not been set.
    let no_highlight = cli.no_highlight || !cfg.display.highlight;
    let pager_enabled = cfg.display.pager;
    let timing = cfg.display.timing;
    let safety_enabled = cfg.safety.destructive_warning;
    let vi_mode = cfg.display.vi_mode;

    // Apply config display.border default if it wasn't set via -P border=N.
    // The CLI -P args were already applied above via apply_cli_pset; if
    // border is still at the struct default (1) and the config sets a value,
    // apply the config value here.
    if pset.border == 1 {
        if let Some(v) = cfg.display.border {
            pset.border = v.min(2);
        }
    }

    // Initialise pager_command from the PAGER environment variable.
    // A non-empty PAGER that is not "on"/"off" sets an external pager.
    // An empty or absent PAGER leaves the built-in pager as default.
    let pager_command = std::env::var("PAGER")
        .ok()
        .filter(|v| !v.is_empty() && v != "on" && v != "off");

    // Keep ReplSettings.expanded in sync with pset.expanded so that both the
    // REPL path and the -c path see a consistent expanded mode.
    let expanded = pset.expanded;

    // Pager min-lines threshold from config; 0 means always page (default).
    let pager_min_lines = cfg.display.pager_min_lines.unwrap_or(0);

    repl::ReplSettings {
        echo_hidden: cli.echo_hidden,
        expanded,
        pset,
        vars,
        output_target,
        log_file,
        echo_queries: cli.echo_queries,
        echo_errors: cli.echo_errors,
        single_step: cli.single_step,
        single_line: cli.single_line,
        single_transaction: cli.single_transaction,
        quiet: cli.quiet,
        debug: cli.debug,
        no_highlight,
        pager_enabled,
        pager_command,
        pager_min_lines,
        timing,
        safety_enabled,
        vi_mode,
        config: cfg.clone(),
        exec_mode: if cli.yolo {
            repl::ExecMode::Yolo
        } else {
            repl::ExecMode::default()
        },
        i_know_what_im_doing: cli.i_know_what_im_doing,
        project_context: project.postgres_md.clone(),
        ai_context_files: cfg.ai.project_context_files.clone(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

// TODO: Replace #[tokio::main] with explicit runtime construction
// to optimize thread count per operating mode (issue #2, finding #9).
#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() {
    // Install the default rustls CryptoProvider before any TLS operations.
    // Required because multiple dependencies (tokio-postgres-rustls, reqwest)
    // pull in different crypto backends, preventing auto-selection.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    let mut cli = Cli::parse();

    // Initialise structured logging before anything else.
    //
    // --debug sets level to Debug; --log-level overrides explicitly;
    // default is Warn so routine runs are silent.
    let log_level = if cli.debug {
        logging::Level::Debug
    } else {
        cli.log_level
            .as_deref()
            .and_then(logging::Level::from_str)
            .unwrap_or(logging::Level::Warn)
    };

    // Load config once up front: needed for logging rotation settings and
    // then reused below for the full startup path.  Previously this was
    // called twice (once for logging, once after early-exit guards), which
    // doubled the TOML parsing overhead on every invocation.
    let (base_cfg, config_warnings) = config::load_config();
    let rotation = logging::RotationConfig::from_mb(
        base_cfg.logging.max_file_size_mb,
        base_cfg.logging.max_files,
    );

    if let Some(path) = cli.log_file.as_deref() {
        logging::init_rotating(log_level, std::path::PathBuf::from(path), rotation);
    } else {
        logging::init(log_level, None);
    }

    // --generate-wrappers: emit SQL and exit (no DB connection needed).
    if let Some(ref pg_ver_str) = cli.generate_wrappers {
        let pg_version: u32 = pg_ver_str.parse().unwrap_or(16);
        print!("{}", setup::generate_setup_sql(pg_version));
        return;
    }

    // --compat: print psql compatibility report and exit (no DB connection).
    if cli.compat {
        compat::print_compat_report();
        return;
    }

    // --update / --update-check: self-update logic (no DB connection needed).
    if cli.update || cli.update_check {
        let http = match reqwest::Client::builder().user_agent("rpg").build() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rpg: failed to build HTTP client: {e}");
                std::process::exit(2);
            }
        };

        match update::check_latest_version(&http).await {
            Ok(info) => {
                let current = env!("CARGO_PKG_VERSION");
                if info.version == current {
                    println!("rpg is up to date ({current})");
                } else {
                    println!("rpg {} is available (current: {current})", info.version);
                }
                update::record_update_check();

                if cli.update {
                    println!("Downloading update from {}", info.download_url);
                    match update::download_and_replace(&http, &info.download_url).await {
                        Ok(()) => {
                            println!("rpg updated to {} — please restart.", info.version);
                        }
                        Err(e) => {
                            eprintln!("rpg: update failed: {e}");
                            std::process::exit(2);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("rpg: update check failed: {e}");
                std::process::exit(2);
            }
        }
        return;
    }

    // Print config warnings now that logging is initialised.  Suppressed
    // by --quiet as before.
    for w in &config_warnings {
        if !cli.quiet {
            eprintln!("rpg: warning: {w}");
        }
    }

    // Load project config (.rpg.toml) and merge it on top of user config.
    let project_result = config::load_project_config();
    let cfg = config::merge_project_config(base_cfg, &project_result.config);

    // Print project config startup messages (suppressed by --quiet).
    if !cli.quiet {
        if let Some(ref p) = project_result.config_path {
            eprintln!("Using project config: {}", p.display());
        }
        if let Some(ref p) = project_result.postgres_md_path {
            eprintln!("Loaded project context: {}", p.display());
        }
    }

    // If the first positional argument starts with '@', treat it as a named
    // connection profile.  CLI flags still take precedence over profile
    // values — only fields that are not already set by flags are filled in.
    let profile_name = cli
        .dbname_pos
        .as_deref()
        .filter(|s| s.starts_with('@'))
        .map(|s| s[1..].to_owned());

    // Track the ssh_tunnel from a named profile (CLI --ssh-tunnel wins).
    let mut profile_ssh_tunnel: Option<config::SshTunnelConfig> = None;

    if let Some(ref name) = profile_name {
        if let Some(profile) = config::get_profile(&cfg, name) {
            if cli.host.is_none() {
                cli.host.clone_from(&profile.host);
            }
            if cli.port.is_none() {
                cli.port = profile.port;
            }
            if cli.dbname.is_none() {
                cli.dbname.clone_from(&profile.dbname);
            }
            if cli.username.is_none() {
                cli.username.clone_from(&profile.username);
            }
            if cli.sslmode.is_none() {
                cli.sslmode.clone_from(&profile.sslmode);
            }
            // Carry the profile's ssh_tunnel; CLI --ssh-tunnel overrides it.
            if cli.ssh_tunnel.is_none() {
                profile_ssh_tunnel.clone_from(&profile.ssh_tunnel);
            }
            // Clear the positional dbname so connection resolution does not
            // misinterpret "@production" as a literal database name.
            cli.dbname_pos = None;
        } else {
            eprintln!("rpg: unknown profile \"@{name}\"");
            eprintln!(
                "Configure profiles in {} under [connections.{name}]",
                config::user_config_path_display()
            );
            std::process::exit(2);
        }
    }

    // Apply [connection] config defaults for any fields not already set by
    // a CLI flag or named profile.  Config values are a last resort before
    // environment variables (PGHOST etc.) and libpq defaults.
    if cli.host.is_none() && cli.host_pos.is_none() {
        cli.host.clone_from(&cfg.connection.host);
    }
    if cli.port.is_none() && cli.port_pos.is_none() {
        cli.port = cfg
            .connection
            .port
            .as_deref()
            .and_then(|p| p.parse::<u16>().ok());
    }
    if cli.username.is_none() && cli.user_pos.is_none() {
        cli.username.clone_from(&cfg.connection.user);
    }
    if cli.dbname.is_none() && cli.dbname_pos.is_none() {
        cli.dbname.clone_from(&cfg.connection.dbname);
    }
    if cli.sslmode.is_none() {
        cli.sslmode.clone_from(&cfg.connection.sslmode);
    }

    let mut opts = cli.conn_opts();

    // Profile ssh_tunnel fills in when CLI --ssh-tunnel was not given.
    if opts.ssh_tunnel.is_none() {
        opts.ssh_tunnel = profile_ssh_tunnel;
    }

    // If an SSH tunnel is configured, establish it now and redirect the
    // Postgres host/port to the local tunnel endpoint.  The `_tunnel` handle
    // must stay alive for the entire process (dropping it kills the tunnel).
    let _tunnel: Option<ssh_tunnel::SshTunnel> = if let Some(ref tcfg) = opts.ssh_tunnel {
        let target_host = opts.host.clone().unwrap_or_else(|| "localhost".to_owned());
        let target_port = opts.port.unwrap_or(5432);
        match ssh_tunnel::open_tunnel(tcfg, &target_host, target_port).await {
            Ok(tunnel) => {
                if !cli.quiet {
                    eprintln!(
                        "rpg: SSH tunnel established \
                         (127.0.0.1:{} → {}:{})",
                        tunnel.local_port, target_host, target_port
                    );
                }
                opts.host = Some("127.0.0.1".to_owned());
                opts.port = Some(tunnel.local_port);
                Some(tunnel)
            }
            Err(e) => {
                eprintln!("rpg: {e}");
                std::process::exit(2);
            }
        }
    } else {
        None
    };

    // Resolve parameters once; pass into connect() so both display and the
    // actual driver use the exact same values (avoids double-resolve drift).
    let params = match connection::resolve_params(&opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rpg: {e}");
            std::process::exit(2);
        }
    };

    match connection::connect(params, &opts).await {
        Ok((client, resolved)) => {
            use std::io::IsTerminal;
            logging::info(
                "connection",
                &format!(
                    "connected: host={} port={} user={} dbname={}",
                    resolved.host, resolved.port, resolved.user, resolved.dbname
                ),
            );
            let is_piped = !cli.interactive && !std::io::stdin().is_terminal();
            let is_scripting = !cli.command.is_empty() || cli.file.is_some();
            let is_interactive = !is_scripting && !is_piped;

            let mut settings = build_settings(&cli, &cfg, &project_result);

            // Capability detection: in non-interactive mode (-c, -f, piped
            // input) we skip the pg_ash / pooler / managed-provider probes
            // since those results are only used for the interactive banner,
            // the statusline, and governance decisions.  We still fetch the
            // server version so that logging and daemon mode have it.
            //
            // In interactive mode we run the full detect() so the banner,
            // pg_ash statusline, and governance framework all work.
            settings.db_capabilities = if is_interactive || cli.daemon {
                capabilities::detect(&client).await
            } else {
                // Lightweight path: only the server version query.
                capabilities::DbCapabilities {
                    server_version: capabilities::detect_server_version_pub(&client).await,
                    ..Default::default()
                }
            };

            if !cli.quiet && is_interactive {
                // Version banner — matches psql's style of showing version on
                // connect. Only shown for interactive sessions, not -c/-f/pipe.
                let server_ver = settings
                    .db_capabilities
                    .server_version
                    .as_deref()
                    .unwrap_or("unknown");
                println!("{} (server PostgreSQL {})", version_string(), server_ver,);
                println!("Type \"help\" for help.");
                println!("{}", connection::connection_info(&resolved));
            }

            if let capabilities::PgAshStatus::Available { ref version } =
                settings.db_capabilities.pg_ash
            {
                if !cli.quiet && is_interactive {
                    let ver = version.as_deref().unwrap_or("unknown version");
                    logging::info("capabilities", &format!("pg_ash detected: {ver}"));
                }
            }

            // Detect whether the connected role is a superuser so the prompt
            // can show `#` instead of `>`.  Only needed for the interactive
            // prompt; skip in scripting / piped / daemon mode.
            settings.is_superuser = if is_interactive {
                capabilities::detect_superuser(&client).await
            } else {
                false
            };

            // --check: run all analyzers once, print summary, exit with
            // severity code (0=healthy, 1=warning, 2=critical).
            if cli.check {
                let exit_code = check::run_health_check(&client).await;
                std::process::exit(exit_code);
            }

            // --report [format]: run all analyzers, print detailed report,
            // exit with severity code (0=healthy, 1=warning, 2=critical).
            if let Some(ref format) = cli.report {
                let report_registry = {
                    let connectors_cfg = cfg.connectors.clone().unwrap_or_default();
                    connector_setup::build_connector_registry(&connectors_cfg)
                };
                let exit_code = report::run_report(&client, format, &report_registry).await;
                std::process::exit(exit_code);
            }

            let exit_code = if cli.daemon {
                // Daemon mode: headless continuous monitoring.
                let pid_path = cli
                    .pid_file
                    .as_ref()
                    .map_or_else(daemon::default_pid_path, std::path::PathBuf::from);

                if let Some(existing) = daemon::check_existing_pid(&pid_path) {
                    eprintln!("rpg: daemon already running (PID {existing})");
                    std::process::exit(1);
                }
                if let Err(e) = daemon::write_pid_file(&pid_path) {
                    eprintln!("rpg: could not write PID file: {e}");
                    std::process::exit(2);
                }

                let mut channels = vec![daemon::NotificationChannel::Stderr];
                if let Some(ref url) = cli.slack_webhook {
                    channels.push(daemon::NotificationChannel::Slack {
                        webhook_url: url.clone(),
                    });
                }
                if let Some(ref url) = cli.webhook_url {
                    channels.push(daemon::NotificationChannel::Webhook {
                        url: url.clone(),
                        secret: cli.webhook_secret.clone(),
                    });
                }
                if let Some(ref key) = cli.pagerduty_key {
                    channels.push(daemon::NotificationChannel::PagerDuty {
                        routing_key: key.clone(),
                    });
                }
                if let (Some(ref token), Some(ref chat_id)) =
                    (&cli.telegram_bot_token, &cli.telegram_chat_id)
                {
                    channels.push(daemon::NotificationChannel::Telegram {
                        bot_token: token.clone(),
                        chat_id: chat_id.clone(),
                    });
                }

                let connector_registry = {
                    let connectors_cfg = cfg.connectors.clone().unwrap_or_default();
                    connector_setup::build_connector_registry(&connectors_cfg)
                };

                daemon::run(
                    &client,
                    &cfg,
                    &resolved.dbname,
                    &channels,
                    cli.health_port,
                    cli.github_repo.as_deref(),
                    &connector_registry,
                )
                .await;

                daemon::remove_pid_file(&pid_path);
                0
            } else if !cli.command.is_empty() {
                // -c CMD [--c CMD ...]: execute commands in order and exit.
                // Mirror psql: stop on first non-zero exit and propagate it.
                let mut exit_code = 0i32;
                for cmd in &cli.command {
                    exit_code = repl::exec_command(&client, cmd, &mut settings, &resolved).await;
                    if exit_code != 0 {
                        break;
                    }
                }
                exit_code
            } else if let Some(ref path) = cli.file {
                // -f file: execute file and exit.
                repl::exec_file(&client, path, &mut settings, &resolved).await
            } else if is_piped {
                // Piped / redirected stdin: execute non-interactively.
                repl::exec_stdin(&client, &mut settings, &resolved).await
            } else {
                // Interactive REPL — consumes client and resolved.
                repl::run_repl(client, resolved, settings, cli.no_readline, cli.no_psqlrc).await
            };

            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Err(e) => {
            eprintln!("rpg: {e}");
            std::process::exit(2);
        }
    }
}
