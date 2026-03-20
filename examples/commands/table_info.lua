-- table_info.lua — table health check for a given table.
--
-- Usage:
--   \table_info orders           -- info for public.orders
--   \table_info myschema.orders  -- info for a specific schema
--
-- Shows: row estimate, live/dead tuples, last vacuum/analyze timestamps,
-- table size, and index count.
--
-- Copyright 2026

local rpg = require("rpg")

rpg.register_command({
    name = "table_info",
    description = "Health check for a table: size, bloat, vacuum status",
    handler = function(args)
        local tbl = args[1]
        if not tbl or tbl == "" then
            rpg.print("Usage: \\table_info <table>  (e.g. \\table_info orders)")
            return
        end

        -- Split optional schema prefix.
        local schema, tablename
        local dot = string.find(tbl, "%.")
        if dot then
            schema    = string.sub(tbl, 1, dot - 1)
            tablename = string.sub(tbl, dot + 1)
        else
            schema    = "public"
            tablename = tbl
        end

        -- Main health-check query.
        local result = rpg.query(string.format([[
            select
                s.relname                              as table_name,
                s.n_live_tup                           as live_rows,
                s.n_dead_tup                           as dead_rows,
                s.n_mod_since_analyze                  as mod_since_analyze,
                coalesce(s.last_vacuum::text,   'never') as last_vacuum,
                coalesce(s.last_autovacuum::text, 'never') as last_autovacuum,
                coalesce(s.last_analyze::text,  'never') as last_analyze,
                pg_size_pretty(
                    pg_total_relation_size(
                        quote_ident(s.schemaname) || '.' || quote_ident(s.relname)
                    )
                )                                      as total_size,
                pg_size_pretty(
                    pg_relation_size(
                        quote_ident(s.schemaname) || '.' || quote_ident(s.relname)
                    )
                )                                      as table_size,
                pg_size_pretty(
                    pg_total_relation_size(
                        quote_ident(s.schemaname) || '.' || quote_ident(s.relname)
                    )
                    - pg_relation_size(
                        quote_ident(s.schemaname) || '.' || quote_ident(s.relname)
                    )
                )                                      as index_size,
                (
                    select count(*)
                    from pg_indexes as i
                    where
                        i.schemaname = s.schemaname
                        and i.tablename = s.relname
                )::text                                as index_count
            from pg_stat_user_tables as s
            where
                s.schemaname = '%s'
                and s.relname = '%s'
        ]], schema, tablename))

        rpg.print(string.format(
            "Table health: %s.%s on %s\n",
            schema, tablename, rpg.dbname()
        ))

        local rows = result.rows
        if not rows or not rows[1] then
            rpg.print(string.format(
                "Table '%s.%s' not found (or no statistics collected yet).",
                schema, tablename
            ))
            return
        end

        local cols = result.columns
        local row  = rows[1]
        for i = 1, #cols do
            rpg.print(string.format("%-22s %s", cols[i] .. ":", row[i] or "NULL"))
        end
    end,
})
