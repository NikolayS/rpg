#!/bin/bash
set -euo pipefail

# =========================================================================
# build-psql-wasm.sh - Build psql WASM binary with OpenSSL/SSL support
#
# Heavy dependencies (emsdk, postgres, openssl, libedit) are cloned into .build/
# which is gitignored. Only this script and patch_poll.js are committed.
#
# Prerequisites:
#   macOS:  brew install autoconf automake libtool pkg-config node
#   Linux:  apt install build-essential autoconf automake libtool pkg-config bison flex nodejs
#
# First run clones ~2GB of dependencies and takes 10-20 minutes.
# Subsequent runs reuse cached builds and take ~1-2 minutes.
# =========================================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WEB_CONSOLE_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="${SCRIPT_DIR}/.build"
EMSDK_DIR="${BUILD_DIR}/emsdk"
PG_DIR="${BUILD_DIR}/postgres"
OPENSSL_DIR="${BUILD_DIR}/openssl-src"
WASM_OUT="${BUILD_DIR}/wasm-build"
SYSROOT="${EMSDK_DIR}/upstream/emscripten/cache/sysroot"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[BUILD]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
err() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

mkdir -p "${BUILD_DIR}"

# -------------------------------------------------------------------------
# Step 0: Install and activate Emscripten
# -------------------------------------------------------------------------
EMSDK_VERSION="4.0.23"

if [ ! -d "${EMSDK_DIR}" ]; then
  log "Cloning Emscripten SDK (${EMSDK_VERSION})..."
  git clone https://github.com/emscripten-core/emsdk.git "${EMSDK_DIR}"
  cd "${EMSDK_DIR}"
  ./emsdk install ${EMSDK_VERSION}
  ./emsdk activate ${EMSDK_VERSION}
fi

log "Activating Emscripten SDK..."
source "${EMSDK_DIR}/emsdk_env.sh"
emcc --version | head -1

# -------------------------------------------------------------------------
# Step 1: Build OpenSSL (if not already installed in sysroot)
# -------------------------------------------------------------------------
if [ ! -f "${SYSROOT}/lib/wasm32-emscripten/libssl.a" ] || \
   [ ! -f "${SYSROOT}/include/openssl/ssl.h" ]; then

  log "Building OpenSSL for WASM..."

  if [ ! -d "${OPENSSL_DIR}" ]; then
    log "Cloning OpenSSL 3.4.1..."
    git clone --depth 1 --branch openssl-3.4.1 \
      https://github.com/openssl/openssl.git "${OPENSSL_DIR}"
  fi

  cd "${OPENSSL_DIR}"
  [ -f Makefile ] && emmake make clean 2>/dev/null || true

  log "Configuring OpenSSL (TLS 1.3 only, stripped)..."
  ./Configure \
    linux-generic32 \
    no-asm \
    no-threads \
    no-shared \
    no-dso \
    no-engine \
    no-afalgeng \
    no-async \
    no-dgram \
    no-ui-console \
    no-tests \
    no-apps \
    no-ssl3 no-tls1 no-tls1_1 no-tls1_2 \
    no-dtls no-dtls1 no-dtls1_2 \
    no-des no-rc2 no-rc4 no-rc5 no-idea no-cast no-bf no-camellia \
    no-seed no-whirlpool no-md2 no-md4 no-mdc2 no-rmd160 \
    no-sm2 no-sm3 no-sm4 no-siphash no-aria no-blake2 \
    no-srp no-srtp no-sctp no-ct no-cms no-ocsp no-ts no-cmp \
    no-comp no-gost no-legacy no-deprecated \
    -DOPENSSL_NO_SECURE_MEMORY \
    -DNO_SYSLOG \
    -DHAVE_FORK=0 \
    --prefix="${SYSROOT}" \
    --openssldir="${SYSROOT}/ssl" \
    --libdir=lib/wasm32-emscripten \
    CC=emcc CXX=em++ AR=emar RANLIB=emranlib

  log "Compiling OpenSSL..."
  emmake make -j$(sysctl -n hw.ncpu 2>/dev/null || nproc) build_generated libssl.a libcrypto.a

  log "Installing OpenSSL to sysroot..."
  emmake make install_sw

  [ -f "${SYSROOT}/include/openssl/ssl.h" ] || err "OpenSSL headers not installed"
  [ -f "${SYSROOT}/lib/wasm32-emscripten/libssl.a" ] || err "libssl.a not installed"
  [ -f "${SYSROOT}/lib/wasm32-emscripten/libcrypto.a" ] || err "libcrypto.a not installed"
  log "OpenSSL build complete."
