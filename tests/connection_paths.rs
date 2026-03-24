//! Integration tests covering the connection path matrix from issue #709.
//!
//! Tests exercise all connection methods supported by rpg:
//!   A — TCP (basic, password, explicit db)
//!   B — Unix socket
//!   C — URI and key-value connstring
//!   D — SSL modes (prefer, require, fallback)
//!   E — Environment variables (PGHOST, PGPORT, PGUSER, PGPASSWORD, etc.)
//!   F — Error message quality (#708 fix)
//!   G — Authentication methods (trust, SCRAM)
//!   H — Multiple -c flags and -f file execution
//!
//! ## Local defaults
//!
//! Port 15433 — trust, no password, user=postgres
//! Port 15434 — SCRAM, user=postgres, password=testpass
//! Port 15436 — TLS self-signed, user=postgres, password=testpass
//!
//! ## CI env vars (set by the connection-tests job in checks.yml)
//!
//! Override local defaults via environment:
//!   CONN_TRUST_HOST   / CONN_TRUST_PORT   / CONN_TRUST_USER
//!   CONN_SCRAM_HOST   / CONN_SCRAM_PORT   / CONN_SCRAM_USER / CONN_SCRAM_PASSWORD / CONN_SCRAM_DATABASE
//!   CONN_TLS_HOST     / CONN_TLS_PORT     / CONN_TLS_USER   / CONN_TLS_PASSWORD
//!
//! These are mapped from the CI job's TEST_PG_* vars by the helper functions below.
//!
//! Run locally:
//! ```sh
//! cargo test --test connection_paths
//! ```

use std::fs;
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rpg() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rpg"))
}

