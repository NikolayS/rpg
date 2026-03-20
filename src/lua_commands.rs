//! Lua-based custom meta-command support for Rpg.
//!
//! Allows users to define custom backslash commands via Lua scripts placed in
//! `~/.config/rpg/commands/*.lua`.  Each script calls `rpg.register_command`
//! to declare its name, description, and handler function.
//!
//! ## Lua API exposed to scripts
//!
//! ```lua
//! local rpg = require("rpg")
//!
//! rpg.register_command({
//!     name = "my_cmd",
//!     description = "Does something useful",
//!     handler = function(args)
//!         local result = rpg.query("select version()")
//!         for _, row in ipairs(result.rows) do
//!             rpg.print(row[1])
//!         end
//!     end
//! })
//! ```
//!
//! ## API functions
//!
//! - `rpg.register_command(def)` — register a command definition table
//! - `rpg.query(sql)`           — execute a read-only query; return `{columns, rows}`
//! - `rpg.print(text)`          — print to stdout
//! - `rpg.pager(text)`          — print to stdout (pager wrapping done by Rpg)
//! - `rpg.version()`            — return the rpg version string
//! - `rpg.dbname()`             — return the current database name
//!
//! ## Feature gate
//!
//! Compiled only when the `lua` feature is enabled.  Without it the registry
//! is always empty and `execute_command` returns a feature-absent error.
//!
//! Copyright 2026

// ---------------------------------------------------------------------------
// Public types — always compiled
// ---------------------------------------------------------------------------

/// A registered custom meta-command loaded from a Lua script.
#[derive(Clone, Debug)]
pub struct CustomCommand {
    /// Command name without the leading backslash (e.g. `"bloat_check"`).
    pub name: String,
    /// Short one-line description shown by `\commands`.
    pub description: String,
    /// Absolute path of the Lua script that registered this command.
    #[allow(dead_code)]
    pub source_path: String,
}

// ---------------------------------------------------------------------------
// LuaRegistry — conditionally compiled
// ---------------------------------------------------------------------------

/// Registry of all successfully loaded custom commands.
///
/// In the feature-enabled build the Lua VM is kept alive for the duration of
/// the session so registered handler closures remain valid.  In the
/// feature-disabled build the struct is always empty.
#[cfg(feature = "lua")]
pub struct LuaRegistry {
    /// Registered commands, in discovery order.
    pub commands: Vec<CustomCommand>,
    /// Embedded Lua 5.4 VM — alive for the entire session.
    pub lua: mlua::Lua,
}

#[cfg(not(feature = "lua"))]
pub struct LuaRegistry {
    /// Always empty when Lua support is not compiled in.
    pub commands: Vec<CustomCommand>,
}

// ---------------------------------------------------------------------------
// Lua-enabled implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "lua")]
mod lua_impl {
    use std::sync::{Arc, Mutex};

    use mlua::{Function, Lua, Table, Value};

    use super::{CustomCommand, LuaRegistry};

    // -----------------------------------------------------------------------
    // Thread-local client pointer
    // -----------------------------------------------------------------------
    //
    // During a Lua handler call we need `rpg.query` to perform synchronous
    // Postgres queries.  Because mlua closures are `'static`, we cannot
    // capture `&Client` directly.  Instead we store the pointer in a
    // thread-local immediately before invoking Lua and clear it afterwards.
    //
    // The pointer is valid for the duration of the call because:
    //   - `dispatch_meta` holds `&Client` alive for the entire call.
    //   - Lua execution is synchronous — no other task sees the pointer.
    //
    // This is the standard pattern for passing non-'static context into
    // C-style callbacks.
    thread_local! {
        static CLIENT_PTR: std::cell::Cell<*const tokio_postgres::Client> =
            const { std::cell::Cell::new(std::ptr::null()) };
    }

