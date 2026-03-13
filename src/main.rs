//! Samo — self-driving Postgres agent and psql-compatible terminal.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! samo-specific options, then dispatches to the appropriate subsystem.

use clap::Parser;

mod connection;
mod describe;
mod metacmd;
#[allow(dead_code)]
mod output;
mod pattern;
#[allow(dead_code)]
mod query;
mod repl;

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

    /// Set printing option (like `\pset`).
    #[arg(short = 'P', long, value_name = "VAR[=ARG]")]
    pset: Option<String>,

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
            // Print connection info only in interactive mode.
            use std::io::IsTerminal;
            let is_piped = !cli.interactive && !std::io::stdin().is_terminal();
            let is_scripting = cli.command.is_some() || cli.file.is_some();
            if !cli.quiet && !is_scripting && !is_piped {
                println!("{}", connection::connection_info(&resolved));
            }

            let settings = repl::ReplSettings {
                echo_hidden: cli.echo_hidden,
                ..Default::default()
            };

            let exit_code = if let Some(ref cmd) = cli.command {
                // -c "SQL": execute single command and exit.
                repl::exec_command(&client, cmd, &settings, &resolved).await
            } else if let Some(ref path) = cli.file {
                // -f file: execute file and exit.
                repl::exec_file(&client, path, &settings).await
            } else if is_piped {
                // Piped / redirected stdin: execute non-interactively.
                repl::exec_stdin(&client, &settings).await
            } else {
                // Interactive REPL.
                repl::run_repl(&client, &resolved, settings, cli.no_readline).await
            };

            drop(client);
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
