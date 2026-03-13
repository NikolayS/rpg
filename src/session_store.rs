//! SQLite-backed session persistence for Samo.
//!
//! Stores connection parameters and usage statistics across REPL sessions.
//! The database is kept at `~/.local/share/samo/sessions.db` (XDG data home).
//!
//! Schema (DDL uses lowercase keywords per style guide):
//!
//! ```sql
//! create table if not exists sessions (
//!     id text primary key,
//!     host text,
//!     port integer,
//!     username text,
//!     dbname text,
//!     created_at text not null,
//!     last_used text not null,
//!     query_count integer default 0,
//!     name text
//! )
//! ```

use rusqlite::{params, Connection};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Session record
// ---------------------------------------------------------------------------

/// A persisted session record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Unique session identifier (UUID-like hex string generated at save time).
    pub id: String,
    /// Database server host.
    pub host: Option<String>,
    /// Database server port.
    pub port: Option<u16>,
    /// Database user name.
    pub username: Option<String>,
    /// Database name.
    pub dbname: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// ISO 8601 timestamp of last use.
    pub last_used: String,
    /// Total queries executed in this session.
    pub query_count: u32,
    /// Optional friendly name (set by `\session save [name]`).
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// SQLite-backed store for session records.
pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    /// Open (or create) the session store at the default XDG data path.
    ///
    /// Creates all intermediate directories as needed.
    ///
    /// # Errors
    /// Returns an error string if the data directory cannot be resolved,
    /// the directory cannot be created, or the `SQLite` database cannot be
    /// opened or migrated.
    pub fn open() -> Result<Self, String> {
        let path = db_path().ok_or_else(|| "cannot resolve data directory".to_owned())?;
        Self::open_at(&path)
    }

    /// Open (or create) the session store at an explicit path.
    ///
    /// Used by unit tests to open an in-memory or temp-file database.
    pub fn open_at(path: &PathBuf) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create data directory: {e}"))?;
        }

        let conn =
            Connection::open(path).map_err(|e| format!("cannot open session database: {e}"))?;

        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Open an in-memory `SQLite` database (for tests).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("in-memory db error: {e}"))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Apply the schema migration (idempotent — `create table if not exists`).
    fn migrate(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "create table if not exists sessions (
                    id text primary key,
                    host text,
                    port integer,
                    username text,
                    dbname text,
                    created_at text not null,
                    last_used text not null,
                    query_count integer default 0,
                    name text
                );",
            )
            .map_err(|e| format!("schema migration failed: {e}"))
    }

    // -----------------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------------

    /// Insert or replace a session record.
    ///
    /// Uses `insert or replace` so that re-connecting to the same `id`
    /// (e.g. after a `\session resume`) updates the existing row.
    pub fn upsert(&self, rec: &SessionRecord) -> Result<(), String> {
        self.conn
            .execute(
                "insert or replace into sessions
                    (id, host, port, username, dbname,
                     created_at, last_used, query_count, name)
                 values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    rec.id,
                    rec.host,
                    rec.port.map(u32::from),
                    rec.username,
                    rec.dbname,
                    rec.created_at,
                    rec.last_used,
                    rec.query_count,
                    rec.name,
                ],
            )
            .map(|_| ())
            .map_err(|e| format!("upsert failed: {e}"))
    }

    /// Update `last_used` and `query_count` for an existing session.
    ///
    /// A no-op (not an error) when `id` does not exist.
    #[allow(dead_code)]
    pub fn touch(&self, id: &str, last_used: &str, query_count: u32) -> Result<(), String> {
        self.conn
            .execute(
                "update sessions
                 set last_used = ?1, query_count = ?2
                 where id = ?3",
                params![last_used, query_count, id],
            )
            .map(|_| ())
            .map_err(|e| format!("touch failed: {e}"))
    }

    /// Set the friendly name for a session (used by `\session save [name]`).
    #[allow(dead_code)]
    pub fn set_name(&self, id: &str, name: &str) -> Result<(), String> {
        self.conn
            .execute(
                "update sessions set name = ?1 where id = ?2",
                params![name, id],
            )
            .map(|_| ())
            .map_err(|e| format!("set_name failed: {e}"))
    }

    /// Delete a session by id.
    ///
    /// Returns `true` if a row was deleted, `false` if `id` was not found.
    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let n = self
            .conn
            .execute("delete from sessions where id = ?1", params![id])
            .map_err(|e| format!("delete failed: {e}"))?;
        Ok(n > 0)
    }

    // -----------------------------------------------------------------------
    // Read operations
    // -----------------------------------------------------------------------

    /// Return all sessions ordered by `last_used` descending (most recent first).
    pub fn list(&self) -> Result<Vec<SessionRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "select id, host, port, username, dbname,
                        created_at, last_used, query_count, name
                 from sessions
                 order by last_used desc",
            )
            .map_err(|e| format!("prepare failed: {e}"))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    host: row.get(1)?,
                    port: row
                        .get::<_, Option<u32>>(2)?
                        .map(|p| u16::try_from(p).unwrap_or(5432)),
                    username: row.get(3)?,
                    dbname: row.get(4)?,
                    created_at: row.get(5)?,
                    last_used: row.get(6)?,
                    query_count: row.get::<_, u32>(7).unwrap_or(0),
                    name: row.get(8)?,
                })
            })
            .map_err(|e| format!("query failed: {e}"))?;

        rows.map(|r| r.map_err(|e| format!("row error: {e}")))
            .collect()
    }

    /// Look up a single session by id.
    ///
    /// Returns `Ok(None)` when not found.
    pub fn get(&self, id: &str) -> Result<Option<SessionRecord>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "select id, host, port, username, dbname,
                        created_at, last_used, query_count, name
                 from sessions
                 where id = ?1",
            )
            .map_err(|e| format!("prepare failed: {e}"))?;

        let mut rows = stmt
            .query_map(params![id], |row| {
                Ok(SessionRecord {
                    id: row.get(0)?,
                    host: row.get(1)?,
                    port: row
                        .get::<_, Option<u32>>(2)?
                        .map(|p| u16::try_from(p).unwrap_or(5432)),
                    username: row.get(3)?,
                    dbname: row.get(4)?,
                    created_at: row.get(5)?,
                    last_used: row.get(6)?,
                    query_count: row.get::<_, u32>(7).unwrap_or(0),
                    name: row.get(8)?,
                })
            })
            .map_err(|e| format!("query failed: {e}"))?;

        match rows.next() {
            Some(Ok(rec)) => Ok(Some(rec)),
            Some(Err(e)) => Err(format!("row error: {e}")),
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Path helper
// ---------------------------------------------------------------------------

/// Return the path to `~/.local/share/samo/sessions.db`.
///
/// Uses `dirs::data_dir()` which respects `$XDG_DATA_HOME` on Linux and
/// returns the appropriate platform path on macOS and Windows.
pub fn db_path() -> Option<PathBuf> {
    let mut p = dirs::data_dir()?;
    p.push("samo");
    p.push("sessions.db");
    Some(p)
}

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

/// Generate a simple session ID from the current timestamp and a counter.
///
/// Format: `<unix_secs_hex><counter_hex>` — 16 hex chars total.
/// Not cryptographically random, but unique enough for local session tracking.
pub fn new_session_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{secs:08x}{count:08x}")
}

