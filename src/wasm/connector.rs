//! WASM WebSocket transport for tokio-postgres.
//!
//! Connects to a ws-proxy that pipes binary WebSocket frames to a real
//! Postgres TCP socket.  This allows `tokio-postgres` to operate in the
//! browser without Emscripten's POSIX socket emulation.
//!
//! ## Why not `tokio::spawn`?
//!
//! `ws_stream_wasm::WsIo` is `!Send` because it wraps browser APIs that
//! are bound to the main thread.  The connection future must therefore be
//! driven via `wasm_bindgen_futures::spawn_local`, which runs on the
//! single-threaded WASM executor.

use async_io_stream::IoStream;
use tokio_postgres::tls::NoTlsStream;
use tokio_postgres::{Client, Config, Connection, NoTls};
use wasm_bindgen::JsValue;
use ws_stream_wasm::{WsMeta, WsStreamIo};

/// WebSocket-backed connector for `tokio-postgres` in WASM.
///
/// Wraps a WebSocket URL pointing at a `ws-proxy` instance that forwards
/// binary frames to a Postgres TCP socket.
///
/// # Example (conceptual — wired up in `entry.rs`)
///
/// ```ignore
/// let connector = WasmConnector::new("ws://localhost:9091", None);
/// let config = "host=localhost dbname=mydb user=app".parse::<Config>()?;
/// let (client, connection) = connector.connect(&config).await?;
///
/// // Drive the connection on the local executor (WsIo is !Send).
/// wasm_bindgen_futures::spawn_local(async move {
///     if let Err(e) = connection.await {
///         web_sys::console::error_1(&format!("pg connection error: {e}").into());
///     }
/// });
///
/// // Use `client` normally.
/// let rows = client.query("select 1 as n", &[]).await?;
/// ```
pub struct WasmConnector {
    /// WebSocket URL of the ws-proxy (e.g. `ws://127.0.0.1:9091`).
    pub ws_url: String,
    /// Optional auth token sent as the first WebSocket message before Postgres
    /// negotiation begins.  Required when ws-proxy is started with `--token`.
    pub token: Option<String>,
}

impl WasmConnector {
    /// Create a new connector targeting the given WebSocket proxy URL.
    ///
    /// `token` must be `Some(secret)` when the ws-proxy requires authentication
    /// (i.e. started with `--token`/`WS_PROXY_TOKEN`).  Pass `None` for
    /// unauthenticated dev instances.
    pub fn new(ws_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            ws_url: ws_url.into(),
            token,
        }
    }

    /// Connect to Postgres through the WebSocket proxy.
    ///
    /// Returns the `tokio-postgres` client and connection future.  The
    /// caller **must** drive the connection future to completion — see
    /// [`spawn_connection`] for the recommended pattern.
    ///
    /// If a token was provided, it is sent as a JSON auth frame
    /// (`{"token":"..."}`) before any Postgres protocol bytes.
    pub async fn connect(
        &self,
        pg_config: &Config,
    ) -> Result<
        (
            Client,
            Connection<IoStream<WsStreamIo, Vec<u8>>, NoTlsStream>,
        ),
        Box<dyn std::error::Error>,
    > {
        let (_ws_meta, mut ws_stream) = WsMeta::connect(&self.ws_url, None).await?;

        // Send auth frame before Postgres negotiation when a token is configured.
        if let Some(ref tok) = self.token {
            use futures::SinkExt;
            use ws_stream_wasm::WsMessage;
            let auth_frame = format!(r#"{{"token":"{}"}}"#, tok);
            ws_stream.send(WsMessage::Text(auth_frame)).await?;
        }

        let io = ws_stream.into_io();
        let (client, connection) = pg_config.connect_raw(io, NoTls).await?;
        Ok((client, connection))
    }

    /// Connect and automatically spawn the connection driver on the local
    /// WASM executor.  Returns only the client handle.
    ///
    /// This is the convenience method for browser use — it calls
    /// `wasm_bindgen_futures::spawn_local` internally so the caller does
    /// not need to manage the connection future.
    pub async fn connect_spawned(
        &self,
        pg_config: &Config,
    ) -> Result<Client, Box<dyn std::error::Error>> {
        let (client, connection) = self.connect(pg_config).await?;
        spawn_connection(connection);
        Ok(client)
    }
}

/// Spawn the `tokio-postgres` connection driver on the local WASM executor.
///
/// Uses `wasm_bindgen_futures::spawn_local` because `WsIo` is `!Send` and
/// cannot be used with `tokio::spawn`.  Errors are logged to the browser
/// console via `web_sys::console::error_1`.
pub fn spawn_connection(connection: Connection<IoStream<WsStreamIo, Vec<u8>>, NoTlsStream>) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = connection.await {
            web_sys::console::error_1(&format!("pg connection error: {e}").into());
        }
    });
}

/// Convert any `Box<dyn Error>` into a `JsValue` for wasm-bindgen returns.
pub(crate) fn to_js_err(e: Box<dyn std::error::Error>) -> JsValue {
    JsValue::from_str(&e.to_string())
}
