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
//! - `app.pgmustard.com`  — pgMustard EXPLAIN analyser (requires API key)
//!
//! After a successful upload the shareable URL is printed and copied to the
//! system clipboard when a suitable clipboard tool is available.

/// Upload `plan_text` to the chosen service and return the shareable URL.
///
/// `service` must be `"depesz"`, `"dalibo"`, or `"pgmustard"`
/// (case-insensitive).  Returns an error string on failure.
///
/// For pgMustard, the API key is resolved from config (via `api_key_env`)
/// or directly from the `PGMUSTARD_API_KEY` environment variable.
/// `plan_json` must be provided for pgMustard — it is the JSON array output
/// of `EXPLAIN (ANALYZE, FORMAT JSON)`.  `query_text` is the original SQL
/// that was explained.
pub async fn share_explain_plan(
    plan_text: &str,
    service: &str,
    cfg: Option<&crate::config::PgMustardConfig>,
    plan_json: Option<&serde_json::Value>,
    query_text: Option<&str>,
) -> Result<String, String> {
    match service {
        "depesz" => upload_depesz(plan_text).await,
        "dalibo" => upload_dalibo(plan_text).await,
        "pgmustard" => {
            let api_key = cfg
                .and_then(crate::config::PgMustardConfig::resolve_api_key)
                .or_else(|| match std::env::var("PGMUSTARD_API_KEY") {
                    Ok(val) if !val.is_empty() => Some(val),
                    _ => None,
                })
                .ok_or_else(|| {
                    "pgMustard API key not found.\n\
                     Set PGMUSTARD_API_KEY in your environment, or add to \
                     ~/.config/rpg/config.toml:\n\
                     \n\
                     [pgmustard]\n\
                     api_key_env = \"PGMUSTARD_API_KEY\""
                        .to_owned()
                })?;
            let json = plan_json.ok_or_else(|| {
                "pgMustard requires JSON plan output.\n\
                 Run an EXPLAIN query first, then use \\explain share pgmustard."
                    .to_owned()
            })?;
            upload_pgmustard(json, query_text.unwrap_or(""), &api_key).await
        }
        other => Err(format!(
            "unknown service \"{other}\"; valid options: depesz, dalibo, pgmustard"
        )),
    }
}

/// POST to explain.depesz.com and return the plan URL.
///
/// The service responds with a 302 redirect to the plan page.  `reqwest` with
/// `redirect::Policy::none()` lets us capture the `Location` header directly
/// instead of following the redirect (which would return HTML).
async fn upload_depesz(plan_text: &str) -> Result<String, String> {
    #[cfg(not(target_arch = "wasm32"))]
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    let client = reqwest::Client::builder()
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
    #[cfg(not(target_arch = "wasm32"))]
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;
    #[cfg(target_arch = "wasm32")]
    let client = reqwest::Client::builder()
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

/// POST to app.pgmustard.com and return the plan URL.
///
/// pgMustard expects a JSON body with `plan` (a JSON array from
/// `EXPLAIN (ANALYZE, FORMAT JSON)`), `query_text`, and `name` fields,
/// plus an `Authorization: Bearer <api_key>` header.  On success the
/// response JSON contains an `explore_url` field with the shareable URL.
async fn upload_pgmustard(
    plan_json: &serde_json::Value,
    query_text: &str,
    api_key: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();

    let payload = serde_json::json!({
        "plan": plan_json,
        "query_text": query_text,
        "name": "rpg"
    });

    let response = client
        .post("https://app.pgmustard.com/api/v1/save")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("request to app.pgmustard.com failed: {e}"))?;

    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "app.pgmustard.com returned {status}: invalid or missing API key.\n\
             Check that PGMUSTARD_API_KEY is set correctly."
        ));
    }

    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "app.pgmustard.com returned unexpected status {status}: {body}"
        ));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("failed to read app.pgmustard.com response: {e}"))?;

    // Parse JSON response — pgMustard returns `explore_url` with the full URL.
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(url) = json.get("explore_url").and_then(|v| v.as_str()) {
            if !url.is_empty() {
                return Ok(url.to_owned());
            }
        }
    }

    Err(format!(
        "app.pgmustard.com returned status {status} \
         but the URL could not be extracted from the response"
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_depesz_url --------------------------------------------------

    #[test]
    fn extract_depesz_url_finds_url_in_href_attribute() {
        let body = r#"<a href="https://explain.depesz.com/s/abc123">view plan</a>"#;
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/abc123".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_finds_bare_url_on_its_own_line() {
        let body = "https://explain.depesz.com/s/xyz789";
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/xyz789".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_stops_at_closing_double_quote() {
        let body = r#"href="https://explain.depesz.com/s/abc123" class="link""#;
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/abc123".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_stops_at_whitespace() {
        let body = "see https://explain.depesz.com/s/abc123 for details";
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/abc123".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_stops_at_single_quote() {
        let body = "href='https://explain.depesz.com/s/abc123' id='x'";
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/abc123".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_returns_none_for_unrelated_content() {
        let body = "<html><p>No Depesz URL here</p></html>";
        assert_eq!(extract_depesz_url(body), None);
    }

    #[test]
    fn extract_depesz_url_returns_none_for_empty_body() {
        assert_eq!(extract_depesz_url(""), None);
    }

    #[test]
    fn extract_depesz_url_returns_none_when_only_domain_without_s_path() {
        // A URL without the /s/ path should not match.
        let body = "https://explain.depesz.com/about";
        assert_eq!(extract_depesz_url(body), None);
    }

    #[test]
    fn extract_depesz_url_picks_first_matching_line() {
        let body = "https://explain.depesz.com/s/first\nhttps://explain.depesz.com/s/second";
        assert_eq!(
            extract_depesz_url(body),
            Some("https://explain.depesz.com/s/first".to_owned()),
        );
    }

    #[test]
    fn extract_depesz_url_handles_url_with_alphanumeric_slug() {
        let body = "https://explain.depesz.com/s/xY3z-abc";
        // The URL continues until whitespace/quote — dash is kept.
        let result = extract_depesz_url(body);
        assert!(result.is_some(), "expected Some, got None");
        assert!(result.unwrap().contains("explain.depesz.com/s/"));
    }

    // -- share_explain_plan (paths that don't require network calls) ---------

    #[test]
    fn share_unknown_service_returns_descriptive_error() {
        // The unknown-service branch returns immediately without network I/O.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = rt.block_on(share_explain_plan(
            "plan text",
            "bogusservice",
            None,
            None,
            None,
        ));
        assert!(result.is_err(), "unknown service must return Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("bogusservice"),
            "error message should name the unknown service: {msg}",
        );
        assert!(
            msg.contains("depesz") && msg.contains("dalibo") && msg.contains("pgmustard"),
            "error should list valid options: {msg}",
        );
    }

    #[test]
    fn share_unknown_service_case_sensitive() {
        // "Depesz" (capitalised) is also rejected: the match is exact.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = rt.block_on(share_explain_plan("plan", "Depesz", None, None, None));
        assert!(result.is_err(), "capitalised service name must return Err");
    }

    #[test]
    fn share_empty_service_name_returns_error() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let result = rt.block_on(share_explain_plan("plan", "", None, None, None));
        assert!(result.is_err(), "empty service name must return Err");
    }
}
