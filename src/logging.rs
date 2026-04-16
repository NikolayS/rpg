//! Structured logging for Rpg.
//!
//! Provides a simple, lightweight logging system with configurable
//! log levels, output targets, and structured formatting.
//! Does NOT use the `log` or `tracing` crate — keeps dependencies minimal.
//!
//! # Log rotation
//!
//! When a `log_file` path and non-zero `max_file_size_mb` are provided to
//! [`init`], the logger automatically rotates the active file whenever it
//! would exceed `max_file_size_mb` MiB:
//!
//! - `rpg.log`   → renamed to `rpg.log.1`
//! - `rpg.log.1` → renamed to `rpg.log.2`
//! - …
//! - `rpg.log.{max_files}` → deleted
//!
//! A fresh `rpg.log` is then opened for writing.  Set `max_file_size_mb = 0`
//! to disable rotation entirely.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Log level
// ---------------------------------------------------------------------------

/// Log level (severity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    /// Return the fixed-width uppercase label for this level.
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Parse a level from a string (case-insensitive).
    ///
    /// Returns `None` if the string does not match a known level.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Rotation config
// ---------------------------------------------------------------------------

/// Log-rotation parameters threaded into [`init`].
#[derive(Debug, Clone, Copy)]
pub struct RotationConfig {
    /// Rotate when the active file exceeds this many bytes.  `0` = disabled.
    pub max_bytes: u64,
    /// Maximum number of `.log.N` archives to keep.
    pub max_files: u32,
}

impl RotationConfig {
    /// Build from MiB + file count (the values stored in `LoggingConfig`).
    pub fn from_mb(max_file_size_mb: u32, max_files: u32) -> Self {
        Self {
            max_bytes: u64::from(max_file_size_mb) * 1024 * 1024,
            max_files,
        }
    }

    /// Return `true` if rotation is enabled (non-zero size limit).
    pub fn enabled(self) -> bool {
        self.max_bytes > 0
    }
}

// ---------------------------------------------------------------------------
// Global logger
// ---------------------------------------------------------------------------

/// Global logger instance. Initialised once via [`init`].
static LOGGER: OnceLock<Arc<Mutex<Logger>>> = OnceLock::new();

/// Internal logger state.
struct Logger {
    /// Maximum level to emit.
    level: Level,
    /// File sink — `Some` when `--log-file` was given.
    file: Option<FileSink>,
    /// Whether to also write to stderr.
    stderr: bool,
}

/// Wraps an open log file together with the metadata needed for rotation.
struct FileSink {
    /// Path to the active log file (e.g. `/var/log/rpg/rpg.log`).
    path: PathBuf,
    /// Open handle to `path`.
    writer: Box<dyn Write + Send>,
    /// Bytes written to the current file (approximate; updated after each
    /// write so we avoid a `stat()` on every log line in the common case).
    bytes_written: u64,
    /// Rotation parameters.
    rotation: RotationConfig,
}

impl FileSink {
    /// Write `data` to the file, rotating first if the size threshold would
    /// be exceeded.
    fn write_all(&mut self, data: &[u8]) {
        // Rotate before writing if the file would overflow.
        if self.rotation.enabled()
            && self.bytes_written + data.len() as u64 > self.rotation.max_bytes
        {
            self.rotate();
        }
        if self.writer.write_all(data).is_ok() {
            self.bytes_written += data.len() as u64;
        }
    }

    /// Flush the underlying writer.
    fn flush(&mut self) {
        let _ = self.writer.flush();
    }

    /// Rotate archived files and open a fresh active log.
    ///
    /// ```text
    /// rpg.log.{max_files}   → deleted
    /// rpg.log.{max_files-1} → rpg.log.{max_files}
    /// …
    /// rpg.log.1             → rpg.log.2
    /// rpg.log               → rpg.log.1
    /// (new) rpg.log         opened for writing
    /// ```
    fn rotate(&mut self) {
        // Flush + drop the current writer before renaming.
        let _ = self.writer.flush();

        let max = self.rotation.max_files;

        // Delete the oldest archive if it exists.
        let oldest = numbered_path(&self.path, max);
        if oldest.exists() {
            let _ = fs::remove_file(&oldest);
        }

        // Shift existing archives: N-1 → N, …, 1 → 2.
        for n in (1..max).rev() {
            let src = numbered_path(&self.path, n);
            let dst = numbered_path(&self.path, n + 1);
            if src.exists() {
                let _ = fs::rename(&src, &dst);
            }
        }

        // Rename the active file to .1.
        if self.path.exists() {
            let archive = numbered_path(&self.path, 1);
            let _ = fs::rename(&self.path, &archive);
        }

        // Open a new active file.
        if let Ok(f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.writer = Box::new(f);
            self.bytes_written = 0;
        } else {
            // If we cannot open the new file, fall back to stderr.
            self.writer = Box::new(std::io::stderr());
            self.bytes_written = 0;
        }
    }
}

