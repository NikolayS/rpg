//! JSON file-backed session persistence for Samo.
//!
//! Stores connection parameters and usage statistics across REPL sessions.
//! The file is kept at `~/.local/share/samo/sessions.json` (XDG data home).
//!
//! The store is a JSON array of [`SessionRecord`] objects. Every mutating
//! operation reads the current file, applies the change in memory, then writes
//! atomically via a temp file + rename so a crash never leaves a corrupt file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Session record
// ---------------------------------------------------------------------------

/// A persisted session record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// JSON file-backed store for session records.
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// Open (or create) the session store at the default XDG data path.
    ///
    /// Creates all intermediate directories as needed.
    ///
    /// # Errors
    /// Returns an error string if the data directory cannot be resolved or
    /// the directory cannot be created.
    pub fn open() -> Result<Self, String> {
        let path = store_path().ok_or_else(|| "cannot resolve data directory".to_owned())?;
        Self::open_at(&path)
    }

    /// Open (or create) the session store at an explicit path.
    ///
    /// Used by unit tests to open a temp-file store.
    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create data directory: {e}"))?;
        }
        Ok(Self { path: path.to_path_buf() })
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Load all records from the JSON file.
    ///
    /// Returns an empty `Vec` if the file does not yet exist.
    fn load(&self) -> Result<Vec<SessionRecord>, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str::<Vec<SessionRecord>>(&text)
                .map_err(|e| format!("cannot parse sessions file: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read sessions file: {e}")),
        }
    }

    /// Persist `records` to the JSON file atomically.
    ///
    /// Writes to `<path>.tmp` then renames into place so a crash during
    /// the write never leaves a corrupt primary file.
    fn save(&self, records: &[SessionRecord]) -> Result<(), String> {
        let json = serde_json::to_string_pretty(records)
            .map_err(|e| format!("cannot serialise sessions: {e}"))?;

        // Build a sibling temp-file path.
        let mut tmp = self.path.clone();
        let file_name = tmp
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sessions.json")
            .to_owned();
        tmp.set_file_name(format!("{file_name}.tmp"));

        std::fs::write(&tmp, &json).map_err(|e| format!("cannot write sessions tmp file: {e}"))?;

        std::fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot rename sessions tmp file: {e}"))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Write operations
    // -----------------------------------------------------------------------

    /// Insert or replace a session record.
    ///
    /// If a record with the same `id` already exists it is replaced in-place;
    /// otherwise the new record is appended.
    pub fn upsert(&self, rec: &SessionRecord) -> Result<(), String> {
        let mut records = self.load()?;
        match records.iter().position(|r| r.id == rec.id) {
            Some(idx) => records[idx] = rec.clone(),
            None => records.push(rec.clone()),
        }
        self.save(&records)
    }

    /// Update `last_used` and `query_count` for an existing session.
    ///
    /// A no-op (not an error) when `id` does not exist.
    #[allow(dead_code)]
    pub fn touch(&self, id: &str, last_used: &str, query_count: u32) -> Result<(), String> {
        let mut records = self.load()?;
        if let Some(rec) = records.iter_mut().find(|r| r.id == id) {
            last_used.clone_into(&mut rec.last_used);
            rec.query_count = query_count;
            self.save(&records)?;
        }
        Ok(())
    }

    /// Set the friendly name for a session (used by `\session save [name]`).
    #[allow(dead_code)]
    pub fn set_name(&self, id: &str, name: &str) -> Result<(), String> {
        let mut records = self.load()?;
        if let Some(rec) = records.iter_mut().find(|r| r.id == id) {
            rec.name = Some(name.to_owned());
            self.save(&records)?;
        }
        Ok(())
    }

    /// Delete a session by id.
    ///
    /// Returns `true` if a record was deleted, `false` if `id` was not found.
    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let mut records = self.load()?;
        let before = records.len();
        records.retain(|r| r.id != id);
        let deleted = records.len() < before;
        if deleted {
            self.save(&records)?;
        }
        Ok(deleted)
    }

    // -----------------------------------------------------------------------
    // Read operations
    // -----------------------------------------------------------------------

    /// Return all sessions ordered by `last_used` descending (most recent first).
    pub fn list(&self) -> Result<Vec<SessionRecord>, String> {
        let mut records = self.load()?;
        records.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        Ok(records)
    }

    /// Look up a single session by id.
    ///
    /// Returns `Ok(None)` when not found.
    pub fn get(&self, id: &str) -> Result<Option<SessionRecord>, String> {
        let records = self.load()?;
        Ok(records.into_iter().find(|r| r.id == id))
    }
}

// ---------------------------------------------------------------------------
// Path helper
// ---------------------------------------------------------------------------

/// Return the path to `~/.local/share/samo/sessions.json`.
///
/// Uses `dirs::data_dir()` which respects `$XDG_DATA_HOME` on Linux and
/// returns the appropriate platform path on macOS and Windows.
pub fn store_path() -> Option<PathBuf> {
    let mut p = dirs::data_dir()?;
    p.push("samo");
    p.push("sessions.json");
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
    use std::path::PathBuf;

    fn tmp_store() -> (SessionStore, PathBuf) {
        // Use a unique temp path per test (process-id + thread-id style).
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "samo_test_sessions_{n}_{}.json",
            std::process::id()
        ));
        let store = SessionStore::open_at(&path).unwrap();
        (store, path)
    }

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
        let (store, path) = tmp_store();
        let rec = make_record("aaa", "localhost", "mydb");
        store.upsert(&rec).unwrap();

        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "aaa");
        assert_eq!(list[0].dbname, Some("mydb".to_owned()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_empty() {
        let (store, path) = tmp_store();
        let list = store.list().unwrap();
        assert!(list.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delete_existing() {
        let (store, path) = tmp_store();
        let rec = make_record("bbb", "localhost", "testdb");
        store.upsert(&rec).unwrap();

        let deleted = store.delete("bbb").unwrap();
        assert!(deleted);

        let list = store.list().unwrap();
        assert!(list.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let (store, path) = tmp_store();
        let deleted = store.delete("doesnotexist").unwrap();
        assert!(!deleted);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn save_with_name() {
        let (store, path) = tmp_store();
        let rec = make_record("ccc", "db.example.com", "prod");
        store.upsert(&rec).unwrap();
        store.set_name("ccc", "production").unwrap();

        let found = store.get("ccc").unwrap().unwrap();
        assert_eq!(found.name, Some("production".to_owned()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn get_not_found() {
        let (store, path) = tmp_store();
        let result = store.get("missing").unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn upsert_updates_existing() {
        let (store, path) = tmp_store();
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
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn touch_updates_fields() {
        let (store, path) = tmp_store();
        let rec = make_record("eee", "localhost", "mydb");
        store.upsert(&rec).unwrap();
        store.touch("eee", "2026-03-14T10:00:00Z", 42).unwrap();

        let found = store.get("eee").unwrap().unwrap();
        assert_eq!(found.last_used, "2026-03-14T10:00:00Z");
        assert_eq!(found.query_count, 42);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_ordered_by_last_used_desc() {
        let (store, path) = tmp_store();
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
        let _ = std::fs::remove_file(path);
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