else
  log "OpenSSL already installed in sysroot, skipping build."
fi

# -------------------------------------------------------------------------
# Step 1.5a: Build termcap stub for WASM (needed by libedit)
# -------------------------------------------------------------------------
if [ ! -f "${SYSROOT}/lib/wasm32-emscripten/libcurses.a" ] || \
   [ ! -f "${SYSROOT}/include/curses.h" ]; then
  log "Building termcap stub for WASM..."
  emcc -O2 -c -o "${BUILD_DIR}/termcap_stub.o" "${SCRIPT_DIR}/termcap_stub.c"
  emar rcs "${SYSROOT}/lib/wasm32-emscripten/libcurses.a" "${BUILD_DIR}/termcap_stub.o"
  # Also install as libtermcap and libncurses for anything that links -ltermcap or -lncurses
  cp "${SYSROOT}/lib/wasm32-emscripten/libcurses.a" "${SYSROOT}/lib/wasm32-emscripten/libtermcap.a"
  cp "${SYSROOT}/lib/wasm32-emscripten/libcurses.a" "${SYSROOT}/lib/wasm32-emscripten/libncurses.a"

  # Install headers (curses.h, termcap.h, ncurses.h) so libedit configure finds them
  cp "${SCRIPT_DIR}/termcap_stub.h" "${SYSROOT}/include/curses.h"
  cp "${SCRIPT_DIR}/termcap_stub.h" "${SYSROOT}/include/termcap.h"
  cp "${SCRIPT_DIR}/termcap_stub.h" "${SYSROOT}/include/ncurses.h"
  log "termcap stub installed."
else
  log "termcap stub already installed, skipping."
fi

# -------------------------------------------------------------------------
# Step 1.5b: Build libedit (BSD editline) for readline support
# -------------------------------------------------------------------------
LIBEDIT_DIR="${BUILD_DIR}/libedit-src"
LIBEDIT_VERSION="libedit-20240808-3.1"

if [ ! -f "${SYSROOT}/lib/wasm32-emscripten/libedit.a" ] || \
   [ ! -f "${SYSROOT}/include/readline/readline.h" ]; then

  log "Building libedit for WASM..."

  if [ ! -d "${LIBEDIT_DIR}" ]; then
    log "Downloading libedit ${LIBEDIT_VERSION}..."
    cd "${BUILD_DIR}"
    curl -L -o libedit.tar.gz \
      "https://thrysoee.dk/editline/${LIBEDIT_VERSION}.tar.gz"
    mkdir -p "${LIBEDIT_DIR}"
    tar xzf libedit.tar.gz -C "${LIBEDIT_DIR}" --strip-components=1
    rm libedit.tar.gz
  fi

  cd "${LIBEDIT_DIR}"
  [ -f Makefile ] && emmake make clean 2>/dev/null || true

  log "Configuring libedit..."
  CFLAGS="-D__STDC_ISO_10646__=201103L" \
  emconfigure ./configure \
    --host=wasm32-unknown-emscripten \
    --prefix="${SYSROOT}" \
    --libdir="${SYSROOT}/lib/wasm32-emscripten" \
    --disable-shared \
    --enable-static \
    --disable-examples

  log "Compiling libedit..."
  emmake make -j$(sysctl -n hw.ncpu 2>/dev/null || nproc)

  log "Installing libedit to sysroot..."
  emmake make install

  [ -f "${SYSROOT}/lib/wasm32-emscripten/libedit.a" ] || err "libedit.a not installed"
  [ -f "${SYSROOT}/include/readline/readline.h" ] || \
    [ -f "${SYSROOT}/include/editline/readline.h" ] || err "libedit headers not installed"
  log "libedit build complete."
else
  log "libedit already installed in sysroot, skipping build."
fi

# -------------------------------------------------------------------------
# Step 2: Get and configure PostgreSQL
# -------------------------------------------------------------------------
if [ ! -d "${PG_DIR}" ]; then
  log "Cloning PostgreSQL..."
  git clone --depth 1 --branch REL_18_STABLE \
    https://github.com/postgres/postgres.git "${PG_DIR}"
fi

cd "${PG_DIR}"

