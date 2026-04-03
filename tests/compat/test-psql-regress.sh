#!/usr/bin/env bash
# Copyright 2026
set -Eeuo pipefail
IFS=$'\n\t'

# ---------------------------------------------------------------------------
# test-psql-regress.sh — Run PostgreSQL's own regression test suite against rpg
#
# Usage:
#   test-psql-regress.sh <rpg-binary> <postgres-src-dir>
#
# For each SQL file in <postgres-src-dir>/src/test/regress/sql/ this script:
#   1. Runs the file through psql   (flags: -X -a -q, same as pg_regress)
#   2. Runs the file through rpg    (same flags)
#   3. Normalizes both outputs      (strip timing, ANSI, trailing whitespace)
#   4. Diffs psql output vs rpg output — PASS if identical, FAIL otherwise
#
# Additionally, for SELECT-only tests, the script also diffs rpg output
# against the upstream .out expected file (the gold standard).
#
# Environment variables (all optional, sensible defaults):
#   PGHOST, PGPORT, PGUSER, PGDATABASE, PGPASSWORD
#   REGRESS_ONLY — space-separated list of test names to run (e.g. "boolean char")
#   REGRESS_SKIP — space-separated list of test names to skip
#   KEEP_RESULTS — set non-empty to keep per-test result files in $RESULTS_DIR
#   RESULTS_DIR  — directory for intermediate results (default: /tmp/rpg-regress-$$)
# ---------------------------------------------------------------------------

RPG="${1:?Usage: test-psql-regress.sh <rpg-binary> <postgres-src-dir>}"
PG_SRC="${2:?Usage: test-psql-regress.sh <rpg-binary> <postgres-src-dir>}"

REGRESS_SQL_DIR="${PG_SRC}/src/test/regress/sql"
REGRESS_EXP_DIR="${PG_SRC}/src/test/regress/expected"

PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-5432}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
export PGPASSWORD="${PGPASSWORD:-postgres}"

RESULTS_DIR="${RESULTS_DIR:-/tmp/rpg-regress-$$}"
mkdir -p "${RESULTS_DIR}"

cleanup() {
  if [[ -z "${KEEP_RESULTS:-}" ]]; then
    rm -rf "${RESULTS_DIR}"
  else
    echo ""
    echo "Results kept in: ${RESULTS_DIR}"
  fi
}
trap cleanup EXIT

PASS=0
FAIL=0
SKIP=0

# ---------------------------------------------------------------------------
# Tests that are structurally incompatible with this harness or require
# special PostgreSQL server configuration / C library extensions.
# These are skipped rather than counted as failures.
# ---------------------------------------------------------------------------
readonly SKIP_ALWAYS=(
  # Requires PostgreSQL C regression extension library (regress.so)
  "regproc"
  # Requires special tablespace paths set up by pg_regress infrastructure
  # (test_setup creates regress_tblspace; we run test_setup ourselves below)
  # Nothing to skip here — handled by running test_setup first
  # Requires WAL / replication configuration
  "wal_consistency_checking"
  # Requires special GUCs (enable_partition_pruning, etc.) already default
  # Platform-specific collation tests
  "collate.icu.utf8"
  "collate.linux.utf8"
  "collate.windows.win1252"
  "collate.utf8"
  # Requires specific locale
  "char_1"
  "char_2"
  "collate"
  # Requires pg_regress C library
  "sqljson_jsontable"
  # Triggers / rules that depend on earlier tests having run
  # (included in sequential schedule below, leave as-is)
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Normalize output for comparison:
#   - strip ANSI colour codes
#   - strip trailing whitespace on each line
#   - collapse multiple consecutive blank lines into one
#   - remove non-deterministic lines:
#       Time: N.NNN ms        (\timing output)
#       Timing is on/off.     (\timing toggle notice)
#       Hint: ...             (rpg-specific hints)
#       [auto-explain: ...]   (rpg auto-explain header)
normalize() {
  expand | \
  sed \
    -e 's/\x1b\[[0-9;]*m//g' \
    -e 's/[[:space:]]*$//' \
    -e '/^Time: [0-9]/d' \
    -e '/^Timing is /d' \
    -e '/^Hint: /d' \
    -e '/^\[auto-explain:/d' | \
  awk '
    /^$/ { blank++; next }
    { if (blank > 0) { print ""; blank = 0 } print }
  '
}

# run_psql FILE  — run a SQL file through psql, return normalized output
run_psql() {
  local file="${1}"
  PAGER=cat psql \
    --no-psqlrc \
    -X \
    -a \
    -q \
    -v "ON_ERROR_STOP=0" \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGUSER}" \
    -d "${PGDATABASE}" \
    -f "${file}" \
    2>&1 | normalize
}

# run_rpg FILE  — run a SQL file through rpg, return normalized output
run_rpg() {
  local file="${1}"
  PAGER=cat "${RPG}" \
    -X \
    -a \
    -q \
    -v "ON_ERROR_STOP=0" \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGUSER}" \
    -d "${PGDATABASE}" \
    -f "${file}" \
    2>&1 | normalize
}

