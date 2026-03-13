//! Samo — self-driving Postgres agent and psql-compatible terminal.
//!
//! This is the CLI entry point. It parses psql-compatible flags and
//! samo-specific options, then dispatches to the appropriate subsystem.

use std::io::Read;

use clap::Parser;

mod connection;
mod output;
mod query;

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

    /// Build output configuration from CLI flags.
    fn output_config(&self) -> output::OutputConfig {
        // Warn about output format flags that are not yet implemented.
        if self.csv {
            eprintln!("samo: --csv is not yet supported, using default output");
        }
        if self.json {
            eprintln!("samo: --json is not yet supported, using default output");
        }

        output::OutputConfig {
            no_align: self.no_align,
            tuples_only: self.tuples_only,
            ..Default::default()
        }
    }

    /// Returns `true` when running in a non-interactive scripting mode
    /// (`-c`, `-f`, or piped stdin).  Connection info is suppressed in these
    /// modes to match psql behaviour.
    fn is_scripting_mode(&self) -> bool {
        self.command.is_some() || self.file.is_some()
    }
}

// ---------------------------------------------------------------------------
// Detect whether stdin is a TTY
// ---------------------------------------------------------------------------

/// Returns `true` when stdin is connected to a terminal (interactive session).
fn stdin_is_tty() -> bool {
    // Use `isatty` on the raw fd 0.
    // Safety: fd 0 is always open for the lifetime of the process.
    #[cfg(unix)]
    {
        // SAFETY: libc::isatty takes an fd and is always safe to call.
        unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix (Windows) we conservatively say stdin is a TTY so
        // we don't accidentally hang trying to read piped input.
        true
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
            // In -c / -f / piped-stdin mode psql does not print it.
            let is_piped = !cli.interactive && !stdin_is_tty();
            let interactive = !cli.is_scripting_mode() && !is_piped;
            if !cli.quiet && interactive {
                println!("{}", connection::connection_info(&resolved));
            }

            let cfg = cli.output_config();

            // Dispatch: -c → execute command and exit.
            if let Some(ref sql) = cli.command {
                run_sql_and_exit(&client, sql, &cfg).await;
            }

            // Dispatch: -f → execute file and exit.
            if let Some(ref path) = cli.file {
                run_file_and_exit(&client, path, &cfg).await;
            }

            // Dispatch: piped stdin (non-TTY, non-interactive).
            if is_piped {
                let mut sql = String::new();
                std::io::stdin()
                    .read_to_string(&mut sql)
                    .unwrap_or_else(|e| {
                        eprintln!("samo: could not read stdin: {e}");
                        std::process::exit(1);
                    });
                run_sql_and_exit(&client, &sql, &cfg).await;
            }

            // Interactive mode: REPL (issue #20 — not yet implemented).
            if !cli.quiet {
                println!("(interactive mode not yet implemented — disconnecting)");
            }
            drop(client);
        }
        Err(e) => {
            eprintln!("samo: {e}");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// Execution helpers
// ---------------------------------------------------------------------------

/// Execute SQL string, print output, exit with appropriate code.
async fn run_sql_and_exit(client: &tokio_postgres::Client, sql: &str, cfg: &output::OutputConfig) {
    match query::execute_sql(client, sql).await {
        Ok(outcome) => {
            let formatted = output::format_outcome(&outcome, cfg);
            print!("{formatted}");
            std::process::exit(0);
        }
        Err(query::QueryError::Postgres(ref pg_err)) => {
            let msg = output::format_pg_error(pg_err, Some(sql), cfg);
            eprint!("{msg}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("samo: {e}");
            std::process::exit(1);
        }
    }
}

/// Read SQL from a file, execute it, print output, exit.
///
/// The file is read once and the SQL string is reused for both execution
/// and error-position display, avoiding a TOCTOU race where the file could
/// change between the first and second read.
async fn run_file_and_exit(
    client: &tokio_postgres::Client,
    path: &str,
    cfg: &output::OutputConfig,
) {
    // Read the file once up-front so we have the SQL for error position display.
    let sql = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("samo: could not read \"{path}\": {e}");
            std::process::exit(1);
        }
    };

    match query::execute_sql(client, &sql).await {
        Ok(outcome) => {
            let formatted = output::format_outcome(&outcome, cfg);
            print!("{formatted}");
            std::process::exit(0);
        }
        Err(query::QueryError::Postgres(ref pg_err)) => {
            let msg = output::format_pg_error(pg_err, Some(&sql), cfg);
            eprint!("{msg}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("samo: {e}");
            std::process::exit(1);
        }
    }
}
