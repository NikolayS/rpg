//! WASM entry point for rpg, exposed to JavaScript via `wasm-bindgen`.
//!
//! This module provides [`run_rpg`], the main function called from the
//! browser to start the rpg REPL.  It:
//!
//! 1. Installs a panic hook that routes Rust panics to `console.error`.
//! 2. Connects to Postgres through the [`WasmConnector`] WebSocket transport.
//! 3. Creates a [`WasmLineSender`] and exposes it as `window.rpgLineSender`
//!    so xterm.js can push input lines into the REPL channel.
//! 4. Launches the rpg REPL loop via [`crate::repl::run_repl`].
//!
//! ## JavaScript usage
//!
//! ```javascript
//! import init, { run_rpg } from './pkg/rpg.js';
//!
//! await init();
//! // run_rpg returns once the REPL exits (EOF / \quit).
//! await run_rpg("ws://localhost:9091", "mydb", "myuser", null);
//!
//! // After calling run_rpg, xterm.js keystrokes should call:
//! //   window.rpgLineSender.push_line(line);  // on Enter
//! //   window.rpgLineSender.send_eof();        // on Ctrl-D
//! ```

use wasm_bindgen::prelude::*;

use super::connector::{to_js_err, WasmConnector};
use super::line_reader::wasm_line_channel;

/// Start the rpg terminal in the browser.
///
/// Connects to Postgres via the WebSocket proxy at `ws_url`, then runs the
/// rpg REPL.  Input is read from `window.rpgLineSender` which is set before
/// the REPL loop starts so JS can immediately push lines.
///
/// # Arguments
///
/// * `ws_url` — WebSocket URL of the ws-proxy (e.g. `ws://localhost:9091`).
/// * `initial_db` — Optional database name.
/// * `user` — Optional Postgres user; defaults to `"rpg"` if omitted.
/// * `password` — Optional Postgres password; omit for trust-auth connections.
///
/// # Errors
///
/// Returns a `JsValue` error if the connection fails or the REPL encounters
/// an unrecoverable error.
#[wasm_bindgen]
pub async fn run_rpg(
    ws_url: String,
    initial_db: Option<String>,
    user: Option<String>,
    password: Option<String>,
) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    web_sys::console::log_1(&format!("rpg: connecting to {ws_url}").into());

    // Build a tokio-postgres Config for the connection.
    let mut pg_config = tokio_postgres::Config::new();
    let (ws_host, ws_port) = parse_ws_host_port(&ws_url);
    pg_config.host(&ws_host);
    pg_config.port(ws_port);

    let db = initial_db.clone().unwrap_or_else(|| "postgres".to_owned());
    let pg_user = user.clone().unwrap_or_else(|| "rpg".to_owned());
    pg_config.dbname(&db);
    pg_config.user(&pg_user);
    if let Some(ref pw) = password {
        pg_config.password(pw.as_str());
    }

    let connector = WasmConnector::new(&ws_url, None);
    let client = connector
        .connect_spawned(&pg_config)
        .await
        .map_err(|e| to_js_err(e))?;

    web_sys::console::log_1(&format!("rpg: connected to {db} as {pg_user}").into());

    // Create the input channel and expose the sender to JS.
    let (sender, reader) = wasm_line_channel();
    let js_sender = JsValue::from(sender);
    js_sys::Reflect::set(&js_sys::global(), &"rpgLineSender".into(), &js_sender).map_err(|e| e)?;

    web_sys::console::log_1(&"rpg: ready — type SQL and press Enter; \\q or \\quit to exit".into());

    // Build minimal ConnParams and ReplSettings for the REPL.
    let mut params = crate::connection::ConnParams::default();
    params.host = ws_host;
    params.port = ws_port;
    params.dbname = db;
    params.user = pg_user;
    params.password = password;

    let settings = crate::repl::ReplSettings {
        no_highlight: true,
        config: crate::config::Config::default(),
        ..crate::repl::ReplSettings::default()
    };

    // `reader` is kept alive so the JS sender remains functional; the REPL
    // reads input through its own mechanism.
    let _reader = reader;
    crate::repl::run_repl(client, params, settings, true, true).await;

    web_sys::console::log_1(&"rpg: session ended".into());
    Ok(())
}

/// Extract host and port from a WebSocket URL.
///
/// Parses `ws://host:port/path` or `wss://host:port/path`.
/// Returns `("localhost", 9091)` on parse failure.
fn parse_ws_host_port(url: &str) -> (String, u16) {
    let without_scheme = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .unwrap_or(url);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        let port = port_str.parse::<u16>().unwrap_or(9091);
        (host.to_owned(), port)
    } else {
        (authority.to_owned(), 9091)
    }
}
