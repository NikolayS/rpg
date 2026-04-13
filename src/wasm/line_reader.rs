//! Channel-based line reader for the WASM REPL.
//!
//! JavaScript pushes lines via [`WasmLineSender::push_line`]; Rust reads them
//! via [`WasmLineReader::next_line`].  This replaces `std::io::stdin` which is
//! not available in the browser.

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use wasm_bindgen::prelude::*;

/// JavaScript-facing handle for sending input lines into the Rust REPL.
///
/// Obtain one via [`wasm_line_channel`] and expose it to JS so xterm.js can
/// push user input as the user types.
#[wasm_bindgen]
pub struct WasmLineSender {
    tx: UnboundedSender<Option<String>>,
}

#[wasm_bindgen]
impl WasmLineSender {
    /// Push a line of user input into the REPL.
    ///
    /// Call this from JavaScript whenever the user presses Enter in the
    /// terminal, passing the current input line (without a trailing newline).
    pub fn push_line(&self, line: String) {
        let _ = self.tx.send(Some(line));
    }

    /// Signal EOF (Ctrl-D / terminal closed). The REPL exits cleanly.
    pub fn send_eof(&self) {
        let _ = self.tx.send(None);
    }
}

/// Rust-facing reader end of the line channel.
pub struct WasmLineReader {
    rx: UnboundedReceiver<Option<String>>,
}

impl WasmLineReader {
    /// Wait for the next line of input from JavaScript.
    ///
    /// Returns `None` on EOF (JS called [`WasmLineSender::send_eof`] or the
    /// sender was dropped).
    pub async fn next_line(&mut self) -> Option<String> {
        self.rx.recv().await.flatten()
    }
}

/// Create a linked `(WasmLineSender, WasmLineReader)` pair.
pub fn wasm_line_channel() -> (WasmLineSender, WasmLineReader) {
    let (tx, rx) = unbounded_channel();
    (WasmLineSender { tx }, WasmLineReader { rx })
}
