//! WASM support modules for running rpg in the browser.
//!
//! This module is only compiled on `wasm32` targets and provides:
//!
//! - [`connector`] — WebSocket transport for `tokio-postgres`, connecting
//!   through a `ws-proxy` that bridges to a real Postgres TCP socket.
//! - [`entry`] — `wasm-bindgen` entry point exposed to JavaScript.
//!
//! ## Architecture
//!
//! ```text
//! Browser (rpg.wasm)
//!   └─ WasmConnector ──WebSocket──▶ ws-proxy.js ──TCP──▶ PostgreSQL
//! ```
//!
//! The connector uses `ws_stream_wasm` to obtain an `AsyncRead + AsyncWrite`
//! stream from a browser WebSocket, then hands it to `tokio-postgres`'s
//! `connect_raw`.  Because `WsIo` is `!Send`, the connection future must be
//! driven by `wasm_bindgen_futures::spawn_local` rather than `tokio::spawn`.
//!
//! ## Compilation
//!
//! All code is `#[cfg(target_arch = "wasm32")]`-gated — it is invisible on
//! native builds and does not affect `cargo check` / `cargo test`.
//!
//! The required crate dependencies (`ws_stream_wasm`, `wasm-bindgen`,
//! `wasm-bindgen-futures`, `web-sys`, `console_error_panic_hook`) will be
//! added to `Cargo.toml` in Sprint 1.  Until then, these files exist with
//! correct imports but will not be compiled.

#[cfg(target_arch = "wasm32")]
pub mod connector;

#[cfg(target_arch = "wasm32")]
pub mod entry;

#[cfg(target_arch = "wasm32")]
pub mod line_reader;
