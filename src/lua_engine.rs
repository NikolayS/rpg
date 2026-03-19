// Copyright 2026 Rpg Authors
// SPDX-License-Identifier: Apache-2.0

//! Lua scripting engine for user-defined meta commands.
//!
//! Provides [`LuaEngine`] which initialises a Lua 5.4 VM, exposes a small
//! `rpg` API table, and lets users register custom backslash commands from
//! Lua scripts.  Scripts are loaded automatically from:
//!
//! - `~/.config/rpg/lua/*.lua`
//! - `.rpg/lua/*.lua` (project-local)
//!
//! Custom commands registered via `rpg.register_command(name, desc, callback)`
//! are dispatched by the REPL when the user types `\<name>`.

use std::collections::HashMap;
use std::path::Path;

use mlua::{Function, Lua, RegistryKey, Result as LuaResult};

/// A user-registered Lua command.
#[allow(dead_code)]
pub struct LuaCommand {
    /// Command name (without the leading `\`).
    pub name: String,
    /// Short description shown in help output.
    pub description: String,
    /// Registry key for the Lua callback function.
    pub callback_key: RegistryKey,
}

/// Lua scripting runtime.
///
/// Holds the Lua VM and a registry of user-defined commands.  The engine is
/// designed to be stored behind `Arc<Mutex<..>>` so it can be accessed from
/// both the REPL dispatch path and tab completion.
pub struct LuaEngine {
    lua: Lua,
    commands: HashMap<String, LuaCommand>,
    /// Accumulated output from `rpg.print()` calls during a callback.
    output_buffer: Vec<String>,
}

impl LuaEngine {
    /// Create a new engine and expose the `rpg` API table.
    ///
    /// # Errors
    ///
    /// Returns an error if the Lua VM cannot be initialised or if registering
    /// the API table fails.
    pub fn new() -> LuaResult<Self> {
        let lua = Lua::new();

        // Open standard libraries (string, table, math, etc.).
        // `Lua::new()` already loads the standard libraries by default.

        // Create the `rpg` API table.
        {
            let rpg = lua.create_table()?;

            // rpg.print(text) — buffer text for later display.
            let print_fn = lua.create_function(|lua_ctx, text: String| {
                // Store into the registry under a well-known key.
                let tbl: mlua::Table = lua_ctx.named_registry_value("_rpg_output")?;
                let len = tbl.raw_len();
                tbl.raw_set(len + 1, text)?;
                Ok(())
            })?;
            rpg.set("print", print_fn)?;

            // rpg.register_command(name, description, callback) — register a
            // custom backslash command.  The actual insertion into
            // `self.commands` happens in `load_script` after the chunk runs.
            // Here we just store pending registrations in a registry table.
            let register_fn =
                lua.create_function(|lua_ctx, (name, desc, cb): (String, String, Function)| {
                    let tbl: mlua::Table = lua_ctx.named_registry_value("_rpg_pending_cmds")?;
                    let entry = lua_ctx.create_table()?;
                    entry.set("name", name)?;
                    entry.set("desc", desc)?;
                    entry.set("cb", cb)?;
                    let len = tbl.raw_len();
                    tbl.raw_set(len + 1, entry)?;
                    Ok(())
                })?;
            rpg.set("register_command", register_fn)?;

            lua.globals().set("rpg", rpg)?;
        }

        // Initialise registry tables.
        let output_tbl = lua.create_table()?;
        lua.set_named_registry_value("_rpg_output", output_tbl)?;

        let pending_cmds = lua.create_table()?;
        lua.set_named_registry_value("_rpg_pending_cmds", pending_cmds)?;

        Ok(Self {
            lua,
            commands: HashMap::new(),
            output_buffer: Vec::new(),
        })
    }

    /// Load and execute a single Lua script file.
    ///
    /// Any commands registered via `rpg.register_command()` during execution
    /// are harvested and added to the command registry.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or contains Lua syntax
    /// errors.
    pub fn load_script(&mut self, path: &Path) -> LuaResult<()> {
        let source = std::fs::read_to_string(path).map_err(mlua::Error::external)?;
        let chunk_name = path.display().to_string();

        // Clear pending commands table before running.
        let fresh = self.lua.create_table()?;
        self.lua
            .set_named_registry_value("_rpg_pending_cmds", fresh)?;

        // Execute the script.
        self.lua.load(&source).set_name(&chunk_name).exec()?;

        // Harvest pending command registrations.
        let pending: mlua::Table = self.lua.named_registry_value("_rpg_pending_cmds")?;
        for pair in pending.sequence_values::<mlua::Table>() {
            let entry = pair?;
            let name: String = entry.get("name")?;
            let desc: String = entry.get("desc")?;
            let cb: Function = entry.get("cb")?;
            let key = self.lua.create_registry_value(cb)?;
            self.commands.insert(
                name.clone(),
                LuaCommand {
                    name,
                    description: desc,
                    callback_key: key,
                },
            );
        }

        Ok(())
    }

