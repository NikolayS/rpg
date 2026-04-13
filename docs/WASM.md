<!-- Copyright 2026 Nikolay Samokhvalov / postgres.ai -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# rpg WASM — Browser Build (Experimental)

> **Status: Experimental.** The WASM build is functional for interactive SQL
> and most meta-commands, but some features are unavailable due to platform
> constraints (no filesystem, no shell, no ratatui). See
> [Known Limitations](#known-limitations) below.

Run rpg in the browser as a WebAssembly module. SQL queries, meta-commands,
and rpg diagnostics work against a real Postgres server via a WebSocket proxy.

## Architecture

```text
Browser (xterm.js)
  │  keystrokes
  ▼
rpg.wasm (WasmLineReader channel)
  │  Postgres wire protocol
  ▼
WebSocket (binary frames)
  │
  ▼
ws-proxy.js (Node.js)
  │  raw TCP
  ▼
PostgreSQL
```

`rpg.wasm` compiles to `wasm32-unknown-unknown` and runs on a single-threaded
Tokio runtime. The `WasmConnector` opens a browser `WebSocket` via
`ws_stream_wasm`, which yields an `AsyncRead + AsyncWrite` stream. That stream
is passed to `tokio-postgres`'s `connect_raw` for standard Postgres wire
protocol negotiation. Because the underlying `WsIo` type is `!Send`, the
connection driver runs on `wasm_bindgen_futures::spawn_local` instead of
`tokio::spawn`.

## Quick Start

### 1. Install prerequisites

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack       # or: cargo install wasm-bindgen-cli
npm install ws                # ws-proxy dependency
```

### 2. Build the WASM module

Use the provided build script:

```bash
./wasm/build-rpg-wasm.sh
```

Or build manually:

```bash
wasm-pack build \
  --target web \
  --release \
  --features wasm \
  --out-dir wasm/pkg
```

Artifacts land in `wasm/pkg/` (`rpg.js`, `rpg_bg.wasm`).

### 3. Start the WebSocket proxy

```bash
node wasm/ws-proxy.js --pg-host 127.0.0.1 --pg-port 5432
```

The proxy listens on `ws://127.0.0.1:9091` by default.

### 4. Serve the browser UI

```bash
cd wasm && python3 -m http.server 8080
```

### 5. Open the browser

Navigate to `http://localhost:8080`. Enter connection details in the toolbar
and click **Connect**.

## What Works

- **SQL queries** with psql-style tabular formatting (`\x` expanded mode supported)
- **Meta-commands:** `\d`, `\dt`, `\dn`, `\du`, `\di`, `\dv`, `\df`, `\l`, `\conninfo`, `\timing`, `\x`, `\set`, `\echo`, `\?`
- **rpg commands:** `/version`, `/dba`, `/help`
- **Error messages** with SQLSTATE codes and line/caret position markers
- **Line editing:** arrow keys, command history (Up/Down), Ctrl-U/K/W/L, Home/End, Delete, Backspace
- **Connection** to any Postgres 14-18 server via the WebSocket proxy

## Known Limitations

Commands that require native OS facilities show a friendly error message
instead of panicking — e.g. `\i: file include is not available on
wasm32-unknown-unknown (no filesystem)`.

| Feature | Reason |
|---|---|
| `/ash` | Requires `ratatui` — not available on `wasm32-unknown-unknown` |
| `/rpg` | Requires `ratatui` |
| `\e` (edit in `$EDITOR`) | No editor / `std::process::Command` in the browser |
| `\!` (shell command) | No `std::process::Command` on `wasm32-unknown-unknown` |
| `\i`, `\ir` (include file) | No `std::fs` on `wasm32-unknown-unknown` |
| `\o`, `\w` (file output) | No `std::fs` |
| `\cd` | No filesystem |
| `\copy` | No local filesystem |
| `\lo_import`, `\lo_export` | No `std::fs` |
| `\g file`, `\g \|cmd` | No filesystem / shell |
| `\s filename` (save history) | No `std::fs` |
| `\setenv` | `std::env::set_var` unavailable on `wasm32-unknown-unknown` |
| `\password` | Requires `rpassword` — not available on WASM |
| `/plan save` | No `std::fs` |
| Tab completion | Not yet wired; `WasmLineReader` does not implement completion callbacks |
| AI commands (`/ask`, `/fix`, `/explain`, `/optimize`) | Require `reqwest` streaming, which is limited on WASM |
| Multi-statement command tags | When multiple statements are sent in one line, the command tag from the first is reused for subsequent ones (cosmetic, queries execute correctly) |

## Output Routing

Standard `println!` / `eprintln!` write to file descriptors 1 and 2, which are
sinks on `wasm32-unknown-unknown` (there is no OS). rpg uses custom macros to
route all output to the browser:

| Macro | Native target | WASM target |
|---|---|---|
| `rpg_println!` | `println!` | `web_sys::console::log_1` |
| `rpg_eprintln!` | `eprintln!` | `web_sys::console::error_1` |
| `rpg_print!` | `print!` | `web_sys::console::log_1` |
| `rpg_eprint!` | `eprint!` | `web_sys::console::error_1` |

On the browser side, `index.html` intercepts `console.log` and `console.error`
and writes the output into the xterm.js terminal. Prompt lines (matching the
pattern `dbname=> `) are detected with a regex so the cursor stays on the
prompt line instead of advancing to the next row.

Multi-line strings are split line-by-line in `wasm::io::wasm_print` /
`wasm_eprint` before calling `console.log` so that xterm.js renders each line
on its own row.

## Building

### Cargo.toml structure

The `[lib]` section sets `crate-type = ["cdylib", "rlib"]` so `wasm-pack` /
`wasm-bindgen` can produce the WASM module (cdylib) while native builds still
get the rlib.

Dependencies are split by target:

- **`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`** — native-only
  crates: `rustyline`, `crossterm`, `ratatui`, `rpassword`, `russh`,
  `tokio-rustls`, multi-threaded Tokio (`rt-multi-thread`, `signal`, `process`).
- **`[target.'cfg(target_arch = "wasm32")'.dependencies]`** — WASM-only crates:
  `ws_stream_wasm`, `async_io_stream`, `wasm-bindgen`, `wasm-bindgen-futures`,
  `web-sys`, `js-sys`, `console_error_panic_hook`, and `getrandom` with the
  `wasm_js` feature for browser-compatible randomness.

All WASM source code is gated behind `#[cfg(target_arch = "wasm32")]` and is
invisible to native `cargo check` / `cargo test`.

### Build script

`wasm/build-rpg-wasm.sh` automates the full build:

1. Installs `wasm-pack` if missing
2. Ensures `wasm32-unknown-unknown` target is added
3. Runs `wasm-pack build --target web --release --features wasm`
4. Optionally runs `wasm-opt -Oz` (from binaryen) for size optimization

## WebSocket Proxy

`wasm/ws-proxy.js` is a Node.js process that bridges WebSocket connections from
the browser to a Postgres TCP socket. One TCP connection is created per
WebSocket connection.

### Options

| Flag | Env var | Default | Description |
|---|---|---|---|
| `--pg-host` | `PG_HOST` | `127.0.0.1` | Postgres TCP host |
| `--pg-port` | `PG_PORT` | `5432` | Postgres TCP port |
| `--listen-port` | `PROXY_PORT` | `9091` | WebSocket listen port |
| `--listen-host` | `PROXY_HOST` | `127.0.0.1` | WebSocket listen host |
| `--token` | `WS_PROXY_TOKEN` | none | Auth token (see below) |

### Authentication

When `--token <secret>` (or `WS_PROXY_TOKEN`) is set, the first WebSocket
message from each client must be a JSON auth frame:

```json
{"token": "<secret>"}
```

If the token does not match, the connection is closed with code `4001`
(Unauthorized). If the first message is not valid JSON, code `4002` (Invalid
auth frame) is used. When no token is configured, the proxy runs
unauthenticated (acceptable for local development only).

### Backpressure

The proxy implements bidirectional backpressure:

- **WS to TCP:** if the TCP write buffer is full, incoming WebSocket reads are
  paused until TCP drains.
- **TCP to WS:** if `ws.bufferedAmount` exceeds 64 KiB, TCP reads are paused
  until the WebSocket drains.

This prevents unbounded memory growth when either side is slower than the other.
