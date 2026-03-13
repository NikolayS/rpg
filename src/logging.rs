//! Structured logging for Samo.
//!
//! Provides a simple, lightweight logging system with configurable
//! log levels, output targets, and structured formatting.
//! Does NOT use the `log` or `tracing` crate — keeps dependencies minimal.

use std::io::Write;
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
// Global logger
// ---------------------------------------------------------------------------

/// Global logger instance. Initialised once via [`init`].
static LOGGER: OnceLock<Arc<Mutex<Logger>>> = OnceLock::new();

/// Internal logger state.
struct Logger {
    /// Maximum level to emit.
    level: Level,
    /// Optional file sink (appended to).
    file: Option<Box<dyn Write + Send>>,
    /// Whether to also write to stderr.
    stderr: bool,
}

/// Initialise the global logger.
///
/// May be called only once; subsequent calls are silently ignored (the
/// `OnceLock` guarantees this).
pub fn init(level: Level, log_file: Option<Box<dyn Write + Send>>) {
    let logger = Logger {
        level,
        file: log_file,
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
    if let Some(ref mut f) = logger.file {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
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
/// Replaces `password=<value>` patterns with `password=***`.
/// The search is case-insensitive.
#[allow(dead_code)]
pub fn mask_credentials(s: &str) -> String {
    let lower = s.to_lowercase();
    if let Some(idx) = lower.find("password=") {
        let prefix = &s[..idx + 9]; // "password=" is 9 bytes
        let rest = &s[idx + 9..];
        let end = rest.find([' ', '&', ';']).unwrap_or(rest.len());
        format!("{prefix}***{}", &rest[end..])
    } else {
        s.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Simple timestamp (no external dependency)
// ---------------------------------------------------------------------------

/// Return a `HH:MM:SS` string based on the current UTC wall clock.
///
/// Avoids pulling in the `chrono` crate.
fn current_time_hms() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
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
}
