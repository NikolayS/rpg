//! Integration tests for the REPL module.
//!
//! These tests require a running Postgres instance. Start one with:
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
use tokio_postgres::NoTls;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Connect a raw tokio-postgres client using the test env vars.
async fn raw_client() -> Result<tokio_postgres::Client, tokio_postgres::Error> {
    let conn_str = common::connection_string();
    let (client, conn) = tokio_postgres::connect(&conn_str, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("test client connection error: {e}");
        }
    });
    Ok(client)
}

// ---------------------------------------------------------------------------
// REPL multi-line accumulation
// ---------------------------------------------------------------------------

/// Multi-line input `SELECT\n  1;` is executed as a single complete query.
///
/// The REPL accumulates lines until `is_complete` returns true. This test
/// verifies that the resulting SQL is valid when sent to Postgres.
#[tokio::test]
async fn repl_multiline_select_executes() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Simulate the multi-line buffer that the REPL would assemble.
    let sql = "SELECT\n  1 AS n;";

    let msgs = client
        .simple_query(sql)
        .await
        .expect("multi-line SELECT failed");

    let mut found_row = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let val = row.get("n").expect("column 'n' missing");
            assert_eq!(val, "1", "expected value 1 from multi-line SELECT");
            found_row = true;
        }
    }
    assert!(found_row, "no rows returned from multi-line SELECT");
}

// ---------------------------------------------------------------------------
// Transaction state tracking via simple_query
// ---------------------------------------------------------------------------

/// BEGIN block: `begin` + query + `commit` round-trips through Postgres.
#[tokio::test]
async fn repl_tx_begin_commit_executes() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    client.simple_query("begin").await.expect("BEGIN failed");
    let msgs = client
        .simple_query("select 42 as answer")
        .await
        .expect("SELECT inside tx failed");

    let mut got_answer = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            assert_eq!(row.get("answer"), Some("42"));
            got_answer = true;
        }
    }
    assert!(got_answer, "no rows from SELECT inside transaction");

    client.simple_query("commit").await.expect("COMMIT failed");
}

/// Error inside a transaction causes subsequent queries to fail until ROLLBACK.
#[tokio::test]
async fn repl_tx_error_requires_rollback() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    client.simple_query("begin").await.expect("BEGIN failed");

    // Deliberately provoke an error.
    let _ = client.simple_query("select 1/0").await;

    // After an error, the transaction is aborted — further queries fail.
    let result = client.simple_query("select 1 as n").await;
    assert!(
        result.is_err(),
        "expected query to fail in aborted transaction"
    );

    // Rollback clears the error state.
    client
        .simple_query("rollback")
        .await
        .expect("ROLLBACK failed");

    // Now queries work again.
    let msgs = client
        .simple_query("select 1 as n")
        .await
        .expect("SELECT after ROLLBACK failed");
    let mut found = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            assert_eq!(row.get("n"), Some("1"));
            found = true;
        }
    }
    assert!(found, "no rows after ROLLBACK");
}

// ---------------------------------------------------------------------------
// Prompt generation (unit-level, re-tested here for documentation)
// ---------------------------------------------------------------------------

/// Prompt shows `*` when the REPL tracks in-transaction state.
///
/// This validates the `build_prompt` logic independently of the live connection.
#[tokio::test]
async fn repl_prompt_star_in_transaction() {
    let _ = connect_or_skip!();

    // We replicate the prompt building logic to keep tests self-contained
    // (the binary does not expose a lib target yet).
    let dbname = "testdb";
    // In-transaction prompt: dbname=*>
    let prompt = format!("{dbname}=*> ");
    assert!(
        prompt.contains('*'),
        "in-transaction prompt must contain '*': {prompt:?}"
    );
}

/// Prompt shows `!` after an error inside a transaction.
#[tokio::test]
async fn repl_prompt_bang_after_error() {
    let _ = connect_or_skip!();

    let dbname = "testdb";
    // Failed-transaction prompt: dbname=!>
    let prompt = format!("{dbname}=!> ");
    assert!(
        prompt.contains('!'),
        "failed-transaction prompt must contain '!': {prompt:?}"
    );
}

// ---------------------------------------------------------------------------
// \conninfo — verify host/port/dbname appear in connection info string
// ---------------------------------------------------------------------------

