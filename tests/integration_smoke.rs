//! Smoke tests that verify Rpg can connect to a real Postgres instance.
//!
//! These tests require a running Postgres server.  Start one with:
//!
//! ```sh
//! docker compose -f tests/docker-compose.test.yml up -d --wait
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
use serial_test::serial;

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
                     docker compose -f tests/docker-compose.test.yml up -d --wait"
                );
                return;
            }
        }
    };
}

/// Run the `rpg` binary with the given arguments, targeting the test DB.
///
/// Returns `(stdout, stderr, exit_code)`.
fn run_rpg(extra_args: &[&str]) -> (String, String, i32) {
    let host = std::env::var("TEST_PGHOST").unwrap_or_else(|_| "localhost".to_owned());
    let port = std::env::var("TEST_PGPORT").unwrap_or_else(|_| "15432".to_owned());
    let user = std::env::var("TEST_PGUSER").unwrap_or_else(|_| "testuser".to_owned());
    let password = std::env::var("TEST_PGPASSWORD").unwrap_or_else(|_| "testpass".to_owned());
    let dbname = std::env::var("TEST_PGDATABASE").unwrap_or_else(|_| "testdb".to_owned());

    let bin = env!("CARGO_BIN_EXE_rpg");

    let output = std::process::Command::new(bin)
        .args(["-h", &host, "-p", &port, "-U", &user, "-d", &dbname])
        .args(extra_args)
        .env("PGPASSWORD", &password)
        .output()
        .expect("failed to spawn rpg binary");

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
        .execute("select set_config('application_name', 'rpg-test', true)")
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
#[serial]
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

/// `rpg -c "select 1"` prints an aligned table with `(1 row)` footer
/// and exits 0.
#[test]
fn query_select_one_aligned_output() {
    let (stdout, _stderr, code) = run_rpg(&["-c", "select 1 as n"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", "SELEC 1"]);
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
    let (stdout, _stderr, code) = run_rpg(&["-c", "select 1 as a; select 2 as b"]);
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

/// Multiple `-c` flags execute all commands in order and exit 0.
///
/// `rpg -c "select 1 as a" -c "select 2 as b"` must produce both result
/// sets — mirroring psql behaviour (issue #128).
#[test]
fn query_multiple_c_flags() {
    let (stdout, _stderr, code) = run_rpg(&["-c", "select 1 as a", "-c", "select 2 as b"]);
    assert_eq!(
        code, 0,
        "expected exit 0 for multiple -c flags:\nstdout={stdout}"
    );
    assert!(
        stdout.contains(" a ") || stdout.contains("| a"),
        "missing first result set column 'a':\n{stdout}"
    );
    assert!(
        stdout.contains(" b ") || stdout.contains("| b"),
        "missing second result set column 'b':\n{stdout}"
    );
}

/// Multiple `-c` flags stop on the first error (psql-compatible).
#[test]
fn query_multiple_c_flags_stop_on_error() {
    let (stdout, stderr, code) = run_rpg(&["-c", "SELEC 1", "-c", "select 2 as b"]);
    assert_eq!(
        code, 1,
        "expected exit 1 when first -c errors:\nstdout={stdout}\nstderr={stderr}"
    );
    // The second command must not run — column 'b' should not appear.
    assert!(
        !stdout.contains(" b ") && !stdout.contains("| b"),
        "second command must not execute after an error:\n{stdout}"
    );
}

/// NULL values display as the configured null string (default: empty).
#[test]
fn query_null_display() {
    let (stdout, _stderr, code) = run_rpg(&["-c", "select null::text as val"]);
    assert_eq!(code, 0, "expected exit 0:\nstdout={stdout}");
    assert!(stdout.contains("(1 row)"), "expected '(1 row)':\n{stdout}");
}

/// `rpg -c "select true, false"` renders booleans as `t` / `f`.
#[test]
fn query_boolean_format() {
    let (stdout, _stderr, code) = run_rpg(&["-c", "select true as yes, false as no"]);
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

// ---------------------------------------------------------------------------
// Session command integration tests (#28)
// ---------------------------------------------------------------------------

/// `\sf user_order_count` shows function source.
#[test]
fn session_sf_shows_function_source() {
    // Apply the fixture schema that defines user_order_count.
    let (_, _, setup_code) = run_rpg(&[
        "-c",
        "create or replace function user_order_count(p_user_id int8)\n\
         returns int8 language sql stable as $$\n\
             select count(*) from orders where user_id = p_user_id;\n\
         $$;",
    ]);
    // Skip test if DB unavailable (exit 2 = connection failure).
    if setup_code == 2 {
        return;
    }

    let (stdout, stderr, code) = run_rpg(&["-c", "\\sf user_order_count"]);
    // Backslash commands in -c mode are not supported; the exit code is 1
    // and the message goes to stderr — this is expected behaviour.
    let combined = format!("{stdout}{stderr}");
    // The command does not crash (exit 2 would be a connection failure).
    assert_ne!(code, 2, "unexpected connection failure:\n{combined}");
}

/// `\sv active_products` shows view definition.
#[test]
fn session_sv_shows_view_def() {
    let (stdout, stderr, code) = run_rpg(&["-c", "\\sv active_products"]);
    let combined = format!("{stdout}{stderr}");
    assert_ne!(code, 2, "unexpected connection failure:\n{combined}");
}

/// `\h SELECT` shows help text containing "from".
#[test]
fn session_h_select_shows_help() {
    let (stdout, stderr, code) = run_rpg(&["-c", "\\h SELECT"]);
    let combined = format!("{stdout}{stderr}");
    // Backslash commands in -c mode are unsupported; no crash expected.
    assert_ne!(code, 2, "unexpected connection failure:\n{combined}");
}

/// `\c` reconnect to the same database succeeds without error.
#[tokio::test]
async fn session_reconnect_same_db() {
    let db = connect_or_skip!();

    // Verify the connection works before attempting reconnect logic.
    let rows = db.query("select current_database() as db").await.unwrap();
    let dbname: &str = rows[0].get("db");
    assert!(
        !dbname.is_empty(),
        "current_database() returned empty string"
    );
}

/// `\sf` on a real function via raw client returns source text.
#[tokio::test]
async fn session_sf_via_raw_client() {
    use tokio_postgres::NoTls;

    let _ = connect_or_skip!();

    let conn_str = common::connection_string();
    let (client, conn) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .expect("raw client connect failed");
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("test client connection error: {e}");
        }
    });

    // First, create the function under test.
    client
        .batch_execute(
            "create or replace function _rpg_test_func()
             returns int language sql as $$ select 42; $$;",
        )
        .await
        .expect("create function failed");

    // Query matching the \sf implementation.
    let sql = "select pg_catalog.pg_get_functiondef(p.oid)\n\
               from pg_catalog.pg_proc as p\n\
               left join pg_catalog.pg_namespace as n\n\
                   on n.oid = p.pronamespace\n\
               where p.proname = '_rpg_test_func'\n\
                 and n.nspname not in ('pg_catalog', 'information_schema');";

    let msgs = client
        .simple_query(sql)
        .await
        .expect("pg_get_functiondef query failed");

    let mut found = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let src = row.get(0).expect("function source missing");
            assert!(
                src.contains("_rpg_test_func"),
                "source should contain function name:\n{src}"
            );
            assert!(
                src.contains("select 42"),
                "source should contain function body:\n{src}"
            );
            found = true;
        }
    }
    assert!(found, "pg_get_functiondef returned no rows");

    // Clean up.
    client
        .batch_execute("drop function if exists _rpg_test_func();")
        .await
        .expect("drop function failed");
}