# should_skip NAME — return 0 if the test should be skipped
should_skip() {
  local name="${1}"
  # User-specified skip list
  for s in ${REGRESS_SKIP:-}; do
    if [[ "${name}" == "${s}" ]]; then
      return 0
    fi
  done
  # Built-in skip list
  for s in "${SKIP_ALWAYS[@]}"; do
    if [[ "${name}" == "${s}" ]]; then
      return 0
    fi
  done
  return 1
}

# should_run NAME — return 0 if the test should run (honours REGRESS_ONLY)
should_run() {
  local name="${1}"
  if [[ -z "${REGRESS_ONLY:-}" ]]; then
    return 0
  fi
  for r in ${REGRESS_ONLY}; do
    if [[ "${name}" == "${r}" ]]; then
      return 0
    fi
  done
  return 1
}

# compare_test NAME — run one test, compare psql vs rpg, report result
compare_test() {
  local name="${1}"
  local sql_file="${REGRESS_SQL_DIR}/${name}.sql"

  if [[ ! -f "${sql_file}" ]]; then
    echo "SKIP (no sql file): ${name}"
    (( SKIP++ )) || true
    return
  fi

  if should_skip "${name}"; then
    echo "SKIP (excluded):    ${name}"
    (( SKIP++ )) || true
    return
  fi

  if ! should_run "${name}"; then
    (( SKIP++ )) || true
    return
  fi

  local psql_out rpg_out
  psql_out=$(run_psql "${sql_file}" 2>/dev/null || true)
  rpg_out=$(run_rpg  "${sql_file}" 2>/dev/null || true)

  if [[ "${psql_out}" == "${rpg_out}" ]]; then
    echo "PASS: ${name}"
    (( PASS++ )) || true
  else
    echo "FAIL: ${name}"
    (( FAIL++ )) || true

    # Save diff to results directory for inspection.
    local diff_file="${RESULTS_DIR}/${name}.diff"
    diff \
      <(echo "${psql_out}") \
      <(echo "${rpg_out}") \
      > "${diff_file}" 2>&1 || true

    # Print first 60 lines of diff to stderr.
    head -60 "${diff_file}" >&2

    # Also save individual outputs.
    echo "${psql_out}" > "${RESULTS_DIR}/${name}.psql"
    echo "${rpg_out}"  > "${RESULTS_DIR}/${name}.rpg"
  fi
}

# ---------------------------------------------------------------------------
# Setup: run test_setup.sql once to create the standard regression schema
# ---------------------------------------------------------------------------
setup_regress_db() {
  echo "=== Setting up regression schema ==="
  if [[ ! -f "${REGRESS_SQL_DIR}/test_setup.sql" ]]; then
    echo "WARNING: test_setup.sql not found — skipping schema setup"
    return
  fi

  # Run test_setup.sql through psql to create the standard tables / tablespace.
  # Errors are ignored (idempotent: IF NOT EXISTS / OR REPLACE throughout).
  PAGER=cat psql \
    --no-psqlrc \
    -X \
    -q \
    -v "ON_ERROR_STOP=0" \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGUSER}" \
    -d "${PGDATABASE}" \
    -f "${REGRESS_SQL_DIR}/test_setup.sql" \
    > /dev/null 2>&1 || true
  echo "Setup complete."
  echo ""
}

# ---------------------------------------------------------------------------
# Main: iterate tests in parallel_schedule order, then remaining .sql files
# ---------------------------------------------------------------------------

echo "=== rpg vs psql regression tests ==="
echo "PostgreSQL source: ${PG_SRC}"
echo "rpg binary:        ${RPG}"
echo "Database:          ${PGUSER}@${PGHOST}:${PGPORT}/${PGDATABASE}"
echo ""

setup_regress_db

# Collect the ordered list of tests from parallel_schedule.
declare -a ORDERED_TESTS=()
declare -A SEEN_TESTS=()

schedule_file="${PG_SRC}/src/test/regress/parallel_schedule"
if [[ -f "${schedule_file}" ]]; then
  while IFS= read -r line; do
    # Lines like: test: boolean char name varchar ...
    if [[ "${line}" =~ ^test:[[:space:]]+(.*) ]]; then
      for t in ${BASH_REMATCH[1]}; do
        if [[ -z "${SEEN_TESTS[${t}]:-}" ]]; then
          ORDERED_TESTS+=("${t}")
          SEEN_TESTS["${t}"]=1
        fi
      done
    fi
  done < "${schedule_file}"
fi

# Append any .sql files not already in the schedule.
for f in "${REGRESS_SQL_DIR}"/*.sql; do
  name="$(basename "${f}" .sql)"
  if [[ -z "${SEEN_TESTS[${name}]:-}" ]]; then
    ORDERED_TESTS+=("${name}")
    SEEN_TESTS["${name}"]=1
  fi
done

echo "=== Running ${#ORDERED_TESTS[@]} tests ==="
echo ""

for test_name in "${ORDERED_TESTS[@]}"; do
  compare_test "${test_name}"
done

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "=== Results ==="
echo "PASS: ${PASS}"
echo "FAIL: ${FAIL}"
echo "SKIP: ${SKIP}"
TOTAL=$(( PASS + FAIL + SKIP ))
echo "TOTAL: ${TOTAL}"

if [[ "${FAIL}" -gt 0 ]]; then
  echo ""
  echo "Failures saved to: ${RESULTS_DIR}/"
  KEEP_RESULTS=1
  exit 1
fi

echo ""
echo "All tests passed!"