    /// Set the thread-local client pointer before calling a Lua handler.
    ///
    /// # Safety
    ///
    /// `client` must remain valid and unaliased for the entire duration of
    /// the subsequent Lua handler call.  Always call [`clear_client_ptr`]
    /// once the Lua call returns.
    pub unsafe fn set_client_ptr(client: *const tokio_postgres::Client) {
        CLIENT_PTR.with(|c| c.set(client));
    }

    /// Clear the thread-local client pointer after a Lua handler returns.
    pub fn clear_client_ptr() {
        CLIENT_PTR.with(|c| c.set(std::ptr::null()));
    }

    // -----------------------------------------------------------------------
    // Command discovery directory
    // -----------------------------------------------------------------------

    fn commands_dir() -> Option<std::path::PathBuf> {
        dirs::config_dir().map(|d| d.join("rpg").join("commands"))
    }

    // -----------------------------------------------------------------------
    // Registry construction
    // -----------------------------------------------------------------------

    /// Scan `~/.config/rpg/commands/*.lua` and build a `LuaRegistry`.
    ///
    /// Scripts that fail to load are skipped with an error printed to stderr.
    pub fn load(dbname: &str) -> LuaRegistry {
        let lua = match Lua::new_with(mlua::StdLib::ALL_SAFE, mlua::LuaOptions::default()) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("rpg: lua: failed to initialise Lua 5.4 runtime: {e}");
                return LuaRegistry {
                    commands: Vec::new(),
                    lua: Lua::new(),
                };
            }
        };

        // Pending registrations collected by `rpg.register_command`.
        let pending: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let dbname_arc: Arc<Mutex<String>> = Arc::new(Mutex::new(dbname.to_owned()));

        if let Err(e) = install_rpg_module(&lua, &Arc::clone(&pending), &Arc::clone(&dbname_arc)) {
            eprintln!("rpg: lua: failed to install rpg API: {e}");
            return LuaRegistry {
                commands: Vec::new(),
                lua,
            };
        }

        let Some(script_dir) = commands_dir() else {
            return LuaRegistry {
                commands: Vec::new(),
                lua,
            };
        };

        let Ok(read_dir) = std::fs::read_dir(&script_dir) else {
            // Directory absent — silently return empty registry.
            return LuaRegistry {
                commands: Vec::new(),
                lua,
            };
        };

        let mut entries: Vec<std::path::PathBuf> = read_dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("lua"))
            .collect();
        entries.sort();

        let mut commands: Vec<CustomCommand> = Vec::new();

        for path in entries {
            let source_path = path.to_string_lossy().into_owned();
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("rpg: lua: skipping {source_path}: cannot read: {e}");
                    continue;
                }
            };

            let before = pending.lock().unwrap().len();
            if let Err(e) = lua.load(&src).set_name(&source_path).exec() {
                eprintln!("rpg: lua: skipping {source_path}: {e}");
                continue;
            }

            // Tag all newly-registered commands with this script's path.
            let snapshot = pending.lock().unwrap().clone();
            for (name, desc) in snapshot.into_iter().skip(before) {
                commands.push(CustomCommand {
                    name,
                    description: desc,
                    source_path: source_path.clone(),
                });
            }
        }

        LuaRegistry { commands, lua }
    }

    // -----------------------------------------------------------------------
    // rpg module installation
    // -----------------------------------------------------------------------

    /// Install the `rpg` global table and `require("rpg")` shim into the VM.
    ///
    /// The `pending` Arc receives `(name, description)` for each
    /// `rpg.register_command` call during script loading.
    /// The `dbname_arc` is read by `rpg.dbname()` at handler-call time.
    #[allow(clippy::too_many_lines)]
    fn install_rpg_module(
        lua: &Lua,
        pending: &Arc<Mutex<Vec<(String, String)>>>,
        dbname_arc: &Arc<Mutex<String>>,
    ) -> mlua::Result<()> {
        let rpg = lua.create_table()?;

        // rpg.register_command({ name, description, handler })
        // Metadata only — the handler is stored by a Lua-side shim below.
        {
            let pending_ref = Arc::clone(pending);
            let reg = lua.create_function(move |_lua, def: Table| {
                let name: String = def.get::<String>("name").unwrap_or_default();
                let desc: String = def.get::<String>("description").unwrap_or_default();
                if name.is_empty() {
                    return Err(mlua::Error::RuntimeError(
                        "rpg.register_command: 'name' is required".to_owned(),
                    ));
                }
                pending_ref.lock().unwrap().push((name, desc));
                Ok(())
            })?;
            rpg.set("register_command", reg)?;
        }

        // rpg.print(text)
        rpg.set(
            "print",
            lua.create_function(|_lua, text: String| {
                println!("{text}");
                Ok(())
            })?,
        )?;

        // rpg.pager(text) — identical output; paging handled by the caller.
        rpg.set(
            "pager",
            lua.create_function(|_lua, text: String| {
                println!("{text}");
                Ok(())
            })?,
        )?;

        // rpg.version()
        rpg.set(
            "version",
            lua.create_function(|_lua, ()| Ok(crate::version_string().to_owned()))?,
        )?;

        // rpg.dbname()
        {
            let dbname_ref = Arc::clone(dbname_arc);
            rpg.set(
                "dbname",
                lua.create_function(move |_lua, ()| Ok(dbname_ref.lock().unwrap().clone()))?,
            )?;
        }

        // rpg.query(sql) — executes a read-only Postgres query.
        //
        // Reads the thread-local client pointer set by `execute_command`.
        // The query is wrapped in a read-only transaction automatically.
        // Blocks the thread via `Handle::block_on` — caller must have already
        // entered `block_in_place` to make this safe on a multi-thread runtime.
        rpg.set(
            "query",
            lua.create_function(|lua, sql: String| {
                let client_ptr = CLIENT_PTR.with(std::cell::Cell::get);
                if client_ptr.is_null() {
                    return Err(mlua::Error::RuntimeError(
                        "rpg.query: no database connection available".to_owned(),
                    ));
                }
                let wrapped = format!("start transaction read only; {sql}; commit");

                // Block the current thread to await the async query.
                // Safe because the caller uses `tokio::task::block_in_place`.
                let result = tokio::runtime::Handle::current().block_on(async {
                    // SAFETY: pointer is valid for the duration of this
                    // synchronous Lua execution frame; set/cleared by
                    // `execute_command`.
                    let client: &tokio_postgres::Client = unsafe { &*client_ptr };
                    client.simple_query(&wrapped).await
                });

                match result {
                    Err(e) => Err(mlua::Error::RuntimeError(format!("rpg.query: {e}"))),
                    Ok(messages) => {
                        let result_tbl = lua.create_table()?;
                        let col_names_tbl = lua.create_table()?;
                        let rows_tbl = lua.create_table()?;
                        let mut col_names: Vec<String> = Vec::new();
                        let mut row_idx = 0usize;

                        for msg in &messages {
                            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                                if col_names.is_empty() {
                                    for (i, col) in row.columns().iter().enumerate() {
                                        let name = col.name().to_owned();
                                        col_names.push(name.clone());
                                        col_names_tbl.set(i + 1, name)?;
                                    }
                                }
                                let lua_row = lua.create_table()?;
                                for ci in 0..row.len() {
                                    match row.get(ci) {
                                        Some(v) => lua_row.set(ci + 1, v)?,
                                        None => {
                                            lua_row.set(ci + 1, Value::Nil)?;
                                        }
                                    }
                                }
                                row_idx += 1;
                                rows_tbl.set(row_idx, lua_row)?;
                            }
                        }

                        result_tbl.set("columns", col_names_tbl)?;
                        result_tbl.set("rows", rows_tbl)?;
                        Ok(result_tbl)
                    }
                }
            })?,
        )?;

        // _handlers: name → function, populated by the Lua-side shim below.
        rpg.set("_handlers", lua.create_table()?)?;

        lua.globals().set("rpg", rpg)?;

        // `require("rpg")` shim.
        lua.load(r#"package.preload["rpg"] = function() return rpg end"#)
            .exec()?;

        // Wrap `register_command` to also store the handler function.
        lua.load(
            r#"
local _reg_orig = rpg.register_command
rpg.register_command = function(def)
    _reg_orig(def)
    if type(def.handler) == "function" then
        rpg._handlers[def.name] = def.handler
    end
end
"#,
        )
        .exec()?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Handler invocation
    // -----------------------------------------------------------------------

    /// Update the `rpg.dbname()` return value before each handler call.
    pub fn update_dbname(lua: &Lua, dbname: &str) -> mlua::Result<()> {
        let dbname_owned = dbname.to_owned();
        let f = lua.create_function(move |_lua, ()| Ok(dbname_owned.clone()))?;
        let rpg: Table = lua.globals().get("rpg")?;
        rpg.set("dbname", f)?;
        Ok(())
    }

    /// Invoke the named handler with the given argument list.
    ///
    /// The thread-local client pointer **must** be set before calling this.
    pub fn invoke(lua: &Lua, name: &str, args: &[&str]) -> Result<(), String> {
        let rpg: Table = lua
            .globals()
            .get("rpg")
            .map_err(|e| format!("rpg: lua: {e}"))?;
        let handlers: Table = rpg
            .get("_handlers")
            .map_err(|e| format!("rpg: lua: _handlers: {e}"))?;
        let handler: Function = handlers
            .get::<Function>(name)
            .map_err(|_| format!("rpg: lua: no handler registered for \\{name}"))?;

        let args_tbl = lua.create_table().map_err(|e| format!("rpg: lua: {e}"))?;
        for (i, arg) in args.iter().enumerate() {
            args_tbl
                .set(i + 1, *arg)
                .map_err(|e| format!("rpg: lua: {e}"))?;
        }

        handler
            .call::<()>(args_tbl)
            .map_err(|e| format!("rpg: lua: \\{name}: {e}"))
    }
}