/// Return the path for archive number `n` (e.g. `rpg.log` → `rpg.log.3`).
fn numbered_path(base: &std::path::Path, n: u32) -> PathBuf {
    let mut s = base.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Public init / set_level
// ---------------------------------------------------------------------------

/// Initialise the global logger (no rotation).
///
/// `log_file` is a pre-opened writer; pass `None` to log to stderr only.
/// May be called only once; subsequent calls are silently ignored.
pub fn init(level: Level, log_file: Option<Box<dyn Write + Send>>) {
    init_with_rotation(level, None, log_file.map(|w| (PathBuf::new(), w)), None);
}

/// Initialise the global logger with optional file rotation.
///
/// - `log_path` — path to the active log file (needed to build archive names).
/// - `rotation`  — rotation config; pass `None` to disable.
///
/// May be called only once; subsequent calls are silently ignored.
pub fn init_rotating(level: Level, log_path: PathBuf, rotation: RotationConfig) {
    // Determine initial bytes written so the first rotation threshold is
    // computed correctly even if the file already exists.
    let existing_bytes = fs::metadata(&log_path).map_or(0, |m| m.len());

    match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(f) => {
            let sink = FileSink {
                path: log_path,
                writer: Box::new(f),
                bytes_written: existing_bytes,
                rotation,
            };
            let logger = Logger {
                level,
                file: Some(sink),
                stderr: true,
            };
            let _ = LOGGER.set(Arc::new(Mutex::new(logger)));
        }
        Err(e) => {
            rpg_eprintln!("rpg: --log-file: {e}");
            std::process::exit(2);
        }
    }
}

/// Internal helper used by both public `init` variants.
fn init_with_rotation(
    level: Level,
    _path: Option<PathBuf>,
    writer: Option<(PathBuf, Box<dyn Write + Send>)>,
    _rotation: Option<RotationConfig>,
) {
    let file = writer.map(|(_p, w)| FileSink {
        path: PathBuf::new(),
        writer: w,
        bytes_written: 0,
        rotation: RotationConfig {
            max_bytes: 0,
            max_files: 0,
        },
    });
    let logger = Logger {
        level,
        file,
        stderr: true,
    };
    let _ = LOGGER.set(Arc::new(Mutex::new(logger)));
}

/// Change the log level at runtime.
///
/// Does nothing if the logger has not been initialised.
pub fn set_level(level: Level) {
    let Some(logger_arc) = LOGGER.get() else {
        return;
    };
    if let Ok(mut logger) = logger_arc.lock() {
        logger.level = level;
    }
}

// ---------------------------------------------------------------------------
// Core log function
// ---------------------------------------------------------------------------

