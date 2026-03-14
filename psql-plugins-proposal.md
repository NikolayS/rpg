# Proposal: Plugin System for psql

- **Author:** Nik Samokhvalov 
- **Status:** Draft
- **Target:** PostgreSQL hackers mailing list

---

## Problem

psql has no extension mechanism. Every enhancement — syntax highlighting, schema-aware completion, AI integration, DBA diagnostics, custom commands — requires either:

1. Patching psql's source and maintaining a fork
2. Building an entirely separate client (pgcli, rpg, etc.)
3. External wrapper scripts that lose psql's connection state

The PostgreSQL server has had `CREATE EXTENSION` since 9.1 (2011). Fifteen years later, the client has nothing equivalent. This forces fragmentation: useful features exist across a dozen incompatible tools instead of composing within the standard client.

## Proposal

Add a plugin API to psql that allows dynamically loaded shared libraries to:

1. Register custom commands (backslash or other prefix)
2. Hook into the query lifecycle (pre-execute, post-execute, on-error)
3. Extend tab completion
4. Access connection state and result sets

## Design Principles

1. **Zero overhead when unused.** No plugins loaded → identical behavior to current psql.
2. **Server extension model as precedent.** Mirror patterns PostgreSQL developers already know: `shared_preload_libraries`, hook functions, `_PG_init`.
3. **Minimal core changes.** The plugin API is a thin dispatch layer, not an architectural rewrite.
4. **Security-conscious.** Same trust model as server extensions — the DBA chooses what to load.

## Architecture

### Plugin Lifecycle

```
psql startup
  → read plugin_preload_libraries from .psqlrc
  → dlopen() each plugin .so/.dylib
  → call _psql_plugin_init(ctx) for each
  → plugins register commands, hooks, completers
  
psql running
  → user types \foo → dispatch to registered plugin command
  → user executes SQL → fire pre_query / post_query / on_error hooks
  → user presses Tab → call plugin completers after built-in completion

psql shutdown
  → call _psql_plugin_fini() for each plugin
  → dlclose()
```

### Plugin Entry Point

```c
/*
 * Every plugin exports this symbol, called once at load time.
 * The context struct provides registration functions.
 */
void _psql_plugin_init(PsqlPluginContext *ctx);

/* Optional cleanup. */
void _psql_plugin_fini(void);
```

### Registration API

```c
typedef struct PsqlPluginContext
{
    /* Identity */
    int         api_version;        /* PSQL_PLUGIN_API_VERSION */
    const char *psql_version;       /* e.g. "17.0" */
    
    /* Connection access (read-only by default) */
    PGconn     *conn;               /* current libpq connection */
    
    /* Registration functions */
    void (*register_command)(
        const char *name,           /* command name without prefix, e.g. "dba" */
        char        prefix,         /* '\\' for backslash, '/' for slash */
        PsqlPluginCmdFn handler,    /* callback */
        const char *help_short,     /* one-line help */
        const char *help_long       /* detailed help */
    );
    
    void (*register_hook)(
        PsqlHookPoint point,        /* PRE_QUERY, POST_QUERY, ON_ERROR, ON_CONNECT */
        PsqlPluginHookFn handler
    );
    
    void (*register_completer)(
        PsqlPluginCompleterFn handler,
        int priority                /* lower = called first */
    );
    
    /* Output functions (respect psql's current format settings) */
    void (*print_msg)(const char *fmt, ...);
    void (*print_table)(PGresult *res);     /* uses current \pset */
    
    /* Variable access */
    const char *(*get_variable)(const char *name);
    void (*set_variable)(const char *name, const char *value);
    
} PsqlPluginContext;
```

### Hook Points

```c
typedef enum PsqlHookPoint
{
    PSQL_HOOK_ON_CONNECT,       /* after connection established */
    PSQL_HOOK_ON_DISCONNECT,    /* before connection closes */
    PSQL_HOOK_PRE_QUERY,        /* before SQL is sent to server */
    PSQL_HOOK_POST_QUERY,       /* after results received */
    PSQL_HOOK_ON_ERROR,         /* after an error response */
    PSQL_HOOK_ON_RESULT         /* for each result set row group */
} PsqlHookPoint;

/* Hook callback signature */
typedef PsqlHookResult (*PsqlPluginHookFn)(
    PsqlHookPoint point,
    const char *query,          /* SQL text (NULL for connect/disconnect) */
    PGresult *result,           /* result set (NULL for pre_query) */
    void *private_data          /* plugin-private state */
);

typedef enum PsqlHookResult
{
    PSQL_HOOK_CONTINUE,         /* proceed normally */
    PSQL_HOOK_SKIP,             /* skip default processing (pre_query only) */
    PSQL_HOOK_HANDLED           /* plugin handled output, skip default display */
} PsqlHookResult;
```