// ---------------------------------------------------------------------------
// LuaRegistry public API
// ---------------------------------------------------------------------------

impl LuaRegistry {
    /// Scan `~/.config/rpg/commands/*.lua` and build the registry.
    ///
    /// `dbname` is exposed to scripts via `rpg.dbname()`.
    pub fn load(dbname: &str) -> Self {
        #[cfg(feature = "lua")]
        {
            lua_impl::load(dbname)
        }
        #[cfg(not(feature = "lua"))]
        {
            let _ = dbname;
            LuaRegistry {
                commands: Vec::new(),
            }
        }
    }

    /// Find a registered command by name.
    pub fn get(&self, name: &str) -> Option<&CustomCommand> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// Execute a registered Lua command handler synchronously.
    ///
    /// This function **must** be called from within
    /// `tokio::task::block_in_place` so that the Lua → `rpg.query` →
    /// `block_on` chain does not deadlock the tokio runtime.
    ///
    /// `client` is the Postgres connection used by `rpg.query()` inside the
    /// handler.  It must remain valid and unaliased for the duration of the
    /// call.
    #[cfg(feature = "lua")]
    pub fn execute_command(
        &self,
        name: &str,
        args: &[&str],
        dbname: &str,
        client: &tokio_postgres::Client,
    ) -> Result<(), String> {
        lua_impl::update_dbname(&self.lua, dbname).map_err(|e| format!("rpg: lua: {e}"))?;

        // Safety: client lives for the duration of the `block_in_place` call
        // in `dispatch_meta`.  We clear the pointer before returning.
        unsafe {
            lua_impl::set_client_ptr(std::ptr::from_ref(client));
        }
        let result = lua_impl::invoke(&self.lua, name, args);
        lua_impl::clear_client_ptr();
        result
    }

    #[cfg(not(feature = "lua"))]
    pub fn execute_command(
        &self,
        _name: &str,
        _args: &[&str],
        _dbname: &str,
        _client: &tokio_postgres::Client,
    ) -> Result<(), String> {
        Err("rpg was built without Lua support \
             (recompile with --features lua)"
            .to_owned())
    }
}