/// Emit a log record.
///
/// Does nothing if the logger has not been initialised or if `level` is
/// above the configured maximum.
pub fn log(level: Level, component: &str, message: &str) {
    let Some(logger_arc) = LOGGER.get() else {
        return;
    };
    let Ok(mut logger) = logger_arc.lock() else {
        return;
    };

    if level > logger.level {
        return;
    }

    let now = current_time_hms();
    let line = format!(
        "[{now}] [{label}] [{component}] {message}\n",
        label = level.label()
    );

    if logger.stderr {
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
    if let Some(ref mut sink) = logger.file {
        sink.write_all(line.as_bytes());
        sink.flush();
    }
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

/// Log at `Error` level.
#[allow(dead_code)]
pub fn error(component: &str, msg: &str) {
    log(Level::Error, component, msg);
}

/// Log at `Warn` level.
#[allow(dead_code)]
pub fn warn(component: &str, msg: &str) {
    log(Level::Warn, component, msg);
}

/// Log at `Info` level.
pub fn info(component: &str, msg: &str) {
    log(Level::Info, component, msg);
}

/// Log at `Debug` level.
pub fn debug(component: &str, msg: &str) {
    log(Level::Debug, component, msg);
}

/// Log at `Trace` level.
pub fn trace(component: &str, msg: &str) {
    log(Level::Trace, component, msg);
}

// ---------------------------------------------------------------------------
// Credential masking
// ---------------------------------------------------------------------------

/// Mask sensitive values in connection strings.
///
/// Handles two formats:
/// - Key-value: `password=<value>` → `password=***` (case-insensitive)
/// - URI: `scheme://user:password@host` → `scheme://user:***@host`
#[allow(dead_code)]
pub fn mask_credentials(s: &str) -> String {
    // First, handle URI-style credentials: scheme://user:password@host
    // Find "://" and then look for ":password@" after it.
    let result = if let Some(scheme_end) = s.find("://") {
        // Everything after "://"
        let after_scheme = &s[scheme_end + 3..];
        // The userinfo is the part before the first '@'
        if let Some(at_pos) = after_scheme.find('@') {
            let userinfo = &after_scheme[..at_pos];
            // Only mask if there's a colon in the userinfo (user:password form)
            if let Some(colon_pos) = userinfo.find(':') {
                // Reconstruct: prefix + scheme + "://" + user + ":***@" + rest
                let prefix = &s[..scheme_end + 3];
                let user = &userinfo[..colon_pos];
                let rest = &after_scheme[at_pos..]; // includes '@'
                format!("{prefix}{user}:***{rest}")
            } else {
                s.to_owned()
            }
        } else {
            s.to_owned()
        }
    } else {
        s.to_owned()
    };

    // Then, handle key=value style: password=<value>
    let lower = result.to_lowercase();
    if let Some(idx) = lower.find("password=") {
        let prefix = &result[..idx + 9]; // "password=" is 9 bytes
        let rest = &result[idx + 9..];
        let end = rest.find([' ', '&', ';']).unwrap_or(rest.len());
        format!("{prefix}***{}", &rest[end..])
    } else {
        result
    }
}

// ---------------------------------------------------------------------------
// Simple timestamp (no external dependency)
// ---------------------------------------------------------------------------

/// Return a `HH:MM:SS` string based on the current UTC wall clock.
///
/// Avoids pulling in the `chrono` crate.
fn current_time_hms() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    #[cfg(target_arch = "wasm32")]
    let duration = web_time::SystemTime::now()
        .duration_since(web_time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Level::from_str ------------------------------------------------------

    #[test]
    fn level_from_str_all_variants() {
        assert_eq!(Level::from_str("error"), Some(Level::Error));
        assert_eq!(Level::from_str("warn"), Some(Level::Warn));
        assert_eq!(Level::from_str("warning"), Some(Level::Warn));
        assert_eq!(Level::from_str("info"), Some(Level::Info));
        assert_eq!(Level::from_str("debug"), Some(Level::Debug));
        assert_eq!(Level::from_str("trace"), Some(Level::Trace));
    }

    #[test]
    fn level_from_str_case_insensitive() {
        assert_eq!(Level::from_str("ERROR"), Some(Level::Error));
        assert_eq!(Level::from_str("Debug"), Some(Level::Debug));
        assert_eq!(Level::from_str("TRACE"), Some(Level::Trace));
    }

    #[test]
    fn level_from_str_unknown_returns_none() {
        assert_eq!(Level::from_str("verbose"), None);
        assert_eq!(Level::from_str(""), None);
        assert_eq!(Level::from_str("fatal"), None);
    }

    // -- Level ordering -------------------------------------------------------

    #[test]
    fn level_ordering_error_lt_warn() {
        assert!(Level::Error < Level::Warn);
    }

    #[test]
    fn level_ordering_warn_lt_info() {
        assert!(Level::Warn < Level::Info);
    }

    #[test]
    fn level_ordering_info_lt_debug() {
        assert!(Level::Info < Level::Debug);
    }

    #[test]
    fn level_ordering_debug_lt_trace() {
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn level_ordering_full_chain() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    // -- mask_credentials -----------------------------------------------------

    #[test]
    fn mask_credentials_masks_password() {
        let input = "host=localhost password=secret dbname=mydb";
        let output = mask_credentials(input);
        assert_eq!(output, "host=localhost password=*** dbname=mydb");
    }

    #[test]
    fn mask_credentials_no_password_unchanged() {
        let input = "host=localhost dbname=mydb user=admin";
        let output = mask_credentials(input);
        assert_eq!(output, input);
    }

    #[test]
    fn mask_credentials_password_at_end() {
        let input = "host=localhost password=topsecret";
        let output = mask_credentials(input);
        assert_eq!(output, "host=localhost password=***");
    }

    #[test]
    fn mask_credentials_password_with_semicolon_sep() {
        let input = "password=abc;host=localhost";
        let output = mask_credentials(input);
        assert_eq!(output, "password=***;host=localhost");
    }

    #[test]
    fn mask_credentials_password_with_ampersand_sep() {
        let input = "user=foo&password=bar&host=db";
        let output = mask_credentials(input);
        assert_eq!(output, "user=foo&password=***&host=db");
    }

    #[test]
    fn mask_credentials_uri_with_password() {
        let input = "postgresql://alice:s3cr3t@db.example.com:5432/mydb";
        let output = mask_credentials(input);
        assert_eq!(output, "postgresql://alice:***@db.example.com:5432/mydb");
    }

    #[test]
    fn mask_credentials_uri_no_password() {
        let input = "postgresql://alice@db.example.com/mydb";
        let output = mask_credentials(input);
        assert_eq!(output, "postgresql://alice@db.example.com/mydb");
    }

    #[test]
    fn mask_credentials_uri_postgres_scheme() {
        let input = "postgres://user:hunter2@localhost/testdb";
        let output = mask_credentials(input);
        assert_eq!(output, "postgres://user:***@localhost/testdb");
    }

    // -- current_time_hms -----------------------------------------------------

    #[test]
    fn current_time_hms_valid_format() {
        let ts = current_time_hms();
        // Must be exactly HH:MM:SS — 8 chars, two colons.
        assert_eq!(ts.len(), 8, "timestamp length should be 8, got: {ts}");
        let parts: Vec<&str> = ts.split(':').collect();
        assert_eq!(parts.len(), 3, "expected 3 colon-separated parts in: {ts}");
        for part in &parts {
            let n: u64 = part.parse().expect("each part should be numeric");
            assert!(n < 60, "each part should be < 60 (got {n} in {ts})");
        }
    }

    // -- numbered_path --------------------------------------------------------

    #[test]
    fn numbered_path_appends_suffix() {
        let base = PathBuf::from("/tmp/rpg.log");
        assert_eq!(numbered_path(&base, 1), PathBuf::from("/tmp/rpg.log.1"));
        assert_eq!(numbered_path(&base, 3), PathBuf::from("/tmp/rpg.log.3"));
    }

    // -- RotationConfig -------------------------------------------------------

    #[test]
    fn rotation_config_from_mb_zero_disabled() {
        let r = RotationConfig::from_mb(0, 5);
        assert!(!r.enabled());
        assert_eq!(r.max_bytes, 0);
    }

    #[test]
    fn rotation_config_from_mb_nonzero_enabled() {
        let r = RotationConfig::from_mb(10, 5);
        assert!(r.enabled());
        assert_eq!(r.max_bytes, 10 * 1024 * 1024);
    }

    // -- FileSink rotation logic ---------------------------------------------

    /// Build a `FileSink` pointing at `path` with the given threshold.
    fn make_sink(path: PathBuf, max_bytes: u64, max_files: u32) -> FileSink {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("open test log file");
        FileSink {
            path,
            writer: Box::new(f),
            bytes_written: 0,
            rotation: RotationConfig {
                max_bytes,
                max_files,
            },
        }
    }

    #[test]
    fn rotation_triggers_at_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("test.log");

        // 20-byte threshold; write 15 bytes first so the next write overflows.
        let mut sink = make_sink(log_path.clone(), 20, 3);
        sink.write_all(b"123456789012345"); // 15 bytes — under threshold
        assert!(log_path.exists());
        assert!(!numbered_path(&log_path, 1).exists());

        sink.write_all(b"0123456789"); // 10 more bytes: 15+10 > 20 → rotate
                                       // After rotation the archive .1 must exist.
        assert!(
            numbered_path(&log_path, 1).exists(),
            "expected .1 archive after rotation"
        );
        // The active file exists and contains the post-rotation write.
        assert!(log_path.exists());
    }

    #[test]
    fn rotation_deletes_oldest_when_max_files_exceeded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("test.log");

        // max_files = 2, threshold = 5 bytes
        let mut sink = make_sink(log_path.clone(), 5, 2);

        // Each write is 6 bytes → triggers a rotation every time.
        for _ in 0..4 {
            sink.write_all(b"AAAAAA"); // 6 bytes > 5 → rotate before write
        }

        // .1 and .2 must exist; .3 must NOT (max_files = 2).
        assert!(numbered_path(&log_path, 1).exists(), ".1 should exist");
        assert!(numbered_path(&log_path, 2).exists(), ".2 should exist");
        assert!(
            !numbered_path(&log_path, 3).exists(),
            ".3 should NOT exist (max_files=2)"
        );
    }

    #[test]
    fn no_rotation_when_disabled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("test.log");

        // max_bytes = 0 → disabled
        let mut sink = make_sink(log_path.clone(), 0, 5);
        // Write lots of data; no archive should ever be created.
        for _ in 0..10 {
            sink.write_all(b"0123456789");
        }
        assert!(!numbered_path(&log_path, 1).exists());
    }
}
