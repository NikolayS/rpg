#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

# =========================================================================
# build-rpg-wasm.sh — Build rpg as a WASM binary via wasm32-unknown-emscripten
#
# Follows the same approach as build-psql-wasm.sh for the psql WASM port.
# Produces rpg.js + rpg.wasm in public/rpg/ for use with a WebSocket proxy.
#
# Heavy tooling (emsdk, Rust wasm32-unknown-emscripten target) is installed
# on first run and cached in .build/; subsequent runs are fast.
#
# Prerequisites (Linux):
#   apt install build-essential curl nodejs
#   rustup target add wasm32-unknown-emscripten
#
# Prerequisites (macOS):
#   brew install node
#   rustup target add wasm32-unknown-emscripten
#
# Environment variables:
#   WS_URL — WebSocket proxy URL (default: ws://127.0.0.1:9090)
#             The proxy bridges WebSocket frames to a TCP Postgres connection.
# =========================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "${SCRIPT_DIR}")"
BUILD_DIR="${SCRIPT_DIR}/.build"
EMSDK_DIR="${BUILD_DIR}/emsdk"
WASM_OUT="${REPO_DIR}/public/rpg"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log()  { echo -e "${GREEN}[BUILD]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $1"; }
err()  { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

mkdir -p "${BUILD_DIR}"

# -------------------------------------------------------------------------
# Step 0: Install / activate Emscripten SDK
# -------------------------------------------------------------------------
EMSDK_VERSION="4.0.23"

if [[ ! -d "${EMSDK_DIR}" ]]; then
  log "Cloning Emscripten SDK (${EMSDK_VERSION})..."
  git clone https://github.com/emscripten-core/emsdk.git "${EMSDK_DIR}"
  cd "${EMSDK_DIR}"
  ./emsdk install "${EMSDK_VERSION}"
  ./emsdk activate "${EMSDK_VERSION}"
  cd "${REPO_DIR}"
fi

log "Activating Emscripten SDK..."
# shellcheck disable=SC1091
source "${EMSDK_DIR}/emsdk_env.sh"
emcc --version | head -1

# -------------------------------------------------------------------------
# Step 1: Ensure the Rust wasm32-unknown-emscripten target is installed
# -------------------------------------------------------------------------
if ! rustup target list --installed 2>/dev/null | grep -q wasm32-unknown-emscripten; then
  log "Adding wasm32-unknown-emscripten Rust target..."
  rustup target add wasm32-unknown-emscripten
fi

# -------------------------------------------------------------------------
# Step 2: Set Emscripten compiler flags for Cargo/cc-rs
#
# EMCC_CFLAGS / EMMAKEN_CFLAGS tell the cc-rs build helper to use emcc and
# pass Asyncify + WebSocket flags so any C shims compile correctly.
# -------------------------------------------------------------------------
export EMCC_CFLAGS="\
  -sWASM=1 \
  -sASYNCIFY \
  -sASYNCIFY_STACK_SIZE=262144 \
  -sUSE_PTHREADS=0 \
"
export EMMAKEN_CFLAGS="${EMCC_CFLAGS}"

# Tell the Rust linker to use emcc.
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER="emcc"

# Emscripten-specific linker flags injected via RUSTFLAGS.
#
# -sWEBSOCKET_URL: all TCP sockets in the WASM binary are transparently
#   proxied to this WebSocket endpoint (same proxy used by psql WASM).
#   Override at build time: WS_URL=wss://proxy.example.com ./build-rpg-wasm.sh
#
# TODO: once the Rust-side WebSocket connector (wasm/ws-proxy.js) is fully
#   implemented, the WS_URL should be configurable at runtime rather than
#   baked in at link time.
WS_URL="${WS_URL:-ws://127.0.0.1:9090}"
export RUSTFLAGS="\
  -C link-arg=-sWASM=1 \
  -C link-arg=-sASYNCIFY \
  -C link-arg=-sASYNCIFY_STACK_SIZE=262144 \
  -C link-arg=\"-sWEBSOCKET_URL=${WS_URL}\" \
  -C link-arg=-sUSE_PTHREADS=0 \
  -C link-arg=-sALLOW_MEMORY_GROWTH=1 \
  -C link-arg=\"-sEXPORTED_RUNTIME_METHODS=[\\\"callMain\\\",\\\"FS\\\",\\\"ENV\\\"]\" \
  -C link-arg=-sINVOKE_RUN=0 \
  -C link-arg=\"-sENVIRONMENT=web\" \
  -C link-arg=-sEXIT_RUNTIME=0 \
  -C link-arg=-sFORCE_FILESYSTEM=1 \
  -C link-arg=-sMODULARIZE=1 \
  -C link-arg=\"-sEXPORT_NAME=createRpg\" \
  -C link-arg=-sSTACK_SIZE=131072 \
"

# -------------------------------------------------------------------------
# Step 3: Build rpg for wasm32-unknown-emscripten
#
# KNOWN BLOCKER (2026-03-27):
#   mio v1.x (used by tokio 1.x for its I/O reactor) explicitly removed
#   wasm32-unknown-emscripten support.  cargo check will fail with:
#
#     error: This wasm target is unsupported by mio.
#            If using Tokio, disable the net feature.
#
#   tokio-postgres also uses tokio::net (TCP) internally, so switching to
#   no-net tokio is not straightforward without a custom async transport.
#
#   Potential paths forward:
#   a) Downgrade to mio 0.6 + tokio 0.2 (old API, significant effort).
#   b) Patch mio to add emscripten support (upstream contribution needed).
#   c) Implement a WebSocket-backed tokio::net::TcpStream shim for WASM
#      and patch tokio-postgres to use it (see wasm/ws-proxy.js for the
#      proxy-side counterpart that already exists).
#   d) Target wasm32-unknown-unknown instead, using wasm-bindgen + a
#      browser-native WebSocket transport (different architecture).
#
#   The cfg-gating changes in Cargo.toml / src/ are ready for whichever
#   path is chosen; they do not affect native builds.
# -------------------------------------------------------------------------
log "Building rpg for wasm32-unknown-emscripten (release)..."
cd "${REPO_DIR}"
cargo build \
  --target wasm32-unknown-emscripten \
  --release \
  2>&1

WASM_BIN="${REPO_DIR}/target/wasm32-unknown-emscripten/release"
if [[ ! -f "${WASM_BIN}/rpg.js" ]]; then
  err "Build did not produce rpg.js — check linker output above."
fi
log "Build succeeded: $(du -h "${WASM_BIN}/rpg.wasm" | cut -f1) rpg.wasm"

# -------------------------------------------------------------------------
# Step 4: Optimize with wasm-opt (if available)
# -------------------------------------------------------------------------
WASM_OPT="${EMSDK_DIR}/upstream/bin/wasm-opt"
if [[ -x "${WASM_OPT}" ]]; then
  log "Running wasm-opt -Oz..."
  "${WASM_OPT}" -Oz \
    --enable-bulk-memory \
    --enable-bulk-memory-opt \
    --enable-nontrapping-float-to-int \
    "${WASM_BIN}/rpg.wasm" \
    -o "${WASM_BIN}/rpg.wasm"
  log "After wasm-opt: $(du -h "${WASM_BIN}/rpg.wasm" | cut -f1) rpg.wasm"
else
  warn "wasm-opt not found at ${WASM_OPT}, skipping optimisation."
fi

# -------------------------------------------------------------------------
# Step 5: Copy artifacts to public/rpg/
# -------------------------------------------------------------------------
log "Copying WASM artifacts to ${WASM_OUT}..."
mkdir -p "${WASM_OUT}"
cp "${WASM_BIN}/rpg.js" "${WASM_BIN}/rpg.wasm" "${WASM_OUT}/"

log "Build complete!"
echo ""
echo "WASM artifacts:"
echo "  ${WASM_OUT}/rpg.js   ($(du -h "${WASM_OUT}/rpg.js"   | cut -f1))"
echo "  ${WASM_OUT}/rpg.wasm ($(du -h "${WASM_OUT}/rpg.wasm" | cut -f1))"
echo ""
echo "WebSocket proxy (ws://127.0.0.1:9090) must be running before rpg.js"
echo "is loaded in the browser.  See wasm/ws-proxy.js for proxy setup docs."