# Patch libpq: stub out setsockopt for TCP/socket-level options.
# Emscripten WebSocket sockets don't support TCP_NODELAY, keepalive, etc.
# and newer Emscripten versions return ENOPROTOOPT instead of silently succeeding.
if ! grep -q "WASM_SETSOCKOPT_STUB" src/interfaces/libpq/fe-connect.c 2>/dev/null; then
  log "Patching libpq setsockopt for WASM compatibility..."
  cat > /tmp/wasm_setsockopt_patch.h << 'PATCH'
/* WASM_SETSOCKOPT_STUB: Emscripten WebSocket sockets don't support
 * TCP-level socket options (TCP_NODELAY, keepalive, etc.) or SO_NOSIGPIPE.
 * All setsockopt calls in this file are for these — stub them all out. */
#ifdef __EMSCRIPTEN__
#define setsockopt(fd, level, optname, optval, optlen) 0
#endif
PATCH
  # Insert patch after #include "libpq-int.h" (guaranteed unconditional)
  PATCH_LINE=$(grep -n '#include "libpq-int.h"' src/interfaces/libpq/fe-connect.c | head -1 | cut -d: -f1)
  sed -i.bak "${PATCH_LINE}r /tmp/wasm_setsockopt_patch.h" src/interfaces/libpq/fe-connect.c
  rm -f src/interfaces/libpq/fe-connect.c.bak /tmp/wasm_setsockopt_patch.h
fi

if [ ! -f src/include/pg_config.h ] || ! grep -q "USE_OPENSSL 1" src/include/pg_config.h 2>/dev/null || \
   ! grep -q "HAVE_LIBREADLINE 1" src/include/pg_config.h 2>/dev/null; then
  log "Configuring PostgreSQL with OpenSSL and libedit support..."

  [ -f src/Makefile.global ] && emmake make distclean 2>/dev/null || true

  CPPFLAGS="-I${SYSROOT}/include" \
  LDFLAGS="-L${SYSROOT}/lib/wasm32-emscripten" \
  LIBS="-lssl -lcrypto -ledit -lcurses" \
  emconfigure ./configure \
    --host=wasm32-unknown-emscripten \
    --with-template=linux \
    --with-libedit-preferred \
    --without-zlib \
    --with-ssl=openssl \
    --without-icu \
    --disable-spinlocks \
    --disable-atomics \
    --disable-thread-safety

  grep -q "define USE_OPENSSL 1" src/include/pg_config.h || \
    err "PostgreSQL configure did not detect OpenSSL"
  grep -q "HAVE_LIBREADLINE 1" src/include/pg_config.h || \
    warn "PostgreSQL configure did not detect libedit/readline (tab completion will not work)"
  log "PostgreSQL configured with OpenSSL and libedit."
else
  log "PostgreSQL already configured with OpenSSL and libedit, skipping configure."
fi

# -------------------------------------------------------------------------
# Step 3: Build PostgreSQL modules
# -------------------------------------------------------------------------
log "Building PostgreSQL modules..."
cd src/common && emmake make && cd ../..
cd src/port && emmake make && cd ../..

# libpq: build objects (shared lib link may fail, that's OK)
cd src/interfaces/libpq && emmake make 2>/dev/null || true && cd ../../..

# fe_utils
cd src/fe_utils && emmake make && cd ../..

# psql: generate source files then compile objects manually
cd src/bin/psql
emmake make psqlscanslash.c sql_help.c tab-complete.c 2>/dev/null || true

for obj in command common copy crosstabview describe help input large_obj mainloop prompt psqlscanslash sql_help startup stringutils tab-complete variables; do
  emcc -Wall -Wmissing-prototypes -Wpointer-arith -Wdeclaration-after-statement \
    -Werror=vla -Wendif-labels -Wmissing-format-attribute -Wcast-function-type \
    -Wformat-security -Wmissing-variable-declarations -fno-strict-aliasing -fwrapv \
    -fexcess-precision=standard -Wno-unused-command-line-argument \
    -Wno-compound-token-split-by-macro -Wno-format-truncation -Wno-cast-function-type-strict \
    -O2 -I. -I. -I../../../src/interfaces/libpq -I../../../src/include \
    -I"${SYSROOT}/include" -c -o ${obj}.o ${obj}.c
done
cd ../../..

# Verify SSL objects
[ -f src/interfaces/libpq/fe-secure-common.o ] || err "fe-secure-common.o not built"
[ -f src/interfaces/libpq/fe-secure-openssl.o ] || err "fe-secure-openssl.o not built"
log "PostgreSQL modules built."