fn run(mut cmd: Command) -> (String, String, i32) {
    let out = cmd.output().expect("failed to run rpg");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

// --- Trust postgres (no password) ---

fn trust_host() -> String {
    // CI: TEST_PGHOST (the standard integration service, testuser/testpass)
    // Fallback for our local setup
    std::env::var("CONN_TRUST_HOST")
        .or_else(|_| std::env::var("TEST_PGHOST"))
        .unwrap_or_else(|_| "localhost".to_owned())
}

fn trust_port() -> String {
    std::env::var("CONN_TRUST_PORT")
        .or_else(|_| std::env::var("TEST_PGPORT"))
        .unwrap_or_else(|_| "15433".to_owned())
}

fn trust_user() -> String {
    std::env::var("CONN_TRUST_USER")
        .or_else(|_| std::env::var("TEST_PGUSER"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

fn trust_password() -> Option<String> {
    // Only supply a password if the env says so (CI uses testuser/testpass)
    std::env::var("CONN_TRUST_PASSWORD")
        .or_else(|_| std::env::var("TEST_PGPASSWORD"))
        .ok()
}

fn trust_database() -> String {
    std::env::var("CONN_TRUST_DATABASE")
        .or_else(|_| std::env::var("TEST_PGDATABASE"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

// --- SCRAM postgres (password required) ---

fn scram_host() -> String {
    std::env::var("CONN_SCRAM_HOST")
        .or_else(|_| std::env::var("TEST_PG_SCRAM_HOST"))
        .unwrap_or_else(|_| "localhost".to_owned())
}

fn scram_port() -> String {
    std::env::var("CONN_SCRAM_PORT")
        .or_else(|_| std::env::var("TEST_PG_SCRAM_PORT"))
        .unwrap_or_else(|_| "15434".to_owned())
}

fn scram_user() -> String {
    std::env::var("CONN_SCRAM_USER")
        .or_else(|_| std::env::var("TEST_PG_SCRAM_USER"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

fn scram_password() -> String {
    std::env::var("CONN_SCRAM_PASSWORD")
        .or_else(|_| std::env::var("TEST_PG_SCRAM_PASSWORD"))
        .unwrap_or_else(|_| "testpass".to_owned())
}

fn scram_database() -> String {
    std::env::var("CONN_SCRAM_DATABASE")
        .or_else(|_| std::env::var("TEST_PG_SCRAM_DATABASE"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

// --- TLS postgres (self-signed cert) ---

fn tls_host() -> String {
    std::env::var("CONN_TLS_HOST")
        .or_else(|_| std::env::var("TEST_PG_TLS_HOST"))
        .unwrap_or_else(|_| "localhost".to_owned())
}

fn tls_port() -> String {
    std::env::var("CONN_TLS_PORT")
        .or_else(|_| std::env::var("TEST_PG_TLS_PORT"))
        .unwrap_or_else(|_| "15436".to_owned())
}

fn tls_user() -> String {
    std::env::var("CONN_TLS_USER")
        .or_else(|_| std::env::var("TEST_PG_TLS_USER"))
        .unwrap_or_else(|_| "postgres".to_owned())
}

fn tls_password() -> String {
    std::env::var("CONN_TLS_PASSWORD")
        .or_else(|_| std::env::var("TEST_PG_TLS_PASSWORD"))
        .unwrap_or_else(|_| "testpass".to_owned())
}

fn tls_database() -> String {
    std::env::var("CONN_TLS_DATABASE").unwrap_or_else(|_| "postgres".to_owned())
}

/// Apply trust credentials to a command (host, port, user, optional password).
fn apply_trust(cmd: &mut Command) {
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
}

// ---------------------------------------------------------------------------
// Group A — TCP
// ---------------------------------------------------------------------------

/// A1: basic TCP connection — SELECT 1 returns "1".
#[test]
fn a1_basic_tcp() {
    let mut cmd = rpg();
    apply_trust(&mut cmd);
    cmd.arg("-c").arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "a1: expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "a1: expected '1' in output\nstdout: {stdout}"
    );
}

/// A2: TCP with PGPASSWORD — SCRAM auth succeeds.
#[test]
fn a2_tcp_pgpassword() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &scram_host(),
        "-p",
        &scram_port(),
        "-U",
        &scram_user(),
        "-d",
        &scram_database(),
        "-c",
        "SELECT 1",
    ])
    .env("PGPASSWORD", scram_password());
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "a2: expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "a2: expected '1' in output\nstdout: {stdout}"
    );
}

/// A3: TCP with explicit -d flag — current_database() returns the named db.
#[test]
fn a3_tcp_explicit_db() {
    let db = trust_database();
    let mut cmd = rpg();
    apply_trust(&mut cmd);
    cmd.args(["-d", &db, "-c", "SELECT current_database()"]);
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "a3: expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&db),
        "a3: expected database name '{db}' in output\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group B — Unix socket
// ---------------------------------------------------------------------------

/// B1: Unix socket via explicit -h /tmp -p 5437.
///
/// Skipped when `/tmp/.s.PGSQL.5437` is absent.
#[test]
fn b1_socket_explicit_host() {
    let socket_port = std::env::var("CONN_SOCKET_PORT").unwrap_or_else(|_| "5437".to_owned());
    let socket_dir = std::env::var("CONN_SOCKET_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    let socket_path = format!("{socket_dir}/.s.PGSQL.{socket_port}");

    if !Path::new(&socket_path).exists() {
        eprintln!("SKIP b1_socket_explicit_host: {socket_path} not found");
        return;
    }

    let socket_user = std::env::var("CONN_SOCKET_USER").unwrap_or_else(|_| "tars".to_owned());

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &socket_dir,
        "-p",
        &socket_port,
        "-U",
        &socket_user,
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "b1: expected exit 0 via socket\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "b1: expected '1' in socket output\nstdout: {stdout}"
    );
}

/// B2: Unix socket via PGHOST/PGPORT env vars.
///
/// Skipped when socket file is absent.
#[test]
fn b2_socket_pghost_env() {
    let socket_port = std::env::var("CONN_SOCKET_PORT").unwrap_or_else(|_| "5437".to_owned());
    let socket_dir = std::env::var("CONN_SOCKET_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    let socket_path = format!("{socket_dir}/.s.PGSQL.{socket_port}");

    if !Path::new(&socket_path).exists() {
        eprintln!("SKIP b2_socket_pghost_env: {socket_path} not found");
        return;
    }

    let socket_user = std::env::var("CONN_SOCKET_USER").unwrap_or_else(|_| "tars".to_owned());

    let mut cmd = rpg();
    cmd.env("PGHOST", &socket_dir)
        .env("PGPORT", &socket_port)
        .env("PGUSER", &socket_user)
        .arg("-c")
        .arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "b2: expected exit 0 via PGHOST socket env\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "b2: expected '1' via PGHOST env socket\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group C — URI / connstring
// ---------------------------------------------------------------------------

/// C1: `postgres://` URI scheme.
#[test]
fn c1_postgres_uri() {
    let uri = format!(
        "postgres://{}@{}:{}/{}",
        trust_user(),
        trust_host(),
        trust_port(),
        trust_database()
    );
    let mut cmd = rpg();
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    cmd.arg(&uri).arg("-c").arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c1: expected exit 0 with postgres:// URI\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "c1: expected '1' in output\nstdout: {stdout}"
    );
}

/// C2: `postgresql://` scheme (alias for `postgres://`).
#[test]
fn c2_postgresql_scheme() {
    let uri = format!(
        "postgresql://{}@{}:{}/{}",
        trust_user(),
        trust_host(),
        trust_port(),
        trust_database()
    );
    let mut cmd = rpg();
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    cmd.arg(&uri).arg("-c").arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c2: expected exit 0 with postgresql:// URI\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "c2: expected '1' in output\nstdout: {stdout}"
    );
}

/// C3: URI with `?sslmode=require` against a TLS server — pg_stat_ssl.ssl=t.
#[test]
fn c3_uri_with_sslmode() {
    let uri = format!(
        "postgres://{}:{}@{}:{}/{}?sslmode=require",
        tls_user(),
        tls_password(),
        tls_host(),
        tls_port(),
        tls_database()
    );
    let mut cmd = rpg();
    cmd.arg(&uri)
        .arg("-c")
        .arg("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c3: expected exit 0 with sslmode=require URI\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('t'),
        "c3: expected ssl=t in pg_stat_ssl\nstdout: {stdout}"
    );
}

/// C4: URI with `?host=/tmp&port=5437` host query param for Unix socket.
///
/// Skipped when socket file is absent.
#[test]
fn c4_uri_host_query_param() {
    let socket_port = std::env::var("CONN_SOCKET_PORT").unwrap_or_else(|_| "5437".to_owned());
    let socket_dir = std::env::var("CONN_SOCKET_DIR").unwrap_or_else(|_| "/tmp".to_owned());
    let socket_path = format!("{socket_dir}/.s.PGSQL.{socket_port}");

    if !Path::new(&socket_path).exists() {
        eprintln!("SKIP c4_uri_host_query_param: {socket_path} not found");
        return;
    }

    let socket_user = std::env::var("CONN_SOCKET_USER").unwrap_or_else(|_| "tars".to_owned());
    let uri =
        format!("postgres:///postgres?host={socket_dir}&port={socket_port}&user={socket_user}");
    let mut cmd = rpg();
    cmd.arg(&uri).arg("-c").arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c4: expected exit 0 via URI socket host param\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "c4: expected '1' in output\nstdout: {stdout}"
    );
}

/// C5: key=value connstring passed as positional argument.
#[test]
fn c5_key_value_connstring() {
    let connstr = format!(
        "host={} port={} user={} dbname={}",
        trust_host(),
        trust_port(),
        trust_user(),
        trust_database()
    );
    let pw_connstr;
    let full_connstr = if let Some(pw) = trust_password() {
        pw_connstr = format!("{connstr} password={pw}");
        &pw_connstr
    } else {
        &connstr
    };
    let mut cmd = rpg();
    cmd.arg(full_connstr).arg("-c").arg("SELECT 1");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c5: expected exit 0 with key=value connstring\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "c5: expected '1' in output\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group D — SSL modes
// ---------------------------------------------------------------------------

/// D1: `sslmode=prefer` against a TLS server with a self-signed cert — ssl=t.
///
/// This is the v0.8.2 fix (#726): prefer must succeed even with self-signed cert.
#[test]
fn d1_sslmode_prefer_self_signed() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &tls_host(),
        "-p",
        &tls_port(),
        "-U",
        &tls_user(),
        "--sslmode",
        "prefer",
        "-c",
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    ])
    .env("PGPASSWORD", tls_password());
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "d1: expected exit 0 with sslmode=prefer self-signed cert\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('t'),
        "d1: expected ssl=t with sslmode=prefer\nstdout: {stdout}"
    );
}

/// D2: `sslmode=require` against a TLS server with a self-signed cert — ssl=t.
///
/// This is the v0.8.1 fix (#711): require must succeed even with self-signed cert.
#[test]
fn d2_sslmode_require_self_signed() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &tls_host(),
        "-p",
        &tls_port(),
        "-U",
        &tls_user(),
        "--sslmode",
        "require",
        "-c",
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    ])
    .env("PGPASSWORD", tls_password());
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "d2: expected exit 0 with sslmode=require self-signed cert\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('t'),
        "d2: expected ssl=t with sslmode=require\nstdout: {stdout}"
    );
}

/// D3: `sslmode=require` against a plain (no-TLS) server — must fail with SSL
/// error in stderr.
#[test]
fn d3_sslmode_require_no_tls_server() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "--sslmode",
        "require",
        "-c",
        "SELECT 1",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "d3: expected nonzero exit when sslmode=require against plain server\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("ssl"),
        "d3: expected SSL-related error in output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// D4: `sslmode=prefer` against a plain server — plaintext fallback, exit 0.
#[test]
fn d4_sslmode_prefer_no_tls_server() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "--sslmode",
        "prefer",
        "-c",
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "d4: expected exit 0 with sslmode=prefer fallback to plaintext\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('f'),
        "d4: expected ssl=f on plain server with sslmode=prefer\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group E — Environment variables
// ---------------------------------------------------------------------------

/// E1: `PGHOST` + `PGPORT` + `PGUSER` env vars — no CLI connection flags.
#[test]
fn e1_pghost_pgport() {
    let mut cmd = rpg();
    cmd.env("PGHOST", trust_host())
        .env("PGPORT", trust_port())
        .env("PGUSER", trust_user())
        .arg("-c")
        .arg("SELECT 1");
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e1: expected exit 0 via PGHOST/PGPORT/PGUSER\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "e1: expected '1' in output\nstdout: {stdout}"
    );
}

