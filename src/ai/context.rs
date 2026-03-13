//! Schema context builder for LLM prompts.
//!
//! Queries `pg_catalog` and formats a compact DDL stub (table + column names +
//! types + primary key markers) suitable for inclusion in an LLM system
//! prompt. Full constraint detail is intentionally omitted to keep the
//! context small.

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Schema context builder
// ---------------------------------------------------------------------------

/// Build a compact schema description for inclusion in an LLM system prompt.
///
/// Queries `pg_catalog` for all user tables and their columns, then formats
/// the result as `CREATE TABLE` stubs.  System schemas (`pg_catalog`,
/// `information_schema`, `pg_toast`) are excluded.
///
/// # Errors
///
/// Returns an error string if the catalog query fails.
pub async fn build_schema_context(client: &tokio_postgres::Client) -> Result<String, String> {
    let sql = r"
        select
            n.nspname as schema,
            c.relname as table_name,
            a.attname as column_name,
            pg_catalog.format_type(a.atttypid, a.atttypmod) as type,
            case when exists (
                select 1
                from pg_index as i
                join pg_attribute as ia
                    on ia.attrelid = i.indrelid
                    and ia.attnum = any(i.indkey)
                where
                    i.indrelid = c.oid
                    and i.indisprimary
                    and ia.attname = a.attname
            ) then true else false end as is_pk
        from pg_catalog.pg_class as c
        join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        join pg_catalog.pg_attribute as a
            on a.attrelid = c.oid
        where
            c.relkind in ('r', 'p')
            and a.attnum > 0
            and not a.attisdropped
            and n.nspname not in (
                'pg_catalog', 'information_schema', 'pg_toast'
            )
        order by
            n.nspname,
            c.relname,
            a.attnum
    ";

    let mut output = String::new();
    let mut current_table = String::new();

    for msg in client.simple_query(sql).await.map_err(|e| e.to_string())? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let schema = row.get(0).unwrap_or("");
            let table = row.get(1).unwrap_or("");
            let column = row.get(2).unwrap_or("");
            let type_name = row.get(3).unwrap_or("");
            let is_pk = row.get(4).unwrap_or("") == "t";

            let qualified = format!("{schema}.{table}");
            if qualified != current_table {
                if !current_table.is_empty() {
                    output.push_str(");\n");
                }
                // Infallible: writing to a String never returns an error.
                let _ = writeln!(output, "create table {qualified} (");
                current_table = qualified;
            }

            let pk_suffix = if is_pk { " primary key" } else { "" };
            let _ = writeln!(output, "  {column} {type_name}{pk_suffix},");
        }
    }

    if !current_table.is_empty() {
        output.push_str(");\n");
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Wait event context builder
// ---------------------------------------------------------------------------

/// SQL to collect current wait event breakdown from `pg_stat_activity`.
const WAIT_SNAPSHOT_SQL: &str = "\
    SELECT \
        coalesce(wait_event_type, 'CPU/Running') AS wait_type, \
        coalesce(wait_event, 'active') AS wait_event, \
        count(*) AS sessions \
    FROM pg_stat_activity \
    WHERE pid != pg_backend_pid() \
      AND backend_type = 'client backend' \
      AND state = 'active' \
    GROUP BY wait_event_type, wait_event \
    ORDER BY sessions DESC \
    LIMIT 15";

/// SQL to collect historical top waits from `pg_ash` (last 5 minutes).
const PG_ASH_RECENT_WAITS_SQL: &str = "\
    SELECT \
        wait_event_type, \
        wait_event, \
        count(*) AS samples \
    FROM ash.ash_log \
    WHERE sample_time > now() - interval '5 minutes' \
      AND wait_event IS NOT NULL \
    GROUP BY wait_event_type, wait_event \
    ORDER BY samples DESC \
    LIMIT 15";

/// Build a wait event context string for inclusion in LLM prompts.
///
/// When `pg_ash` is available, includes both a current snapshot from
/// `pg_stat_activity` and recent historical data from `ash.ash_log`.
/// Otherwise, only the current snapshot is included.
///
/// Returns an empty string if no wait data is found (quiet databases).
pub async fn build_wait_context(client: &tokio_postgres::Client, pg_ash_available: bool) -> String {
    let mut sections = Vec::new();

    // Current wait snapshot from pg_stat_activity.
    if let Ok(snapshot) = format_simple_query(client, WAIT_SNAPSHOT_SQL).await {
        if !snapshot.is_empty() {
            sections.push(format!(
                "Current wait events (pg_stat_activity snapshot):\n{snapshot}"
            ));
        }
    }

    // Historical wait data from pg_ash (if available).
    if pg_ash_available {
        if let Ok(history) = format_simple_query(client, PG_ASH_RECENT_WAITS_SQL).await {
            if !history.is_empty() {
                sections.push(format!(
                    "Recent wait events (pg_ash, last 5 minutes):\n{history}"
                ));
            }
        }
    }

    sections.join("\n\n")
}

/// Execute a query and format results as pipe-delimited text.
///
/// Returns the formatted output or an error string. Returns an empty
/// string when the query produces no rows.
async fn format_simple_query(client: &tokio_postgres::Client, sql: &str) -> Result<String, String> {
    let messages = client.simple_query(sql).await.map_err(|e| e.to_string())?;

    let mut headers: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();

    for msg in messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if headers.is_empty() {
                headers = (0..row.len())
                    .map(|i| {
                        row.columns()
                            .get(i)
                            .map_or_else(|| format!("col{i}"), |c| c.name().to_owned())
                    })
                    .collect();
            }
            let vals: Vec<String> = (0..row.len())
                .map(|i| row.get(i).unwrap_or("").to_owned())
                .collect();
            rows.push(vals);
        }
    }

    if rows.is_empty() {
        return Ok(String::new());
    }

    let mut out = headers.join(" | ");
    out.push('\n');
    for row in &rows {
        out.push_str(&row.join(" | "));
        out.push('\n');
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Unit tests (string-building logic, no DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    /// Simulate the string-building logic from `build_schema_context` with
    /// synthetic row data to verify the output format without a database.
    fn build_from_rows(rows: &[(&str, &str, &str, &str, bool)]) -> String {
        let mut output = String::new();
        let mut current_table = String::new();

        for &(schema, table, column, type_name, is_pk) in rows {
            let qualified = format!("{schema}.{table}");
            if qualified != current_table {
                if !current_table.is_empty() {
                    output.push_str(");\n");
                }
                let _ = writeln!(output, "create table {qualified} (");
                current_table = qualified;
            }
            let pk_suffix = if is_pk { " primary key" } else { "" };
            let _ = writeln!(output, "  {column} {type_name}{pk_suffix},");
        }

        if !current_table.is_empty() {
            output.push_str(");\n");
        }

        output
    }

    #[test]
    fn empty_schema_returns_empty_string() {
        let out = build_from_rows(&[]);
        assert!(out.is_empty());
    }

    #[test]
    fn single_table_single_column() {
        let rows = [("public", "users", "id", "bigint", true)];
        let out = build_from_rows(&rows);
        assert!(out.contains("create table public.users ("));
        assert!(out.contains("  id bigint primary key,"));
        assert!(out.ends_with(");\n"));
    }

    #[test]
    fn single_table_multiple_columns() {
        let rows = [
            ("public", "users", "id", "bigint", true),
            ("public", "users", "name", "text", false),
            ("public", "users", "email", "text", false),
        ];
        let out = build_from_rows(&rows);
        // Only one CREATE TABLE header.
        assert_eq!(out.matches("create table").count(), 1);
        assert!(out.contains("  id bigint primary key,"));
        assert!(out.contains("  name text,"));
        assert!(out.contains("  email text,"));
    }

    #[test]
    fn multiple_tables_produce_multiple_stubs() {
        let rows = [
            ("public", "orders", "id", "bigint", true),
            ("public", "orders", "total", "integer", false),
            ("public", "users", "id", "bigint", true),
        ];
        let out = build_from_rows(&rows);
        assert_eq!(out.matches("create table").count(), 2);
        assert!(out.contains("create table public.orders ("));
        assert!(out.contains("create table public.users ("));
    }

    #[test]
    fn non_pk_column_has_no_suffix() {
        let rows = [("public", "t", "col", "text", false)];
        let out = build_from_rows(&rows);
        assert!(out.contains("  col text,"));
        assert!(!out.contains("primary key"));
    }

    #[test]
    fn schema_prefix_included() {
        let rows = [("myschema", "mytable", "id", "int4", true)];
        let out = build_from_rows(&rows);
        assert!(out.contains("create table myschema.mytable ("));
    }

    #[test]
    fn wait_snapshot_sql_is_valid_syntax() {
        // Verify the SQL constant is non-empty and contains expected keywords.
        assert!(super::WAIT_SNAPSHOT_SQL.contains("pg_stat_activity"));
        assert!(super::WAIT_SNAPSHOT_SQL.contains("wait_event_type"));
        assert!(super::WAIT_SNAPSHOT_SQL.contains("GROUP BY"));
    }

    #[test]
    fn pg_ash_recent_waits_sql_is_valid_syntax() {
        assert!(super::PG_ASH_RECENT_WAITS_SQL.contains("ash.ash_log"));
        assert!(super::PG_ASH_RECENT_WAITS_SQL.contains("wait_event"));
        assert!(super::PG_ASH_RECENT_WAITS_SQL.contains("5 minutes"));
    }
}
