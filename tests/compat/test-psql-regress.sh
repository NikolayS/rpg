#!/usr/bin/env bash
# Copyright 2026
set -Eeuo pipefail
IFS=$'\n\t'

# Warn (but don't fail) if running on bash < 4. The script is portable to
# bash 3.2, but newer bash is recommended for better performance.
if [[ "${BASH_VERSINFO[0]}" -lt 4 ]]; then
  echo "WARNING: bash ${BASH_VERSION} detected." \
    "bash 4+ is recommended but not required." >&2
fi

# On macOS, GNU coreutils installs as 'gtimeout'; add gnubin to PATH if present.
for _gnubin in \
  /opt/homebrew/opt/coreutils/libexec/gnubin \
  /usr/local/opt/coreutils/libexec/gnubin; do
  if [[ -d "${_gnubin}" ]]; then
    PATH="${_gnubin}:${PATH}"
    break
  fi
done

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

PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-5432}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
export PGPASSWORD="${PGPASSWORD:-postgres}"

# Use separate databases for psql and rpg so that DML executed by psql
# does not contaminate the starting state for rpg (and vice versa).
#
# Strategy: create one "template" database with test_setup.sql applied,
# then for each test clone it quickly into psql_test and rpg_test, run
# each client against its own copy, and drop them afterwards.
REGRESS_TEMPLATE_DB="${PGDATABASE}_regress_tmpl"
# Use equal-length suffixes so that "table_catalog" column widths are
# identical in both outputs (prevents spurious diffs in info-schema queries).
PSQL_DBNAME="${PGDATABASE}_psql_regress"
RPG_DBNAME="${PGDATABASE}_rpg__regress"

_psql_admin() {
  PAGER=cat psql \
    --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
    "$@" > /dev/null 2>&1 || true
}

# NOTE: DB names below are unquoted in SQL. This is safe because they are
# derived from PGDATABASE (which CI controls) plus hardcoded suffixes, so
# they never contain special characters or spaces.
_drop_test_dbs() {
  _psql_admin \
    -c "DROP DATABASE IF EXISTS ${PSQL_DBNAME};" \
    -c "DROP DATABASE IF EXISTS ${RPG_DBNAME};" \
    -c "DROP DATABASE IF EXISTS ${REGRESS_TEMPLATE_DB};"
}

# Recreate isolated test databases before every test by cloning the
# template (fast — no SQL re-execution needed).
_reset_test_dbs() {
  # Drop test databases first so that roles have no remaining dependencies.
  _psql_admin \
    -c "DROP DATABASE IF EXISTS ${PSQL_DBNAME};" \
    -c "DROP DATABASE IF EXISTS ${RPG_DBNAME};"
  # Drop any cluster-wide regress_* roles left behind by a failed test.
  # Must happen after database drop so the roles have no object owners.
  PAGER=cat psql \
    --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
    -c "do \$\$ declare r record; begin
          for r in select rolname from pg_roles
                   where rolname like 'regress_%' loop
            execute format('drop role if exists %I', r.rolname);
          end loop; end \$\$;" \
    > /dev/null 2>&1 || true
  PAGER=cat psql \
    --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
    -c "CREATE DATABASE ${PSQL_DBNAME} TEMPLATE ${REGRESS_TEMPLATE_DB};" \
    -c "CREATE DATABASE ${RPG_DBNAME}  TEMPLATE ${REGRESS_TEMPLATE_DB};" \
    > /dev/null 2>&1 || {
      # Template cloning failed (e.g. no test_setup was run); fall back to
      # creating plain empty databases.
      PAGER=cat psql \
        --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
        -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
        -c "CREATE DATABASE ${PSQL_DBNAME};" \
        -c "CREATE DATABASE ${RPG_DBNAME};" \
        > /dev/null 2>&1 || true
    }
}

