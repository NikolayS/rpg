-- bloat_check.lua — report table sizes for a schema or table pattern.
--
-- Usage:
--   \bloat_check              -- all tables in public schema
--   \bloat_check myschema     -- tables in the given schema
--   \bloat_check myschema.%   -- tables matching a LIKE pattern
--
-- Copyright 2026

local rpg = require("rpg")

rpg.register_command({
    name = "bloat_check",
    description = "Show top-10 largest tables (optionally filtered by schema)",
    handler = function(args)
        local schema = args[1] or "public"
        local sql = string.format([[
            select
                schemaname as schema,
                tablename  as table,
                pg_size_pretty(
                    pg_total_relation_size(
                        quote_ident(schemaname) || '.' || quote_ident(tablename)
                    )
                ) as total_size,
                pg_size_pretty(
                    pg_relation_size(
                        quote_ident(schemaname) || '.' || quote_ident(tablename)
                    )
                ) as table_size,
                pg_size_pretty(
                    pg_total_relation_size(
                        quote_ident(schemaname) || '.' || quote_ident(tablename)
                    )
                    - pg_relation_size(
                        quote_ident(schemaname) || '.' || quote_ident(tablename)
                    )
                ) as index_size
            from pg_tables
            where schemaname = '%s'
            order by
                pg_total_relation_size(
                    quote_ident(schemaname) || '.' || quote_ident(tablename)
                ) desc
            limit 10
        ]], schema)

        local result = rpg.query(sql)

        -- Print a simple header.
        rpg.print(string.format(
            "Top tables in schema '%s' on %s\n",
            schema, rpg.dbname()
        ))

        -- Column headers.
        local cols = result.columns
        if cols and #cols > 0 then
            local header = ""
            for i = 1, #cols do
                header = header .. string.format("%-20s", cols[i])
            end
            rpg.print(header)
            rpg.print(string.rep("-", 80))
        end

        -- Rows.
        local rows = result.rows
        if rows then
            for _, row in ipairs(rows) do
                local line = ""
                for i = 1, #row do
                    line = line .. string.format("%-20s", row[i] or "NULL")
                end
                rpg.print(line)
            end
        end
    end,
})
