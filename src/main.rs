//! Rpg — modern Postgres terminal with built-in diagnostics and AI assistant.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! rpg-specific options, then dispatches to the appropriate subsystem.

use clap::Parser;

// Output macros — must be declared before all other modules so they are in scope.
#[macro_use]
mod macros;

// Core modules.
mod ai;
#[cfg(not(target_arch = "wasm32"))]
mod ash;
mod capabilities;
mod compat;
#[cfg(not(target_arch = "wasm32"))]
mod complete;
mod conditional;
mod config;
mod connection;
mod copy;
mod crosstab;
mod dba;
mod describe;
mod explain;
mod highlight;
#[cfg(not(target_arch = "wasm32"))]
mod history_picker;
mod init;
#[allow(dead_code, unused_imports)]
mod input;
mod io;
mod large_object;
mod logging;
mod lua_commands;
mod markdown;
mod metacmd;
mod named;
mod output;
mod pager;
mod pattern;
mod query;
mod repl;
mod report;
#[allow(
    clippy::all,
    dead_code,
    unused_imports,
    clippy::pedantic,
    clippy::nursery
)]
#[cfg(not(target_arch = "wasm32"))]
mod rpg;
mod safety;
mod session;
mod session_store;
mod setup;
mod slashcmd;
// SSH tunneling uses russh which is not available for WASM targets.
#[cfg(not(target_arch = "wasm32"))]
mod ssh_tunnel;
mod statusline;
mod term;
mod update;
mod vars;
// WASM browser support: WebSocket connector and wasm-bindgen entry point.
// Only compiled on wasm32 targets — invisible to native builds.
#[cfg(target_arch = "wasm32")]
mod wasm;

/// Build-time git commit hash injected by `build.rs` (8 hex chars).
const GIT_HASH: &str = env!("RPG_GIT_HASH");

/// Build-time date (UTC, `YYYY-MM-DD`) injected by `build.rs`.
const BUILD_DATE: &str = env!("RPG_BUILD_DATE");

/// Number of commits since the last version tag, injected by `build.rs`.
/// Zero when this commit is exactly the tagged release.
const COMMITS_SINCE_TAG: u32 = {
    match option_env!("RPG_COMMITS_SINCE_TAG") {
        Some(s) => {
            // const-compatible decimal parse
            let bytes = s.as_bytes();
            let mut n: u32 = 0;
            let mut i = 0;
            while i < bytes.len() {
                n = n * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            n
        }
        None => 0,
    }
};

/// One-line version string: `rpg 0.2.0 (abc1234, built 2026-03-13)`.
///
/// Exposed as `pub` so that meta-command handlers can print it without
/// duplicating the formatting logic.
pub fn version_string() -> &'static str {
    // Leak is fine: called at most a handful of times, lives for the
    // process lifetime.
    //
    // Examples:
    //   "rpg 0.9.0 (588b8d0a, built 2026-03-28)"          ← exact tag
    //   "rpg 0.9.0+3-abc12345 (abc12345, built 2026-03-28)" ← 3 commits past tag
    Box::leak(
        if COMMITS_SINCE_TAG == 0 {
            format!(
                "rpg {} ({}, built {})",
                env!("CARGO_PKG_VERSION"),
                GIT_HASH,
                BUILD_DATE,
            )
        } else {
            format!(
                "rpg {}+{}-{} ({}, built {})",
                env!("CARGO_PKG_VERSION"),
                COMMITS_SINCE_TAG,
                GIT_HASH,
                GIT_HASH,
                BUILD_DATE,
            )
        }
        .into_boxed_str(),
    )
}

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// Assemble the clap version string: delegates to [`version_string`].
fn long_version() -> &'static str {
    version_string()
}