### Command Handler Signature

```c
typedef bool (*PsqlPluginCmdFn)(
    PsqlPluginContext *ctx,
    const char *args,           /* everything after the command name */
    void *private_data
);
```

### Completer Extension

```c
/*
 * Called after psql's built-in completion.
 * Returns a NULL-terminated array of candidate strings, or NULL for none.
 * psql merges plugin candidates with its own.
 */
typedef char **(*PsqlPluginCompleterFn)(
    const char *text,           /* word being completed */
    const char *line,           /* full input line */
    int start,                  /* byte offset of word start */
    int end                     /* byte offset of cursor */
);
```

### Configuration

```
-- .psqlrc
\set plugin_preload_libraries '/usr/local/lib/psql/plugins/dba_diag.so,/usr/local/lib/psql/plugins/ai_assist.so'
```

Or via environment variable:
```bash
export PSQL_PLUGIN_PATH="/usr/local/lib/psql/plugins"
export PSQL_PRELOAD_PLUGINS="dba_diag,ai_assist"
```

### Plugin Discovery

```
~/.local/lib/psql/plugins/     # user plugins
/usr/local/lib/psql/plugins/   # system plugins
$PSQL_PLUGIN_PATH/             # custom path
```

Each plugin directory may contain:
```
dba_diag/
  dba_diag.so           # the shared library
  plugin.control        # metadata (name, version, description, min_psql_version)
```

## Example: DBA Diagnostics Plugin

```c
#include "psql_plugin.h"

static PsqlPluginContext *g_ctx;

static bool cmd_dba(PsqlPluginContext *ctx, const char *args, void *priv)
{
    if (strcmp(args, "activity") == 0)
    {
        PGresult *res = PQexec(ctx->conn,
            "SELECT pid, usename, state, wait_event_type, wait_event, "
            "left(query, 80) AS query "
            "FROM pg_stat_activity "
            "WHERE pid != pg_backend_pid() "
            "ORDER BY state, query_start");
        
        if (PQresultStatus(res) == PGRES_TUPLES_OK)
            ctx->print_table(res);
        else
            ctx->print_msg("Error: %s", PQerrorMessage(ctx->conn));
        
        PQclear(res);
        return true;
    }
    /* ... other subcommands ... */
    return false;
}

void _psql_plugin_init(PsqlPluginContext *ctx)
{
    g_ctx = ctx;
    ctx->register_command("dba", '\\', cmd_dba,
        "DBA diagnostic queries",
        "\\dba activity  — current connections and queries\n"
        "\\dba bloat     — table and index bloat estimates\n"
        "\\dba locks     — lock tree visualization\n"
        "\\dba vacuum    — vacuum status and dead tuples\n");
}
```

## Example: AI Assistant Plugin

```c
#include "psql_plugin.h"
#include <curl/curl.h>

static PsqlHookResult on_error_hook(
    PsqlHookPoint point,
    const char *query,
    PGresult *result,
    void *priv)
{
    const char *errmsg = PQresultErrorMessage(result);
    
    /* Call LLM API to suggest a fix */
    char *suggestion = call_llm_api(query, errmsg, get_schema_context());
    if (suggestion)
    {
        g_ctx->print_msg("\n💡 Suggestion: %s\n", suggestion);
        free(suggestion);
    }
    
    return PSQL_HOOK_CONTINUE;  /* still show the original error */
}

static bool cmd_ask(PsqlPluginContext *ctx, const char *args, void *priv)
{
    char *sql = nl_to_sql(args, get_schema_context());
    if (sql)
    {
        ctx->print_msg("Generated SQL:\n%s\n", sql);
        /* Optionally execute: PQexec(ctx->conn, sql) */
        free(sql);
    }
    return true;
}

void _psql_plugin_init(PsqlPluginContext *ctx)
{
    g_ctx = ctx;
    ctx->register_command("ask", '/', cmd_ask,
        "Ask AI to generate SQL from natural language", NULL);
    ctx->register_hook(PSQL_HOOK_ON_ERROR, on_error_hook);
}
```

