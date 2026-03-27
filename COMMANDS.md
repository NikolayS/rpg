# COMMANDS.md — rpg command namespace convention

## Convention

- `\` — psql-compatible commands only. Any command psql supports uses `\`.
- `/` — rpg-specific extensions. All rpg-added commands use `/`.

**New rpg commands always use `/`. Never add new `\` commands that are not in psql.**

## Rationale

psql users expect `\` commands to behave exactly as in psql. Mixing rpg-specific
commands into the `\` namespace creates confusion and maintenance burden. The `/`
namespace is unambiguous: if it starts with `/`, it is an rpg extension.

## psql-compatible `\` commands (keep as-is)

These match psql behaviour and must not be changed:

`\a`, `\c`, `\C`, `\cd`, `\copy`, `\d`, `\D`, `\db`, `\dc`, `\df`, `\di`,
`\dl`, `\dn`, `\do`, `\dp`, `\ds`, `\dt`, `\dT`, `\du`, `\dv`, `\e`, `\echo`,
`\encoding`, `\f`, `\g`, `\gx`, `\h`, `\H`, `\i`, `\ir`, `\l`, `\lo_*`, `\o`,
`\p`, `\password`, `\prompt`, `\pset`, `\q`, `\qecho`, `\r`, `\s`, `\set`,
`\setenv`, `\sf`, `\sv`, `\t`, `\T`, `\timing`, `\unset`, `\w`, `\watch`,
`\x`, `\z`, `\!`, `\?`

## rpg-specific `/` commands

### AI

| Command | Description |
|---|---|
| `/ask <prompt>` | natural language to SQL |
| `/explain [query]` | explain the last (or given) query plan |
| `/fix` | diagnose and fix the last error |
| `/optimize [query]` | suggest query optimizations |
| `/describe <table>` | AI-generated table description |
| `/init` | generate `.rpg.toml` and `POSTGRES.md` |
| `/clear` | clear AI conversation context |
| `/compact [focus]` | compact conversation context |
| `/budget` | show token usage and remaining budget |

### DBA diagnostics

| Command | Description |
|---|---|
| `/dba [subcommand]` | database diagnostics (activity, locks, bloat, etc.) |
| `/ash [args]` | active session history (requires pg_ash extension) |

### Input/execution modes

| Command | Description |
|---|---|
| `/sql` | switch to SQL input mode (default) |
| `/text2sql` / `/t2s` | switch to text-to-SQL input mode |
| `/plan` | enter plan execution mode |
| `/yolo` | YOLO mode: text2sql + auto-execute |
| `/interactive` | return to interactive mode (default) |
| `/mode` | show current input and execution mode |

### Named queries

| Command | Description |
|---|---|
| `/ns <name> <query>` | save a named query |
| `/n <name> [args...]` | execute a named query |
| `/n+` | list all named queries |
| `/nd <name>` | delete a named query |
| `/np <name>` | print a named query without executing |

### REPL management

| Command | Description |
|---|---|
| `/profiles` | list configured connection profiles |
| `/session list` | show recent sessions |
| `/session save [name]` | save current session |
| `/session delete <id>` | delete a session |
| `/session resume <id>` | reconnect using a saved session |
| `/refresh` | reload schema cache for tab completion |
| `/log-file <path>` | start logging queries (no arg = stop) |
| `/explain-share <service>` | upload last EXPLAIN plan to external visualiser |
| `/commands` | list custom Lua meta-commands |
| `/version` | show rpg version and build information |
| `/f2` | toggle schema-aware tab completion |
| `/f3` | toggle single-line mode |
| `/f4` | toggle Vi/Emacs editing mode |
| `/f5` | toggle auto-EXPLAIN |

## Deprecated `\` aliases

The following rpg-specific commands were originally `\`-prefixed. They still
work but print a deprecation notice and will be removed in a future release.

| Deprecated | Use instead |
|---|---|
| `\dba` | `/dba` |
| `\sql` | `/sql` |
| `\text2sql` / `\t2s` | `/text2sql` / `/t2s` |
| `\mode` | `/mode` |
| `\plan` | `/plan` |
| `\yolo` | `/yolo` |
| `\interactive` | `/interactive` |
| `\profiles` | `/profiles` |
| `\refresh` | `/refresh` |
| `\session` | `/session` |
| `\log-file` | `/log-file` |
| `\explain share` | `/explain-share` |
| `\commands` | `/commands` |
| `\version` | `/version` |
| `\f2` / `\f3` / `\f4` / `\f5` | `/f2` / `/f3` / `/f4` / `/f5` |
| `\ns` / `\n` / `\n+` / `\nd` / `\np` | `/ns` / `/n` / `/n+` / `/nd` / `/np` |
