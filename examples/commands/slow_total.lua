-- slow_total.lua — top queries by total execution time.
--
-- Usage:
--   \slow_total        -- top 10 queries by total execution time
--   \slow_total 20     -- top N queries
--
-- Requires pg_stat_statements extension:
--   create extension if not exists pg_stat_statements;
--
-- Copyright 2026

local rpg = require("rpg")

rpg.register_command({
    name = "slow_total",
    description = "Top 10 queries by total execution time",
    handler = function(args)
        local limit = args[1] or "10"
        local result = rpg.query(string.format([[
            select
                calls,
                round(total_exec_time::numeric, 0) as total_ms,
                round(mean_exec_time::numeric, 2) as avg_ms,
                left(query, 100) as query
            from pg_stat_statements
            order by total_exec_time desc
            limit %s
        ]], limit))

        rpg.print(string.format(
            "Top %s queries by total execution time on %s\n",
            limit, rpg.dbname()
        ))

        local cols = result.columns
        if cols and #cols > 0 then
            local header = ""
            for i = 1, #cols do
                if i < #cols then
                    header = header .. string.format("%-12s", cols[i])
                else
                    header = header .. cols[i]
                end
            end
            rpg.print(header)
            rpg.print(string.rep("-", 100))
        end

        local rows = result.rows
        if rows then
            for _, row in ipairs(rows) do
                local line = ""
                for i = 1, #row do
                    if i < #row then
                        line = line .. string.format("%-12s", row[i] or "NULL")
                    else
                        line = line .. (row[i] or "NULL")
                    end
                end
                rpg.print(line)
            end
        end
    end,
})
