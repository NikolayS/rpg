#!/usr/bin/env bash
set -euo pipefail

# =========================================================================
# build-rpg-wasm.sh — Build rpg as WASM via wasm-pack (wasm32-unknown-unknown)
#
# This is the wasm-bindgen build path, complementing the Emscripten build
# (build-rpg-wasm-emscripten.sh).  It targets wasm32-unknown-unknown and
# uses wasm-pack to produce an ES module + .wasm file for browser use.
#
# The key difference from the Emscripten path:
#   - No POSIX emulation layer — lighter, smaller output.
#   - WebSocket transport handled explicitly by src/wasm/connector.rs
#     via ws_stream_wasm (not Emscripten's socket-to-WS remapping).
#   - wasm-bindgen exposes run_rpg() directly to JavaScript.
#
# Prerequisites:
#   - Rust toolchain with wasm32-unknown-unknown target
#   - wasm-pack (installed automatically if missing)
#   - wasm-opt (from binaryen; optional, for size optimisation)
#
# Environment:
#   WASM_OUT  — output directory (default: wasm/pkg)
#
# =========================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "${SCRIPT_DIR}")"
WASM_OUT="${WASM_OUT:-${SCRIPT_DIR}/pkg}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[build]${NC} $1"; }
warn() { echo -e "${YELLOW}[warn]${NC}  $1"; }
err()  { echo -e "${RED}[error]${NC} $1" >&2; exit 1; }

# -------------------------------------------------------------------------
# Step 0: Ensure wasm-pack is installed
# -------------------------------------------------------------------------
if ! command -v wasm-pack &>/dev/null; then
  log "Installing wasm-pack..."
  cargo install wasm-pack
fi
log "wasm-pack $(wasm-pack --version)"

# -------------------------------------------------------------------------
# Step 1: Ensure the Rust target is available
# -------------------------------------------------------------------------
if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-unknown; then
  log "Adding wasm32-unknown-unknown target..."
  rustup target add wasm32-unknown-unknown
fi

# -------------------------------------------------------------------------
# Step 2: Build with wasm-pack
#
# --target web: produces ES module (no bundler required)
# --features wasm: enables the wasm feature flag in Cargo.toml
# --out-dir: where the .js + .wasm artifacts land
#
# KNOWN BLOCKER (2026-03-27):
#   The wasm dependencies (ws_stream_wasm, wasm-bindgen, web-sys,
#   console_error_panic_hook) are not yet in Cargo.toml — they will be
#   added when Sprint 1 merges.  Until then this build will fail at the
#   dependency resolution step.
# -------------------------------------------------------------------------
log "Building rpg for wasm32-unknown-unknown (release)..."
cd "${REPO_DIR}"

wasm-pack build \
  --target web \
  --release \
  --features wasm \
  --out-dir "${WASM_OUT}" \
  2>&1

log "wasm-pack build succeeded"

# -------------------------------------------------------------------------
# Step 3: Optimise with wasm-opt (if available)
# -------------------------------------------------------------------------
WASM_FILE="${WASM_OUT}/rpg_bg.wasm"

if [[ -f "${WASM_FILE}" ]] && command -v wasm-opt &>/dev/null; then
  BEFORE=$(du -h "${WASM_FILE}" | cut -f1)
  log "Running wasm-opt -Oz (before: ${BEFORE})..."
  wasm-opt -Oz \
    --enable-bulk-memory \
    --enable-nontrapping-float-to-int \
    "${WASM_FILE}" \
    -o "${WASM_FILE}"
  AFTER=$(du -h "${WASM_FILE}" | cut -f1)
  log "After wasm-opt: ${AFTER}"
elif [[ -f "${WASM_FILE}" ]]; then
  warn "wasm-opt not found; skipping optimisation."
fi

# -------------------------------------------------------------------------
# Done
# -------------------------------------------------------------------------
log "Build complete!"
echo ""
echo "Artifacts in ${WASM_OUT}:"
ls -lh "${WASM_OUT}"/*.wasm "${WASM_OUT}"/*.js 2>/dev/null || true
echo ""
echo "Usage:"
echo "  1. Start the ws-proxy:  node wasm/ws-proxy.js --pg-host localhost"
echo "  2. Serve wasm/pkg/ and load rpg.js in the browser"
echo "  3. Call: await run_rpg('ws://localhost:9091', 'mydb')"