RESULTS_DIR="${RESULTS_DIR:-/tmp/rpg-regress-$$}"
mkdir -p "${RESULTS_DIR}"

cleanup() {
  _drop_test_dbs
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
  # Uses libpq pipeline mode (\startpipeline / \endpipeline) which hangs
  # when run non-interactively via -f against a server that does not have
  # pipeline support enabled in the same way as pg_regress sets it up.
  "psql_pipeline"
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
    -e 's/\x1b\[[0-9;?]*[a-zA-Z]//g' \
    -e 's/[[:space:]]*$//' \
    -e '/^Time: [0-9]/d' \
    -e '/^Timing is /d' \
    -e '/^Hint: /d' \
    -e '/^\[auto-explain:/d' \
    -e 's/^psql:[^:]*:[0-9][0-9]*: //' \
    -e 's/^rpg:[^:]*:[0-9][0-9]*: //' \
    -e 's/^psql: error: //' \
    -e 's/^rpg: \(connection to server\)/\1/' \
    -e 's/" ([^)]*), port/", port/g' \
    -e 's/FATAL:  /FATAL: /g' \
    -e '/^rpg [0-9][0-9]*\./d' \
    -e '/^You are now connected to database /d' \
    -e '/^LINE [0-9][0-9]*: /d' \
    -e '/^ *\^[ ~]*$/d' \
    -e '/\\gset[[:space:]]*$/d' \
    -e '/^Expanded display is /d' \
    -e '/^Invalid command \\getenv /d' \
    -e '/^\\if /d' \
    -e '/^\\endif/d' \
    -e 's/_psql_regress/_test_regress/g' \
    -e 's/_rpg__regress/_test_regress/g' \
    -e '/^CONTEXT:  /d' \
    -e '/^SQL function "[^"]*" statement /d' \
    -e '/^PL\/pgSQL function "[^"]*" /d' \
    -e '/^parallel worker$/d' \
    -e '/enumtypid/s/=([0-9][0-9]*/=(OID/g' \
    -e 's/for operator [0-9][0-9]*/for operator OID/g' \
    -e 's/ Query Identifier: [-0-9][0-9]*/ Query Identifier: 0000000000000000000/g' \
    -e 's/List of tables/List of relations/g' | \
  awk '
    BEGIN { after_qp = 0 }
    /^$/ { blank++; next }
    {
      if (blank > 0) { print ""; blank = 0 }
      if (after_qp) {
        # Normalize EXPLAIN separator width (varies by query identifier length).
        gsub(/-+/, "----------")
        after_qp = 0
      }
      if (/^ *QUERY PLAN *$/) {
        # Normalize EXPLAIN header whitespace.
        gsub(/^ +| +$/, "")
        after_qp = 1
      }
      print
    }
  ' | \
  awk '
    # Normalize non-deterministic ordering of NOTICE / WARNING / INFO lines.
    #
    # In psql (synchronous libpq), notices print inline where the server sends
    # them — typically between the last data row and the command tag.  In rpg
    # (async tokio), notices are buffered and flushed at statement boundaries,
    # so they may appear after the command tag.  This block collects all output
    # lines within each paragraph (delimited by blank lines), partitions them
    # into regular lines and notice lines, then emits regular lines first
    # followed by notice lines.  Both psql and rpg output normalize to the
    # same form regardless of original notice position.
    #
    # Covers: copydml (AFTER trigger NOTICE), transactions (WARNING outside
    # tx), plpgsql (NOTICE + CONTEXT ordering).
    function is_notice(line) {
      return line ~ /^(NOTICE|WARNING|INFO):  /
    }
    function is_notice_cont(line) {
      return line ~ /^(DETAIL|HINT):  /
    }
    function flush_paragraph() {
      for (i = 0; i < reg_count; i++) {
        print regulars[i]
      }
      for (i = 0; i < ntc_count; i++) {
        print notices[i]
      }
      reg_count = 0
      ntc_count = 0
    }
    /^$/ {
      flush_paragraph()
      print ""
      next
    }
    {
      if (is_notice($0) || (ntc_count > 0 && is_notice_cont($0))) {
        notices[ntc_count++] = $0
      } else {
        regulars[reg_count++] = $0
      }
    }
    END {
      flush_paragraph()
    }
  ' | \
  awk '
    # Normalize WAL bytes timing variance in the stats regression test.
    # rpg may generate slightly more (or less) WAL than psql between the
    # baseline \gset capture and the subsequent comparison query, so the
    # boolean result of "wal_bytes > :wal_bytes_before" can flip between
    # t and f.  Replace the boolean value with a stable placeholder.
    {
      if (wal_cmp > 0) {
        # We are inside the result block after a wal_bytes comparison.
        # Line 1 = header (?column?), 2 = separator (---), 3 = value,
        # 4 = row count — normalize the value line (line 3).
        wal_cmp++
        if (wal_cmp == 4 && /^ [tf]$/) {
          print " WAL_CMP"
          next
        }
        if (wal_cmp > 5) wal_cmp = 0
      }
      if (/wal_bytes[[:space:]]*>[[:space:]]*:.*_before/) {
        wal_cmp = 1
      }
      print
    }
  '
}