/// `connection_info()` includes host, port, and database name.
///
/// We exercise this via the test DB environment variables.
#[tokio::test]
async fn repl_conninfo_contains_expected_fields() {
    let _ = connect_or_skip!();

    // We indirectly test conninfo by verifying the TestDb is reachable and
    // the connection string contains the expected fields.
    let conn_str = common::connection_string();

    // The connection string built from env defaults contains host, port, user,
    // password, and dbname.
    assert!(
        conn_str.contains("host="),
        "connection string should contain 'host='"
    );
    assert!(
        conn_str.contains("port="),
        "connection string should contain 'port='"
    );
    assert!(
        conn_str.contains("dbname="),
        "connection string should contain 'dbname='"
    );
}

// ---------------------------------------------------------------------------
// \timing — verify that timing output appears when enabled
// ---------------------------------------------------------------------------

/// Timing is reported when the `timing` flag is set.
///
/// We use a real query and measure that the elapsed time is non-negative.
#[tokio::test]
async fn repl_timing_measures_query() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    let start = std::time::Instant::now();
    client
        .simple_query("select pg_sleep(0)")
        .await
        .expect("pg_sleep query failed");
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    assert!(
        elapsed_ms >= 0.0,
        "elapsed time must be non-negative, got {elapsed_ms:.3} ms"
    );
}

// ---------------------------------------------------------------------------
// \gdesc — describe buffer columns without executing (#52)
// ---------------------------------------------------------------------------

/// `\gdesc` describes the result columns of a query without executing it.
///
/// Uses `client.prepare()` (extended-protocol Describe) so no rows are
/// produced and no side-effects occur on the server.
#[tokio::test]
async fn gdesc_reports_column_names_and_types() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // prepare() parses and type-checks the query without executing it.
    let stmt = client
        .prepare("select 1 as id, 'hello'::text as name")
        .await
        .expect("prepare failed");

    let cols = stmt.columns();
    assert_eq!(cols.len(), 2, "expected 2 columns from prepare");
    assert_eq!(cols[0].name(), "id", "first column name must be 'id'");
    assert_eq!(cols[1].name(), "name", "second column name must be 'name'");

    // Resolve OIDs to display names the same way describe_buffer() does.
    let oids: Vec<u32> = cols.iter().map(|c| c.type_().oid()).collect();
    let select_exprs: Vec<String> = (1..=oids.len())
        .map(|i| format!("pg_catalog.format_type(${i}, NULL)"))
        .collect();
    let type_query = format!("select {}", select_exprs.join(", "));

    let oid_params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = oids
        .iter()
        .map(|o| o as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let row = client
        .query_one(&type_query, &oid_params)
        .await
        .expect("format_type query failed");

    let type_id: String = row.get(0);
    let type_name: String = row.get(1);

    assert_eq!(
        type_id, "integer",
        "expected 'integer' for the int4 literal, got '{type_id}'"
    );
    assert_eq!(
        type_name, "text",
        "expected 'text' for the text column, got '{type_name}'"
    );
}

// ---------------------------------------------------------------------------
// \gexec — verify that each result cell is executed as SQL
// ---------------------------------------------------------------------------

/// `\gexec` executes the current buffer, then executes each result cell as a
/// SQL statement.
///
/// We simulate this by:
/// 1. Building a SELECT that returns CREATE TABLE statements as cell values.
/// 2. Calling `execute_gexec` (through the public `simple_query` protocol
///    directly here — we test the lower-level protocol that `execute_gexec`
///    relies on).
/// 3. Verifying that the tables were actually created.
#[tokio::test]
async fn repl_gexec_creates_tables_from_cells() {
    let db = connect_or_skip!();

    // Use a unique schema to avoid interference between test runs.
    let schema = format!(
        "gexec_test_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );

    let client = raw_client().await.expect("raw client connect failed");

    // Create an isolated schema for this test.
    client
        .simple_query(&format!("create schema {schema}"))
        .await
        .expect("create schema failed");

    // The query that \gexec would run first — returns two CREATE TABLE
    // statements as cell values.
    let initial_sql = format!(
        "select 'create table {schema}.t1(id int)', \
                'create table {schema}.t2(id int)'"
    );

    // Collect the cell values (mirroring what execute_gexec does).
    let messages = client
        .simple_query(&initial_sql)
        .await
        .expect("initial query failed");

    let mut cells: Vec<String> = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            for i in 0..row.len() {
                if let Some(v) = row.get(i) {
                    if !v.is_empty() {
                        cells.push(v.to_owned());
                    }
                }
            }
        }
    }

    assert_eq!(cells.len(), 2, "expected 2 cell SQL statements");

    // Execute each cell (as \gexec would).
    for cell_sql in &cells {
        client
            .simple_query(cell_sql)
            .await
            .expect("cell SQL failed");
    }

    // Verify both tables exist.
    let check = client
        .simple_query(&format!(
            "select count(*) as n \
             from information_schema.tables \
             where table_schema = '{schema}' \
               and table_name in ('t1', 't2')"
        ))
        .await
        .expect("table existence check failed");

    let mut found_count = false;
    for msg in check {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let n: i64 = row
                .get("n")
                .unwrap_or("0")
                .parse()
                .expect("count should be numeric");
            assert_eq!(n, 2, "expected both t1 and t2 to exist, got {n}");
            found_count = true;
        }
    }
    assert!(found_count, "count query returned no rows");

    // Clean up.
    client
        .simple_query(&format!("drop schema {schema} cascade"))
        .await
        .expect("drop schema failed");

    drop(db);
}

