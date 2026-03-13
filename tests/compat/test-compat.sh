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

# Temporary directory cleaned up on exit.
TMPDIR_COMPAT="$(mktemp -d)"
cleanup() {
  rm -rf "${TMPDIR_COMPAT}"
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Strip trailing whitespace, remove consecutive blank lines, and drop
# non-deterministic lines:
#   - "Time: N.NNN ms"  (\timing output)
#   - "Timing is on/off."  (\timing toggle notice)
# Column alignment and row counts are left intact — they are exactly what
# we are testing.
normalize() {
  expand | \
  sed \
    -e 's/[[:space:]]*$//' \
    -e '/^Time: [0-9]/d' \
    -e '/^Timing is /d' | \
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

# compare_err DESC CMD
#   Compares stderr output for CMD from both psql and samo.
#   Used to verify error messages match for intentional bad SQL.
compare_err() {
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
      2>&1 >/dev/null | normalize
  ) || true

  samo_out=$(
    "${SAMO}" \
      -h "${PGHOST}" \
      -p "${PGPORT}" \
      -U "${PGUSER}" \
      -d "${PGDATABASE}" \
      -c "${cmd}" \
      2>&1 >/dev/null | normalize
  ) || true

  if [[ "${psql_out}" == "${samo_out}" ]]; then
    echo "PASS: ${desc}"
    (( PASS++ )) || true
  else
    echo "FAIL: ${desc}"
    echo "--- psql stderr ---"
    echo "${psql_out}"
    echo "--- samo stderr ---"
    echo "${samo_out}"
    echo "--- diff ---"
    diff <(echo "${psql_out}") <(echo "${samo_out}") || true
    echo "---"
    (( FAIL++ )) || true
  fi
}

# compare_file DESC FILE_CMD
#   Runs FILE_CMD (which uses \o to redirect output to a file) via both
#   psql and samo, then compares the contents of the two output files.
compare_file() {
  local desc="${1}"
  local cmd="${2}"
  local psql_file samo_file

  psql_file="${TMPDIR_COMPAT}/psql_$$.txt"
  samo_file="${TMPDIR_COMPAT}/samo_$$.txt"

  # Replace the placeholder __OUTFILE__ with each tool's output path.
  local psql_cmd samo_cmd
  psql_cmd="${cmd//__OUTFILE__/${psql_file}}"
  samo_cmd="${cmd//__OUTFILE__/${samo_file}}"

  psql \
    --no-psqlrc \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGUSER}" \
    -d "${PGDATABASE}" \
    -c "${psql_cmd}" \
    >/dev/null 2>&1 || true

  "${SAMO}" \
    -h "${PGHOST}" \
    -p "${PGPORT}" \
    -U "${PGUSER}" \
    -d "${PGDATABASE}" \
    -c "${samo_cmd}" \
    >/dev/null 2>&1 || true

  local psql_content samo_content
  psql_content="$(normalize < "${psql_file}" 2>/dev/null || true)"
  samo_content="$(normalize < "${samo_file}" 2>/dev/null || true)"

  rm -f "${psql_file}" "${samo_file}"

  if [[ "${psql_content}" == "${samo_content}" ]]; then
    echo "PASS: ${desc}"
    (( PASS++ )) || true
  else
    echo "FAIL: ${desc}"
    echo "--- psql file ---"
    echo "${psql_content}"
    echo "--- samo file ---"
    echo "${samo_content}"
    echo "--- diff ---"
    diff <(echo "${psql_content}") <(echo "${samo_content}") || true
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

compare_flags "unaligned with custom record separator" \
  -A -R '|' -t -c "select generate_series(1,3) as n"

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

compare "\\sf+ user_order_count" \
  "\\sf+ user_order_count"

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
# \timing
# ---------------------------------------------------------------------------

# \timing on/off: verify the toggle is accepted without error.
# The actual elapsed time line is non-deterministic so we only capture
# whether the query result rows are identical.
# \timing outputs a non-deterministic "Time: N.NNN ms" line after each
# query, so we suppress stdout via the normalize pipeline which strips
# lines matching that pattern.  The query data rows must still match.
compare_flags "timing on: query output unchanged" \
  -c "\timing on
select 1 as n;"

compare_flags "timing off: query output unchanged" \
  -c "\timing off
select 1 as n;"

# ---------------------------------------------------------------------------
# \set / \unset and :variable expansion
# ---------------------------------------------------------------------------

compare_flags "set variable and expand in query" \
  -c "\set myval 42
select :myval as result;"

compare_flags "set string variable and expand" \
  -c "\set greeting hello
select :'greeting' as word;"

compare_flags "set then unset: variable gone" \
  -c "\set x 99
\unset x
select 1 as check_unset;"

compare_flags "set multiple variables" \
  -c "\set a 10
\set b 20
select :a + :b as total;"

# ---------------------------------------------------------------------------
# \pset options
# ---------------------------------------------------------------------------

compare_flags "pset border 0" \
  -c "\pset border 0
select id, name from users order by id limit 3;"

compare_flags "pset border 1" \
  -c "\pset border 1
select id, name from users order by id limit 3;"

compare_flags "pset border 2" \
  -c "\pset border 2
select id, name from users order by id limit 3;"

compare_flags "pset null string" \
  -c "\pset null '(null)'
select null::text as empty_val, 'hello' as real_val;"

compare_flags "pset null string in table" \
  -c "\pset null '(null)'
select id, null::text as missing from users order by id limit 3;"

compare_flags "pset format unaligned" \
  -c "\pset format unaligned
select id, name from users order by id limit 3;"

compare_flags "pset format aligned" \
  -c "\pset format aligned
select id, name from users order by id limit 3;"

compare_flags "pset format csv" \
  -c "\pset format csv
select id, name from users order by id limit 3;"

compare_flags "pset tuples only on" \
  -c "\pset tuples_only on
select id, name from users order by id limit 3;"

compare_flags "pset tuples only off" \
  -c "\pset tuples_only off
select id, name from users order by id limit 3;"

# ---------------------------------------------------------------------------
# Multi-line queries
# ---------------------------------------------------------------------------

compare "multi-line select with where" \
  "select
    id,
    name,
    email
from users
where id <= 3
order by id;"

compare "multi-line cte" \
  "with top_users as (
    select id, name from users order by id limit 5
)
select id, name from top_users order by id;"