/// E2: PGPORT env var overrides default port.
#[test]
fn e2_pgport_env() {
    let mut cmd = rpg();
    cmd.args(["-h", &trust_host(), "-U", &trust_user(), "-c", "SELECT 1"])
        .env("PGPORT", trust_port());
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e2: expected exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "e2: expected '1' in output\nstdout: {stdout}"
    );
}

/// E3: `PGUSER` env var — current_user() returns the expected user.
#[test]
fn e3_pguser() {
    let user = trust_user();
    let mut cmd = rpg();
    cmd.env("PGUSER", &user).args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-c",
        "SELECT current_user",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e3: expected exit 0 via PGUSER\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&user),
        "e3: expected user '{user}' in current_user output\nstdout: {stdout}"
    );
}

/// E4: `PGAPPNAME` sets application_name visible in pg_stat_activity.
#[test]
fn e4_pgappname() {
    let mut cmd = rpg();
    apply_trust(&mut cmd);
    cmd.env("PGAPPNAME", "myapp")
        .arg("-c")
        .arg("SELECT application_name FROM pg_stat_activity WHERE pid = pg_backend_pid()");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e4: expected exit 0 with PGAPPNAME\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("myapp"),
        "e4: expected application_name='myapp' in pg_stat_activity\nstdout: {stdout}"
    );
}

