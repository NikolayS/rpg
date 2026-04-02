#!/usr/bin/env bash
set -Eeuo pipefail
DB="${1:-demo_saas}"
while true; do
  psql -d "${DB}" -c "select count(*) from orders o cross join (select 1 from generate_series(1,200)) g" >/dev/null 2>&1 &
  psql -d "${DB}" -c "select pg_advisory_lock(42); select pg_sleep(0.2); select pg_advisory_unlock(42)" >/dev/null 2>&1 &
  psql -d "${DB}" -c "select pg_advisory_lock(42); select pg_sleep(0.1); select pg_advisory_unlock(42)" >/dev/null 2>&1 &
  psql -d "${DB}" -c "begin; select pg_sleep(0.3); commit" >/dev/null 2>&1 &
  psql -d "${DB}" -c "select * from orders order by random() limit 1" >/dev/null 2>&1 &
  sleep 1.5
done
