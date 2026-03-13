use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    // Embed git commit hash into the binary at compile time.
    // Falls back to "unknown" gracefully when git is unavailable
    // (e.g., CI builds from tarballs or environments without git).
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned());

    println!("cargo:rustc-env=SAMO_GIT_HASH={hash}");

    // Embed the build date (UTC, YYYY-MM-DD format).
    // Uses the SOURCE_DATE_EPOCH env var for reproducible builds when set;
    // otherwise falls back to the current UTC date.
    let build_date = build_date();
    println!("cargo:rustc-env=SAMO_BUILD_DATE={build_date}");

    // Re-run if HEAD changes (branch switch).
    println!("cargo:rerun-if-changed=.git/HEAD");

    // Also watch the ref that HEAD points to (new commits on current branch).
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(ref_path) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{}", ref_path.trim());
        }
    }
}

/// Return the build date as `YYYY-MM-DD` (UTC).
///
/// Respects `SOURCE_DATE_EPOCH` for reproducible builds; falls back to the
/// system clock when the variable is absent or unparseable.
fn build_date() -> String {
    // Try SOURCE_DATE_EPOCH first (seconds since Unix epoch).
    if let Ok(epoch_str) = std::env::var("SOURCE_DATE_EPOCH") {
        if let Ok(epoch) = epoch_str.trim().parse::<i64>() {
            return epoch_to_date(epoch);
        }
    }

    // Fall back to the current UTC time via std::time.
    // UNIX_EPOCH is always before SystemTime::now(), so the subtraction
    // cannot underflow; i64 is sufficient for dates past year 2000.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_i64, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    epoch_to_date(secs)
}

/// Convert Unix epoch seconds to a `YYYY-MM-DD` string (UTC, no external deps).
fn epoch_to_date(epoch: i64) -> String {
    // Days since Unix epoch.
    let days = epoch / 86_400;

    // Gregorian calendar computation (valid for dates after 1970-01-01).
    // Algorithm: civil_from_days — Howard Hinnant, public domain.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}
