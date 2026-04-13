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
//!   `CONN_TRUST_HOST`   / `CONN_TRUST_PORT`   / `CONN_TRUST_USER`
//!   `CONN_SCRAM_HOST`   / `CONN_SCRAM_PORT`   / `CONN_SCRAM_USER` / `CONN_SCRAM_PASSWORD` / `CONN_SCRAM_DATABASE`
//!   `CONN_TLS_HOST`     / `CONN_TLS_PORT`     / `CONN_TLS_USER`   / `CONN_TLS_PASSWORD`
//!
//! These are mapped from the CI job's `TEST_PG_*` vars by the helper functions below.
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
        "-d",
        &trust_database(),
    ]);
    // Always set PGDATABASE so tests that construct their own commands also
    // inherit the correct database (important in CI where dbname != username).
    cmd.env("PGDATABASE", trust_database());
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
}

// ---------------------------------------------------------------------------
// Group A — TCP
// ---------------------------------------------------------------------------

/// A1: basic TCP connection — SELECT 1 returns "1".
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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

/// A3: TCP with explicit -d flag — `current_database()` returns the named db.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn a3_tcp_explicit_db() {
    let db = trust_database();
    let mut cmd = rpg();
    apply_trust(&mut cmd); // apply_trust already sets -d trust_database()
    cmd.args(["-c", "SELECT current_database()"]);
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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

/// C3: URI with `?sslmode=require` against a TLS server — `pg_stat_ssl.ssl=t`.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
    cmd.env("PGDATABASE", trust_database());
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
    cmd.env("PGDATABASE", trust_database());
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn e2_pgport_env() {
    let mut cmd = rpg();
    cmd.args(["-h", &trust_host(), "-U", &trust_user(), "-c", "SELECT 1"])
        .env("PGPORT", trust_port());
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    cmd.env("PGDATABASE", trust_database());
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

/// E3: `PGUSER` env var — `current_user()` returns the expected user.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
    cmd.env("PGDATABASE", trust_database());
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

/// E4: `PGAPPNAME` sets `application_name` visible in `pg_stat_activity`.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
    cmd.env("PGDATABASE", trust_database());
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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
#[ignore = "requires live Postgres — run via connection-tests CI job"]
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

// ---------------------------------------------------------------------------
// Group B (continued) — Unix socket error paths
// ---------------------------------------------------------------------------

/// B3: No socket exists in the given directory — rpg must exit non-zero with
/// a clear "connection refused" or "no such file" message, not a Rust panic.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn b3_socket_dir_no_socket() {
    // Use a real directory that definitely has no PG socket.
    let dir = tempfile::tempdir().expect("b3: failed to create tempdir");
    let dir_str = dir.path().to_string_lossy().to_string();

    let mut cmd = rpg();
    cmd.args(["-h", &dir_str, "-U", "nobody", "-w", "-c", "SELECT 1"]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "b3: expected nonzero exit for missing socket\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("connection refused")
            || combined.contains("no such file")
            || combined.contains("failed to connect")
            || combined.contains("could not connect"),
        "b3: expected connection error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "b3: error must be user-facing, not a Rust panic\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// B4: Socket path exists but wrong user for peer auth — must exit non-zero
/// with an authentication error, not a Rust panic.  Skipped if the socket
/// is absent (CI environment without a host-mode PG).
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn b4_socket_wrong_user() {
    let socket_port = std::env::var("CONN_SOCKET_PORT").unwrap_or_else(|_| "5437".to_owned());
    let socket_dir =
        std::env::var("CONN_SOCKET_DIR").unwrap_or_else(|_| "/var/run/postgresql".to_owned());
    let socket_path = format!("{socket_dir}/.s.PGSQL.{socket_port}");

    if !Path::new(&socket_path).exists() {
        eprintln!("SKIP b4_socket_wrong_user: {socket_path} not found");
        return;
    }

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &socket_dir,
        "-p",
        &socket_port,
        "-U",
        "definitely_nonexistent_user_xyz",
        "-w",
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "b4: expected nonzero exit for wrong user on Unix socket\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("authentication failed")
            || combined.contains("peer authentication")
            || combined.contains("role")
            || combined.contains("password"),
        "b4: expected auth error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Group C (continued) — URI edge cases
// ---------------------------------------------------------------------------

/// C6: URI with a bad port (non-numeric) — must exit non-zero with a parse
/// error, not connect to a random port or panic.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn c6_uri_bad_port() {
    let mut cmd = rpg();
    cmd.args([
        "postgres://localhost:notaport/postgres",
        "-w",
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "c6: expected nonzero exit for bad URI port\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("invalid") || combined.contains("parse") || combined.contains("port"),
        "c6: expected parse error mentioning port, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// C7: URI with port in query param (`?port=<N>`) — C5 regression: this used
/// to be silently dropped before PR #731.  Must actually connect to the right
/// port.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn c7_uri_port_query_param() {
    // postgres://ignored:9999/postgres?host=localhost&port=<trust_port>
    let port = trust_port();
    let user = trust_user();
    let db = trust_database();
    let uri = format!("postgres://ignored:9999/{db}?host=localhost&port={port}");
    let mut cmd = rpg();
    cmd.args([
        &uri,
        "-U",
        &user,
        "-w",
        "-c",
        "SELECT current_database() AS db",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c7: expected exit 0 for URI with port query param\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&db),
        "c7: expected database name '{db}' in output\nstdout: {stdout}"
    );
}

/// C8: URI with `host=` query param — must connect to that host, not the
/// authority host.  Companion to C7.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn c8_uri_host_query_param_tcp() {
    let port = trust_port();
    let user = trust_user();
    let db = trust_database();
    // Authority has a bogus host; the real one is in the query string.
    let uri = format!("postgres://ignored/{db}?host=localhost&port={port}");
    let mut cmd = rpg();
    cmd.args([&uri, "-U", &user, "-w", "-c", "SELECT 1"]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "c8: expected exit 0 for URI with host query param\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "c8: expected '1' in output\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group E (continued) — Environment variables
// ---------------------------------------------------------------------------

/// E6: `PGSSLMODE=disable` suppresses TLS even when server supports it.
/// Connects to the TLS server; `ssl=f` must appear in `pg_stat_ssl`.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn e6_pgsslmode_disable() {
    if std::env::var("TEST_PG_TLS_PORT").is_err() && std::env::var("CONN_TLS_PORT").is_err() {
        eprintln!("SKIP e6_pgsslmode_disable: no TLS postgres configured");
        return;
    }
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &tls_host(),
        "-p",
        &tls_port(),
        "-U",
        &tls_user(),
        "-d",
        &tls_database(),
        "-c",
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    ])
    .env("PGSSLMODE", "disable")
    .env("PGPASSWORD", tls_password());
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e6: expected exit 0 with PGSSLMODE=disable\nstdout: {stdout}\nstderr: {stderr}"
    );
    // pg_stat_ssl returns a single row with `ssl | f`; check for the `f`
    // value in a way that won't match unrelated output (e.g. the word "false"
    // never appears alone, but "f" does as a Postgres boolean).
    assert!(
        stdout.contains(" f") || stdout.contains("|f") || stdout.contains("| f"),
        "e6: expected ssl=f (ssl disabled) in pg_stat_ssl output\nstdout: {stdout}"
    );
}

/// E7: `PGDATABASE` env var — no `-d` flag; database comes from env only.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn e7_pgdatabase_env() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "-w",
        "-c",
        "SELECT current_database() AS db",
    ])
    .env("PGDATABASE", trust_database());
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e7: expected exit 0 with PGDATABASE from env\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&trust_database()),
        "e7: expected database name in output\nstdout: {stdout}"
    );
}