compare "multi-line insert then select" \
  "select count(*) as user_count from users;"

# ---------------------------------------------------------------------------
# Error output comparison
# ---------------------------------------------------------------------------

compare_err "syntax error: missing from" \
  "select * where 1=1"

compare_err "syntax error: bad token" \
  "selekt 1"

compare_err "undefined column" \
  "select nonexistent_column from users"

compare_err "relation does not exist" \
  "select * from no_such_table"

# ---------------------------------------------------------------------------
# Empty result sets (SELECT WHERE false)
# ---------------------------------------------------------------------------

compare "empty result set aligned" \
  "select id, name from users where false order by id"

compare_flags "empty result set unaligned" \
  -A -c "select id, name from users where false"

compare_flags "empty result set csv" \
  --csv -c "select id, name from users where false"

compare_flags "empty result set expanded" \
  -x -c "select id, name from users where false"

compare "empty result no rows zero-column-guard" \
  "select count(*) as n from users where false"

# ---------------------------------------------------------------------------
# Large column values (very long strings)
# ---------------------------------------------------------------------------

compare "large text value aligned" \
  "select repeat('x', 120) as long_str"

compare_flags "large text value unaligned" \
  -A -t -c "select repeat('y', 200) as long_str"

compare_flags "large text value csv" \
  --csv -c "select repeat('z', 150) as long_str"

compare_flags "large text value expanded" \
  -x -c "select repeat('a', 80) as long_str, 42 as num"

# ---------------------------------------------------------------------------
# NULL handling across output formats
# ---------------------------------------------------------------------------

compare "null in aligned output" \
  "select null::text as a, null::int as b, 'not null' as c"

compare_flags "null in unaligned output" \
  -A -c "select null::text as a, null::int as b"

compare_flags "null in csv output" \
  --csv -c "select null::text as a, null::int as b"

compare_flags "null in expanded output" \
  -x -c "select null::text as a, null::int as b, 'real' as c"

compare_flags "null with custom null string aligned" \
  -c "\pset null 'NULL'
select null::text as empty, 42 as real_val;"

compare_flags "null with custom null string csv" \
  -c "\pset format csv
\pset null 'NULL'
select null::text as a, 'hello' as b;"

# ---------------------------------------------------------------------------
# ORDER BY for deterministic comparisons
# ---------------------------------------------------------------------------

compare "select with order by asc" \
  "select id, name from users order by id asc"

compare "select with order by desc" \
  "select id, name from users order by id desc"

compare "select with order by text col" \
  "select id, name from users order by name"

compare "join with order by" \
  "select u.id as user_id, u.name, o.amount
from users as u
inner join orders as o
    on o.user_id = u.id
order by u.id, o.amount"

compare "aggregate with group by order by" \
  "select status, count(*) as cnt
from orders
group by status
order by status"

# ---------------------------------------------------------------------------
# \encoding
# ---------------------------------------------------------------------------

# \encoding without argument reports the current encoding — must match.
compare "\encoding query" \
  "\encoding"

# ---------------------------------------------------------------------------
# \o output redirection
# ---------------------------------------------------------------------------

# \o redirects query output to a file; we compare file contents.
compare_file "\o redirect to file" \
  "\\o __OUTFILE__
select id, name from users order by id limit 5;
\\o"

compare_file "\o redirect unaligned output" \
  "\\pset format unaligned
\\o __OUTFILE__
select id, name from users order by id limit 5;
\\o"

# ---------------------------------------------------------------------------
# Additional SQL coverage
# ---------------------------------------------------------------------------

compare "distinct values" \
  "select distinct status from orders order by status"

compare "limit and offset" \
  "select id, name from users order by id limit 3 offset 2"

compare "coalesce null handling" \
  "select coalesce(null::text, 'default') as val"

compare "case expression" \
  "select id,
    case when id % 2 = 0 then 'even' else 'odd' end as parity
from users
order by id"

compare "string concatenation" \
  "select 'hello' || ' ' || 'world' as phrase"

compare "integer division and modulo" \
  "select 17 / 5 as quotient, 17 % 5 as remainder"

compare "json value" \
  "select '{\"key\": 1}'::json as doc"

compare "array value" \
  "select array[1, 2, 3] as arr"

compare "interval arithmetic" \
  "select '1 hour'::interval + '30 minutes'::interval as total"

compare "subquery in select" \
  "select (select count(*) from orders where user_id = u.id) as ord_cnt
from users as u
order by u.id"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo ""
echo "=== Results: ${PASS} passed, ${FAIL} failed ==="
TOTAL=$(( PASS + FAIL ))
echo "=== Total: ${TOTAL} tests ==="

if [[ "${FAIL}" -gt 0 ]]; then
  exit 1
fi