/// Rpg — modern Postgres terminal with built-in diagnostics and AI assistant.
///
/// A psql-compatible interface with built-in AI and database diagnostics.
#[derive(Parser, Debug)]
#[command(
    name = "rpg",
    version = long_version(),
    about = "Modern Postgres terminal with built-in diagnostics and AI assistant",
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
    ///
    /// Accepts a single port or a comma-separated list matching the hosts in
    /// `-h` (`-p 5432,5433`).  A single port is applied to all hosts.
    #[arg(short = 'p', long, value_name = "PORT")]
    port: Option<String>,

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

    /// Echo all input to stdout (psql -a compatibility).
    ///
    /// Each SQL statement and meta-command is written to stdout before it is
    /// executed.  This is identical to psql's `-a` / `--echo-all` flag and is
    /// required to match the output format produced by `pg_regress` (which
    /// invokes psql with `-a -q`).
    #[arg(short = 'a', long = "echo-all")]
    echo_all: bool,

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

    /// Markdown table output format.
    #[arg(long)]
    markdown: bool,

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
        // `-p` now accepts a string to support comma-separated ports for
        // multi-host connections.  Parse a single u16 for backward-compat
        // callers that use `opts.port`; keep the raw string in `port_str`
        // so `resolve_hosts` can expand per-host ports.
        let port_u16: Option<u16> = self
            .port
            .as_deref()
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse().ok());
        connection::CliConnOpts {
            host: self.host.clone(),
            port: port_u16,
            port_str: self.port.clone(),
            username: self.username.clone(),
            dbname: self.dbname.clone(),
            dbname_pos: self.dbname_pos.clone(),
            user_pos: self.user_pos.clone(),
            host_pos: self.host_pos.clone(),
            port_pos: self.port_pos.clone(),
            force_password: self.password,
            no_password: self.no_password,
            sslmode: self.sslmode.clone(),
            // SSH tunneling is not available on WASM targets.
            #[cfg(not(target_arch = "wasm32"))]
            ssh_tunnel: self.ssh_tunnel.as_deref().and_then(|s| {
                ssh_tunnel::SshTunnelSpec::parse(s).map(config::SshTunnelConfig::from)
            }),
            #[cfg(target_arch = "wasm32")]
            ssh_tunnel: None,
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
                "markdown" => output::OutputFormat::Markdown,
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
    } else if cli.markdown {
        pset.format = output::OutputFormat::Markdown;
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
    // Sync into pset so NULL-cell dim rendering is also suppressed when
    // highlighting is disabled via CLI flag or config.
    pset.no_highlight = no_highlight;
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
        echo_all: cli.echo_all,
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
        project_context: project.postgres_md.clone(),
        ai_context_files: cfg.ai.project_context_files.clone(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // On WASM, wasm-bindgen calls main() automatically via __wbindgen_start.
    // block_on() is incompatible with the browser event loop, so main() is a
    // no-op here — run_rpg() in src/wasm/entry.rs is the real browser entry.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime");
        rt.block_on(async_main());
    }
}