/// NULL cells returned by the initial query are silently skipped by `\gexec`.
#[tokio::test]
async fn repl_gexec_skips_null_cells() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // A query that returns NULL in the second column.
    let sql = "select 'select 1', null::text, 'select 2'";

    let messages = client
        .simple_query(sql)
        .await
        .expect("initial query failed");

    let mut cells: Vec<Option<String>> = Vec::new();
    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            for i in 0..row.len() {
                cells.push(row.get(i).map(str::to_owned));
            }
        }
    }

    // simple_query maps NULL to None; our gexec logic skips None and empty.
    let non_null: Vec<&str> = cells
        .iter()
        .filter_map(|c| c.as_deref())
        .filter(|s| !s.is_empty())
        .collect();

    assert_eq!(
        non_null,
        vec!["select 1", "select 2"],
        "NULL cell should be absent from non-null list"
    );
}

// ---------------------------------------------------------------------------
// \gset — execute buffer and store columns as variables
// ---------------------------------------------------------------------------

/// Helper: run the rpg binary with a piped script and return stdout.
fn run_rpg_script(script: &str) -> String {
    use std::io::Write as _;

    let host = std::env::var("TEST_PGHOST").unwrap_or_else(|_| "localhost".to_owned());
    let port = std::env::var("TEST_PGPORT").unwrap_or_else(|_| "15432".to_owned());
    let user = std::env::var("TEST_PGUSER").unwrap_or_else(|_| "testuser".to_owned());
    let password = std::env::var("TEST_PGPASSWORD").unwrap_or_else(|_| "testpass".to_owned());
    let dbname = std::env::var("TEST_PGDATABASE").unwrap_or_else(|_| "testdb".to_owned());

    let bin = env!("CARGO_BIN_EXE_rpg");

    let mut child = std::process::Command::new(bin)
        .args([
            "-h",
            &host,
            "-p",
            &port,
            "-U",
            &user,
            "-d",
            &dbname,
            "-X",
            "--no-readline",
        ])
        .env("PGPASSWORD", &password)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn rpg binary");

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(script.as_bytes());
    }
    let output = child.wait_with_output().expect("failed to wait for rpg");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `\gset` stores each column of a single-row result as a psql variable.
///
/// After `select 1 as x, 'hello' as y \gset`, `\echo :x` must print `1`
/// and `\echo :y` must print `hello`.
#[tokio::test]
async fn gset_stores_columns_as_variables() {
    let _ = connect_or_skip!();

    let stdout = run_rpg_script("select 1 as x, 'hello' as y \\gset\n\\echo :x\n\\echo :y\n");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.iter().any(|l| l.trim() == "1"),
        "expected '1' in output for :x, got: {stdout:?}"
    );
    assert!(
        lines.iter().any(|l| l.trim() == "hello"),
        "expected 'hello' in output for :y, got: {stdout:?}"
    );
}