/// E5: `PGSSLMODE=require` against a TLS server — ssl=t.
#[test]
fn e5_pgsslmode() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &tls_host(),
        "-p",
        &tls_port(),
        "-U",
        &tls_user(),
        "-c",
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    ])
    .env("PGPASSWORD", tls_password())
    .env("PGSSLMODE", "require");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e5: expected exit 0 with PGSSLMODE=require\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('t'),
        "e5: expected ssl=t via PGSSLMODE=require\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group F — Error message quality (#708 fix)
// ---------------------------------------------------------------------------

/// F1: Wrong password — stderr contains "authentication failed", not raw "db error".
#[test]
fn f1_wrong_password_clear_error() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &scram_host(),
        "-p",
        &scram_port(),
        "-U",
        &scram_user(),
        "-d",
        &scram_database(),
        "-w", // never prompt
        "-c",
        "SELECT 1",
    ])
    .env("PGPASSWORD", "definitely_wrong_password_xyz");
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f1: expected nonzero exit for wrong password\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("authentication failed") || combined.contains("password"),
        "f1: expected clear authentication error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Must not be a raw internal error with no context
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "f1: error must be user-facing, not a Rust panic\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// F2: Connection refused — stderr contains "Connection refused", not raw "db error".
#[test]
fn f2_connection_refused_clear_error() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        "127.0.0.1",
        "-p",
        "19999",
        "-U",
        "nobody",
        "-d",
        "nobody",
        "-w",
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f2: expected nonzero exit for connection refused\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("connection refused") || combined.contains("refused"),
        "f2: expected 'Connection refused' in error output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// F3: Unknown hostname — stderr contains a DNS error message, not raw "db error".
