#!/usr/bin/env bash
set -Eeuo pipefail

# Background workload for /ash demo recording.
# Generates mixed wait events so the timeline is colorful.
# Usage: bash demos/ash-cursor-workload.sh &

DB="${1:-demo_saas}"

while true; do
  # CPU-bound: hash join on large set
  psql -d "${DB}" -c "select count(*) from orders o1 cross join (select 1 from generate_series(1,500)) g" >/dev/null 2>&1 &

  # IO-bound: seq scan
  psql -d "${DB}" -c "select * from orders order by random() limit 1" >/dev/null 2>&1 &

  # Lock contention: advisory locks
  psql -d "${DB}" -c "select pg_advisory_lock(42); select pg_sleep(0.3); select pg_advisory_unlock(42)" >/dev/null 2>&1 &
  psql -d "${DB}" -c "select pg_advisory_lock(42); select pg_sleep(0.1); select pg_advisory_unlock(42)" >/dev/null 2>&1 &

  # LWLock: buffer activity
  psql -d "${DB}" -c "select count(*) from pg_class, pg_attribute where pg_class.oid = pg_attribute.attrelid" >/dev/null 2>&1 &

  # Idle in transaction
  psql -d "${DB}" -c "begin; select pg_sleep(0.5); commit" >/dev/null 2>&1 &

  # Short queries (Client wait)
  for _ in $(seq 1 5); do
    psql -d "${DB}" -c "select 1" >/dev/null 2>&1 &
  done

  sleep 0.8
done
