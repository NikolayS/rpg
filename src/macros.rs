// Copyright 2026 Nikolay Samokhvalov / postgres.ai
// SPDX-License-Identifier: Apache-2.0

//! Output macros that route to `web_sys::console` on WASM.
//!
//! On native targets these expand to the standard `println!` / `eprintln!` /
//! `print!` / `eprint!` macros with zero overhead.  On `wasm32-unknown-unknown`
//! they format the arguments into a `String` and call the helpers in
//! [`crate::wasm::io`], which emit via `web_sys::console::log_1` (stdout) or
//! `web_sys::console::error_1` (stderr).
//!
//! # Why
//!
//! In `wasm32-unknown-unknown` there is no OS — file descriptors 1 and 2 are
//! sinks.  The standard `println!` macro silently discards output.  These
//! wrappers ensure all user-visible output reaches the browser console (and
//! from there, the xterm.js terminal via the interceptor in `index.html`).

/// Like `println!`, but routes to `web_sys::console::log_1` on WASM.
#[macro_export]
macro_rules! rpg_println {
    () => {
        $crate::rpg_print!("\n")
    };
    ($($arg:tt)*) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            println!($($arg)*)
        }
        #[cfg(target_arch = "wasm32")]
        {
            $crate::wasm::io::wasm_print(&format!($($arg)*))
        }
    }};
}

/// Like `eprintln!`, but routes to `web_sys::console::error_1` on WASM.
#[macro_export]
macro_rules! rpg_eprintln {
    () => {
        $crate::rpg_eprint!("\n")
    };
    ($($arg:tt)*) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            eprintln!($($arg)*)
        }
        #[cfg(target_arch = "wasm32")]
        {
            $crate::wasm::io::wasm_eprint(&format!($($arg)*))
        }
    }};
}

/// Like `print!`, but routes to `web_sys::console::log_1` on WASM.
#[macro_export]
macro_rules! rpg_print {
    ($($arg:tt)*) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            print!($($arg)*)
        }
        #[cfg(target_arch = "wasm32")]
        {
            $crate::wasm::io::wasm_print(&format!($($arg)*))
        }
    }};
}

/// Like `eprint!`, but routes to `web_sys::console::error_1` on WASM.
#[macro_export]
macro_rules! rpg_eprint {
    ($($arg:tt)*) => {{
        #[cfg(not(target_arch = "wasm32"))]
        {
            eprint!($($arg)*)
        }
        #[cfg(target_arch = "wasm32")]
        {
            $crate::wasm::io::wasm_eprint(&format!($($arg)*))
        }
    }};
}
