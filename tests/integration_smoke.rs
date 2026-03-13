//! Smoke tests that verify Samo can connect to a real Postgres instance.
//!
//! These tests require a running Postgres server.  Start one with:
//!
//! ```sh
//! docker compose -f docker-compose.test.yml up -d --wait
//! ```
//!
//! Then run with:
//!
//! ```sh
//! cargo test --features integration
//! ```

#![cfg(feature = "integration")]

mod common;

use common::TestDb;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Macro that skips a test with a human-readable message when the DB is
/// unreachable (e.g. Docker not running locally), rather than panicking.
macro_rules! connect_or_skip {
    () => {
        match TestDb::connect().await {
            Ok(db) => db,
            Err(e) => {
                if std::env::var("CI").is_ok() {
                    panic!("database unreachable in CI — this should not happen: {e}");
                }
                eprintln!(
                    "skipping integration test — cannot connect to test DB: {e}\n\
                     Start Postgres with: \
                     docker compose -f docker-compose.test.yml up -d --wait"
                );
                return;
            }
        }
    };
}

/// Run the `samo` binary with the given arguments, targeting the test DB.
///
/// Returns `(stdout, stderr, exit_code)`.
fn run_samo(extra_args: &[&str]) -> (String, String, i32) {
    let host = std::env::var("TEST_PGHOST").unwrap_or_else(|_| "localhost".to_owned());
    let port = std::env::var("TEST_PGPORT").unwrap_or_else(|_| "15432".to_owned());
    let user = std::env::var("TEST_PGUSER").unwrap_or_else(|_| "testuser".to_owned());
    let password = std::env::var("TEST_PGPASSWORD").unwrap_or_else(|_| "testpass".to_owned());
    let dbname = std::env::var("TEST_PGDATABASE").unwrap_or_else(|_| "testdb".to_owned());

    let bin = env!("CARGO_BIN_EXE_samo");

    let output = std::process::Command::new(bin)
        .args(["-h", &host, "-p", &port, "-U", &user, "-d", &dbname])
        .args(extra_args)
        .env("PGPASSWORD", &password)
        .output()
        .expect("failed to spawn samo binary");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let code = output.status.code().unwrap_or(-1);

    (stdout, stderr, code)
}

// ---------------------------------------------------------------------------
// Existing connectivity tests
// ---------------------------------------------------------------------------

/// Verify basic connectivity: `SELECT 1` must return the integer 1.
#[tokio::test]
async fn smoke_select_one() {
    let db = connect_or_skip!();
    let rows = db.query("select 1 as n").await.expect("select 1 failed");
    assert_eq!(rows.len(), 1, "expected exactly one row");
    let n: i32 = rows[0].get("n");
    assert_eq!(n, 1, "expected value 1");
}

/// Verify that `execute` works for DDL/DML statements.
#[tokio::test]
async fn smoke_execute() {
    let db = connect_or_skip!();
    // set_config is a void-returning function; execute is appropriate here.
    let affected = db
        .execute("select set_config('application_name', 'samo-test', true)")
        .await
        .expect("execute failed");
    // SELECT returns 1 row, execute returns rows affected
    assert_eq!(affected, 1, "expected 1 row affected");
}

/// Verify that the server version is Postgres 16.
#[tokio::test]
async fn smoke_server_version() {
    let db = connect_or_skip!();
    let rows = db
        .query("select current_setting('server_version_num')::int as v")
        .await
        .expect("server_version_num query failed");
    let version: i32 = rows[0].get("v");
    // server_version_num for PG 16.x is 160000–169999
    assert!(
        (160_000..170_000).contains(&version),
        "expected Postgres 16, got server_version_num={version}"
    );
}

/// Load the test schema fixture and run basic queries against it.
#[tokio::test]
async fn smoke_schema_and_data() {
    let db = connect_or_skip!();

    // Clean slate: drop tables if they exist from a previous run.
    db.teardown_schema().await.expect("teardown failed");

    // Apply the fixture schema + seed data.
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    // Verify row counts match the seed data.
    let users = db.query("select count(*) as n from users").await.unwrap();
    let user_count: i64 = users[0].get("n");
    assert_eq!(user_count, 10, "expected 10 seed users, got {user_count}");

    let products = db
        .query("select count(*) as n from products")
        .await
        .unwrap();
    let product_count: i64 = products[0].get("n");
    assert_eq!(
        product_count, 10,
        "expected 10 seed products, got {product_count}"
    );

    let orders = db.query("select count(*) as n from orders").await.unwrap();
    let order_count: i64 = orders[0].get("n");
    assert_eq!(
        order_count, 12,
        "expected 12 seed orders, got {order_count}"
    );

    // Verify a join across tables works.
    let rows = db
        .query(
            "select
                 u.name as user_name,
                 count(o.id) as order_count
             from users as u
             left join orders as o
                 on o.user_id = u.id
             group by
                 u.id,
                 u.name
             order by
                 u.id",
        )
        .await
        .expect("join query failed");
    assert_eq!(rows.len(), 10, "expected 10 rows (one per user)");

    // Teardown to leave DB clean for subsequent runs.
    db.teardown_schema().await.expect("teardown failed");
}