#[allow(clippy::too_many_lines)]
#[cfg(not(target_arch = "wasm32"))]
async fn async_main() {
    // Install the default rustls CryptoProvider before any TLS operations.
    // Required because multiple dependencies (tokio-postgres-rustls, reqwest)
    // pull in different crypto backends, preventing auto-selection.
    #[cfg(not(target_arch = "wasm32"))]
    {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .ok();
    }

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
                    // Self-update requires filesystem access (not available on WASM).
                    #[cfg(not(target_arch = "wasm32"))]
                    {
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
                    #[cfg(target_arch = "wasm32")]
                    {
                        eprintln!("rpg: self-update not supported in browser");
                        std::process::exit(2);
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

    // Print project config startup messages only in interactive mode.
    // Suppress when: --quiet, -c/-f scripting flags, or stdin is not a TTY.
    {
        use std::io::IsTerminal;
        let is_scripting = !cli.command.is_empty() || cli.file.is_some();
        let is_piped = !cli.interactive && !std::io::stdin().is_terminal();
        let show = !cli.quiet && !is_scripting && !is_piped;
        if show {
            if let Some(ref p) = project_result.config_path {
                eprintln!("Using project config: {}", p.display());
            }
            if let Some(ref p) = project_result.postgres_md_path {
                eprintln!("Loaded project context: {}", p.display());
            }
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
                cli.port = profile.port.map(|p| p.to_string());
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
        cli.port = cfg.connection.port.clone();
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
    // SSH tunneling is not available on WASM targets (no russh dependency).
    #[cfg(not(target_arch = "wasm32"))]
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
    // On WASM the tunnel handle is a unit placeholder so the borrow checker
    // sees the variable used regardless of cfg.
    #[cfg(target_arch = "wasm32")]
    let _tunnel: Option<()> = None;

    // Resolve parameters once; pass into connect() so both display and the
    // actual driver use the exact same values (avoids double-resolve drift).
    let (params, initial_password) = match connection::resolve_params(&opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rpg: {e}");
            std::process::exit(2);
        }
    };

    // Resolve the password before connect() so that the connect function
    // never calls a password-source function internally.  This keeps
    // CodeQL's taint analysis from attributing password-derived taint to
    // connect()'s return values.
    let password = match connection::resolve_password_value(
        initial_password,
        &params,
        opts.force_password,
        opts.no_password,
        false,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rpg: {e}");
            std::process::exit(2);
        }
    };

    match connection::connect(params, password.as_deref()).await {
        Ok((client, mut resolved, resolved_tls)) => {
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

            // Detect server version for logging; skip heavier capability
            // probes in non-interactive mode.
            settings.db_capabilities = if is_interactive {
                capabilities::detect(&client).await
            } else {
                // Lightweight path: server version + SCS (needed for correct
                // backslash parsing even in non-interactive mode).
                capabilities::DbCapabilities {
                    server_version: capabilities::detect_server_version_pub(&client).await,
                    standard_conforming_strings:
                        capabilities::detect_standard_conforming_strings_pub(&client).await,
                    ..Default::default()
                }
            };

            if !cli.quiet && is_interactive {
                // Connect banner — shown for interactive sessions only.
                let server_ver = settings
                    .db_capabilities
                    .server_version
                    .as_deref()
                    .unwrap_or("unknown");

                // rpg version line
                println!("{}", version_string());
                // Server version (full, including distro build info when present)
                println!("Server: PostgreSQL {server_ver}");
                // Connection details + SSL status — matching psql startup output.
                // Use fields from the untainted `resolved` struct (password
                // and tls_info were returned separately from connect() to
                // keep ConnParams free of CodeQL cleartext-logging taint).
                println!(
                    "{}",
                    connection::connection_info(&connection::ConnDisplayInfo {
                        host: &resolved.host,
                        port: resolved.port,
                        user: &resolved.user,
                        dbname: &resolved.dbname,
                        resolved_addr: resolved.resolved_addr.as_deref(),
                        tls_info: resolved_tls.as_ref(),
                    })
                );

                // LLM status
                let ai_status = {
                    let ai = &settings.config.ai;
                    match (ai.provider.as_deref(), ai.api_key_env.as_deref()) {
                        (Some(provider), Some(env_var)) => {
                            let model = ai.model.as_deref().unwrap_or("default");
                            let key_set = std::env::var_os(env_var).is_some_and(|v| !v.is_empty());
                            if key_set {
                                format!("AI: {provider}/{model}")
                            } else {
                                format!(
                                    "AI: {provider}/{model} (key not set — edit ~/.config/rpg/config.toml)"
                                )
                            }
                        }
                        _ => "AI: not configured — edit ~/.config/rpg/config.toml".to_owned(),
                    }
                };
                println!("{ai_status}");

                // Command convention hint
                println!(
                    "Type \\? for help. \\-commands are psql-compatible; /-commands are rpg extensions (AI and non-AI)."
                );
                println!();
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
            // prompt; skip in scripting / piped mode.
            settings.is_superuser = if is_interactive {
                capabilities::detect_superuser(&client).await
            } else {
                false
            };

            // Store password and TLS info in params — needed for
            // reconnection and \conninfo in the REPL.
            resolved.password = password;
            resolved.tls_info = resolved_tls;

            // --report [format]: run all analyzers, print detailed report,
            // exit with severity code (0=healthy, 1=warning, 2=critical).
            let exit_code = if let Some(ref format) = cli.report {
                report::run_report(&client, format).await
            } else if !cli.command.is_empty() {
                // -c CMD [--c CMD ...]: execute commands in order and exit.
                // Mirror psql: stop on first non-zero exit and propagate it.
                let mut code = 0i32;
                for cmd in &cli.command {
                    code = repl::exec_command(&client, cmd, &mut settings, &resolved).await;
                    if code != 0 {
                        break;
                    }
                }
                code
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- apply_cli_pset ------------------------------------------------------

    #[test]
    fn apply_cli_pset_format_aligned() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=aligned");
        assert_eq!(pset.format, output::OutputFormat::Aligned);
    }

    #[test]
    fn apply_cli_pset_format_csv() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=csv");
        assert_eq!(pset.format, output::OutputFormat::Csv);
    }

    #[test]
    fn apply_cli_pset_format_json() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=json");
        assert_eq!(pset.format, output::OutputFormat::Json);
    }

    #[test]
    fn apply_cli_pset_format_unaligned() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=unaligned");
        assert_eq!(pset.format, output::OutputFormat::Unaligned);
    }

    #[test]
    fn apply_cli_pset_format_markdown() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=markdown");
        assert_eq!(pset.format, output::OutputFormat::Markdown);
    }

    #[test]
    fn apply_cli_pset_format_html() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=html");
        assert_eq!(pset.format, output::OutputFormat::Html);
    }

    #[test]
    fn apply_cli_pset_format_wrapped() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "format=wrapped");
        assert_eq!(pset.format, output::OutputFormat::Wrapped);
    }

    #[test]
    fn apply_cli_pset_border_0() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "border=0");
        assert_eq!(pset.border, 0);
    }

    #[test]
    fn apply_cli_pset_border_1() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "border=1");
        assert_eq!(pset.border, 1);
    }

    #[test]
    fn apply_cli_pset_border_2() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "border=2");
        assert_eq!(pset.border, 2);
    }

    #[test]
    fn apply_cli_pset_border_clamped_to_2() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "border=5");
        assert_eq!(pset.border, 2, "border must be clamped to max 2");
    }

    #[test]
    fn apply_cli_pset_null_display_string() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "null=NULL");
        assert_eq!(pset.null_display, "NULL");
    }

    #[test]
    fn apply_cli_pset_null_display_empty() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "null=");
        assert_eq!(pset.null_display, "");
    }

    #[test]
    fn apply_cli_pset_fieldsep_comma() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "fieldsep=,");
        assert_eq!(pset.field_sep, ",");
    }

    #[test]
    fn apply_cli_pset_fieldsep_tab() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "fieldsep=\t");
        assert_eq!(pset.field_sep, "\t");
    }

    #[test]
    fn apply_cli_pset_tuples_only_on() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "tuples_only=on");
        assert!(pset.tuples_only);
    }

    #[test]
    fn apply_cli_pset_tuples_only_true() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "tuples_only=true");
        assert!(pset.tuples_only);
    }

    #[test]
    fn apply_cli_pset_tuples_only_1() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "tuples_only=1");
        assert!(pset.tuples_only);
    }

    #[test]
    fn apply_cli_pset_tuples_only_off() {
        let mut pset = output::PsetConfig {
            tuples_only: true,
            ..Default::default()
        };
        apply_cli_pset(&mut pset, "tuples_only=off");
        assert!(!pset.tuples_only);
    }

    #[test]
    fn apply_cli_pset_t_shorthand_for_tuples_only() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "t=on");
        assert!(pset.tuples_only);
    }

    #[test]
    fn apply_cli_pset_footer_off() {
        let mut pset = output::PsetConfig::default();
        assert!(pset.footer, "footer must default to true");
        apply_cli_pset(&mut pset, "footer=off");
        assert!(!pset.footer);
    }

    #[test]
    fn apply_cli_pset_footer_false() {
        let mut pset = output::PsetConfig::default();
        apply_cli_pset(&mut pset, "footer=false");
        assert!(!pset.footer);
    }

    #[test]
    fn apply_cli_pset_footer_on() {
        let mut pset = output::PsetConfig {
            footer: false,
            ..Default::default()
        };
        apply_cli_pset(&mut pset, "footer=on");
        assert!(pset.footer);
    }

    #[test]
    fn apply_cli_pset_unknown_option_is_silently_ignored() {
        let mut pset = output::PsetConfig::default();
        let before_format = pset.format;
        apply_cli_pset(&mut pset, "unknownoption=somevalue");
        assert_eq!(pset.format, before_format);
    }

    #[test]
    fn apply_cli_pset_no_value_for_border_is_no_op() {
        let mut pset = output::PsetConfig::default();
        let before_border = pset.border;
        // "border" with no value and no '=' — treated as (option="border", value=None).
        apply_cli_pset(&mut pset, "border");
        // No value → parse::<u8>().ok() returns None → no-op.
        assert_eq!(pset.border, before_border);
    }

    // -- version_string ------------------------------------------------------

    #[test]
    fn version_string_starts_with_rpg() {
        let v = version_string();
        assert!(
            v.starts_with("rpg "),
            "version string must start with 'rpg ': {v:?}"
        );
    }

    #[test]
    fn version_string_contains_cargo_pkg_version() {
        let v = version_string();
        assert!(
            v.contains(env!("CARGO_PKG_VERSION")),
            "version string must contain package version: {v:?}",
        );
    }

    #[test]
    fn version_string_contains_built_keyword() {
        let v = version_string();
        assert!(
            v.contains("built "),
            "version string must contain 'built ': {v:?}"
        );
    }

    #[test]
    fn version_string_is_non_empty() {
        assert!(!version_string().is_empty());
    }
}
