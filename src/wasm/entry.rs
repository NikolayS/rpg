//! WASM entry point for rpg, exposed to JavaScript via `wasm-bindgen`.
//!
//! This module provides [`run_rpg`], the main function called from the
//! browser to start the rpg REPL.  It:
//!
//! 1. Installs a panic hook that routes Rust panics to `console.error`.
//! 2. Connects to Postgres through the [`WasmConnector`] WebSocket transport.
//! 3. Launches the rpg REPL loop.
//!
//! ## JavaScript usage
//!
//! ```javascript
//! import init, { run_rpg } from './pkg/rpg.js';
//!
//! await init();
//! await run_rpg("ws://localhost:9091", "mydb");
//! ```

use wasm_bindgen::prelude::*;

use super::connector::{to_js_err, WasmConnector};

/// Start the rpg terminal in the browser.
///
/// # Arguments
///
/// * `ws_url` — WebSocket URL of the ws-proxy (e.g. `ws://localhost:9091`).
/// * `initial_db` — Optional database name; overrides the connection string
///   default if provided.
///
/// # Errors
///
/// Returns a `JsValue` error if the connection fails or the REPL encounters
/// an unrecoverable error.
#[wasm_bindgen]
pub async fn run_rpg(ws_url: String, initial_db: Option<String>) -> Result<(), JsValue> {
    // Route Rust panics to console.error for debuggability.
    console_error_panic_hook::set_once();

    web_sys::console::log_1(&format!("rpg: connecting to ws-proxy at {ws_url}").into());

    // Build a tokio-postgres Config.  In a full implementation this would
    // accept a connection string from the browser UI; for now we use a
    // minimal config and let the proxy handle routing.
    let mut pg_config = tokio_postgres::Config::new();
    // Host/port are irrelevant — the ws-proxy already knows the target.
    // We still need to set them to satisfy tokio-postgres's config parser.
    pg_config.host("localhost");
    pg_config.port(5432);
    if let Some(ref db) = initial_db {
        pg_config.dbname(db);
    }

    let connector = WasmConnector::new(&ws_url);
    let _client = connector.connect_spawned(&pg_config).await.map_err(to_js_err)?;

    web_sys::console::log_1(&"rpg: connected to postgres".into());

    // TODO(s1-merge): wire up the rpg REPL loop here.
    //
    // Once Sprint 1 lands, this will:
    //   1. Initialize a WasmLineReader (browser-side input channel).
    //   2. Create the rpg Repl struct with the client.
    //   3. Enter the main REPL loop (repl::run).
    //
    // The WasmLineReader will bridge JavaScript input events (e.g. from an
    // xterm.js terminal) into the Rust async channel that the REPL reads
    // from, replacing rustyline which is not available in WASM.

    // Placeholder — will be replaced once S1 merges the REPL plumbing.
    web_sys::console::warn_1(&"rpg: REPL loop not yet wired (waiting for S1 merge)".into());

    Ok(())
}