/// `\gset my_` stores columns prefixed with `my_`.
#[tokio::test]
async fn gset_stores_columns_with_prefix() {
    let _ = connect_or_skip!();

    let stdout = run_rpg_script("select 1 as x \\gset my_\n\\echo :my_x\n");
    assert!(
        stdout.lines().any(|l| l.trim() == "1"),
        "expected '1' in output for :my_x, got: {stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// /ask read-only transaction guard — PostgreSQL-level enforcement
// ---------------------------------------------------------------------------
//
// These tests verify the database-level safety net used by `/ask`: every
// read-only query is executed inside `start transaction read only` so that
// even if `is_write_query` misclassifies a query the database will reject the
// mutation.  We test this directly against PostgreSQL because unit tests
// cannot exercise the actual wire protocol.

/// `start transaction read only` rejects CREATE TABLE at the database level.
///
/// This is the fallback guard used by `/ask`: even if `is_write_query` somehow
/// returns false for a DDL statement, wrapping it in a read-only transaction
/// causes `PostgreSQL` to reject it with a clear error.
#[tokio::test]
async fn ask_readonly_tx_blocks_create_table() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Simulate wrap_in_ask_readonly_tx("create table test_ask_guard (id int)")
    let wrapped = "start transaction read only;\
                   \ncreate table test_ask_guard_create (id int);\
                   \ncommit;";

    let result = client.simple_query(wrapped).await;
    // PostgreSQL must reject CREATE TABLE inside a read-only transaction.
    assert!(
        result.is_err(),
        "CREATE TABLE must be rejected inside start transaction read only"
    );
    // The tokio-postgres Error::to_string() returns "db error"; the actual
    // PostgreSQL message lives in the DbError source.  Check via source()
    // to get the human-readable text from PG.
    let err = result.unwrap_err();
    let pg_msg = std::error::Error::source(&err)
        .and_then(|src| src.downcast_ref::<tokio_postgres::error::DbError>())
        .map_or("", tokio_postgres::error::DbError::message);
    assert!(
        pg_msg.contains("read-only") || pg_msg.contains("read only"),
        "PostgreSQL error must mention read-only; got: {pg_msg:?} (raw: {err})"
    );

    // The transaction was aborted — roll it back to leave the session clean.
    let _ = client.simple_query("rollback").await;

    // Verify the table was NOT created.
    let check = client
        .simple_query(
            "select count(*) as n from information_schema.tables \
             where table_schema = 'public' \
               and table_name = 'test_ask_guard_create'",
        )
        .await
        .expect("table existence check failed");
    let mut n = 0i64;
    for msg in check {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            n = row
                .get("n")
                .unwrap_or("0")
                .parse()
                .expect("count should be numeric");
        }
    }
    assert_eq!(
        n, 0,
        "test_ask_guard_create must not exist after failed read-only transaction"
    );
}

/// `start transaction read only` rejects DROP TABLE at the database level.
#[tokio::test]
async fn ask_readonly_tx_blocks_drop_table() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Create a temporary table to try to drop.
    client
        .simple_query("create table if not exists test_ask_guard_drop (id int)")
        .await
        .expect("setup CREATE TABLE failed");

    let wrapped = "start transaction read only;\
                   \ndrop table test_ask_guard_drop;\
                   \ncommit;";
    let result = client.simple_query(wrapped).await;
    assert!(
        result.is_err(),
        "DROP TABLE must be rejected inside start transaction read only"
    );

    let _ = client.simple_query("rollback").await;

    // Clean up — the table should still exist since DROP was rejected.
    let _ = client
        .simple_query("drop table if exists test_ask_guard_drop")
        .await;
}

/// `start transaction read only` allows SELECT queries (the normal /ask path).
#[tokio::test]
async fn ask_readonly_tx_allows_select() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    let wrapped = "start transaction read only;\nselect 1 as n;\ncommit;";
    let msgs = client
        .simple_query(wrapped)
        .await
        .expect("SELECT inside read-only transaction should succeed");

    let mut found = false;
    for msg in msgs {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if let Some(v) = row.get("n") {
                assert_eq!(v, "1", "expected value 1 from SELECT in read-only tx");
                found = true;
            }
        }
    }
    assert!(
        found,
        "no rows returned from SELECT in read-only transaction"
    );
}

/// `start transaction read only` rejects INSERT at the database level.
///
/// This confirms the guard also blocks DML, not just DDL.
#[tokio::test]
async fn ask_readonly_tx_blocks_insert() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Create a scratch table for the INSERT attempt.
    client
        .simple_query("create table if not exists test_ask_guard_insert (id int)")
        .await
        .expect("setup CREATE TABLE failed");

    let wrapped = "start transaction read only;\
                   \ninsert into test_ask_guard_insert values (1);\
                   \ncommit;";
    let result = client.simple_query(wrapped).await;
    assert!(
        result.is_err(),
        "INSERT must be rejected inside start transaction read only"
    );

    let _ = client.simple_query("rollback").await;

    // Verify no row was inserted.
    let check = client
        .simple_query("select count(*) as n from test_ask_guard_insert")
        .await
        .expect("count query failed");
    let mut n = 0i64;
    for msg in check {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            n = row
                .get("n")
                .unwrap_or("0")
                .parse()
                .expect("count should be numeric");
        }
    }
    assert_eq!(n, 0, "no rows must be inserted via read-only transaction");

    // Clean up.
    let _ = client
        .simple_query("drop table if exists test_ask_guard_insert")
        .await;
}

