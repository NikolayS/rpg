#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

# ---------------------------------------------------------------------------
# rpg vs psql connection method parity tests
#
# Usage: test-connections.sh <path-to-rpg-binary>
#
# Runs the same connection forms through both psql and rpg, compares
# output, and verifies error-path behaviour.  Exits non-zero on any
# failure.
# ---------------------------------------------------------------------------

PASS=0
FAIL=0
RPG=""
TMPDIR_CONN=""

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

cleanup() {
  rm -rf "${TMPDIR_CONN}"
}

# Strip binary name ("rpg" / "psql") so \conninfo output compares equal.
# Also normalise trailing whitespace and non-deterministic lines.
normalize() {
  expand | \
  sed \
    -e 's/[[:space:]]*$//' \
    -e 's/\brpg\b/BINARY/g' \
    -e 's/\bpsql\b/BINARY/g' \
    -e '/^Time: [0-9]/d' \
    -e '/^Timing is /d' \
    -e '/^SSL connection /d' | \
  awk '
    /^$/ { blank++; next }
    { if (blank > 0) { print ""; blank = 0 } print }
  '
}

pass_test() {
  echo "PASS: ${1}"
  (( PASS++ )) || true
}

# fail_test DESC PSQL_OUT RPG_OUT
fail_test() {
  local desc="${1}"
  local psql_out="${2}"
  local rpg_out="${3}"
  echo "FAIL: ${desc}"
  echo "--- psql ---"
  echo "${psql_out}"
  echo "--- rpg ---"
  echo "${rpg_out}"
  echo "--- diff ---"
  diff <(echo "${psql_out}") <(echo "${rpg_out}") || true
  echo "---"
  (( FAIL++ )) || true
}

# compare_conn_same DESC ARGS...
#   Passes identical ARGS to both psql and rpg and compares output.
compare_conn_same() {
  local desc="${1}"
  shift
  local psql_out rpg_out
  psql_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
      psql --no-psqlrc "$@" 2>&1 | normalize
  ) || true
  rpg_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
      "${RPG}" "$@" 2>&1 | normalize
  ) || true

  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "${desc}"
  else
    fail_test "${desc}" "${psql_out}" "${rpg_out}"
  fi
}

# expect_failure DESC CMD...
#   Verifies that CMD exits non-zero.
expect_failure() {
  local desc="${1}"
  shift
  local actual_exit=0
  "$@" >/dev/null 2>&1 || actual_exit=$?
  if [[ "${actual_exit}" -ne 0 ]]; then
    pass_test "${desc}"
  else
    echo "FAIL: ${desc} (expected non-zero exit, got 0)"
    (( FAIL++ )) || true
  fi
}

# ---------------------------------------------------------------------------
# Test cases
# ---------------------------------------------------------------------------

# (a) TCP with explicit -h -p -U -d flags
test_tcp_flags() {
  compare_conn_same "TCP flags -h -p -U -d" \
    -h "${TEST_PGHOST}" \
    -p "${TEST_PGPORT}" \
    -U "${TEST_PGUSER}" \
    -d "${TEST_PGDATABASE}" \
    -c '\conninfo'
}

# (b) Bare positional args: dbname user (psql only supports 2 positional
#     args; host and port are passed as flags)
test_positional_args() {
  local rpg_out psql_out
  rpg_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
      "${RPG}" \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -c '\conninfo' \
        "${TEST_PGDATABASE}" \
        "${TEST_PGUSER}" \
        2>&1 | normalize
  ) || true
  psql_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
      psql --no-psqlrc \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -c '\conninfo' \
        "${TEST_PGDATABASE}" \
        "${TEST_PGUSER}" \
        2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "bare positional args (dbname user)"
  else
    fail_test "bare positional args (dbname user)" \
      "${psql_out}" "${rpg_out}"
  fi
}

