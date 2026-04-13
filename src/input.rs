//! Platform-agnostic line reader abstraction.
//!
//! Native: backed by rustyline (`NativeLineReader`).
//! WASM:   backed by a channel fed by JS / xterm.js (`WasmLineReader`).
//!
//! The [`LineReader`] trait unifies both behind a single async interface so
//! the REPL loop can be written once and compiled for either target.

use std::path::Path;

// ---------------------------------------------------------------------------
// LineResult
// ---------------------------------------------------------------------------

/// Outcome of a single readline attempt.
pub enum LineResult {
    /// The user entered a complete line.
    Input(String),
    /// End of input (Ctrl-D / channel closed).
    Eof,
    /// User pressed Ctrl-C (interrupt).
    Interrupted,
    /// An unrecoverable error occurred.
    Err(String),
}

// ---------------------------------------------------------------------------
// LineReader trait
// ---------------------------------------------------------------------------

/// Platform-agnostic line reader.
///
/// The `readline` method is async so that the WASM implementation can await
/// messages from a JavaScript-side input channel without blocking.
///
/// Rust does not yet support `async fn` in traits without boxing or the
/// `async_fn_in_trait` feature.  We use the feature gate here (stabilised
/// in Rust 1.75+) and accept that the returned future is not `Send`.
pub trait LineReader {
    /// Read one line from the user.
    ///
    /// `prompt` is displayed to the user (on native) or ignored (on WASM,
    /// where the prompt is rendered by xterm.js).
    async fn readline(&mut self, prompt: &str) -> LineResult;

    /// Add an entry to the in-memory history.
    fn add_history(&mut self, line: &str);

    /// Persist history to disk.  No-op on WASM.
    fn save_history(&mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>>;

    /// Load history from disk.  No-op on WASM.
    fn load_history(&mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>>;
}

// ---------------------------------------------------------------------------
// Native implementation (rustyline)
// ---------------------------------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{LineReader, LineResult, Path};
    use rustyline::error::ReadlineError;
    use rustyline::history::FileHistory;
    use rustyline::{Config, Editor};

    /// Native line reader backed by rustyline.
    ///
    /// The type parameter `H` is the rustyline helper (e.g. `RpgHelper` for
    /// tab completion and syntax highlighting).  Pass `()` for a bare editor
    /// without helpers.
    pub struct NativeLineReader<H: rustyline::Helper> {
        editor: Editor<H, FileHistory>,
    }

    impl<H: rustyline::Helper> NativeLineReader<H> {
        /// Create a new `NativeLineReader` with the given rustyline config
        /// and helper.
        pub fn new(config: Config, helper: H) -> Result<Self, ReadlineError> {
            let mut editor = Editor::with_config(config)?;
            editor.set_helper(Some(helper));
            Ok(Self { editor })
        }

        /// Return a mutable reference to the underlying rustyline `Editor`.
        ///
        /// This is an escape hatch for advanced configuration (binding keys,
        /// registering event handlers, etc.) that the `LineReader` trait does
        /// not expose.
        pub fn editor_mut(&mut self) -> &mut Editor<H, FileHistory> {
            &mut self.editor
        }

        /// Return a reference to the underlying rustyline `Editor`.
        pub fn editor(&self) -> &Editor<H, FileHistory> {
            &self.editor
        }
    }

    impl<H: rustyline::Helper> LineReader for NativeLineReader<H> {
        async fn readline(&mut self, prompt: &str) -> LineResult {
            match self.editor.readline(prompt) {
                Ok(line) => LineResult::Input(line),
                Err(ReadlineError::Interrupted) => LineResult::Interrupted,
                Err(ReadlineError::Eof) => LineResult::Eof,
                Err(e) => LineResult::Err(e.to_string()),
            }
        }

        fn add_history(&mut self, line: &str) {
            let _ = self.editor.add_history_entry(line);
        }

        fn save_history(&mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
            self.editor.save_history(path)?;
            Ok(())
        }

        fn load_history(&mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
            self.editor.load_history(path)?;
            Ok(())
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::NativeLineReader;

// ---------------------------------------------------------------------------
// WASM implementation (channel-based)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::{LineReader, LineResult, Path};

    /// WASM line reader backed by a `tokio::sync::mpsc` channel.
    ///
    /// The JavaScript side (xterm.js or similar) pushes complete lines into
    /// the channel's sender half.  `readline` awaits the next line.
    pub struct WasmLineReader {
        rx: tokio::sync::mpsc::Receiver<String>,
    }

    impl WasmLineReader {
        /// Create a new `WasmLineReader` from the receiving half of a channel.
        pub fn new(rx: tokio::sync::mpsc::Receiver<String>) -> Self {
            Self { rx }
        }
    }

    impl LineReader for WasmLineReader {
        async fn readline(&mut self, _prompt: &str) -> LineResult {
            match self.rx.recv().await {
                Some(line) => LineResult::Input(line),
                None => LineResult::Eof,
            }
        }

        fn add_history(&mut self, _line: &str) {
            // History is managed by the JS side (or not at all).
        }

        fn save_history(&mut self, _path: &Path) -> Result<(), Box<dyn std::error::Error>> {
            // No filesystem on WASM.
            Ok(())
        }

        fn load_history(&mut self, _path: &Path) -> Result<(), Box<dyn std::error::Error>> {
            // No filesystem on WASM.
            Ok(())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::WasmLineReader;