## Implementation Plan

### Phase 1: Minimal Viable Plugin (target: PG 19 or 20)

- `dlopen`/`dlclose` infrastructure
- `_psql_plugin_init` / `_psql_plugin_fini`
- Custom command registration (backslash only)
- Read-only connection access
- `plugin_preload_libraries` in .psqlrc
- `print_msg` and `print_table` output functions

**Scope:** ~500-800 lines of C added to psql. Touches `command.c` (dispatch), `startup.c` (loading), new `plugin.c`/`plugin.h`.

### Phase 2: Lifecycle Hooks (PG 20 or 21)

- `PRE_QUERY` / `POST_QUERY` / `ON_ERROR` / `ON_CONNECT` hooks
- Hook chaining (multiple plugins, priority ordering)
- Variable get/set access

### Phase 3: Completion & UI (PG 21+)

- Completer extension API
- Syntax highlighting hooks
- Status bar / progress display hooks

## Security Considerations

**Same model as server extensions:**
- Loading a plugin requires filesystem access to place the `.so`
- No network-based plugin discovery or auto-download
- Plugins run in the psql process with full privileges
- `.psqlrc` already executes arbitrary SQL; plugins are no more dangerous
- Optional: `plugin.control` can declare required permissions (network, filesystem, write queries)

**What plugins CANNOT do:**
- Override psql's built-in commands (only register new ones)
- Modify query text in `PRE_QUERY` hook (read-only in Phase 1)
- Access other plugins' private state

## Threading / Async Concern

psql is single-threaded and uses blocking libpq calls. Plugins that make HTTP requests (AI APIs, monitoring webhooks) would block the terminal.

**Proposed solution:** Provide a non-blocking helper API:

```c
/* Plugin starts an async task. psql polls it during idle time. */
PsqlAsyncTask *(*start_async_task)(PsqlAsyncTaskFn fn, void *arg);
bool (*poll_async_task)(PsqlAsyncTask *task);
void *(*get_async_result)(PsqlAsyncTask *task);
```

Alternatively, leave threading to the plugin (plugins can spawn their own threads, with the caveat that libpq calls must stay on the main thread).

## Precedents

| System | Plugin Mechanism | Relevance |
|--------|-----------------|-----------|
| PostgreSQL server | `shared_preload_libraries` + hook system | Direct inspiration |
| Vim | `plugin/`, Vimscript + Lua | Transformed editor ecosystem |
| VS Code | Extension API (TypeScript) | Proved IDE plugins scale |
| Git | Custom commands via `git-foo` in PATH | Minimal, filesystem-based |
| GDB | Python scripting + pretty-printers | Added after 30 years, huge impact |
| SQLite | Loadable extensions (`sqlite3_load_extension`) | Closest analog in DB world |

## FAQ

**Q: Why not just use pgcli/rpg/other tools?**
A: Those are great but fragment the user base. A plugin system lets innovations compose within the standard tool that ships with PostgreSQL. It also means improvements can reach every psql user, not just those who discover and switch to alternative clients.

**Q: Won't this bloat psql?**
A: No. Zero plugins loaded = zero overhead. The plugin dispatch is a single function pointer check in the command handler. The core patch is ~500 lines.

**Q: Why shared libraries and not scripting (Lua, Python)?**
A: Shared libraries match the existing PostgreSQL extension model. A scripting layer could be built as a plugin itself (`psql_lua.so` that embeds Lua and provides a scripting bridge). This keeps the core minimal.

**Q: What about Windows?**
A: Use `LoadLibrary`/`GetProcAddress` instead of `dlopen`/`dlsym`. PostgreSQL server extensions already handle this portability.

---

## Next Steps

1. Gather feedback on this draft
2. Post to pgsql-hackers for discussion
3. Implement Phase 1 proof-of-concept against psql HEAD
4. Submit patch series for commitfest
5. Present at PGCon / PGConf EU

---

*DBLab Inc. — makers of rpg, the modern Postgres terminal.*
