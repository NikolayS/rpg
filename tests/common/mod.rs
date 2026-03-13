//! Shared test helpers for Samo integration tests.
//!
//! Usage:
//! ```rust,ignore
//! mod common;
//! use common::TestDb;
//!
//! #[tokio::test]
//! async fn my_test() {
//!     let db = TestDb::connect().await.unwrap();
//!     db.execute("select 1").await.unwrap();
//! }
//! ```

// Not all helpers are used in every test binary — suppress the warning for
// shared utility code.
#![allow(dead_code)]

use std::env;

use tokio_postgres::{Client, Error, NoTls};

// ---------------------------------------------------------------------------
// Connection defaults (match docker-compose.test.yml)
// ---------------------------------------------------------------------------

const DEFAULT_HOST: &str = "localhost";
const DEFAULT_PORT: &str = "15432";
const DEFAULT_USER: &str = "testuser";
const DEFAULT_PASSWORD: &str = "testpass";
const DEFAULT_DB: &str = "testdb";

/// Returns the connection string built from env vars or compile-time defaults.
///
/// Environment variable overrides:
/// - `TEST_PGHOST`     (default: `localhost`)
/// - `TEST_PGPORT`     (default: `15432`)
/// - `TEST_PGUSER`     (default: `testuser`)
/// - `TEST_PGPASSWORD` (default: `testpass`)
/// - `TEST_PGDATABASE` (default: `testdb`)
pub fn connection_string() -> String {
    let host = env::var("TEST_PGHOST").unwrap_or_else(|_| DEFAULT_HOST.to_owned());
    let port = env::var("TEST_PGPORT").unwrap_or_else(|_| DEFAULT_PORT.to_owned());
    let user = env::var("TEST_PGUSER").unwrap_or_else(|_| DEFAULT_USER.to_owned());
    let password = env::var("TEST_PGPASSWORD").unwrap_or_else(|_| DEFAULT_PASSWORD.to_owned());
    let dbname = env::var("TEST_PGDATABASE").unwrap_or_else(|_| DEFAULT_DB.to_owned());

    format!("host={host} port={port} user={user} password={password} dbname={dbname}")
}

// ---------------------------------------------------------------------------
// TestDb
// ---------------------------------------------------------------------------

/// A thin wrapper around a `tokio_postgres::Client` for integration tests.
pub struct TestDb {
    client: Client,
}

impl TestDb {
    /// Connect to the test database using env vars or docker-compose defaults.
    ///
    /// # Errors
    /// Returns an error if the connection cannot be established.
    pub async fn connect() -> Result<Self, Error> {
        let (client, connection) = tokio_postgres::connect(&connection_string(), NoTls).await?;

        // Drive the connection on a background task.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("test db connection error: {e}");
            }
        });

        Ok(Self { client })
    }

    /// Execute a SQL statement, returning the number of rows affected.
    ///
    /// # Errors
    /// Returns an error if the statement fails.
    pub async fn execute(&self, sql: &str) -> Result<u64, Error> {
        self.client.execute(sql, &[]).await
    }

    /// Query rows and return them as a `Vec` of `tokio_postgres::Row`.
    ///
    /// # Errors
    /// Returns an error if the query fails.
    pub async fn query(&self, sql: &str) -> Result<Vec<tokio_postgres::Row>, Error> {
        self.client.query(sql, &[]).await
    }

    /// Load and execute a SQL file from `tests/fixtures/`.
    ///
    /// The path is resolved relative to the cargo manifest directory at
    /// compile time so it is always correct regardless of the working
    /// directory at test runtime.
    ///
    /// # Errors
    /// Returns an error if reading the file or executing the SQL fails.
    pub async fn run_fixture(&self, name: &str) -> Result<(), anyhow::Error> {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir)
            .join("tests")
            .join("fixtures")
            .join(name);
        let sql = std::fs::read_to_string(&path)?;
        self.client.batch_execute(&sql).await?;
        Ok(())
    }

    /// Drop schema objects created by `schema.sql` so tests can be re-run
    /// idempotently without clearing the entire database.
    ///
    /// # Errors
    /// Returns an error if the teardown SQL fails.
    pub async fn teardown_schema(&self) -> Result<(), Error> {
        self.client
            .batch_execute(
                "drop table if exists orders cascade;
                 drop table if exists products cascade;
                 drop table if exists users cascade;",
            )
            .await
    }
}
