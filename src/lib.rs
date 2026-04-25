// Copyright 2026 Nikolay Samokhvalov / postgres.ai
// SPDX-License-Identifier: Apache-2.0

//! Library entry point for WASM builds.
//!
//! When compiled to `wasm32-unknown-unknown` via `wasm-pack`, this crate
//! exposes [`wasm::entry::run_rpg`] as the JavaScript-callable entry point.
//! On native targets this library is effectively empty — the binary entry
//! point is `src/main.rs`.

// On native targets nothing is compiled from the library crate — the binary
// crate (main.rs) is the only artefact that matters.

// Output macros — must be declared before all other modules so they are in scope.
#[macro_use]
mod macros;

// ---------------------------------------------------------------------------
// Build-time constants and version string (mirrored from main.rs)
// ---------------------------------------------------------------------------

/// Build-time git commit hash injected by `build.rs` (8 hex chars).
#[cfg(target_arch = "wasm32")]
const GIT_HASH: &str = env!("RPG_GIT_HASH");

/// Build-time date (UTC, `YYYY-MM-DD`) injected by `build.rs`.
#[cfg(target_arch = "wasm32")]
const BUILD_DATE: &str = env!("RPG_BUILD_DATE");

/// Number of commits since the last version tag, injected by `build.rs`.
/// Zero when this commit is exactly the tagged release.
#[cfg(target_arch = "wasm32")]
const COMMITS_SINCE_TAG: u32 = {
    match option_env!("RPG_COMMITS_SINCE_TAG") {
        Some(s) => {
            // const-compatible decimal parse
            let bytes = s.as_bytes();
            let mut n: u32 = 0;
            let mut i = 0;
            while i < bytes.len() {
                n = n * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            n
        }
        None => 0,
    }
};

/// One-line version string: `rpg 0.2.0 (abc1234, built 2026-03-13)`.
#[cfg(target_arch = "wasm32")]
pub fn version_string() -> &'static str {
    Box::leak(
        if COMMITS_SINCE_TAG == 0 {
            format!(
                "rpg {} ({}, built {})",
                env!("CARGO_PKG_VERSION"),
                GIT_HASH,
                BUILD_DATE,
            )
        } else {
            format!(
                "rpg {}+{}-{} ({}, built {})",
                env!("CARGO_PKG_VERSION"),
                COMMITS_SINCE_TAG,
                GIT_HASH,
                GIT_HASH,
                BUILD_DATE,
            )
        }
        .into_boxed_str(),
    )
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod ai;
#[cfg(target_arch = "wasm32")]
mod capabilities;
#[cfg(target_arch = "wasm32")]
mod compat;
#[cfg(target_arch = "wasm32")]
mod conditional;
#[cfg(target_arch = "wasm32")]
mod config;
#[cfg(target_arch = "wasm32")]
mod connection;
#[cfg(target_arch = "wasm32")]
mod copy;
#[cfg(target_arch = "wasm32")]
mod crosstab;
#[cfg(target_arch = "wasm32")]
mod dba;
#[cfg(target_arch = "wasm32")]
mod describe;
#[cfg(target_arch = "wasm32")]
mod explain;
#[cfg(target_arch = "wasm32")]
mod highlight;
#[cfg(target_arch = "wasm32")]
mod init;
#[cfg(target_arch = "wasm32")]
#[allow(dead_code, unused_imports)]
mod input;
#[cfg(target_arch = "wasm32")]
mod io;
#[cfg(target_arch = "wasm32")]
mod large_object;
#[cfg(target_arch = "wasm32")]
mod logging;
#[cfg(target_arch = "wasm32")]
mod lua_commands;
#[cfg(target_arch = "wasm32")]
mod markdown;
#[cfg(target_arch = "wasm32")]
mod metacmd;
#[cfg(target_arch = "wasm32")]
mod named;
#[cfg(target_arch = "wasm32")]
mod output;
#[cfg(target_arch = "wasm32")]
mod pager;
#[cfg(target_arch = "wasm32")]
mod pattern;
#[cfg(target_arch = "wasm32")]
mod query;
#[cfg(target_arch = "wasm32")]
mod repl;
#[cfg(target_arch = "wasm32")]
mod report;
#[cfg(target_arch = "wasm32")]
mod safety;
#[cfg(target_arch = "wasm32")]
mod session;
#[cfg(target_arch = "wasm32")]
mod session_store;
#[cfg(target_arch = "wasm32")]
mod setup;
#[cfg(target_arch = "wasm32")]
mod slashcmd;
#[cfg(target_arch = "wasm32")]
mod statusline;
#[cfg(target_arch = "wasm32")]
mod term;
#[cfg(target_arch = "wasm32")]
mod update;
#[cfg(target_arch = "wasm32")]
mod vars;

// WASM browser support: WebSocket connector and wasm-bindgen entry point.
#[cfg(target_arch = "wasm32")]
pub mod wasm;
