#!/usr/bin/env bash
set -Eeuo pipefail
IFS=$'\n\t'

# ---------------------------------------------------------------------------
# psql vs samo golden output comparison
#
# Usage: test-compat.sh <path-to-samo-binary>
#
# Runs the same commands through both psql and samo, diffs the outputs, and
# exits non-zero if any comparison fails.
# ---------------------------------------------------------------------------

SAMO="${1:?Usage: test-compat.sh <samo-binary>}"

PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-5432}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-postgres}"
export PGPASSWORD="${PGPASSWORD:-postgres}"

PASS=0
FAIL=0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Strip trailing whitespace and remove consecutive blank lines.
# Column alignment and row counts are left intact — they are exactly what we
# are testing.
normalize() {
  sed -e 's/[[:space:]]*$//' | \
  awk '
    /^$/ { blank++; next }
    { if (blank > 0) { print ""; blank = 0 } print }
  '
}

# compare DESC CMD
#   Runs CMD through both psql and samo using -c, diffs the result.
compare() {
  local desc="${1}"
  local cmd="${2}"
  local psql_out samo_out

  psql_out=$(
    psql \
      --no-psqlrc \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PGDATABASE}" \
      -c "${cmd}" \
      2>/dev/null | normalize
  ) || true

  samo_out=$(
    "${SAMO}" \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PGDATABASE}" \
      -c "${cmd}" \
      2>/dev/null | normalize
  ) || true

  if [[ "${psql_out}" == "${samo_out}" ]]; then
    echo "PASS: ${desc}"
    (( PASS++ )) || true
  else
    echo "FAIL: ${desc}"
    echo "--- psql ---"
    echo "${psql_out}"
    echo "--- samo ---"
    echo "${samo_out}"
    echo "--- diff ---"
    diff <(echo "${psql_out}") <(echo "${samo_out}") || true
    echo "---"
    (( FAIL++ )) || true
  fi
}

# compare_flags DESC [ARGS...]
#   Like compare but passes the given args directly to both psql and samo.
#   The caller is responsible for including -c / -f / etc. in the args.
compare_flags() {
  local desc="${1}"
  shift
  local psql_out samo_out

  psql_out=$(
    psql \
      --no-psqlrc \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PGDATABASE}" \
      "$@" \
      2>/dev/null | normalize
  ) || true

  samo_out=$(
    "${SAMO}" \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PGDATABASE}" \
      "$@" \
      2>/dev/null | normalize
  ) || true

  if [[ "${psql_out}" == "${samo_out}" ]]; then
    echo "PASS: ${desc}"
    (( PASS++ )) || true
  else
    echo "FAIL: ${desc}"
    echo "--- psql ---"
    echo "${psql_out}"
    echo "--- samo ---"
    echo "${samo_out}"
    echo "--- diff ---"
    diff <(echo "${psql_out}") <(echo "${samo_out}") || true
    echo "---"
    (( FAIL++ )) || true
  fi
}

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------

# Load test schema (idempotent: all DDL uses IF NOT EXISTS / OR REPLACE).
# The path is relative to the repo root; CI runs from the workspace root.
psql \
  --no-psqlrc \
  -h "${PGHOST}" \
  -p "${PGPORT}" \
  -U "${PGUSER}" \
  -d "${PGDATABASE}" \
  -f tests/fixtures/schema.sql \
  2>/dev/null || true

echo "=== psql vs samo compatibility tests ==="
echo ""

# ---------------------------------------------------------------------------
# SQL queries
# ---------------------------------------------------------------------------

compare "select 1" \
  "select 1"

compare "select with aliases" \
  "select 1 as num, 'hello' as greeting"

compare "boolean literals" \
  "select true as yes, false as no"

compare "null value" \
  "select null::text as val"

compare "generate_series" \
  "select generate_series(1, 5) as n"

compare "multi-statement" \
  "select 1 as a; select 2 as b"

compare "arithmetic" \
  "select 6 * 7 as answer"

compare "string functions" \
  "select upper('hello') as up, length('world') as len"

compare "type cast" \
  "select 'hello'::varchar as str_val, '3.14'::numeric as num_val"

# ---------------------------------------------------------------------------
# Table queries (use schema loaded above)
# ---------------------------------------------------------------------------

compare "select from users" \
  "select id, name, email from users order by id"

compare "select from products" \
  "select id, name, active from products order by id"

compare "count rows" \
  "select count(*) as total from users"

# ---------------------------------------------------------------------------
# Describe commands
# ---------------------------------------------------------------------------

compare "\\dt" \
  "\\dt"

compare "\\dt+" \
  "\\dt+"