# (c) URI format
test_uri() {
  local uri="postgresql://${TEST_PGUSER}:${TEST_PGPASSWORD}@${TEST_PGHOST}:${TEST_PGPORT}/${TEST_PGDATABASE}"
  local rpg_out psql_out
  rpg_out=$(
    "${RPG}" "${uri}" -c '\conninfo' 2>&1 | normalize
  ) || true
  psql_out=$(
    psql --no-psqlrc "${uri}" -c '\conninfo' 2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "URI connection string"
  else
    fail_test "URI connection string" "${psql_out}" "${rpg_out}"
  fi
}

# (d) Conninfo keyword=value string
test_conninfo_string() {
  local conninfo_str
  conninfo_str="host=${TEST_PGHOST} port=${TEST_PGPORT} dbname=${TEST_PGDATABASE} user=${TEST_PGUSER} password=${TEST_PGPASSWORD}"
  local rpg_out psql_out
  rpg_out=$(
    "${RPG}" "${conninfo_str}" -c '\conninfo' 2>&1 | normalize
  ) || true
  psql_out=$(
    psql --no-psqlrc "${conninfo_str}" -c '\conninfo' 2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "conninfo keyword=value string"
  else
    fail_test "conninfo keyword=value string" "${psql_out}" "${rpg_out}"
  fi
}

# (e) Environment variables only - no CLI connection flags
test_env_vars_only() {
  local rpg_out psql_out
  rpg_out=$(
    PGHOST="${TEST_PGHOST}" \
    PGPORT="${TEST_PGPORT}" \
    PGUSER="${TEST_PGUSER}" \
    PGPASSWORD="${TEST_PGPASSWORD}" \
    PGDATABASE="${TEST_PGDATABASE}" \
      "${RPG}" -c '\conninfo' 2>&1 | normalize
  ) || true
  psql_out=$(
    PGHOST="${TEST_PGHOST}" \
    PGPORT="${TEST_PGPORT}" \
    PGUSER="${TEST_PGUSER}" \
    PGPASSWORD="${TEST_PGPASSWORD}" \
    PGDATABASE="${TEST_PGDATABASE}" \
      psql --no-psqlrc -c '\conninfo' 2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "env vars only (PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE)"
  else
    fail_test "env vars only (PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE)" \
      "${psql_out}" "${rpg_out}"
  fi
}

# (f) -d flag overrides PGDATABASE env var
#     psql only accepts dbname as a positional arg; passing extra positional
#     args emits a warning and is unreliable.  Test the override via env var
#     instead: set PGDATABASE=wrongdb but pass -d <real-db> - connection
#     should succeed, proving -d wins.
test_flag_overrides_positional() {
  local rpg_out psql_out
  rpg_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
    PGDATABASE=wrongdb \
      "${RPG}" \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c 'select 1' \
        2>&1 | normalize
  ) || true
  psql_out=$(
    env PGPASSWORD="${TEST_PGPASSWORD}" \
    PGDATABASE=wrongdb \
      psql --no-psqlrc \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c 'select 1' \
        2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test "-d flag overrides PGDATABASE env var"
  else
    fail_test "-d flag overrides PGDATABASE env var" "${psql_out}" "${rpg_out}"
  fi
}

# (g) Wrong password fails with non-zero exit
test_wrong_password() {
  expect_failure "wrong password exits non-zero" \
    env PGPASSWORD=wrongpassword \
      "${RPG}" \
        -w \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c 'select 1'
}

# (h) .pgpass file authentication
test_pgpass_file() {
  local pgpass_dir="${TMPDIR_CONN}/pgpass_home"
  mkdir -p "${pgpass_dir}"
  printf '%s:%s:%s:%s:%s\n' \
    "${TEST_PGHOST}" \
    "${TEST_PGPORT}" \
    "${TEST_PGDATABASE}" \
    "${TEST_PGUSER}" \
    "${TEST_PGPASSWORD}" \
    > "${pgpass_dir}/.pgpass"
  chmod 600 "${pgpass_dir}/.pgpass"

  local rpg_out psql_out
  rpg_out=$(
    PGPASSFILE="${pgpass_dir}/.pgpass" \
      "${RPG}" \
        -w \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c 'select 1' \
        2>&1 | normalize
  ) || true
  psql_out=$(
    PGPASSFILE="${pgpass_dir}/.pgpass" \
      psql --no-psqlrc \
        -w \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c 'select 1' \
        2>&1 | normalize
  ) || true
  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    pass_test ".pgpass file authentication"
  else
    fail_test ".pgpass file authentication" "${psql_out}" "${rpg_out}"
  fi
}

# (i) Unix socket connection (only when a socket file actually exists)
test_unix_socket() {
  local socket_file="/var/run/postgresql/.s.PGSQL.${TEST_PGPORT}"
  if [[ -S "${socket_file}" ]]; then
    compare_conn_same "Unix socket connection" \
      -h /var/run/postgresql \
      -p "${TEST_PGPORT}" \
      -U "${TEST_PGUSER}" \
      -d "${TEST_PGDATABASE}" \
      -c '\conninfo'
  else
    echo "SKIP: Unix socket (${socket_file} not found)"
  fi
}

# ---------------------------------------------------------------------------
# Section D - SSL/TLS sslmode coverage
#
# Requires a TLS-enabled postgres reachable at TEST_PG_TLS_HOST:TEST_PG_TLS_PORT.
# If TEST_PG_TLS_PORT is unset the entire section is skipped.
#
# Two postgres instances are used:
#   TLS server  : TEST_PG_TLS_HOST / TEST_PG_TLS_PORT (ssl=on, self-signed cert)
#   Plain server: TEST_PGHOST      / TEST_PGPORT       (ssl=off, existing service)
# ---------------------------------------------------------------------------

# D1 - sslmode=disable: must connect and report ssl=f in pg_stat_ssl
test_ssl_disable() {
  local out exit_code=0
  out=$(
    PGSSLMODE=disable \
    PGPASSWORD="${TEST_PG_TLS_PASSWORD}" \
      "${RPG}" \
        -h "${TEST_PG_TLS_HOST}" \
        -p "${TEST_PG_TLS_PORT}" \
        -U postgres \
        -d postgres \
        -c "select ssl from pg_stat_ssl where pid = pg_backend_pid()" \
        2>&1
  ) || exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "FAIL: D1 sslmode=disable (rpg exited ${exit_code})"
    echo "${out}"
    (( FAIL++ )) || true
    return
  fi
  if echo "${out}" | grep -q "^[[:space:]]*f"; then
    pass_test "D1 sslmode=disable (ssl=f confirmed)"
  else
    echo "FAIL: D1 sslmode=disable (ssl=f not found in output)"
    echo "${out}"
    (( FAIL++ )) || true
  fi
}

# D2 - sslmode=prefer: must connect successfully
test_ssl_prefer() {
  local exit_code=0
  PGSSLMODE=prefer \
  PGPASSWORD="${TEST_PG_TLS_PASSWORD}" \
    "${RPG}" \
      -h "${TEST_PG_TLS_HOST}" \
      -p "${TEST_PG_TLS_PORT}" \
      -U postgres \
      -d postgres \
      -c "select 1" \
      >/dev/null 2>&1 || exit_code=$?
  if [[ "${exit_code}" -eq 0 ]]; then
    pass_test "D2 sslmode=prefer (connected)"
  else
    echo "FAIL: D2 sslmode=prefer (rpg exited ${exit_code})"
    (( FAIL++ )) || true
  fi
}

# D3 - sslmode=require against TLS server: must connect and report ssl=t.
# This is the critical regression test - sslmode=require was broken in v0.8.0
# and fixed in PR #711. See issue #710.
test_ssl_require_ok() {
  local out exit_code=0
  out=$(
    PGSSLMODE=require \
    PGPASSWORD="${TEST_PG_TLS_PASSWORD}" \
      "${RPG}" \
        -h "${TEST_PG_TLS_HOST}" \
        -p "${TEST_PG_TLS_PORT}" \
        -U postgres \
        -d postgres \
        -c "select ssl from pg_stat_ssl where pid = pg_backend_pid()" \
        2>&1
  ) || exit_code=$?
  if [[ "${exit_code}" -ne 0 ]]; then
    echo "FAIL: D3 sslmode=require vs TLS server (rpg exited ${exit_code})"
    echo "${out}"
    (( FAIL++ )) || true
    return
  fi
  if echo "${out}" | grep -q "^[[:space:]]*t"; then
    pass_test "D3 sslmode=require vs TLS server (ssl=t confirmed)"
  else
    echo "FAIL: D3 sslmode=require vs TLS server (ssl=t not found in output)"
    echo "${out}"
    (( FAIL++ )) || true
  fi
}

# D4 - sslmode=require against a plain (no-TLS) server: must exit non-zero
# and emit a message referencing SSL.
test_ssl_require_fail() {
  local out exit_code=0
  out=$(
    PGSSLMODE=require \
    PGPASSWORD="${TEST_PGPASSWORD}" \
      "${RPG}" \
        -h "${TEST_PGHOST}" \
        -p "${TEST_PGPORT}" \
        -U "${TEST_PGUSER}" \
        -d "${TEST_PGDATABASE}" \
        -c "select 1" \
        2>&1
  ) || exit_code=$?
  if [[ "${exit_code}" -eq 0 ]]; then
    echo "FAIL: D4 sslmode=require vs plain server (expected non-zero exit, got 0)"
    (( FAIL++ )) || true
    return
  fi
  if echo "${out}" | grep -qiE "ssl|tls"; then
    pass_test "D4 sslmode=require vs plain server (exited ${exit_code}, SSL/TLS error message present)"
  else
    echo "FAIL: D4 sslmode=require vs plain server (exited ${exit_code} but no SSL/TLS mention in output)"
    echo "${out}"
    (( FAIL++ )) || true
  fi
}

# D5 - sslmode=verify-ca: SKIP - rpg currently fails with UnknownIssuer for
# self-signed certs. See issue #712. Remove this skip once #712 is resolved.
test_ssl_verify_ca() {
  echo "SKIP: D5 sslmode=verify-ca (known bug: UnknownIssuer for self-signed certs, see issue #712)"
}

# D6 - sslmode=verify-full: SKIP - same root cause as D5.
# See issue #712.
test_ssl_verify_full() {
  echo "SKIP: D6 sslmode=verify-full (known bug: UnknownIssuer for self-signed certs, see issue #712)"
}

# Run all Section D tests, skipping if TEST_PG_TLS_PORT is not configured.
test_ssl_section() {
  if [[ -z "${TEST_PG_TLS_PORT:-}" ]]; then
    echo "SKIP: Section D (TEST_PG_TLS_PORT not set - no TLS postgres available)"
    return
  fi

  TEST_PG_TLS_HOST="${TEST_PG_TLS_HOST:-localhost}"
  TEST_PG_TLS_PASSWORD="${TEST_PG_TLS_PASSWORD:-}"

  echo ""
  echo "--- Section D: SSL/TLS sslmode tests (TLS server: ${TEST_PG_TLS_HOST}:${TEST_PG_TLS_PORT}) ---"

  test_ssl_disable
  test_ssl_prefer
  test_ssl_require_ok
  test_ssl_require_fail
  test_ssl_verify_ca
  test_ssl_verify_full
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  RPG="${1:?Usage: test-connections.sh <rpg-binary>}"

  TEST_PGHOST="${TEST_PGHOST:-localhost}"
  TEST_PGPORT="${TEST_PGPORT:-5432}"
  TEST_PGUSER="${TEST_PGUSER:-postgres}"
  TEST_PGPASSWORD="${TEST_PGPASSWORD:-postgres}"
  TEST_PGDATABASE="${TEST_PGDATABASE:-postgres}"

  TMPDIR_CONN="$(mktemp -d)"
  trap cleanup EXIT

  echo "=== rpg vs psql connection tests ==="
  echo ""

  test_tcp_flags
  test_positional_args
  test_uri
  test_conninfo_string
  test_env_vars_only
  test_flag_overrides_positional
  test_wrong_password
  test_pgpass_file
  test_unix_socket
  test_ssl_section

  echo ""
  echo "=== Results: ${PASS} passed, ${FAIL} failed ===" 
  local total=$(( PASS + FAIL ))
  echo "=== Total: ${total} tests ==="

  if [[ "${FAIL}" -gt 0 ]]; then
    exit 1
  fi
}

main "$@"