// ---------------------------------------------------------------------------
// Query execution + output formatting tests (issue #19)
// ---------------------------------------------------------------------------

/// `samo -c "select 1"` prints an aligned table with `(1 row)` footer
/// and exits 0.
#[test]
fn query_select_one_aligned_output() {
    let (stdout, _stderr, code) = run_samo(&["-c", "select 1 as n"]);
    assert_eq!(code, 0, "expected exit 0, got {code}\nstdout: {stdout}");
    assert!(
        stdout.contains("(1 row)"),
        "expected '(1 row)' footer in output:\n{stdout}"
    );
    assert!(
        stdout.contains(" n ") || stdout.contains("| n"),
        "expected column header 'n':\n{stdout}"
    );
    assert!(
        stdout.contains(" 1") || stdout.contains("| 1"),
        "expected value '1':\n{stdout}"
    );
}

/// A syntax error exits 1 and prints an error message to stderr.
#[test]
fn query_syntax_error_exits_1() {
    let (stdout, stderr, code) = run_samo(&["-c", "SELEC 1"]);
    assert_eq!(code, 1, "expected exit 1 for syntax error, got {code}");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.to_uppercase().contains("ERROR"),
        "expected ERROR in output:\n{combined}"
    );
}

/// Multi-statement: `select 1; select 2` prints two result sets.
#[test]
fn query_multi_statement() {
    let (stdout, _stderr, code) = run_samo(&["-c", "select 1 as a; select 2 as b"]);
    assert_eq!(
        code, 0,
        "expected exit 0 for multi-statement:\nstdout={stdout}"
    );
    // Should contain both column headers.
    assert!(
        stdout.contains(" a ") || stdout.contains("| a"),
        "missing 'a':\n{stdout}"
    );
    assert!(
        stdout.contains(" b ") || stdout.contains("| b"),
        "missing 'b':\n{stdout}"
    );
}

/// NULL values display as the configured null string (default: empty).
#[test]
fn query_null_display() {
    let (stdout, _stderr, code) = run_samo(&["-c", "select null::text as val"]);
    assert_eq!(code, 0, "expected exit 0:\nstdout={stdout}");
    assert!(stdout.contains("(1 row)"), "expected '(1 row)':\n{stdout}");
}

/// `samo -c "select true, false"` renders booleans as `t` / `f`.
#[test]
fn query_boolean_format() {
    let (stdout, _stderr, code) = run_samo(&["-c", "select true as yes, false as no"]);
    assert_eq!(code, 0, "expected exit 0:\nstdout={stdout}");
    // psql renders booleans as 't' / 'f'
    assert!(
        stdout.contains(" t ") || stdout.contains("| t") || stdout.contains(" t\n"),
        "expected 't' for true:\n{stdout}"
    );
    assert!(
        stdout.contains(" f ") || stdout.contains("| f") || stdout.contains(" f\n"),
        "expected 'f' for false:\n{stdout}"
    );
}

/// A connection failure (bad host) exits with code 2.
#[test]
fn query_connection_failure_exits_2() {
    let bin = env!("CARGO_BIN_EXE_samo");
    let output = std::process::Command::new(bin)
        .args([
            "-h",
            "127.0.0.1",
            "-p",
            "19999", // port nobody is listening on
            "-U",
            "nobody",
            "-d",
            "nobody",
            "-c",
            "select 1",
            "-w", // never prompt for password
        ])
        .output()
        .expect("failed to spawn samo");
    let code = output.status.code().unwrap_or(-1);
    assert_eq!(
        code, 2,
        "expected exit 2 for connection failure, got {code}"
    );
}

// ---------------------------------------------------------------------------
// Describe-family command integration tests (issue #27)
//
// These tests require the test schema fixture to be loaded.
// Each test loads the fixture, runs the command, and tears down.
// ---------------------------------------------------------------------------