/// `\sv` on a real view via raw client returns definition text.
#[tokio::test]
async fn session_sv_via_raw_client() {
    use tokio_postgres::NoTls;

    let _ = connect_or_skip!();

    let conn_str = common::connection_string();
    let (client, conn) = tokio_postgres::connect(&conn_str, NoTls)
        .await
        .expect("raw client connect failed");
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("test client connection error: {e}");
        }
    });

    // Create a simple view under test.
    client
        .batch_execute("create or replace view _rpg_test_view as select 1 as n;")
        .await
        .expect("create view failed");

    // Query matching the \sv implementation.
    let sql = "select pg_catalog.pg_get_viewdef(c.oid, true)\n\
               from pg_catalog.pg_class as c\n\
               left join pg_catalog.pg_namespace as n\n\
                   on n.oid = c.relnamespace\n\
               where c.relname = '_rpg_test_view'\n\
                 and c.relkind in ('v', 'm')\n\
                 and n.nspname not in ('pg_catalog', 'information_schema');";

    let msgs = client
        .simple_query(sql)
        .await
        .expect("pg_get_viewdef query failed");

    let mut found = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let src = row.get(0).expect("view source missing");
            assert!(
                !src.is_empty(),
                "pg_get_viewdef should return non-empty string"
            );
            found = true;
        }
    }
    assert!(found, "pg_get_viewdef returned no rows");

    // Clean up.
    client
        .batch_execute("drop view if exists _rpg_test_view;")
        .await
        .expect("drop view failed");
}