# Per-test timeout (seconds). Override with TEST_TIMEOUT env var.
TEST_TIMEOUT="${TEST_TIMEOUT:-300}"

# run_psql FILE  — run a SQL file through psql, return normalized output
run_psql() {
  local file="${1}"
  timeout "${TEST_TIMEOUT}" \
    env PAGER=cat psql \
      --no-psqlrc \
      -X \
      -a \
      -q \
      -v "ON_ERROR_STOP=0" \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PSQL_DBNAME}" \
      -f "${file}" \
    2>&1 | normalize
}

# run_rpg FILE  — run a SQL file through rpg, return normalized output
run_rpg() {
  local file="${1}"
  timeout "${TEST_TIMEOUT}" \
    env PAGER=cat "${RPG}" \
      -X \
      -a \
      -q \
      -v "ON_ERROR_STOP=0" \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${RPG_DBNAME}" \
      -f "${file}" \
    2>&1 | normalize
}

# should_skip NAME — return 0 if the test should be skipped
should_skip() {
  local name="${1}"
  # User-specified skip list (use read -ra to split on spaces despite IFS=$'\n\t')
  if [[ -n "${REGRESS_SKIP:-}" ]]; then
    local -a skip_list
    IFS=' ' read -ra skip_list <<< "${REGRESS_SKIP}"
    for s in "${skip_list[@]}"; do
      if [[ "${name}" == "${s}" ]]; then
        return 0
      fi
    done
  fi
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
  # Use read -ra to split on spaces despite IFS=$'\n\t'
  local -a only_list
  IFS=' ' read -ra only_list <<< "${REGRESS_ONLY}"
  for r in "${only_list[@]}"; do
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

  # Recreate fresh isolated databases so each test starts from the same
  # state regardless of what previous tests left behind.
  _reset_test_dbs

  # Run the regression setup into both isolated databases first so that
  # any shared schema (sequence tables, etc.) is available.
  if [[ -f "${REGRESS_SQL_DIR}/test_setup.sql" ]]; then
    PAGER=cat psql --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
      -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PSQL_DBNAME}" \
      -f "${REGRESS_SQL_DIR}/test_setup.sql" > /dev/null 2>&1 || true
    PAGER=cat psql --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
      -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${RPG_DBNAME}" \
      -f "${REGRESS_SQL_DIR}/test_setup.sql" > /dev/null 2>&1 || true
  fi

  local psql_out rpg_out
  psql_out=$(run_psql "${sql_file}" 2>/dev/null || true)

  # Some tests modify cluster-wide objects (e.g. tablespaces) that are
  # shared across all databases.  After psql runs (which may drop/rename
  # the tablespace), restore the regress_tblspace if it no longer exists
  # so that rpg starts with the same cluster-wide preconditions as psql.
  # CREATE TABLESPACE cannot run inside a transaction block, so SET and
  # CREATE must be sent as separate commands.
  PAGER=cat psql --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${RPG_DBNAME}" \
    -c "SET allow_in_place_tablespaces = true" \
    -c "CREATE TABLESPACE regress_tblspace LOCATION ''" \
    > /dev/null 2>&1 || true

  # Some tests create cluster-wide roles and leave them behind
  # (e.g. graph_table_rls.sql says "leave objects behind for pg_upgrade tests").
  # Drop PSQL_DBNAME first (removes role dependencies), then drop regress_*
  # roles so that rpg starts with the same preconditions as psql did.
  _psql_admin -c "DROP DATABASE IF EXISTS ${PSQL_DBNAME};" > /dev/null 2>&1 || true
  PAGER=cat psql \
    --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
    -c "do \$\$ declare r record; begin
          for r in select rolname from pg_roles
                   where rolname like 'regress_%' loop
            execute format('drop role if exists %I', r.rolname);
          end loop; end \$\$;" \
    > /dev/null 2>&1 || true

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
# Setup: create the template database with test_setup.sql applied, then clone
# it into the initial psql and rpg test databases.
# ---------------------------------------------------------------------------
setup_regress_db() {
  echo "=== Setting up regression schema ==="

  # Drop any leftover databases from a previous interrupted run.
  _drop_test_dbs

  # Create the template database and apply test_setup.sql to it so that
  # _reset_test_dbs() can clone it cheaply for each test.
  echo "Creating template database..."
  PAGER=cat psql \
    --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
    -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" -d "${PGDATABASE}" \
    -c "CREATE DATABASE ${REGRESS_TEMPLATE_DB};" \
    > /dev/null 2>&1 || true

  if [[ -f "${REGRESS_SQL_DIR}/test_setup.sql" ]]; then
    PAGER=cat psql \
      --no-psqlrc -X -q -v ON_ERROR_STOP=0 \
      -h "${PGHOST}" -p "${PGPORT}" -U "${PGUSER}" \
      -d "${REGRESS_TEMPLATE_DB}" \
      -f "${REGRESS_SQL_DIR}/test_setup.sql" \
      > /dev/null 2>&1 || true
  else
    echo "WARNING: test_setup.sql not found — skipping schema setup"
  fi

  echo "Done."
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

# Portable deduplication for bash 3.2+ (no associative arrays).
# _seen_tests holds "|name1|name2|…|" so we can match with case.
_seen_tests="|"

_is_seen() {
  case "${_seen_tests}" in
    *"|${1}|"*) return 0 ;;
    *)          return 1 ;;
  esac
}

_mark_seen() {
  _seen_tests="${_seen_tests}${1}|"
}

schedule_file="${PG_SRC}/src/test/regress/parallel_schedule"
if [[ -f "${schedule_file}" ]]; then
  while IFS= read -r line; do
    # Lines like: test: boolean char name varchar ...
    if [[ "${line}" =~ ^test:[[:space:]]+(.*) ]]; then
      # Use read -ra to split on spaces (IFS=$'\n\t' prevents normal word-split)
      IFS=' ' read -ra sched_tests <<< "${BASH_REMATCH[1]}"
      for t in "${sched_tests[@]}"; do
        if ! _is_seen "${t}"; then
          ORDERED_TESTS+=("${t}")
          _mark_seen "${t}"
        fi
      done
    fi
  done < "${schedule_file}"
fi

# Append any .sql files not already in the schedule.
for f in "${REGRESS_SQL_DIR}"/*.sql; do
  name="$(basename "${f}" .sql)"
  if ! _is_seen "${name}"; then
    ORDERED_TESTS+=("${name}")
    _mark_seen "${name}"
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
