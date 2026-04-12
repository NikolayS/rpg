// Copyright 2026, rpg authors.

//! WASM I/O routing helpers.
//!
//! On `wasm32-unknown-unknown`, Rust's `print!` / `eprint!` macros write to
//! file descriptors 1 and 2 which go nowhere — there is no OS.  This module
//! provides thin wrappers that route output to `web_sys::console` instead,
//! making `\d`, `\l`, error messages, and pager output visible in the browser.

use wasm_bindgen::JsValue;

/// Print to the browser console (replaces stdout in WASM).
pub fn wasm_print(s: &str) {
    web_sys::console::log_1(&JsValue::from_str(s));
}

/// Print an error to the browser console (replaces stderr in WASM).
pub fn wasm_eprint(s: &str) {
    web_sys::console::error_1(&JsValue::from_str(s));
}