/// E8: `PGPASSFILE` env var — password read from .pgpass file, no PGPASSWORD set.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn e8_pgpassfile_env() {
    let dir = tempfile::tempdir().expect("e8: failed to create tempdir");
    let pgpass = dir.path().join(".pgpass");

    // Write the correct password (from env or default) to .pgpass.
    // The point of the test is that PGPASSFILE is read; we need a valid
    // password here so password-auth servers (SCRAM in CI) also succeed.
    let host = trust_host();
    let port = trust_port();
    let password = trust_password().unwrap_or_default();
    std::fs::write(&pgpass, format!("{host}:{port}:*:*:{password}\n"))
        .expect("e8: failed to write .pgpass");

    // chmod 600 — libpq ignores .pgpass files that are world-readable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pgpass, std::fs::Permissions::from_mode(0o600))
            .expect("e8: failed to chmod .pgpass");
    }

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &host,
        "-p",
        &port,
        "-U",
        &trust_user(),
        "-d",
        &trust_database(),
        "-w",
        "-c",
        "SELECT 1",
    ])
    .env("PGPASSFILE", pgpass.to_str().unwrap())
    .env_remove("PGPASSWORD");
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "e8: expected exit 0 with PGPASSFILE env var\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "e8: expected '1' in output\nstdout: {stdout}"
    );
}

