# Upstream patches for tokio-postgres

Target repo: https://github.com/rust-postgres/rust-postgres
Base version: tokio-postgres 0.7.16 (both patches also apply cleanly to 0.7.17)

## Patch 1: fix copy_in double-Sync protocol crash

**File:** `0001-fix-copy_in-double-sync.patch`
**Touches:** `tokio-postgres/src/copy_in.rs`, `tokio-postgres/src/query.rs`

**Bug:** `copy_in()` calls `query::encode()` which sends `Bind+Execute+Sync`.
Then `CopyInReceiver` sends another `Sync` with `CopyDone`. Two Syncs produce
two `ReadyForQuery` messages from the server. The second arrives when the
connection driver's response queue is empty, causing "unexpected message from
server" and killing the connection.

**Fix:**
- Add `encode_no_sync()` (Bind+Execute without Sync)
- `copy_in()` uses `encode_no_sync()` — only one Sync per cycle
- `CopyInReceiver` None branch always sends Sync (covers both error and
  success paths)

**Reproduction:** Run PostgreSQL's `copy2` regression test through a
tokio-postgres-based client — the double-Sync crashes the connection on
multi-statement COPY sequences.

## Patch 2: fix PathBuf unused-import warning on Windows

**File:** `0002-fix-pathbuf-cfg-windows.patch`
**Touches:** `tokio-postgres/src/client.rs` (1 line)

**Bug:** `use std::path::PathBuf` is guarded with `#[cfg(feature = "runtime")]`,
but `PathBuf` is only used in the `Unix(PathBuf)` variant which is
`#[cfg(unix)]`. On Windows with the `runtime` feature enabled and
`-D warnings`, this is an unused-import error.

**Fix:** Change `#[cfg(feature = "runtime")]` to `#[cfg(unix)]` on the import.

## Applying

```bash
cd rust-postgres
git apply ../0001-fix-copy_in-double-sync.patch
git apply ../0002-fix-pathbuf-cfg-windows.patch
```
