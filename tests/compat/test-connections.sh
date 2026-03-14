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
    -e '/^Timing is /d' | \
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

# (e) Environment variables only — no CLI connection flags
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
#     instead: set PGDATABASE=wrongdb but pass -d <real-db> — connection
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

# (i) Unix socket connection (only when socket directory exists)
test_unix_socket() {
  if [[ -d /var/run/postgresql ]]; then
    compare_conn_same "Unix socket connection" \
      -h /var/run/postgresql \
      -p "${TEST_PGPORT}" \
      -U "${TEST_PGUSER}" \
      -d "${TEST_PGDATABASE}" \
      -c '\conninfo'
  else
    echo "SKIP: Unix socket connection (/var/run/postgresql not found)"
  fi
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

  echo ""
  echo "=== Results: ${PASS} passed, ${FAIL} failed ==="
  local total=$(( PASS + FAIL ))
  echo "=== Total: ${total} tests ==="

  if [[ "${FAIL}" -gt 0 ]]; then
    exit 1
  fi
}

main "$@"