/// Return the current time as an ISO 8601 string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Avoids the `chrono` crate — computes directly from `SystemTime`.
pub fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Compute date components from Unix timestamp.
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Gregorian date from days since 1970-01-01 (Tomohiko Sakamoto algorithm).
    let (y, mo, d) = days_to_ymd(days_since_epoch);

    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Convert days since the Unix epoch to `(year, month, day)`.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // 400-year Gregorian cycle has 146 097 days.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str, host: &str, dbname: &str) -> SessionRecord {
        SessionRecord {
            id: id.to_owned(),
            host: Some(host.to_owned()),
            port: Some(5432),
            username: Some("alice".to_owned()),
            dbname: Some(dbname.to_owned()),
            created_at: "2026-03-13T00:00:00Z".to_owned(),
            last_used: "2026-03-13T00:00:00Z".to_owned(),
            query_count: 0,
            name: None,
        }
    }

    #[test]
    fn create_and_list() {
        let store = SessionStore::open_in_memory().unwrap();
        let rec = make_record("aaa", "localhost", "mydb");
        store.upsert(&rec).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "aaa");
        assert_eq!(list[0].dbname, Some("mydb".to_owned()));
    }

    #[test]
    fn list_empty() {
        let store = SessionStore::open_in_memory().unwrap();
        let list = store.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn delete_existing() {
        let store = SessionStore::open_in_memory().unwrap();
        let rec = make_record("bbb", "localhost", "testdb");
        store.upsert(&rec).unwrap();

        let deleted = store.delete("bbb").unwrap();
        assert!(deleted);

        let list = store.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let store = SessionStore::open_in_memory().unwrap();
        let deleted = store.delete("doesnotexist").unwrap();
        assert!(!deleted);
    }

    #[test]
    fn save_with_name() {
        let store = SessionStore::open_in_memory().unwrap();
        let rec = make_record("ccc", "db.example.com", "prod");
        store.upsert(&rec).unwrap();
        store.set_name("ccc", "production").unwrap();

        let found = store.get("ccc").unwrap().unwrap();
        assert_eq!(found.name, Some("production".to_owned()));
    }

    #[test]
    fn get_not_found() {
        let store = SessionStore::open_in_memory().unwrap();
        let result = store.get("missing").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn upsert_updates_existing() {
        let store = SessionStore::open_in_memory().unwrap();
        let rec = make_record("ddd", "localhost", "db1");
        store.upsert(&rec).unwrap();

        let updated = SessionRecord {
            id: "ddd".to_owned(),
            dbname: Some("db2".to_owned()),
            query_count: 5,
            ..rec
        };
        store.upsert(&updated).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].dbname, Some("db2".to_owned()));
        assert_eq!(list[0].query_count, 5);
    }

    #[test]
    fn touch_updates_fields() {
        let store = SessionStore::open_in_memory().unwrap();
        let rec = make_record("eee", "localhost", "mydb");
        store.upsert(&rec).unwrap();
        store.touch("eee", "2026-03-14T10:00:00Z", 42).unwrap();

        let found = store.get("eee").unwrap().unwrap();
        assert_eq!(found.last_used, "2026-03-14T10:00:00Z");
        assert_eq!(found.query_count, 42);
    }

    #[test]
    fn list_ordered_by_last_used_desc() {
        let store = SessionStore::open_in_memory().unwrap();
        let mut r1 = make_record("r1", "host1", "db1");
        r1.last_used = "2026-01-01T00:00:00Z".to_owned();
        let mut r2 = make_record("r2", "host2", "db2");
        r2.last_used = "2026-03-01T00:00:00Z".to_owned();
        store.upsert(&r1).unwrap();
        store.upsert(&r2).unwrap();

        let list = store.list().unwrap();
        // Most recently used should be first.
        assert_eq!(list[0].id, "r2");
        assert_eq!(list[1].id, "r1");
    }

    #[test]
    fn now_iso8601_valid_format() {
        let ts = now_iso8601();
        // Must match YYYY-MM-DDTHH:MM:SSZ (20 chars).
        assert_eq!(ts.len(), 20, "timestamp length should be 20, got: {ts}");
        assert!(ts.ends_with('Z'), "timestamp should end with Z: {ts}");
        assert!(ts.contains('T'), "timestamp should contain T: {ts}");
    }

    #[test]
    fn new_session_id_is_16_hex_chars() {
        let id = new_session_id();
        assert_eq!(id.len(), 16, "session id length should be 16, got: {id}");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "session id should be hex: {id}"
        );
    }
}