/// `\dt` lists tables in the test schema.
#[tokio::test]
async fn describe_dt_lists_tables() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\dt"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\dt should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Should list the users, products, orders tables.
    assert!(
        stdout.contains("users"),
        "\\dt output should contain 'users':\n{stdout}"
    );
    assert!(
        stdout.contains("products"),
        "\\dt output should contain 'products':\n{stdout}"
    );
    assert!(
        stdout.contains("orders"),
        "\\dt output should contain 'orders':\n{stdout}"
    );
    // Should show Schema and Name columns.
    assert!(
        stdout.contains("Schema") || stdout.contains("Name"),
        "\\dt output should have column headers:\n{stdout}"
    );
}

/// `\dt users` filters to a single table by name.
#[tokio::test]
async fn describe_dt_with_pattern() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\dt users"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\dt users should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("users"),
        "\\dt users should list 'users':\n{stdout}"
    );
    assert!(
        !stdout.contains("orders"),
        "\\dt users should not list 'orders':\n{stdout}"
    );
}

/// `\d users` describes the users table columns.
#[tokio::test]
async fn describe_d_table() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\d users"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\d users should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Should show column names.
    assert!(
        stdout.contains("id") && stdout.contains("name") && stdout.contains("email"),
        "\\d users should show column names:\n{stdout}"
    );
    // Should show column types.
    assert!(
        stdout.contains("text") || stdout.contains("bigint") || stdout.contains("integer"),
        "\\d users should show column types:\n{stdout}"
    );
}

/// `\d` (no args) lists all relations.
#[tokio::test]
async fn describe_d_no_args_lists_relations() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\d"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\d should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("users"),
        "\\d should list 'users':\n{stdout}"
    );
}

/// `\di` lists indexes.
#[tokio::test]
async fn describe_di_lists_indexes() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\di"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\di should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The fixture creates orders_user_id_idx, orders_status_idx, etc.
    assert!(
        stdout.contains("orders_user_id_idx") || stdout.contains("index"),
        "\\di should list indexes:\n{stdout}"
    );
}

/// `\dn` lists schemas.
#[tokio::test]
async fn describe_dn_lists_schemas() {
    let (stdout, stderr, code) = run_samo(&["-c", r"\dn"]);
    assert_eq!(
        code, 0,
        "\\dn should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // At minimum, 'public' schema must be visible.
    assert!(
        stdout.contains("public"),
        "\\dn should list 'public' schema:\n{stdout}"
    );
}

/// `\du` lists roles.
#[tokio::test]
async fn describe_du_lists_roles() {
    let (stdout, stderr, code) = run_samo(&["-c", r"\du"]);
    assert_eq!(
        code, 0,
        "\\du should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The test user (testuser) should appear.
    assert!(
        stdout.contains("testuser"),
        "\\du should list test role:\n{stdout}"
    );
}

/// `\l` lists databases.
#[tokio::test]
async fn describe_l_lists_databases() {
    let (stdout, stderr, code) = run_samo(&["-c", r"\l"]);
    assert_eq!(
        code, 0,
        "\\l should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // testdb must appear in the database list.
    assert!(
        stdout.contains("testdb"),
        "\\l should list 'testdb':\n{stdout}"
    );
}

/// `\dt+` shows the Size column in addition to the standard columns.
#[tokio::test]
async fn describe_dt_plus_shows_size() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_samo(&["-c", r"\dt+"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\dt+ should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Size"),
        "\\dt+ should show Size column:\n{stdout}"
    );
}

/// `\dx` lists installed extensions.
#[tokio::test]
async fn describe_dx_lists_extensions() {
    let (stdout, stderr, code) = run_samo(&["-c", r"\dx"]);
    assert_eq!(
        code, 0,
        "\\dx should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Output should at least have the header columns.
    assert!(
        stdout.contains("Name") || stdout.contains("Version"),
        "\\dx should show extension columns:\n{stdout}"
    );
}

/// `\df` lists functions (at minimum the output exits 0).
#[tokio::test]
async fn describe_df_lists_functions() {
    let (stdout, stderr, code) = run_samo(&["-c", r"\df"]);
    assert_eq!(
        code, 0,
        "\\df should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Should have standard function columns.
    assert!(
        stdout.contains("Schema") || stdout.contains("Name") || stdout.contains("rows"),
        "\\df should produce output:\n{stdout}"
    );
}