# -------------------------------------------------------------------------
# Step 4: Link WASM binary
# -------------------------------------------------------------------------
log "Linking psql WASM binary..."
mkdir -p "${WASM_OUT}"

# Collect libpq objects (exclude _shlib variants)
LIBPQ_DIR="src/interfaces/libpq"
LIBPQ_OBJS=""
for f in "${LIBPQ_DIR}"/*.o; do
  case "$f" in *_shlib*) continue;; esac
  LIBPQ_OBJS="${LIBPQ_OBJS} ${f}"
done

emcc \
  src/bin/psql/*.o \
  ${LIBPQ_OBJS} \
  src/common/libpgcommon_shlib.a \
  src/common/libpgcommon_excluded_shlib.a \
  src/port/libpgport.a \
  src/fe_utils/libpgfeutils.a \
  -o "${WASM_OUT}/psql.js" \
  -L"${SYSROOT}/lib/wasm32-emscripten" \
  -lssl \
  -lcrypto \
  -ledit \
  -lcurses \
  -sWASM=1 \
  "-sWEBSOCKET_URL=ws://127.0.0.1:9090" \
  -sUSE_PTHREADS=0 \
  "-sEXPORTED_RUNTIME_METHODS=[\"callMain\",\"FS\",\"ENV\"]" \
  -sINVOKE_RUN=0 \
  -sALLOW_MEMORY_GROWTH=1 \
  -sMODULARIZE=1 \
  "-sEXPORT_NAME=createPsql" \
  "-sENVIRONMENT=web" \
  -sSTACK_SIZE=131072 \
  -sASYNCIFY \
  -sASYNCIFY_IGNORE_INDIRECT=0 \
  -sASYNCIFY_STACK_SIZE=262144 \
  "-sASYNCIFY_IMPORTS=[\"__syscall_connect\",\"___syscall_connect\",\"__poll_js\",\"_poll_js\",\"wasi_snapshot_preview1.fd_read\",\"_fd_read\"]" \
  -sEXIT_RUNTIME=0 \
  -sFORCE_FILESYSTEM=1 \
  -Oz \
  --strip-all \
  --minify=0

log "WASM binary linked."
log "psql.wasm size (before wasm-opt): $(du -h "${WASM_OUT}/psql.wasm" | cut -f1)"

# -------------------------------------------------------------------------
# Step 5: Optimize WASM with wasm-opt
# -------------------------------------------------------------------------
WASM_OPT="${EMSDK_DIR}/upstream/bin/wasm-opt"
if [ -x "${WASM_OPT}" ]; then
  log "Running wasm-opt -Oz..."
  "${WASM_OPT}" -Oz \
    --enable-bulk-memory --enable-bulk-memory-opt --enable-nontrapping-float-to-int \
    "${WASM_OUT}/psql.wasm" -o "${WASM_OUT}/psql.wasm"
  log "psql.wasm size (after wasm-opt): $(du -h "${WASM_OUT}/psql.wasm" | cut -f1)"
else
  warn "wasm-opt not found at ${WASM_OPT}, skipping optimization."
fi

# -------------------------------------------------------------------------
# Step 6: Apply patches (JS only)
# -------------------------------------------------------------------------
log "Applying Asyncify patches..."
node "${SCRIPT_DIR}/patch_poll.cjs" "${WASM_OUT}/psql.js"
log "Patches applied."

# -------------------------------------------------------------------------
# Step 7: Copy artifacts to web-console public/psql/
# -------------------------------------------------------------------------
log "Copying WASM artifacts to web-console..."
mkdir -p "${WEB_CONSOLE_DIR}/public/psql"
cp "${WASM_OUT}/psql.js" "${WASM_OUT}/psql.wasm" "${WEB_CONSOLE_DIR}/public/psql/"
log "Copied to public/psql/"

log "Build complete!"
echo ""
echo "WASM artifacts:"
echo "  ${WEB_CONSOLE_DIR}/public/psql/psql.js  ($(du -h "${WEB_CONSOLE_DIR}/public/psql/psql.js" | cut -f1))"
echo "  ${WEB_CONSOLE_DIR}/public/psql/psql.wasm ($(du -h "${WEB_CONSOLE_DIR}/public/psql/psql.wasm" | cut -f1))"
