//! Samo — self-driving Postgres agent and psql-compatible terminal.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! samo-specific options, then dispatches to the appropriate subsystem.

use clap::Parser;

mod complete;
mod conditional;
mod connection;
mod copy;
mod crosstab;
mod dba;
mod describe;
mod highlight;
mod io;
mod metacmd;
#[allow(dead_code)]
mod output;
mod pager;
mod pattern;
#[allow(dead_code)]
mod query;
mod repl;
mod safety;
mod session;
mod vars;

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
    /// Run a single command (SQL or backslash) and exit.
    #[arg(short = 'c', long)]
    command: Option<String>,

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

/// Build a [`repl::ReplSettings`] from the parsed CLI flags.
///
/// Exits the process (code 2) if file-opening operations fail.
fn build_settings(cli: &Cli) -> repl::ReplSettings {
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

    repl::ReplSettings {
        echo_hidden: cli.echo_hidden,
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
        no_highlight: cli.no_highlight,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

// TODO: Replace #[tokio::main] with explicit runtime construction
// to optimize thread count per operating mode (issue #2, finding #9).
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
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
            let is_piped = !cli.interactive && !std::io::stdin().is_terminal();
            let is_scripting = cli.command.is_some() || cli.file.is_some();
            if !cli.quiet && !is_scripting && !is_piped {
                println!("{}", connection::connection_info(&resolved));
            }

            let mut settings = build_settings(&cli);

            let exit_code = if let Some(ref cmd) = cli.command {
                // -c "SQL": execute single command and exit.
                repl::exec_command(&client, cmd, &mut settings, &resolved).await
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