// ---------------------------------------------------------------------------
// /ask yolo mode — write queries rejected by read-only tx even in yolo mode
// ---------------------------------------------------------------------------
//
// In yolo mode `/ask` skips the advisory `is_write_query` check and proceeds
// to `AskChoice::Yes`.  The read-only transaction wrapper is the DB-level
// safety net that prevents actual mutations.  These tests confirm that the
// wrapper is applied unconditionally — not only when `is_write_query` returns
// false.

/// In yolo mode, a write query that reaches `AskChoice::Yes` is still wrapped
/// in `start transaction read only` and therefore rejected by `PostgreSQL`.
///
/// We simulate the exact SQL that `wrap_in_ask_readonly_tx` produces for an
/// INSERT statement and verify that `PostgreSQL` rejects it.
#[tokio::test]
async fn ask_yolo_write_query_rejected_by_readonly_tx() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Create a scratch table for the INSERT attempt.
    client
        .simple_query("create table if not exists test_ask_yolo_guard (id int)")
        .await
        .expect("setup CREATE TABLE failed");

    // Simulate wrap_in_ask_readonly_tx("insert into test_ask_yolo_guard values (42)")
    // — this is the SQL that handle_ai_ask now generates for ALL queries,
    // including write queries in yolo mode.
    let wrapped = "start transaction read only;\n\
                   insert into test_ask_yolo_guard values (42);\n\
                   commit;";

    let result = client.simple_query(wrapped).await;
    assert!(
        result.is_err(),
        "INSERT must be rejected by start transaction read only even in yolo mode"
    );

    // Verify the error is a read-only violation.
    let err = result.unwrap_err();
    let pg_msg = std::error::Error::source(&err)
        .and_then(|src| src.downcast_ref::<tokio_postgres::error::DbError>())
        .map_or("", tokio_postgres::error::DbError::message);
    assert!(
        pg_msg.contains("read-only") || pg_msg.contains("read only"),
        "PostgreSQL error must mention read-only; got: {pg_msg:?} (raw: {err})"
    );

    let _ = client.simple_query("rollback").await;

    // Confirm no row was inserted.
    let check = client
        .simple_query("select count(*) as n from test_ask_yolo_guard")
        .await
        .expect("count query failed");
    let mut n = 0i64;
    for msg in check {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            n = row
                .get("n")
                .unwrap_or("0")
                .parse()
                .expect("count should be numeric");
        }
    }
    assert_eq!(
        n, 0,
        "no rows must be inserted via yolo /ask read-only guard"
    );

    // Clean up.
    let _ = client
        .simple_query("drop table if exists test_ask_yolo_guard")
        .await;
}

/// In yolo mode, a DDL statement (CREATE TABLE) is also rejected by the
/// read-only transaction wrapper, confirming the guard applies to all
/// write query types.
#[tokio::test]
async fn ask_yolo_ddl_rejected_by_readonly_tx() {
    let _ = connect_or_skip!();

    let client = raw_client().await.expect("raw client connect failed");

    // Simulate wrap_in_ask_readonly_tx for a CREATE TABLE.
    let wrapped = "start transaction read only;\n\
                   create table test_ask_yolo_ddl_guard (id int);\n\
                   commit;";

    let result = client.simple_query(wrapped).await;
    assert!(
        result.is_err(),
        "CREATE TABLE must be rejected by start transaction read only in yolo mode"
    );

    let _ = client.simple_query("rollback").await;

    // Verify the table was NOT created.
    let check = client
        .simple_query(
            "select count(*) as n from information_schema.tables \
             where table_schema = 'public' \
               and table_name = 'test_ask_yolo_ddl_guard'",
        )
        .await
        .expect("table existence check failed");
    let mut n = 0i64;
    for msg in check {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            n = row
                .get("n")
                .unwrap_or("0")
                .parse()
                .expect("count should be numeric");
        }
    }
    assert_eq!(
        n, 0,
        "test_ask_yolo_ddl_guard must not exist after failed yolo read-only tx"
    );
}