    /// Load all `*.lua` files from the standard directories.
    ///
    /// Directories that do not exist are silently skipped.  Errors in
    /// individual scripts are printed to stderr but do not prevent other
    /// scripts from loading.
    pub fn load_user_scripts(&mut self) {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();

        // ~/.config/rpg/lua/
        if let Some(config_dir) = dirs::config_dir() {
            dirs.push(config_dir.join("rpg").join("lua"));
        }

        // .rpg/lua/ (project-local)
        dirs.push(std::path::PathBuf::from(".rpg/lua"));

        for dir in &dirs {
            if !dir.is_dir() {
                continue;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let mut paths: Vec<std::path::PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "lua"))
                .collect();
            paths.sort();

            for path in paths {
                if let Err(e) = self.load_script(&path) {
                    eprintln!("rpg: lua: error loading {}: {e}", path.display());
                }
            }
        }
    }

    /// Execute an inline Lua string (from `\lua <code>`).
    ///
    /// Returns any text buffered via `rpg.print()`.
    ///
    /// # Errors
    ///
    /// Returns a Lua error if the code is invalid.
    pub fn exec_inline(&mut self, code: &str) -> LuaResult<Vec<String>> {
        self.clear_output_buffer()?;
        self.lua.load(code).set_name("\\lua").exec()?;
        self.drain_output_buffer()
    }

    /// Execute a Lua script file (from `\luafile <path>`).
    ///
    /// Returns any text buffered via `rpg.print()`.
    ///
    /// # Errors
    ///
    /// Returns a Lua error if the file cannot be read or contains errors.
    pub fn exec_file(&mut self, path: &str) -> LuaResult<Vec<String>> {
        let source = std::fs::read_to_string(path).map_err(mlua::Error::external)?;
        self.clear_output_buffer()?;
        self.lua.load(&source).set_name(path).exec()?;
        self.drain_output_buffer()
    }

    /// Invoke a registered custom command callback.
    ///
    /// `args` is the raw argument string passed after the command name.
    /// Returns `(output_lines, optional_sql)` — the callback may return a
    /// SQL string to be executed by the REPL.
    ///
    /// # Errors
    ///
    /// Returns a Lua error if the callback fails.
    pub fn call_command(&mut self, name: &str, args: &str) -> LuaResult<(Vec<String>, Option<String>)> {
        let cmd = self
            .commands
            .get(name)
            .ok_or_else(|| mlua::Error::external(format!("unknown lua command: {name}")))?;

        let cb: Function = self.lua.registry_value(&cmd.callback_key)?;
        self.clear_output_buffer()?;

        let result: mlua::Value = cb.call(args.to_owned())?;

        let sql = match result {
            mlua::Value::String(s) => Some(s.to_str()?.to_owned()),
            _ => None,
        };

        let output = self.drain_output_buffer()?;
        Ok((output, sql))
    }

    /// Check whether a command name is registered.
    #[allow(dead_code)]
    pub fn has_command(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    /// Return the names of all registered custom commands.
    pub fn command_names(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }

    /// Return a reference to the commands map.
    #[allow(dead_code)]
    pub fn commands(&self) -> &HashMap<String, LuaCommand> {
        &self.commands
    }

    // -- Internal helpers ---------------------------------------------------

    fn clear_output_buffer(&mut self) -> LuaResult<()> {
        self.output_buffer.clear();
        let fresh = self.lua.create_table()?;
        self.lua
            .set_named_registry_value("_rpg_output", fresh)?;
        Ok(())
    }

    fn drain_output_buffer(&mut self) -> LuaResult<Vec<String>> {
        let tbl: mlua::Table = self.lua.named_registry_value("_rpg_output")?;
        let mut lines = Vec::new();
        for v in tbl.sequence_values::<String>() {
            lines.push(v?);
        }
        // Clear for next call.
        let fresh = self.lua.create_table()?;
        self.lua
            .set_named_registry_value("_rpg_output", fresh)?;
        Ok(lines)
    }
}
