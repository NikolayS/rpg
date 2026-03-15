//! Audit log file persistence.
//!
//! Provides append-only JSONL persistence for [`AuditLogEntry`] records,
//! line-by-line loading with corrupt-entry tolerance, and size-based
//! log rotation.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::governance::AuditLogEntry;

/// Default maximum log file size before rotation (10 MiB).
#[allow(dead_code)]
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;

// ---------------------------------------------------------------------------
// persist_entry
// ---------------------------------------------------------------------------

/// Append a single [`AuditLogEntry`] as a JSON line to `path`.
///
/// The file is created if it does not exist.  Each call opens, appends
/// one line, and closes — safe for concurrent writers on the same OS
/// (append mode is atomic for small writes on POSIX).
#[allow(dead_code)]
pub fn persist_entry(path: &Path, entry: &AuditLogEntry) -> io::Result<()> {
    let json =
        serde_json::to_string(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;

    writeln!(file, "{json}")
}

// ---------------------------------------------------------------------------
// load_entries
// ---------------------------------------------------------------------------

/// Read up to `limit` entries from a JSONL audit log file.
///
/// Lines that cannot be parsed as [`AuditLogEntry`] are silently skipped
/// so that a single corrupt record does not prevent loading the rest of
/// the log.  If `limit` is `0`, all entries are returned.  Otherwise the
/// **last** `limit` entries (by file order) are returned.
#[allow(dead_code)]
pub fn load_entries(path: &Path, limit: usize) -> io::Result<Vec<AuditLogEntry>> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let reader = io::BufReader::new(file);
    let mut entries: Vec<AuditLogEntry> = reader
        .lines()
        .filter_map(|line| {
            let line = line.ok()?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str(trimmed).ok()
        })
        .collect();

    if limit > 0 && entries.len() > limit {
        let skip = entries.len() - limit;
        entries.drain(..skip);
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// rotate_if_needed
// ---------------------------------------------------------------------------

/// Rotate the log file if it exceeds `max_bytes`.
///
/// When rotation occurs, the current file is renamed to
/// `<path>.1` (overwriting any previous `.1` backup) and the function
/// returns `true`.  If the file does not exist or is within the size
/// limit, `false` is returned and no files are modified.
#[allow(dead_code)]
pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() > max_bytes => {
            let rotated = path.with_extension("jsonl.1");
            fs::rename(path, rotated)?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::{ActionOutcome, AuditLogEntry, AutonomyLevel, FeatureArea};
    use std::time::{Duration, UNIX_EPOCH};

    fn make_entry(seq: u64) -> AuditLogEntry {
        AuditLogEntry {
            seq,
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seq),
            feature: FeatureArea::Vacuum,
            autonomy_level: AutonomyLevel::Observe,
            action: format!("vacuum analyze public.t{seq}"),
            justification: "dead tuple ratio above threshold".into(),
            outcome: ActionOutcome::Success {
                detail: "ok".into(),
            },
            auditor_note: None,
            verified: None,
        }
    }

    // --- persist_entry -------------------------------------------------------

    #[test]
    fn persist_creates_file_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        assert!(!path.exists());
        persist_entry(&path, &make_entry(0)).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn persist_appends_multiple_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        for i in 0..5u64 {
            persist_entry(&path, &make_entry(i)).unwrap();
        }
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 5);
    }

    #[test]
    fn persist_writes_valid_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let entry = make_entry(42);
        persist_entry(&path, &entry).unwrap();
        let line = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["seq"], 42u64);
    }

    // --- load_entries --------------------------------------------------------

    #[test]
    fn load_nonexistent_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.jsonl");
        let entries = load_entries(&path, 0).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn load_all_entries_no_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        for i in 0..10u64 {
            persist_entry(&path, &make_entry(i)).unwrap();
        }
        let entries = load_entries(&path, 0).unwrap();
        assert_eq!(entries.len(), 10);
    }

    #[test]
    fn load_respects_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        for i in 0..10u64 {
            persist_entry(&path, &make_entry(i)).unwrap();
        }
        let entries = load_entries(&path, 3).unwrap();
        assert_eq!(entries.len(), 3);
        // Should be the LAST 3 entries.
        assert_eq!(entries[0].seq, 7);
        assert_eq!(entries[2].seq, 9);
    }

    #[test]
    fn load_skips_corrupt_lines() {
        use std::io::Write as IoWrite;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        // Write two good entries, one corrupt line, one more good.
        persist_entry(&path, &make_entry(0)).unwrap();
        persist_entry(&path, &make_entry(1)).unwrap();
        // Inject a corrupt line.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{not valid json}}").unwrap();
        drop(f);
        persist_entry(&path, &make_entry(2)).unwrap();

        let entries = load_entries(&path, 0).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].seq, 2);
    }

    #[test]
    fn load_roundtrip_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let original = AuditLogEntry {
            seq: 99,
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_099),
            feature: FeatureArea::IndexHealth,
            autonomy_level: AutonomyLevel::Auto,
            action: "drop index concurrently idx_old".into(),
            justification: "unused for 30 days".into(),
            outcome: ActionOutcome::Vetoed {
                reason: "outside maintenance window".into(),
            },
            auditor_note: Some("retry after 02:00".into()),
            verified: Some(false),
        };
        persist_entry(&path, &original).unwrap();
        let loaded = load_entries(&path, 0).unwrap();
        assert_eq!(loaded.len(), 1);
        let e = &loaded[0];
        assert_eq!(e.seq, original.seq);
        assert_eq!(e.feature, original.feature);
        assert_eq!(e.autonomy_level, original.autonomy_level);
        assert_eq!(e.action, original.action);
        assert_eq!(e.justification, original.justification);
        assert_eq!(e.auditor_note, original.auditor_note);
        assert_eq!(e.verified, original.verified);
    }

    // --- rotate_if_needed ----------------------------------------------------

    #[test]
    fn rotate_returns_false_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let rotated = rotate_if_needed(&path, 1024).unwrap();
        assert!(!rotated);
    }

    #[test]
    fn rotate_returns_false_when_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        persist_entry(&path, &make_entry(0)).unwrap();
        // 10 MiB limit — a single entry is well below.
        let rotated = rotate_if_needed(&path, DEFAULT_MAX_BYTES).unwrap();
        assert!(!rotated);
        assert!(path.exists());
    }

    #[test]
    fn rotate_renames_file_when_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        persist_entry(&path, &make_entry(0)).unwrap();
        // Rotate immediately (limit = 1 byte).
        let rotated = rotate_if_needed(&path, 1).unwrap();
        assert!(rotated);
        assert!(!path.exists(), "original should be gone");
        let backup = path.with_extension("jsonl.1");
        assert!(backup.exists(), "backup should exist");
    }

    #[test]
    fn rotate_overwrites_previous_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        // First rotation.
        persist_entry(&path, &make_entry(0)).unwrap();
        rotate_if_needed(&path, 1).unwrap();
        // Write another entry and rotate again.
        persist_entry(&path, &make_entry(1)).unwrap();
        let rotated = rotate_if_needed(&path, 1).unwrap();
        assert!(rotated);
        // Backup should exist (overwriting previous .1).
        let backup = path.with_extension("jsonl.1");
        assert!(backup.exists());
    }
}
