//! Full diagnostic report mode — run analyzers, produce detailed output,
//! exit with severity code.
//!
//! Exit codes:
//! - **0** — all analyzers found no issues (healthy)
//! - **1** — at least one Warning-level finding, no Critical findings
//! - **2** — at least one Critical-level finding

use tokio_postgres::Client;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run available analyzers against `client`, print a detailed report to
/// stdout, and return an exit code.
///
/// `format` must be `"text"` (default) or `"json"`.
///
/// - `0` — no findings
/// - `1` — warnings only
/// - `2` — at least one critical finding
pub async fn run_report(client: &Client, format: &str) -> i32 {
    // Detect server version for inclusion in the report.
    let server_version = crate::capabilities::detect_server_version_pub(client)
        .await
        .unwrap_or_else(|| "unknown".to_owned());

    match format {
        "json" => run_report_json(&server_version),
        _ => run_report_text(&server_version),
    }
}

// ---------------------------------------------------------------------------
// Text format
// ---------------------------------------------------------------------------

fn run_report_text(server_version: &str) -> i32 {
    println!("=== Rpg Health Report ===");
    println!();
    println!("PostgreSQL server version: {server_version}");
    println!();
    println!(
        "Note: detailed analyzer reports (vacuum, bloat, index health, etc.) \
         have moved to the autonomous agent component."
    );
    println!();
    println!("Use \\dba in the interactive REPL for diagnostic queries.");
    println!();
    println!("=== Summary ===");
    println!("Analyzers: 0 | Critical: 0 | Warnings: 0 | Clean: 0");

    0
}

// ---------------------------------------------------------------------------
// JSON format
// ---------------------------------------------------------------------------

fn run_report_json(server_version: &str) -> i32 {
    let output = serde_json::json!({
        "server_version": server_version,
        "analyzers": {},
        "summary": {
            "total": 0,
            "critical": 0,
            "warnings": 0,
            "clean": 0,
        },
        "note": "Detailed analyzer reports have moved to the autonomous agent component.",
    });

    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .unwrap_or_else(|e| { format!("{{\"error\": \"{e}\"}}") })
    );

    0
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_report_text_exits_zero() {
        // run_report_text should return 0 (clean).
        let code = run_report_text("16.2");
        assert_eq!(code, 0);
    }

    #[test]
    fn run_report_json_exits_zero() {
        let code = run_report_json("16.2");
        assert_eq!(code, 0);
    }
}
