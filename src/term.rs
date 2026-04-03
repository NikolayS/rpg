//! Terminal size helper with WASM fallback.
//!
//! On native targets, delegates to [`crossterm::terminal::size`].
//! On WASM, returns a sensible default (80×24) since there is no real
//! terminal.

/// Return `(columns, rows)` of the terminal.
///
/// Falls back to `(80, 24)` when the size cannot be determined or when
/// running on WASM.
pub fn terminal_size() -> (u16, u16) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        crossterm::terminal::size().unwrap_or((80, 24))
    }
    #[cfg(target_arch = "wasm32")]
    {
        (80, 24)
    }
}