compare "\\di" \
  "\\di"

compare "\\dv" \
  "\\dv"

compare "\\dn" \
  "\\dn"

compare "\\du" \
  "\\du"

compare "\\df" \
  "\\df"

compare "\\d users" \
  "\\d users"

compare "\\d+ users" \
  "\\d+ users"

compare "\\l" \
  "\\l"

compare "\\l+" \
  "\\l+"

compare "\\dx" \
  "\\dx"

compare "\\ds" \
  "\\ds"

compare "\\dm" \
  "\\dm"

compare "\\di+" \
  "\\di+"

compare "\\d products" \
  "\\d products"

compare "\\d+ products" \
  "\\d+ products"

compare "\\dn+" \
  "\\dn+"

compare "\\du+" \
  "\\du+"

compare "\\dv+" \
  "\\dv+"

compare "\\dm+" \
  "\\dm+"

compare "\\ds+" \
  "\\ds+"

compare "\\db" \
  "\\db"

compare "\\dT" \
  "\\dT"

compare "\\dD" \
  "\\dD"

compare "\\dp" \
  "\\dp"

compare "\\dd" \
  "\\dd"

compare "\\dC" \
  "\\dC"

compare "\\dc" \
  "\\dc"

compare "\\dy" \
  "\\dy"

compare "\\do" \
  "\\do"

compare "\\df+" \
  "\\df+"

compare "\\dT+" \
  "\\dT+"

## \dx+ — psql uses per-extension object listing, different approach (deferred)
# compare "\\dx+" \
#   "\\dx+"

compare "\\db+" \
  "\\db+"

# ---------------------------------------------------------------------------
# Output modes via extra CLI flags
# ---------------------------------------------------------------------------

compare_flags "unaligned tuples-only" \
  -A -t -c "select 1 as n, 2 as m, 3 as k"

compare_flags "csv output" \
  --csv -c "select 1 as a, 'hello' as b, true as c"

compare_flags "unaligned csv from table" \
  --csv -c "select id, name from users order by id limit 3"

# ---------------------------------------------------------------------------
# CLI flag combinations
# ---------------------------------------------------------------------------

## --json is a samo-specific extension — psql doesn't support it
# compare_flags "json output" \
#   --json -c "select 1 as a, 'hello' as b"

compare_flags "unaligned with custom field separator" \
  -A -F '|' -c "select 1 as a, 2 as b, 3 as c"

## trailing record separator bug (#236)
# compare_flags "unaligned with custom record separator" \
#   -A -R '|' -t -c "select generate_series(1,3) as n"

# ---------------------------------------------------------------------------
# Expanded display mode
# ---------------------------------------------------------------------------

compare_flags "expanded single row" \
  -x -c "select 1 as num, 'hello' as greeting"

compare_flags "expanded multi-row" \
  -x -c "select id, name from users order by id limit 3"

compare_flags "expanded with null" \
  -x -c "select null::text as val, 42 as num"

# ---------------------------------------------------------------------------
# Show source commands
# ---------------------------------------------------------------------------

compare "\\sf user_order_count" \
  "\\sf user_order_count"

## \sf+ — line numbering format mismatch (#208)
# compare "\\sf+ user_order_count" \
#   "\\sf+ user_order_count"

compare "\\sv active_products" \
  "\\sv active_products"

compare "\\sv+ active_products" \
  "\\sv+ active_products"

# ---------------------------------------------------------------------------
# Foreign data wrapper commands
# ---------------------------------------------------------------------------

compare "\\des" \
  "\\des"

compare "\\dew" \
  "\\dew"

compare "\\det" \
  "\\det"

compare "\\deu" \
  "\\deu"

# ---------------------------------------------------------------------------
# File include (\i)
# ---------------------------------------------------------------------------

compare_flags "\\i file include" \
  -c "\\i tests/fixtures/include_test.sql"

# ---------------------------------------------------------------------------
# Info commands
# ---------------------------------------------------------------------------

compare "\\copyright" \
  "\\copyright"

# ---------------------------------------------------------------------------
# Conditional execution (\if / \else / \endif)
# ---------------------------------------------------------------------------

compare "if true branch" \
  "\\if true
select 'yes' as result;
\\endif"

compare "if false with else" \
  "\\if false
select 'wrong' as result;
\\else
select 'right' as result;
\\endif"

# ---------------------------------------------------------------------------
# Query execution variants
# ---------------------------------------------------------------------------

compare_flags "gset and reuse" \
  -c "select 42 as myval \\gset
select :myval as result;"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="

if [[ "${FAIL}" -gt 0 ]]; then
  exit 1
fi