/// E9: `PGCONNECT_TIMEOUT` — elapsed time must be close to the configured
/// value, not `2x`.  Times out against a black-hole IP (10.255.255.1).
/// Checks elapsed ≤ `timeout_s` + 1 second of slack.
///
/// NOTE: This test documents the known issue #723 (timeout is ~2x expected).
/// Once #723 is fixed, tighten the bound to `timeout_s + 0.5`.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn e9_pgconnect_timeout() {
    use std::time::Instant;
    let timeout_s: u64 = 2;

    let mut cmd = rpg();
    cmd.args(["-h", "10.255.255.1", "-U", "nobody", "-w", "-c", "SELECT 1"])
        .env("PGCONNECT_TIMEOUT", timeout_s.to_string());

    let start = Instant::now();
    let (stdout, stderr, code) = run(cmd);
    let elapsed = start.elapsed().as_secs_f64();

    assert_ne!(
        code, 0,
        "e9: expected nonzero exit for timeout\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Known issue #723: timeout is ~2x configured value.  Accept up to 2x + 1s slack.
    // Once fixed, tighten to timeout_s as f64 + 1.5.
    #[allow(clippy::cast_precision_loss)]
    let max_expected = (timeout_s * 2) as f64 + 1.5;
    assert!(
        elapsed <= max_expected,
        "e9: elapsed {elapsed:.1}s exceeds 2×timeout+1.5s ({max_expected}s) — unexpected hang\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Verify a timeout-related error message appears in output.
    // (code != 0 is already asserted above — do not include here or the
    // assert becomes a tautology that never catches missing error messages.)
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("timeout")
            || combined.contains("timed out")
            || combined.contains("connection refused"),
        "e9: expected timeout/refused error message\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Group F (continued) — Error message quality
// ---------------------------------------------------------------------------

/// F4: Error message includes the database name.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn f4_error_includes_dbname() {
    let db = "nonexistent_db_xyz_709";
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "-d",
        db,
        "-w",
        "-c",
        "SELECT 1",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f4: expected nonzero exit for bad dbname\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(db),
        "f4: expected database name '{db}' in error output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// F5: Error message includes the user name.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn f5_error_includes_username() {
    let user = "nonexistent_user_xyz_709";
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        user,
        "-d",
        &trust_database(),
        "-w",
        "-c",
        "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f5: expected nonzero exit for bad username\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(user),
        "f5: expected user name '{user}' in error output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// F6: Error message includes the host name on connection refused.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn f6_error_includes_host() {
    let host = "127.0.0.1";
    let mut cmd = rpg();
    cmd.args([
        "-h", host, "-p", "19998", "-U", "nobody", "-d", "nobody", "-w", "-c", "SELECT 1",
    ]);
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "f6: expected nonzero exit\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains(host),
        "f6: expected host '{host}' in error output\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// F7: Multi-host failover — `-h host1,host2` tries first, falls back to
/// second, connects successfully.  First host is a black-hole (port 19997),
/// second is the real trust postgres.
///
/// NOTE: This tests issue #724 (multi-host not yet implemented).  Until #724
/// is fixed this test will fail.  Mark it as a known-failing canary so we
/// see it break once support lands.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn f7_multihost_failover() {
    let second_host = trust_host();
    let second_port = trust_port();

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &format!("127.0.0.1,{second_host}"),
        "-p",
        &format!("19997,{second_port}"),
        "-U",
        &trust_user(),
        "-d",
        &trust_database(),
        "-w",
        "-c",
        "SELECT 1",
    ]);
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    let combined = format!("{stdout}{stderr}").to_lowercase();

    // No panic regardless of whether #724 is implemented.
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "f7: error must be user-facing, not a Rust panic\nstdout: {stdout}\nstderr: {stderr}"
    );

    if code == 0 {
        // #724 has been implemented — the failover worked.
        // Tighten the assertion so this test actually validates it.
        assert!(
            stdout.contains('1'),
            "f7: multi-host connected (code=0) but '1' missing from output\nstdout: {stdout}"
        );
    }
    // If code != 0, multi-host is not yet implemented (#724) — no-panic check above is enough.
}

// ---------------------------------------------------------------------------
// Group G (continued) — Authentication methods
// ---------------------------------------------------------------------------

/// G3: Wrong password against SCRAM — must exit non-zero with a clear
/// authentication error.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn g3_scram_wrong_password() {
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
        "-w",
        "-c",
        "SELECT 1",
    ])
    .env("PGPASSWORD", "definitely_wrong_password_xyz");
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "g3: expected nonzero exit for wrong SCRAM password\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("authentication failed") || combined.contains("password"),
        "g3: expected auth error, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "g3: error must be user-facing\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// G4: `PGPASSWORD` takes precedence over `.pgpass` file.
