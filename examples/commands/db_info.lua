-- db_info.lua — print a quick summary of the connected database.
--
-- Usage:
--   \db_info
--
-- Prints the PostgreSQL version, current database name, current user,
-- database size, and connection count.
--
-- Copyright 2026

local rpg = require("rpg")

rpg.register_command({
    name = "db_info",
    description = "Print a quick summary of the connected database",
    handler = function(_args)
        rpg.print("Database: " .. rpg.dbname())
        rpg.print("rpg:      " .. rpg.version())
        rpg.print("")

        -- Server version and current user in one query.
        local info = rpg.query([[
            select
                version()                     as pg_version,
                current_user                  as current_user,
                pg_size_pretty(
                    pg_database_size(current_database())
                )                             as db_size,
                (
                    select count(*)
                    from pg_stat_activity
                    where datname = current_database()
                )::text                       as connections
        ]])

        if info.rows and info.rows[1] then
            local row = info.rows[1]
            local cols = info.columns
            for i = 1, #cols do
                rpg.print(string.format("%-16s %s", cols[i] .. ":", row[i] or ""))
            end
        end
    end,
})