#[test]
fn f3_unknown_host_clear_error() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        "doesnotexist.invalid",
        "-p",
        "5432",
        "-U",
        "nobody",
        "-d",
        "nobody",
        "-w",
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f3: expected nonzero exit for unknown host\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("Name or service not known")
            || stderr.contains("failed to lookup")
            || stderr.contains("nodename nor servname provided")
            || stderr.contains("No such host"),
        "f3: expected DNS error in stderr\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Group G — Authentication methods
// ---------------------------------------------------------------------------

/// G1: Trust auth — connects without any password.
#[test]
fn g1_trust_auth() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "-w", // never prompt for password — must not need one
        "-c",
        "SELECT 1",
    ]);
    // Supply password only if CI needs it (testuser/testpass scenario)
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "g1: expected exit 0 for trust auth\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "g1: expected '1' in trust-auth output\nstdout: {stdout}"
    );
}

/// G2: SCRAM-SHA-256 auth — connects with correct password.
#[test]
fn g2_scram_auth() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &scram_host(),
        "-p",
        &scram_port(),
        "-U",
        &scram_user(),
        "-d",
        &scram_database(),
        "-c",
        "SELECT current_user",
    ])
    .env("PGPASSWORD", scram_password());
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "g2: expected exit 0 for SCRAM auth\nstdout: {stdout}\nstderr: {stderr}"
    );
    let user = scram_user();
    assert!(
        stdout.contains(&user),
        "g2: expected user '{user}' in SCRAM auth output\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group H — Multiple -c flags and -f file
// ---------------------------------------------------------------------------

/// H1: Multiple `-c` flags — both result sets appear in stdout.
#[test]
fn h1_multiple_c_flags() {
    let mut cmd = rpg();
    apply_trust(&mut cmd);
    cmd.args(["-c", "SELECT 1 AS a", "-c", "SELECT 2 AS b"]);
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "h1: expected exit 0 for multiple -c flags\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(" a ") || stdout.contains("| a"),
        "h1: expected first result set column 'a'\nstdout: {stdout}"
    );
    assert!(
        stdout.contains(" b ") || stdout.contains("| b"),
        "h1: expected second result set column 'b'\nstdout: {stdout}"
    );
    assert!(
        stdout.contains('1') && stdout.contains('2'),
        "h1: expected values '1' and '2' in output\nstdout: {stdout}"
    );
}

/// H2: `-f` file flag — executes SQL from a temp file and returns the result.
#[test]
fn h2_f_flag() {
    let dir = tempfile::tempdir().expect("h2: failed to create tempdir");
    let sql_path = dir.path().join("query.sql");
    fs::write(&sql_path, "SELECT 42 AS answer;\n").expect("h2: failed to write sql file");

    let mut cmd = rpg();
    apply_trust(&mut cmd);
    cmd.arg("-f").arg(&sql_path);
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "h2: expected exit 0 for -f flag\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("42"),
        "h2: expected '42' in -f file output\nstdout: {stdout}"
    );
}