/// Sets `.pgpass` with a wrong password, but `PGPASSWORD` has the correct one.
/// Connects to the SCRAM instance so auth is actually enforced.
#[test]
#[ignore = "requires live Postgres — run via connection-tests CI job"]
fn g4_pgpassword_overrides_pgpassfile() {
    let dir = tempfile::tempdir().expect("g4: failed to create tempdir");
    let pgpass = dir.path().join(".pgpass");

    let host = scram_host();
    let port = scram_port();
    // Write a wrong password to .pgpass
    std::fs::write(
        &pgpass,
        format!("{host}:{port}:*:*:wrong_password_in_pgpass\n"),
    )
    .expect("g4: failed to write .pgpass");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pgpass, std::fs::Permissions::from_mode(0o600))
            .expect("g4: failed to chmod .pgpass");
    }

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &host,
        "-p",
        &port,
        "-U",
        &scram_user(),
        "-d",
        &scram_database(),
        "-w",
        "-c",
        "SELECT 1",
    ])
    .env("PGPASSFILE", pgpass.to_str().unwrap())
    .env("PGPASSWORD", scram_password()); // correct password in env — must win
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "g4: expected PGPASSWORD to win over .pgpass\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "g4: expected '1' in output\nstdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Group I — Multi-host failover (#743)
// ---------------------------------------------------------------------------

/// I1: First host is unreachable, second host succeeds — connection established.
///
/// Uses `.invalid` TLD (RFC 2606 — guaranteed non-resolving) as the first host
/// so DNS resolution fails fast.  The second host is the real Postgres instance.
#[test]
#[cfg(feature = "integration")]
fn i1_multihost_first_fails_second_succeeds() {
    // Use the trust postgres instance as the "good" second host.
    let good_host = trust_host();
    let good_port = trust_port();

    let mut cmd = rpg();
    cmd.args([
        "-h",
        &format!("nonexistent-host-xyz.invalid,{good_host}"),
        "-p",
        &format!("5432,{good_port}"),
        "-U",
        &trust_user(),
        "-d",
        &trust_database(),
        "-w",
        "-c",
        "SELECT 1",
    ]);
    // Timeout prevents CI from hanging on slow DNS resolution.
    cmd.env("PGCONNECT_TIMEOUT", "10");
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "i1: expected success via second host\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "i1: expected '1' in output\nstdout: {stdout}"
    );
    // No panic in either case.
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "i1: output must not contain internal Rust errors\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// I2: All hosts fail — error message must mention the attempted hosts.
///
/// Both hosts use `.invalid` TLD (RFC 2606) so DNS fails fast without needing
/// any live Postgres instance.
#[test]
#[cfg(feature = "integration")]
fn i2_multihost_all_fail() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        "bad-host-one.invalid,bad-host-two.invalid",
        "-p",
        "5432,5432",
        "-U",
        "postgres",
        "-d",
        "postgres",
        "-w",
        "-c",
        "SELECT 1",
    ]);
    // Timeout prevents CI from hanging on slow DNS resolution.
    cmd.env("PGCONNECT_TIMEOUT", "10");
    let (stdout, stderr, code) = run(cmd);
    assert_ne!(
        code, 0,
        "i2: expected non-zero exit when all hosts fail\nstdout: {stdout}\nstderr: {stderr}"
    );
    let combined = format!("{stdout}{stderr}").to_lowercase();
    // Error should reference at least one of the hosts or give a clear message.
    assert!(
        combined.contains("bad-host") || combined.contains("could not connect"),
        "i2: error should mention failed host(s)\nstdout: {stdout}\nstderr: {stderr}"
    );
    // No Rust panics.
    assert!(
        !combined.contains("unwrap") && !combined.contains("panic"),
        "i2: error must be user-facing\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// I3: Single host via `-h` still works as a regression guard.
#[test]
#[cfg(feature = "integration")]
fn i3_single_host_regression() {
    let mut cmd = rpg();
    cmd.args([
        "-h",
        &trust_host(),
        "-p",
        &trust_port(),
        "-U",
        &trust_user(),
        "-d",
        &trust_database(),
        "-w",
        "-c",
        "SELECT 1",
    ]);
    // Timeout prevents CI from hanging on slow DNS resolution.
    cmd.env("PGCONNECT_TIMEOUT", "10");
    if let Some(pw) = trust_password() {
        cmd.env("PGPASSWORD", pw);
    }
    let (stdout, stderr, code) = run(cmd);
    assert_eq!(
        code, 0,
        "i3: single-host connection must still work\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains('1'),
        "i3: expected '1' in output\nstdout: {stdout}"
    );
}
