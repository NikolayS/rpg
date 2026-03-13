//! Samo — self-driving Postgres agent and psql-compatible terminal.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! samo-specific options, then dispatches to the appropriate subsystem.

use clap::Parser;

// Core modules.
mod actor;
mod ai;
mod capabilities;
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
mod io;
mod logging;
mod metacmd;
mod named;
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
mod vars;

// Phase 2/3 infrastructure — compiled but not yet wired into the main
// dispatch loop. Each module suppresses dead_code at the item level.
mod anomaly;
mod index_health;
mod issues;
mod rca_actions;
mod verification;

/// Build-time git commit hash injected by `build.rs`.
const GIT_HASH: &str = env!("SAMO_GIT_HASH");

// ---------------------------------------------------------------------------
// Autonomy levels (samo-specific)
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

/// Assemble a long version string like `0.1.0-dev (abc1234)`.
fn long_version() -> &'static str {
    // Leak is fine: called once at startup, lives for the process lifetime.
    Box::leak(format!("{} ({})", env!("CARGO_PKG_VERSION"), GIT_HASH).into_boxed_str())
}

/// Samo — self-driving Postgres agent and psql-compatible terminal.
///
/// A psql-compatible interface with built-in AI and autonomous
/// database health management.
#[derive(Parser, Debug)]
#[command(
    name = "samo",
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

    /// SSL mode (disable, prefer, require).
    #[arg(long, value_name = "SSLMODE")]
    sslmode: Option<String>,

    /// Force password prompt.
    #[arg(short = 'W', long)]
    password: bool,

    /// Never prompt for password.
    #[arg(short = 'w', long = "no-password")]
    no_password: bool,

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

    /// Do not read startup file (~/.psqlrc / ~/.samorc).
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

    /// Echo queries that samo generates internally.
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

    // -- Samo-specific flags ------------------------------------------------
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

    /// Generate `samo_ops` wrapper SQL and exit. Specify PG version (e.g. 14, 16).
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

    /// Path to PID file for daemon mode.
    #[arg(long, value_name = "PATH")]
    pid_file: Option<String>,

    /// GitHub repository (owner/repo) for creating issues from findings.
    #[arg(long, value_name = "OWNER/REPO")]
    github_repo: Option<String>,
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
                    eprintln!("samo: invalid value for -P format: \"{other}\"");
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
            eprintln!("samo: -L: could not open \"{path}\": {e}");
            std::process::exit(2);
        }
    }
}

/// Build a [`repl::ReplSettings`] from the parsed CLI flags and loaded config.
///
/// Config values set defaults; CLI flags take precedence and override them.
/// Exits the process (code 2) if file-opening operations fail.
fn build_settings(cli: &Cli, cfg: &config::Config) -> repl::ReplSettings {
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
            eprintln!("samo: -v requires name=value");
        }
    }

    // -o / --output: redirect query output to file.
    let output_target = cli
        .output
        .as_deref()
        .map(|path| match io::open_output(Some(path)) {
            Ok(w) => w.expect("open_output with Some path returns Some"),
            Err(e) => {
                eprintln!("samo: {e}");
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

    // Initialise pager_command from the PAGER environment variable.
    // A non-empty PAGER that is not "on"/"off" sets an external pager.
    // An empty or absent PAGER leaves the built-in pager as default.
    let pager_command = std::env::var("PAGER")
        .ok()
        .filter(|v| !v.is_empty() && v != "on" && v != "off");

    // Keep ReplSettings.expanded in sync with pset.expanded so that both the
    // REPL path and the -c path see a consistent expanded mode.
    let expanded = pset.expanded;

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
        timing,
        config: cfg.clone(),
        exec_mode: if cli.yolo {
            repl::ExecMode::Yolo
        } else {
            repl::ExecMode::default()
        },
        i_know_what_im_doing: cli.i_know_what_im_doing,
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

    let log_writer: Option<Box<dyn std::io::Write + Send>> = cli.log_file.as_deref().map(|path| {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(f) => Box::new(f) as Box<dyn std::io::Write + Send>,
            Err(e) => {
                eprintln!("samo: --log-file: {e}");
                std::process::exit(2);
            }
        }
    });

    logging::init(log_level, log_writer);

    // --generate-wrappers: emit SQL and exit (no DB connection needed).
    if let Some(ref pg_ver_str) = cli.generate_wrappers {
        let pg_version: u32 = pg_ver_str.parse().unwrap_or(16);
        print!("{}", setup::generate_setup_sql(pg_version));
        return;
    }

    // Load config hierarchy (system then user); non-fatal warnings are
    // printed to stderr unless --quiet suppresses them.
    let (cfg, config_warnings) = config::load_config();
    for w in &config_warnings {
        if !cli.quiet {
            eprintln!("samo: warning: {w}");
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
            // Clear the positional dbname so connection resolution does not
            // misinterpret "@production" as a literal database name.
            cli.dbname_pos = None;
        } else {
            eprintln!("samo: unknown profile \"@{name}\"");
            eprintln!(
                "Configure profiles in {} under [connections.{name}]",
                config::user_config_path_display()
            );
            std::process::exit(2);
        }
    }

    let opts = cli.conn_opts();

    // Resolve parameters once; pass into connect() so both display and the
    // actual driver use the exact same values (avoids double-resolve drift).
    let params = match connection::resolve_params(&opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("samo: {e}");
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
            if !cli.quiet && !is_scripting && !is_piped {
                println!("{}", connection::connection_info(&resolved));
            }

            let mut settings = build_settings(&cli, &cfg);

            // Detect database capabilities (pg_ash, server version, etc.).
            settings.db_capabilities = capabilities::detect(&client).await;
            if let capabilities::PgAshStatus::Available { ref version } =
                settings.db_capabilities.pg_ash
            {
                if !cli.quiet && !is_scripting && !is_piped {
                    let ver = version.as_deref().unwrap_or("unknown version");
                    logging::info("capabilities", &format!("pg_ash detected: {ver}"));
                }
            }

            let exit_code = if cli.daemon {
                // Daemon mode: headless continuous monitoring.
                let pid_path = cli
                    .pid_file
                    .as_ref()
                    .map_or_else(daemon::default_pid_path, std::path::PathBuf::from);

                if let Some(existing) = daemon::check_existing_pid(&pid_path) {
                    eprintln!("samo: daemon already running (PID {existing})");
                    std::process::exit(1);
                }
                if let Err(e) = daemon::write_pid_file(&pid_path) {
                    eprintln!("samo: could not write PID file: {e}");
                    std::process::exit(2);
                }

                let mut channels = vec![daemon::NotificationChannel::Stderr];
                if let Some(ref url) = cli.slack_webhook {
                    channels.push(daemon::NotificationChannel::Slack {
                        webhook_url: url.clone(),
                    });
                }

                daemon::run(
                    &client,
                    &cfg,
                    &resolved.dbname,
                    &channels,
                    cli.health_port,
                    cli.github_repo.as_deref(),
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
            eprintln!("samo: {e}");
            std::process::exit(2);
        }
    }
}
