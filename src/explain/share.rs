// Copyright 2026 Rpg contributors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Upload EXPLAIN plans to external visualiser services.
//!
//! Supports:
//! - `explain.depesz.com` — classic Hubert Lubaczewski visualiser
//! - `explain.dalibo.com`  — Dalibo's EXPLAIN visualiser
//!
//! After a successful upload the shareable URL is printed and copied to the
//! system clipboard when a suitable clipboard tool is available.

/// Upload `plan_text` to the chosen service and return the shareable URL.
///
/// `service` must be `"depesz"` or `"dalibo"` (case-insensitive).
/// Returns an error string on failure.
pub async fn share_explain_plan(plan_text: &str, service: &str) -> Result<String, String> {
    match service {
        "depesz" => upload_depesz(plan_text).await,
        "dalibo" => upload_dalibo(plan_text).await,
        other => Err(format!(
            "unknown service \"{other}\"; valid options: depesz, dalibo"
        )),
    }
}

/// POST to explain.depesz.com and return the plan URL.
///
/// The service responds with a 302 redirect to the plan page.  `reqwest` with
/// `redirect::Policy::none()` lets us capture the `Location` header directly
/// instead of following the redirect (which would return HTML).
async fn upload_depesz(plan_text: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let response = client
        .post("https://explain.depesz.com/")
        .form(&[
            ("plan", plan_text),
            ("title", "rpg"),
            ("from_version", ""),
            ("explain_format", "text"),
        ])
        .send()
        .await
        .map_err(|e| format!("request to explain.depesz.com failed: {e}"))?;

    // depesz returns 302 on success with Location pointing to the plan.
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if location.is_empty() {
            return Err("explain.depesz.com returned a redirect with no Location header".into());
        }
        // Location may be relative (e.g. "/s/abc123") — make it absolute.
        let url = if location.starts_with("http") {
            location.to_owned()
        } else {
            format!("https://explain.depesz.com{location}")
        };
        return Ok(url);
    }

    // Some versions return 200 with a body containing the URL.
    if status.is_success() {
        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read explain.depesz.com response: {e}"))?;
        // Try to extract the URL from the response body.
        // depesz sometimes embeds the URL in the response HTML.
        if let Some(url) = extract_depesz_url(&body) {
            return Ok(url);
        }
        return Err(format!(
            "explain.depesz.com returned status {status} \
             but the URL could not be extracted from the response"
        ));
    }

    Err(format!(
        "explain.depesz.com returned unexpected status {status}"
    ))
}

/// Extract the plan URL from a depesz response body.
///
/// depesz sometimes returns a 200 with the final URL embedded in the HTML or
/// as a plain text response.  This is a best-effort extraction.
fn extract_depesz_url(body: &str) -> Option<String> {
    // Look for a pattern like `href="https://explain.depesz.com/s/..."`
    // or a bare URL on its own line.
    for line in body.lines() {
        let line = line.trim();
        if line.contains("explain.depesz.com/s/") {
            // Try to find a URL-like token.
            if let Some(start) = line.find("https://explain.depesz.com/s/") {
                let rest = &line[start..];
                let end = rest
                    .find(|c: char| c == '"' || c == '\'' || c.is_whitespace())
                    .unwrap_or(rest.len());
                return Some(rest[..end].to_owned());
            }
        }
    }
    None
}

/// POST to explain.dalibo.com and return the plan URL.
///
/// Dalibo accepts a JSON body with `plan`, `query`, and `title` fields
/// and responds with a 302 redirect to the plan page.  We use
/// `redirect::Policy::none()` to capture the `Location` header directly
/// instead of following the redirect automatically (which would give us
/// the rendered HTML page with status 200, making URL extraction fail).
async fn upload_dalibo(plan_text: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let payload = serde_json::json!({
        "plan": plan_text,
        "title": "rpg",
        "query": ""
    });

    let response = client
        .post("https://explain.dalibo.com/new")
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("request to explain.dalibo.com failed: {e}"))?;

    let status = response.status();

    // Dalibo returns 302 on success with Location pointing to the plan.
    if status.is_redirection() {
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if location.is_empty() {
            return Err("explain.dalibo.com returned a redirect with no Location header".into());
        }
        // Location may be relative (e.g. "/plan/abc123") — make it absolute.
        let url = if location.starts_with("http") {
            location.to_owned()
        } else {
            format!("https://explain.dalibo.com{location}")
        };
        return Ok(url);
    }

    Err(format!(
        "explain.dalibo.com returned unexpected status {status}"
    ))
}

/// Copy `text` to the system clipboard.
///
/// Tries the following tools in order:
/// - macOS: `pbcopy`
/// - Linux (X11): `xclip -selection clipboard`
/// - Linux (Wayland): `wl-clipboard` / `wl-copy`
/// - Linux fallback: `xsel --clipboard --input`
///
/// Fails silently if no clipboard tool is available or the copy fails.
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let candidates: &[&[&str]] = &[&["pbcopy"]];

    #[cfg(not(target_os = "macos"))]
    let candidates: &[&[&str]] = &[
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["xsel", "--clipboard", "--input"],
    ];

    for argv in candidates {
        let (prog, args) = argv.split_first().unwrap();
        let Ok(mut child) = Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // Ignore wait errors — clipboard is best-effort.
        let _ = child.wait();
        return;
    }
}