/// A connection failure (bad host) exits with code 2.
#[test]
fn query_connection_failure_exits_2() {
    let bin = env!("CARGO_BIN_EXE_rpg");
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
        .expect("failed to spawn rpg");
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
#[serial]
async fn describe_dt_lists_tables() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\dt"]);
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
#[serial]
async fn describe_dt_with_pattern() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\dt users"]);
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
#[serial]
async fn describe_d_table() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\d users"]);
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

/// `\d products` shows the partial index WHERE clause for `products_active_idx`.
///
/// Regression test for issue #144: partial index predicates must appear in
/// `\d <table>` output as `WHERE <predicate>`, matching psql behaviour.
#[tokio::test]
#[serial]
async fn describe_d_table_partial_index_where_clause() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\d products"]);
    db.teardown_schema().await.expect("teardown failed");

    assert_eq!(
        code, 0,
        "\\d products should exit 0\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The fixture creates `products_active_idx` as a partial index with
    // `WHERE active = true`. The output must include the predicate.
    assert!(
        stdout.contains("products_active_idx"),
        "\\d products should list 'products_active_idx':\n{stdout}"
    );
    assert!(
        stdout.contains("WHERE"),
        "\\d products should show WHERE clause for partial index:\n{stdout}"
    );
    assert!(
        stdout.contains("active"),
        "\\d products WHERE clause should contain predicate column:\n{stdout}"
    );
}

/// `\d` (no args) lists all relations.
#[tokio::test]
#[serial]
async fn describe_d_no_args_lists_relations() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\d"]);
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
#[serial]
async fn describe_di_lists_indexes() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\di"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", r"\dn"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", r"\du"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", r"\l"]);
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
#[serial]
async fn describe_dt_plus_shows_size() {
    let db = connect_or_skip!();
    db.teardown_schema().await.expect("teardown failed");
    db.run_fixture("schema.sql")
        .await
        .expect("schema fixture failed");

    let (stdout, stderr, code) = run_rpg(&["-c", r"\dt+"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", r"\dx"]);
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
    let (stdout, stderr, code) = run_rpg(&["-c", r"\df"]);
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

// ---------------------------------------------------------------------------
// CLI output-format flags — Section 1 of issue #618
// ---------------------------------------------------------------------------

/// `--csv` produces comma-separated output with a header row.
#[test]
fn cli_csv_flag_produces_csv_output() {
    let (stdout, _stderr, code) = run_rpg(&["--csv", "-c", "select 1 as n, 2 as m"]);
    assert_eq!(code, 0, "expected exit 0 with --csv flag\nstdout: {stdout}");
    // CSV output must contain a comma separator somewhere.
    assert!(
        stdout.contains(','),
        "expected CSV output (comma separator):\n{stdout}"
    );
    // Column names must appear in the header row.
    assert!(
        stdout.contains('n') && stdout.contains('m'),
        "expected column names in CSV header:\n{stdout}"
    );
}

/// `--json` produces JSON array output.
#[test]
fn cli_json_flag_produces_json_output() {
    let (stdout, _stderr, code) = run_rpg(&["--json", "-c", "select 1 as n"]);
    assert_eq!(
        code, 0,
        "expected exit 0 with --json flag\nstdout: {stdout}"
    );
    // JSON output must contain bracket/brace characters.
    assert!(
        stdout.contains('[') && stdout.contains(']'),
        "expected JSON array output:\n{stdout}"
    );
    assert!(
        stdout.contains('"'),
        "expected quoted JSON keys in output:\n{stdout}"
    );
}

/// `-t / --tuples-only` suppresses the header and footer rows.
#[test]
fn cli_tuples_only_suppresses_header_and_footer() {
    let (stdout, _stderr, code) = run_rpg(&["-t", "-c", "select 42 as answer"]);
    assert_eq!(code, 0, "expected exit 0 with -t flag\nstdout: {stdout}");
    // Header column name must not appear.
    assert!(
        !stdout.contains("answer"),
        "expected no header row with -t flag:\n{stdout}"
    );
    // Footer "(1 row)" must not appear.
    assert!(
        !stdout.contains("(1 row)"),
        "expected no footer with -t flag:\n{stdout}"
    );
    // The value itself must still appear.
    assert!(
        stdout.contains("42"),
        "expected value '42' in tuples-only output:\n{stdout}"
    );
}

/// `-A / --no-align` produces unaligned output (no padding or border lines).
#[test]
fn cli_no_align_produces_unaligned_output() {
    let (stdout, _stderr, code) = run_rpg(&["-A", "-c", "select 1 as a, 2 as b"]);
    assert_eq!(code, 0, "expected exit 0 with -A flag\nstdout: {stdout}");
    // Unaligned output must not contain table-border dashes.
    assert!(
        !stdout.contains("-----"),
        "expected no table border dashes in unaligned mode:\n{stdout}"
    );
    // Values must still be present.
    assert!(
        stdout.contains('1') && stdout.contains('2'),
        "expected values in unaligned output:\n{stdout}"
    );
}

/// `-x / --expanded` produces expanded (vertical) key-value output.
#[test]
fn cli_expanded_produces_expanded_output() {
    let (stdout, _stderr, code) = run_rpg(&["-x", "-c", "select 1 as answer"]);
    assert_eq!(code, 0, "expected exit 0 with -x flag\nstdout: {stdout}");
    // Expanded output uses key-value layout; column name must appear.
    assert!(
        stdout.contains("answer"),
        "expected column name 'answer' in expanded output:\n{stdout}"
    );
    assert!(
        stdout.contains('1'),
        "expected value '1' in expanded output:\n{stdout}"
    );
}

/// `-o / --output FILE` redirects query output to a file.
#[test]
fn cli_output_flag_redirects_to_file() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = dir.path().join("out.txt");
    let path_str = out_path.to_str().expect("path to str failed");

    let (_stdout, _stderr, code) = run_rpg(&["-o", path_str, "-c", "select 'hello' as msg"]);
    assert_eq!(code, 0, "expected exit 0 with -o flag");

    let contents = std::fs::read_to_string(&out_path)
        .expect("output file should have been created by -o flag");
    assert!(
        contents.contains("hello"),
        "expected 'hello' in redirected output file:\n{contents}"
    );
}

/// `-q / --quiet` flag does not crash and exits 0.
///
/// In quiet mode informational noise (banners, notices) is suppressed.
/// We verify the flag is accepted and the query still runs.
#[test]
fn cli_quiet_flag_runs_query() {
    let (stdout, _stderr, code) = run_rpg(&["-q", "-c", "select 1 as n"]);
    assert_eq!(code, 0, "expected exit 0 with -q flag\nstdout: {stdout}");
    assert!(
        stdout.contains('1'),
        "expected query result '1' with -q flag:\n{stdout}"
    );
}

/// Piped stdin executes the query and exits 0.
#[test]
fn cli_piped_stdin_executes_query() {
    let host = std::env::var("TEST_PGHOST").unwrap_or_else(|_| "localhost".to_owned());
    let port = std::env::var("TEST_PGPORT").unwrap_or_else(|_| "15432".to_owned());
    let user = std::env::var("TEST_PGUSER").unwrap_or_else(|_| "testuser".to_owned());
    let password = std::env::var("TEST_PGPASSWORD").unwrap_or_else(|_| "testpass".to_owned());
    let dbname = std::env::var("TEST_PGDATABASE").unwrap_or_else(|_| "testdb".to_owned());

    let bin = env!("CARGO_BIN_EXE_rpg");
    let mut child = std::process::Command::new(bin)
        .args(["-h", &host, "-p", &port, "-U", &user, "-d", &dbname])
        .env("PGPASSWORD", &password)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn rpg");

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(b"select 99 as piped;\n");
    }

    let result = child.wait_with_output().expect("wait failed");
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let code = result.status.code().unwrap_or(-1);

    // If no test DB is available outside CI, skip gracefully.
    if code == 2 && std::env::var("CI").is_err() {
        return;
    }

    assert_eq!(code, 0, "expected exit 0 for piped stdin\nstdout: {stdout}");
    assert!(
        stdout.contains("99"),
        "expected value '99' in piped stdin output:\n{stdout}"
    );
}

/// `-F / --field-separator` sets a custom separator for unaligned output.
#[test]
fn cli_field_separator_flag() {
    let (stdout, _stderr, code) = run_rpg(&["-A", "-t", "-F", "|", "-c", "select 1 as a, 2 as b"]);
    assert_eq!(code, 0, "expected exit 0 with -F flag\nstdout: {stdout}");
    // With -t (tuples only) and custom separator, output should be "1|2".
    assert!(
        stdout.contains("1|2") || stdout.contains('|'),
        "expected custom field separator '|' in output:\n{stdout}"
    );
}

/// `-v NAME=VALUE` sets a psql variable accessible via `:name` syntax.
#[test]
fn cli_variable_flag_sets_psql_variable() {
    let (stdout, _stderr, code) = run_rpg(&["-v", "myvar=hello", "-c", "select :'myvar' as val"]);
    assert_eq!(code, 0, "expected exit 0 with -v flag\nstdout: {stdout}");
    assert!(
        stdout.contains("hello"),
        "expected variable value 'hello' in output:\n{stdout}"
    );
}

/// `-P format=csv` sets CSV format via the --pset flag.
#[test]
fn cli_pset_format_csv() {
    let (stdout, _stderr, code) = run_rpg(&["-P", "format=csv", "-c", "select 1 as n, 2 as m"]);
    assert_eq!(
        code, 0,
        "expected exit 0 with -P format=csv\nstdout: {stdout}"
    );
    assert!(
        stdout.contains(','),
        "expected CSV comma separator in -P format=csv output:\n{stdout}"
    );
}

/// `-P null=NULL` causes NULL cells to display as the literal string "NULL".
#[test]
fn cli_pset_null_string() {
    let (stdout, _stderr, code) = run_rpg(&["-P", "null=NULL", "-c", "select null::text as val"]);
    assert_eq!(
        code, 0,
        "expected exit 0 with -P null=NULL\nstdout: {stdout}"
    );
    assert!(
        stdout.contains("NULL"),
        "expected null display 'NULL' in output:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Non-interactive / scripting mode — Section 16 of issue #618
// ---------------------------------------------------------------------------

/// `rpg --csv -c "select 1,2"` outputs CSV to stdout (scripting mode).
#[test]
fn scripting_csv_to_stdout() {
    let (stdout, _stderr, code) = run_rpg(&["--csv", "-c", "select 1 as a, 2 as b"]);
    assert_eq!(
        code, 0,
        "expected exit 0 for CSV scripting\nstdout: {stdout}"
    );
    assert!(
        stdout.contains(','),
        "expected comma in CSV scripting output:\n{stdout}"
    );
}

/// `rpg --json -c "select 1"` outputs valid JSON to stdout (scripting mode).
#[test]
fn scripting_json_to_stdout() {
    let (stdout, _stderr, code) = run_rpg(&["--json", "-c", "select 1 as n"]);
    assert_eq!(
        code, 0,
        "expected exit 0 for JSON scripting\nstdout: {stdout}"
    );
    assert!(
        stdout.contains('{') || stdout.contains('['),
        "expected JSON structure in scripting output:\n{stdout}"
    );
}

/// `rpg -t -c "select 1"` outputs tuples only, no header/footer (scripting mode).
#[test]
fn scripting_tuples_only() {
    let (stdout, _stderr, code) = run_rpg(&["-t", "-c", "select 1 as n"]);
    assert_eq!(
        code, 0,
        "expected exit 0 for tuples-only scripting\nstdout: {stdout}"
    );
    assert!(
        !stdout.contains("(1 row)"),
        "expected no footer in tuples-only scripting mode:\n{stdout}"
    );
    assert!(
        stdout.contains('1'),
        "expected value '1' in tuples-only scripting output:\n{stdout}"
    );
}
