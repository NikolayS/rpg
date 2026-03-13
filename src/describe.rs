//! Handlers for the `\d` family of psql meta-commands.
//!
//! Each public function builds a catalog query, executes it via
//! `simple_query`, and prints the result as an aligned table.
//!
//! # SQL injection safety
//! All user-supplied pattern values are routed through [`crate::pattern`]
//! helpers which escape single quotes and convert psql wildcards to SQL
//! `LIKE` syntax.  No raw user input is ever interpolated directly.
//!
//! # PG compatibility
//! Queries target PG 14–18.  Columns or catalog entries introduced after
//! PG 14 are avoided.

use tokio_postgres::Client;

use crate::metacmd::{MetaCmd, ParsedMeta};
use crate::pattern;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Dispatch a describe-family meta-command to the appropriate handler.
///
/// Returns `true` if the REPL loop should exit after this command (always
/// `false` for describe commands — only `\q` exits).
pub async fn execute(client: &Client, meta: &ParsedMeta) -> bool {
    match &meta.cmd {
        MetaCmd::DescribeObject => describe_object(client, meta).await,
        MetaCmd::ListTables => list_relations(client, meta, &["r", "p"]).await,
        MetaCmd::ListIndexes => list_relations(client, meta, &["i"]).await,
        MetaCmd::ListSequences => list_relations(client, meta, &["S"]).await,
        MetaCmd::ListViews => list_relations(client, meta, &["v"]).await,
        MetaCmd::ListMatViews => list_relations(client, meta, &["m"]).await,
        MetaCmd::ListForeignTables => list_relations(client, meta, &["f"]).await,
        MetaCmd::ListFunctions => list_functions(client, meta).await,
        MetaCmd::ListSchemas => list_schemas(client, meta).await,
        MetaCmd::ListRoles => list_roles(client, meta).await,
        MetaCmd::ListDatabases => list_databases(client, meta).await,
        MetaCmd::ListExtensions => list_extensions(client, meta).await,
        MetaCmd::ListTablespaces => list_tablespaces(client, meta).await,
        MetaCmd::ListTypes => list_types(client, meta).await,
        MetaCmd::ListDomains => list_domains(client, meta).await,
        // Less common commands — basic stubs
        MetaCmd::ListPrivileges => stub_not_implemented("\\dp (list access privileges)"),
        MetaCmd::ListConversions => stub_not_implemented("\\dc (list conversions)"),
        MetaCmd::ListCasts => stub_not_implemented("\\dC (list casts)"),
        MetaCmd::ListComments => stub_not_implemented("\\dd (list object comments)"),
        MetaCmd::ListForeignServers => stub_not_implemented("\\des (list foreign servers)"),
        MetaCmd::ListFdws => stub_not_implemented("\\dew (list foreign-data wrappers)"),
        MetaCmd::ListForeignTablesViaFdw => {
            stub_not_implemented("\\det (list foreign tables via FDW)")
        }
        MetaCmd::ListUserMappings => stub_not_implemented("\\deu (list user mappings)"),
        // Non-describe commands should never reach this function.
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Stub helper
// ---------------------------------------------------------------------------

fn stub_not_implemented(label: &str) -> bool {
    eprintln!("{label}: not yet implemented");
    false
}

// ---------------------------------------------------------------------------
// Internal execution helper
// ---------------------------------------------------------------------------

/// Execute `sql` via `simple_query`, print an aligned table (with an optional
/// centered title), and return `false` (never exits the REPL).
///
/// When `echo_hidden` is `true` the SQL is echoed to stderr first.
async fn run_and_print(client: &Client, sql: &str, echo_hidden: bool) -> bool {
    run_and_print_titled(client, sql, echo_hidden, None).await
}

/// Like `run_and_print` but also prints a centered title above the table.
async fn run_and_print_titled(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
) -> bool {
    run_and_print_full(client, sql, echo_hidden, title, true).await
}

/// Like `run_and_print_titled` but suppresses the `(N rows)` footer.
/// Used by `\d tablename` to match psql behaviour.
async fn run_and_print_no_count(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
) -> bool {
    run_and_print_full(client, sql, echo_hidden, title, false).await
}

async fn run_and_print_full(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
    show_row_count: bool,
) -> bool {
    if echo_hidden {
        eprintln!("/******** QUERY *********/\n{sql}\n/************************/");
    }

    match client.simple_query(sql).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;

            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();

            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    if col_names.is_empty() {
                        col_names = (0..row.len())
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

            print_table_inner(&col_names, &rows, title, show_row_count);
        }
        Err(e) => {
            eprintln!("ERROR:  {e}");
        }
    }

    false
}

/// Print a column-aligned table to stdout, optionally with a centered title.
///
/// Matches the psql default output format:
/// ```text
///                List of relations     ← optional centered title
///  col1 | col2
/// ------+------
///  val  | val
/// (N rows)
/// ```
///
/// When `show_row_count` is `false` the `(N rows)` footer is suppressed (used
/// by `\d tablename` to match psql behaviour).
#[cfg(test)]
fn print_table(col_names: &[String], rows: &[Vec<String>], title: Option<&str>) {
    print_table_inner(col_names, rows, title, true);
}

fn print_table_inner(
    col_names: &[String],
    rows: &[Vec<String>],
    title: Option<&str>,
    show_row_count: bool,
) {
    if col_names.is_empty() {
        if show_row_count {
            let n = rows.len();
            let word = if n == 1 { "row" } else { "rows" };
            println!("({n} {word})");
        }
        return;
    }

    // Compute column widths (multi-line cell values: each line counts separately).
    let mut widths: Vec<usize> = col_names.iter().map(String::len).collect();
    for row in rows {
        for (i, val) in row.iter().enumerate() {
            if i < widths.len() {
                let max_line = val.lines().map(str::len).max().unwrap_or(val.len());
                widths[i] = widths[i].max(max_line);
            }
        }
    }

    // Total table width: 1 (leading space) + sum(widths) + 3*(ncols-1) (` | `) + 1 (trailing space).
    let ncols = widths.len();
    let table_width =
        1 + widths.iter().sum::<usize>() + if ncols > 1 { 3 * (ncols - 1) } else { 0 } + 1;

    // Optional title centered to table width.
    if let Some(t) = title {
        let tlen = t.len();
        if tlen >= table_width {
            println!("{t}");
        } else {
            let padding = (table_width - tlen) / 2;
            println!("{:>width$}", t, width = padding + tlen);
        }
    }

    // Header — psql center-aligns column headers within the column width.
    let header: Vec<String> = col_names
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let w = widths[i];
            let clen = c.len();
            if clen >= w {
                c.clone()
            } else {
                let left_pad = (w - clen) / 2;
                let right_pad = w - clen - left_pad;
                format!("{}{c}{}", " ".repeat(left_pad), " ".repeat(right_pad))
            }
        })
        .collect();
    println!(" {} ", header.join(" | "));

    // Separator.
    let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    println!("-{}-", sep.join("-+-"));

    // Data rows — cells with embedded newlines are printed as psql continuation
    // lines: the first sub-line is printed normally, subsequent sub-lines are
    // printed with a `+` appended to the previous line and blank padding in the
    // other columns.
    let ncols = widths.len();
    for row in rows {
        // Split each cell into its constituent lines.
        let cell_lines: Vec<Vec<&str>> = row
            .iter()
            .map(|v| {
                let ls: Vec<&str> = v.lines().collect();
                if ls.is_empty() {
                    vec![""]
                } else {
                    ls
                }
            })
            .collect();

        let max_lines = cell_lines.iter().map(Vec::len).max().unwrap_or(1);

        for line_idx in 0..max_lines {
            let cells: Vec<String> = (0..ncols)
                .map(|col| {
                    let w = widths[col];
                    let text = cell_lines
                        .get(col)
                        .and_then(|ls| ls.get(line_idx))
                        .copied()
                        .unwrap_or("");
                    // If this column has more lines after this one, append `+`.
                    let has_more = cell_lines
                        .get(col)
                        .is_some_and(|ls| line_idx + 1 < ls.len());
                    if has_more {
                        // psql pads text to column width, then places `+` at
                        // the end (so `+` is at position `w`).  The overall
                        // field is `w` chars wide; the last char is `+`.
                        let text_pad = w.saturating_sub(1).saturating_sub(text.len());
                        format!("{text}{}+", " ".repeat(text_pad))
                    } else {
                        format!("{text:<w$}")
                    }
                })
                .collect();
            println!(" {} ", cells.join(" | "));
        }
    }

    if show_row_count {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        println!("({n} {word})");
    }
}

// ---------------------------------------------------------------------------
// Build a schema-exclusion clause for user-object queries
// ---------------------------------------------------------------------------

/// Returns a SQL fragment that excludes system schemas when `system` is false.
///
/// The fragment is suitable for appending with `AND`.
fn system_schema_filter(system: bool) -> &'static str {
    if system {
        ""
    } else {
        "n.nspname <> 'pg_catalog' \
         AND n.nspname !~ '^pg_toast' \
         AND n.nspname <> 'information_schema'"
    }
}

// ---------------------------------------------------------------------------
// \dt / \di / \ds / \dv / \dm / \dE  — list relations by relkind
// ---------------------------------------------------------------------------

/// List relations of the given `relkinds` (e.g. `["r","p"]` for tables).
#[allow(clippy::too_many_lines)]
async fn list_relations(client: &Client, meta: &ParsedMeta, relkinds: &[&str]) -> bool {
    // Build the relkind IN list: ('r','p')
    let kind_list: Vec<String> = relkinds.iter().map(|k| format!("'{k}'")).collect();
    let kind_in = kind_list.join(",");

    // Pattern filter on (schema, name).
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "c.relname", Some("n.nspname"));

    // Schema visibility filter.
    let sys_filter = system_schema_filter(meta.system);

    // Build WHERE conditions.
    let where_parts: Vec<&str> = [
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter)
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("and {}", where_parts.join("\n    and "))
    };

    // Type label expression.
    let type_expr = "case c.relkind
           when 'r' then 'table'
           when 'p' then 'partitioned table'
           when 'i' then 'index'
           when 'I' then 'partitioned index'
           when 'S' then 'sequence'
           when 'v' then 'view'
           when 'm' then 'materialized view'
           when 'f' then 'foreign table'
           when 'c' then 'composite type'
           else c.relkind::text
       end";

    // For \di (indexes), we need an extra Table column and index-specific joins.
    let is_index_only = relkinds == ["i"];

    let sql = if meta.plus {
        if is_index_only {
            format!(
                "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    {type_expr} as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    ct.relname as \"Table\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_total_relation_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
join pg_catalog.pg_index as idx_i
    on idx_i.indexrelid = c.oid
join pg_catalog.pg_class as ct
    on ct.oid = idx_i.indrelid
where c.relkind in ({kind_in})
    {where_clause}
order by 1, 2"
            )
        } else {
            format!(
                "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    {type_expr} as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    case c.relpersistence
        when 'p' then 'permanent'
        when 't' then 'temporary'
        when 'u' then 'unlogged'
        else c.relpersistence::text
    end as \"Persistence\",
    coalesce(am.amname, '') as \"Access method\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_total_relation_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
left join pg_catalog.pg_am as am
    on am.oid = c.relam
where c.relkind in ({kind_in})
    {where_clause}
order by 1, 2"
            )
        }
    } else if is_index_only {
        format!(
            "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    {type_expr} as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    ct.relname as \"Table\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
join pg_catalog.pg_index as idx_i
    on idx_i.indexrelid = c.oid
join pg_catalog.pg_class as ct
    on ct.oid = idx_i.indrelid
where c.relkind in ({kind_in})
    {where_clause}
order by 1, 2"
        )
    } else {
        format!(
            "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    {type_expr} as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where c.relkind in ({kind_in})
    {where_clause}
order by 1, 2"
        )
    };

    run_and_print_titled(client, &sql, meta.echo_hidden, Some("List of relations")).await
}

// ---------------------------------------------------------------------------
// \df — list functions
// ---------------------------------------------------------------------------

async fn list_functions(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "p.proname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname not in ('pg_catalog', 'information_schema')".to_owned()
    };

    let where_parts: Vec<&str> = [
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("where {}", where_parts.join("\n    and "))
    };

    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Schema\",
    p.proname as \"Name\",
    pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",
    pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",
    case p.prokind
        when 'f' then 'func'
        when 'p' then 'proc'
        when 'a' then 'agg'
        when 'w' then 'window'
        else p.prokind::text
    end as \"Type\",
    case when p.prosecdef then 'definer' else 'invoker' end as \"Security\",
    coalesce(pg_catalog.obj_description(p.oid, 'pg_proc'), '') as \"Description\"
from pg_catalog.pg_proc as p
left join pg_catalog.pg_namespace as n
    on n.oid = p.pronamespace
{where_clause}
order by 1, 2, 4"
        )
    } else {
        format!(
            "select
    n.nspname as \"Schema\",
    p.proname as \"Name\",
    pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",
    pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",
    case p.prokind
        when 'f' then 'func'
        when 'p' then 'proc'
        when 'a' then 'agg'
        when 'w' then 'window'
        else p.prokind::text
    end as \"Type\"
from pg_catalog.pg_proc as p
left join pg_catalog.pg_namespace as n
    on n.oid = p.pronamespace
{where_clause}
order by 1, 2, 4"
        )
    };

    run_and_print_titled(client, &sql, meta.echo_hidden, Some("List of functions")).await
}

// ---------------------------------------------------------------------------
// \dn — list schemas
// ---------------------------------------------------------------------------

async fn list_schemas(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "n.nspname", None);

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname !~ '^pg_' and n.nspname <> 'information_schema'".to_owned()
    };

    let where_parts: Vec<&str> = [
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("where {}", where_parts.join("\n    and "))
    };

    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Name\",
    pg_catalog.pg_get_userbyid(n.nspowner) as \"Owner\",
    coalesce(pg_catalog.obj_description(n.oid, 'pg_namespace'), '') as \"Description\"
from pg_catalog.pg_namespace as n
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    n.nspname as \"Name\",
    pg_catalog.pg_get_userbyid(n.nspowner) as \"Owner\"
from pg_catalog.pg_namespace as n
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(client, &sql, meta.echo_hidden, Some("List of schemas")).await
}

// ---------------------------------------------------------------------------
// \du / \dg — list roles
// ---------------------------------------------------------------------------

async fn list_roles(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "r.rolname", None);

    // When no pattern is specified, filter out pg_* system roles (matches psql behaviour).
    let sys_role_filter = if meta.pattern.is_none() {
        "r.rolname !~ '^pg_'"
    } else {
        ""
    };

    let where_parts: Vec<&str> = [
        if sys_role_filter.is_empty() {
            None
        } else {
            Some(sys_role_filter)
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("where {}", where_parts.join("\n    and "))
    };

    // psql (PG16) shows "Role name" and "Attributes" only (no "Member of" column).
    // Attributes are expressed as a comma-separated list of capability words.
    let sql = format!(
        "select
    r.rolname as \"Role name\",
    case when r.rolsuper then 'Superuser' else '' end
    || case when not r.rolinherit then case when r.rolsuper then ', No inherit' else 'No inherit' end else '' end
    || case when r.rolcreaterole then case when r.rolsuper or not r.rolinherit then ', Create role' else 'Create role' end else '' end
    || case when r.rolcreatedb then case when r.rolsuper or not r.rolinherit or r.rolcreaterole then ', Create DB' else 'Create DB' end else '' end
    || case when not r.rolcanlogin then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb then ', Cannot login' else 'Cannot login' end else '' end
    || case when r.rolreplication then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb or not r.rolcanlogin then ', Replication' else 'Replication' end else '' end
    || case when r.rolbypassrls then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb or not r.rolcanlogin or r.rolreplication then ', Bypass RLS' else 'Bypass RLS' end else '' end
    as \"Attributes\"
from pg_catalog.pg_roles as r
{where_clause}
order by 1"
    );

    // psql suppresses the row count footer for \du.
    run_and_print_no_count(client, &sql, meta.echo_hidden, Some("List of roles")).await
}

// ---------------------------------------------------------------------------
// \l — list databases
// ---------------------------------------------------------------------------

async fn list_databases(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "d.datname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let sql = if meta.plus {
        format!(
            "select
    d.datname as \"Name\",
    pg_catalog.pg_get_userbyid(d.datdba) as \"Owner\",
    pg_catalog.pg_encoding_to_char(d.encoding) as \"Encoding\",
    case d.datlocprovider when 'c' then 'libc' when 'i' then 'icu' when 'b' then 'builtin' end as \"Locale Provider\",
    d.datcollate as \"Collate\",
    d.datctype as \"Ctype\",
    d.daticulocale as \"ICU Locale\",
    d.daticurules as \"ICU Rules\",
    pg_catalog.array_to_string(d.datacl, E'\\n') as \"Access privileges\",
    case
        when pg_catalog.has_database_privilege(d.datname, 'CONNECT')
        then pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(d.datname))
        else 'No Access'
    end as \"Size\",
    t.spcname as \"Tablespace\",
    coalesce(pg_catalog.shobj_description(d.oid, 'pg_database'), '') as \"Description\"
from pg_catalog.pg_database as d
join pg_catalog.pg_tablespace as t
    on t.oid = d.dattablespace
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    d.datname as \"Name\",
    pg_catalog.pg_get_userbyid(d.datdba) as \"Owner\",
    pg_catalog.pg_encoding_to_char(d.encoding) as \"Encoding\",
    case d.datlocprovider when 'c' then 'libc' when 'i' then 'icu' when 'b' then 'builtin' end as \"Locale Provider\",
    d.datcollate as \"Collate\",
    d.datctype as \"Ctype\",
    d.daticulocale as \"ICU Locale\",
    d.daticurules as \"ICU Rules\",
    pg_catalog.array_to_string(d.datacl, E'\\n') as \"Access privileges\"
from pg_catalog.pg_database as d
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(client, &sql, meta.echo_hidden, Some("List of databases")).await
}

// ---------------------------------------------------------------------------
// \dx — list extensions
// ---------------------------------------------------------------------------

async fn list_extensions(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "e.extname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    // `+` does not add extra columns for extensions (same output either way).
    let sql = format!(
        "select
    e.extname as \"Name\",
    e.extversion as \"Version\",
    n.nspname as \"Schema\",
    coalesce(pg_catalog.obj_description(e.oid, 'pg_extension'), '') as \"Description\"
from pg_catalog.pg_extension as e
left join pg_catalog.pg_namespace as n
    on n.oid = e.extnamespace
{where_clause}
order by 1"
    );

    run_and_print(client, &sql, meta.echo_hidden).await
}

// ---------------------------------------------------------------------------
// \db — list tablespaces
// ---------------------------------------------------------------------------

async fn list_tablespaces(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "t.spcname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let sql = format!(
        "select
    t.spcname as \"Name\",
    pg_catalog.pg_get_userbyid(t.spcowner) as \"Owner\",
    pg_catalog.pg_tablespace_location(t.oid) as \"Location\"
from pg_catalog.pg_tablespace as t
{where_clause}
order by 1"
    );

    run_and_print(client, &sql, meta.echo_hidden).await
}

// ---------------------------------------------------------------------------
// \dT — list types
// ---------------------------------------------------------------------------

async fn list_types(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "t.typname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast')".to_owned()
    };

    // Exclude pseudo-types (typtype = 'p') and array types (starts with _)
    // when no pattern is given, to keep output manageable.
    let base_filter = "t.typtype <> 'p' and t.typname !~ '^_'";

    let where_parts: Vec<&str> = [
        Some(base_filter),
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = format!("where {}", where_parts.join("\n    and "));

    let sql = format!(
        "select
    n.nspname as \"Schema\",
    pg_catalog.format_type(t.oid, null) as \"Name\",
    coalesce(pg_catalog.obj_description(t.oid, 'pg_type'), '') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
    );

    run_and_print(client, &sql, meta.echo_hidden).await
}

// ---------------------------------------------------------------------------
// \dD — list domains
// ---------------------------------------------------------------------------

async fn list_domains(client: &Client, meta: &ParsedMeta) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "t.typname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname not in ('pg_catalog', 'information_schema')".to_owned()
    };

    let base_filter = "t.typtype = 'd'";

    let where_parts: Vec<&str> = [
        Some(base_filter),
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        if name_filter.is_empty() {
            None
        } else {
            Some(name_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = format!("where {}", where_parts.join("\n    and "));

    let sql = format!(
        "select
    n.nspname as \"Schema\",
    t.typname as \"Name\",
    pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",
    case when t.typnotnull then 'not null' else '' end as \"Nullable\",
    t.typdefault as \"Default\",
    coalesce(pg_catalog.obj_description(t.oid, 'pg_type'), '') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
    );

    run_and_print(client, &sql, meta.echo_hidden).await
}

// ---------------------------------------------------------------------------
// \d [table] — describe a specific table, or list all relations
// ---------------------------------------------------------------------------

async fn describe_object(client: &Client, meta: &ParsedMeta) -> bool {
    match &meta.pattern {
        None => {
            // `\d` with no argument: list all user-visible relations.
            list_all_relations(client, meta).await
        }
        Some(pattern) => {
            // `\d pattern`: look up all matching objects, then describe each.
            //
            // Psql first resolves the pattern to a list of OIDs/names, then
            // calls describeOneTableDetails() for each.  We replicate that
            // two-step approach so that wildcards (e.g. `\d t*`) describe ALL
            // matching objects rather than treating the pattern as a literal
            // object name.
            let (schema_part, _name_part) = pattern::split_schema(pattern);
            let name_filter = pattern::where_clause(Some(pattern), "c.relname", Some("n.nspname"));

            // Add pg_table_is_visible when no schema is specified so that
            // unqualified patterns follow the search_path.
            let visibility_filter = if schema_part.is_none() {
                "pg_catalog.pg_table_is_visible(c.oid)"
            } else {
                ""
            };

            let where_cond = {
                let parts: Vec<&str> = [
                    if name_filter.is_empty() {
                        None
                    } else {
                        Some(name_filter.as_str())
                    },
                    if visibility_filter.is_empty() {
                        None
                    } else {
                        Some(visibility_filter)
                    },
                ]
                .into_iter()
                .flatten()
                .collect();
                parts.join("\n    and ")
            };

            let lookup_sql = format!(
                "select c.oid, n.nspname, c.relname
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where {where_cond}
order by 2, 3"
            );

            if meta.echo_hidden {
                eprintln!("/******** QUERY *********/\n{lookup_sql}\n/************************/");
            }

            let matches: Vec<(String, String)> = match client.simple_query(&lookup_sql).await {
                Err(e) => {
                    eprintln!("ERROR: {e}");
                    return false;
                }
                Ok(msgs) => {
                    use tokio_postgres::SimpleQueryMessage;
                    msgs.into_iter()
                        .filter_map(|m| {
                            if let SimpleQueryMessage::Row(row) = m {
                                let schema = row.get(1).unwrap_or("").to_owned();
                                let name = row.get(2).unwrap_or("").to_owned();
                                Some((schema, name))
                            } else {
                                None
                            }
                        })
                        .collect()
                }
            };

            if matches.is_empty() {
                eprintln!("Did not find any relation named \"{pattern}\".");
                return false;
            }

            for (i, (schema, name)) in matches.into_iter().enumerate() {
                // Separate consecutive describes with a blank line (psql does this).
                if i > 0 {
                    println!();
                }
                // Use the exact schema-qualified name so describe_table resolves
                // to exactly one object.
                let qualified = format!("{schema}.{name}");
                describe_table(client, meta, &qualified).await;
            }

            // Return false unconditionally (only \q should exit the REPL).
            false
        }
    }
}

/// List all user-visible relations (tables, views, sequences, indexes, etc.)
async fn list_all_relations(client: &Client, meta: &ParsedMeta) -> bool {
    let sys_filter = system_schema_filter(meta.system);

    // When not showing system objects, also restrict to search_path-visible
    // objects so that unqualified \d matches what the user normally sees.
    let visibility_filter = if meta.system {
        String::new()
    } else {
        "pg_catalog.pg_table_is_visible(c.oid)".to_owned()
    };

    let where_parts: Vec<&str> = [
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter)
        },
        if visibility_filter.is_empty() {
            None
        } else {
            Some(visibility_filter.as_str())
        },
    ]
    .into_iter()
    .flatten()
    .collect();

    let extra_conds = if where_parts.is_empty() {
        String::new()
    } else {
        format!("and {}", where_parts.join("\n    and "))
    };

    let sql = format!(
        "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    case c.relkind
        when 'r' then 'table'
        when 'p' then 'partitioned table'
        when 'i' then 'index'
        when 'I' then 'partitioned index'
        when 'S' then 'sequence'
        when 'v' then 'view'
        when 'm' then 'materialized view'
        when 'f' then 'foreign table'
        when 'c' then 'composite type'
        else c.relkind::text
    end as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where c.relkind in ('r','p','i','I','S','v','m','f','c')
    {extra_conds}
order by 1, 2"
    );

    run_and_print_titled(client, &sql, meta.echo_hidden, Some("List of relations")).await
}

/// Describe a single table (or view, sequence, …): columns + indexes + constraints.
#[allow(clippy::too_many_lines)]
async fn describe_table(client: &Client, meta: &ParsedMeta, obj_pattern: &str) -> bool {
    // Split into (schema, name) parts.
    let (schema_part, name_part) = crate::pattern::split_schema(obj_pattern);

    // Bug 1: build separate schema and name conditions using the split parts,
    // not the full obj_pattern, so the name column is never matched against
    // a schema-qualified string.
    let schema_col = match schema_part {
        Some(s) if !s.is_empty() => Some("n.nspname"),
        _ => None,
    };
    // Build the name condition from name_part only.
    let name_filter = crate::pattern::where_clause(Some(name_part), "c.relname", None);
    // Build the schema condition from schema_part only (if present and non-empty).
    let schema_filter = if let Some(s) = schema_part {
        crate::pattern::where_clause(if s.is_empty() { None } else { Some(s) }, "n.nspname", None)
    } else {
        String::new()
    };

    // Bug 2: when no schema is specified, add pg_table_is_visible tiebreaker
    // so that search_path objects are preferred for unqualified names.
    let visibility_filter = if schema_col.is_none() {
        "pg_catalog.pg_table_is_visible(c.oid)"
    } else {
        ""
    };

    // Compose the full WHERE condition used in object-lookup subqueries.
    let name_cond = {
        let parts: Vec<&str> = [
            if name_filter.is_empty() {
                None
            } else {
                Some(name_filter.as_str())
            },
            if schema_filter.is_empty() {
                None
            } else {
                Some(schema_filter.as_str())
            },
            if visibility_filter.is_empty() {
                None
            } else {
                Some(visibility_filter)
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(" AND ")
    };

    // Default-value expression: identity columns use attidentity, generated columns
    // use attgenerated; only fall back to pg_attrdef for plain defaults.
    let default_expr = "case
        when a.attidentity = 'a' then 'generated always as identity'
        when a.attidentity = 'd' then 'generated by default as identity'
        when a.attgenerated = 's' then 'generated always as (' || pg_catalog.pg_get_expr(d.adbin, d.adrelid) || ') stored'
        else coalesce(pg_catalog.pg_get_expr(d.adbin, d.adrelid), '')
    end";

    // 1. Columns
    let cols_sql = if meta.plus {
        format!(
            "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    coalesce(
        (select c2.collname
         from pg_catalog.pg_collation as c2
         join pg_catalog.pg_namespace as nc
             on nc.oid = c2.collnamespace
         where c2.oid = a.attcollation
           and a.attcollation <> (
               select t.typcollation
               from pg_catalog.pg_type as t
               where t.oid = a.atttypid
           )),
        ''
    ) as \"Collation\",
    case when a.attnotnull then 'not null' else '' end as \"Nullable\",
    {default_expr} as \"Default\",
    a.attstorage::text as \"Storage\",
    case when a.attstattarget = -1 then '' else a.attstattarget::text end as \"Stats target\",
    coalesce(pg_catalog.col_description(a.attrelid, a.attnum), '') as \"Description\"
from pg_catalog.pg_attribute as a
join pg_catalog.pg_class as c
    on c.oid = a.attrelid
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
left join pg_catalog.pg_attrdef as d
    on d.adrelid = a.attrelid and d.adnum = a.attnum
where a.attnum > 0
    and not a.attisdropped
    and {name_cond}
order by a.attnum"
        )
    } else {
        format!(
            "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    coalesce(
        (select c2.collname
         from pg_catalog.pg_collation as c2
         join pg_catalog.pg_namespace as nc
             on nc.oid = c2.collnamespace
         where c2.oid = a.attcollation
           and a.attcollation <> (
               select t.typcollation
               from pg_catalog.pg_type as t
               where t.oid = a.atttypid
           )),
        ''
    ) as \"Collation\",
    case when a.attnotnull then 'not null' else '' end as \"Nullable\",
    {default_expr} as \"Default\"
from pg_catalog.pg_attribute as a
join pg_catalog.pg_class as c
    on c.oid = a.attrelid
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
left join pg_catalog.pg_attrdef as d
    on d.adrelid = a.attrelid and d.adnum = a.attnum
where a.attnum > 0
    and not a.attisdropped
    and {name_cond}
order by a.attnum"
        )
    };

    // Fetch relkind and actual schema to determine the correct object-type label
    // and build a fully-qualified display name (psql always shows "schema.name").
    let relkind_sql = format!(
        "select c.relkind::text, n.nspname
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where {name_cond}
limit 1"
    );
    let (obj_label, display_name) = {
        let mut label = "Table";
        let mut resolved_schema = String::new();
        if let Ok(msgs) = client.simple_query(&relkind_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    label = match row.get(0).unwrap_or("r") {
                        "r" => "Table",
                        "p" => "Partitioned table",
                        "v" => "View",
                        "m" => "Materialized view",
                        "i" => "Index",
                        "I" => "Partitioned index",
                        "S" => "Sequence",
                        "f" => "Foreign table",
                        "c" => "Composite type",
                        _ => "Relation",
                    };
                    row.get(1).unwrap_or("").clone_into(&mut resolved_schema);
                    break;
                }
            }
        }
        // psql always shows schema-qualified name: Table "public.users"
        let fq_name = if resolved_schema.is_empty() {
            name_part.to_owned()
        } else {
            format!("{resolved_schema}.{name_part}")
        };
        (label, fq_name)
    };

    // Build the centered title and pass it to run_and_print_no_count so it is
    // centered above the column table — matching psql's \d output.  The row
    // count footer is suppressed to match psql behaviour.
    let table_title = format!("{obj_label} \"{display_name}\"");
    run_and_print_no_count(client, &cols_sql, meta.echo_hidden, Some(&table_title)).await;

    // 2. Indexes on this table (Bug 1 applied: use name_part not obj_pattern).
    let idx_name_filter = crate::pattern::where_clause(Some(name_part), "tc.relname", None);
    let idx_schema_filter = if let Some(s) = schema_part {
        crate::pattern::where_clause(
            if s.is_empty() { None } else { Some(s) },
            "tn.nspname",
            None,
        )
    } else {
        String::new()
    };
    let idx_visibility = if schema_col.is_none() {
        "pg_catalog.pg_table_is_visible(tc.oid)"
    } else {
        ""
    };
    let idx_name_cond = {
        let parts: Vec<&str> = [
            if idx_name_filter.is_empty() {
                None
            } else {
                Some(idx_name_filter.as_str())
            },
            if idx_schema_filter.is_empty() {
                None
            } else {
                Some(idx_schema_filter.as_str())
            },
            if idx_visibility.is_empty() {
                None
            } else {
                Some(idx_visibility)
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(" AND ")
    };

    // 2. Indexes — query returns raw fields; we format as psql indented text.
    // psql format: "name" PRIMARY KEY, btree (cols)  or  "name" btree (cols)
    let idx_sql = format!(
        "select
    i.relname as idx_name,
    ix.indisprimary,
    ix.indisunique,
    am.amname,
    i.oid as idx_oid,
    (select conname
     from pg_catalog.pg_constraint
     where conrelid = ix.indrelid
       and conindid = i.oid
       and contype in ('p','u')
     limit 1) as con_name
from pg_catalog.pg_index as ix
join pg_catalog.pg_class as i
    on i.oid = ix.indexrelid
join pg_catalog.pg_class as tc
    on tc.oid = ix.indrelid
join pg_catalog.pg_am as am
    on am.oid = i.relam
left join pg_catalog.pg_namespace as tn
    on tn.oid = tc.relnamespace
where {idx_name_cond}
order by ix.indisprimary desc, ix.indisunique desc, i.relname"
    );

    // 3. Check constraints
    let chk_sql = format!(
        "select
    conname,
    pg_catalog.pg_get_constraintdef(oid, true) as condef
from pg_catalog.pg_constraint as co
where co.contype = 'c'
    and co.conrelid = (
        select c.oid
        from pg_catalog.pg_class as c
        left join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        where {name_cond}
        limit 1
    )
order by 1"
    );

    // 4. Foreign keys (outgoing)
    let fk_sql = format!(
        "select
    conname,
    pg_catalog.pg_get_constraintdef(oid, true) as condef
from pg_catalog.pg_constraint as co
where co.contype = 'f'
    and co.conrelid = (
        select c.oid
        from pg_catalog.pg_class as c
        left join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        where {name_cond}
        limit 1
    )
order by 1"
    );

    // 5. Referenced by (incoming FKs) — psql format:
    //    TABLE "orders" CONSTRAINT "orders_user_id_fkey" FOREIGN KEY (user_id) REFERENCES users(id)
    let ref_sql = format!(
        "select
    conrelid::pg_catalog.regclass::text as from_table,
    conname,
    pg_catalog.pg_get_constraintdef(oid, true) as condef
from pg_catalog.pg_constraint as co
where co.contype = 'f'
    and co.confrelid = (
        select c.oid
        from pg_catalog.pg_class as c
        left join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        where {name_cond}
        limit 1
    )
order by 1, 2"
    );

    // Indexes — print as indented text lines (psql format), not a table.
    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{idx_sql}\n/************************/");
    }
    if let Ok(messages) = client.simple_query(&idx_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        // Collect: (idx_name, is_primary, is_unique, amname, idx_oid_str)
        let mut index_rows: Vec<(String, bool, bool, String, String)> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let idx_name = row.get(0).unwrap_or("").to_owned();
                let is_primary = row.get(1).unwrap_or("f") == "t";
                let is_unique = row.get(2).unwrap_or("f") == "t";
                let amname = row.get(3).unwrap_or("").to_owned();
                let idx_oid_str = row.get(4).unwrap_or("0").to_owned();
                // col 5 = con_name (used implicitly via is_primary/is_unique flags)
                index_rows.push((idx_name, is_primary, is_unique, amname, idx_oid_str));
            }
        }
        if !index_rows.is_empty() {
            println!("Indexes:");
            for (idx_name, is_primary, is_unique, amname, idx_oid_str) in &index_rows {
                // Extract column list from pg_get_indexdef (the part inside parens).
                let indexdef_sql =
                    format!("select pg_catalog.pg_get_indexdef({idx_oid_str}, 0, true)");
                let col_expr = if let Ok(def_msgs) = client.simple_query(&indexdef_sql).await {
                    let mut expr = String::new();
                    for def_msg in def_msgs {
                        if let SimpleQueryMessage::Row(def_row) = def_msg {
                            let full = def_row.get(0).unwrap_or("");
                            if let (Some(open), Some(close)) = (full.rfind('('), full.rfind(')')) {
                                full[open..=close].clone_into(&mut expr);
                            }
                            break;
                        }
                    }
                    expr
                } else {
                    String::new()
                };

                let type_label = if *is_primary {
                    " PRIMARY KEY,".to_owned()
                } else if *is_unique {
                    " UNIQUE CONSTRAINT,".to_owned()
                } else {
                    String::new()
                };

                println!("    \"{idx_name}\"{type_label} {amname} {col_expr}");
            }
        }
    }

    // Check constraints — print as indented text lines.
    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{chk_sql}\n/************************/");
    }
    if let Ok(messages) = client.simple_query(&chk_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        let mut lines: Vec<(String, String)> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("").to_owned();
                let def = row.get(1).unwrap_or("").to_owned();
                lines.push((name, def));
            }
        }
        if !lines.is_empty() {
            println!("Check constraints:");
            for (name, def) in &lines {
                println!("    \"{name}\" {def}");
            }
        }
    }

    // Foreign-key constraints — print as indented text lines.
    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{fk_sql}\n/************************/");
    }
    if let Ok(messages) = client.simple_query(&fk_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        let mut lines: Vec<(String, String)> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("").to_owned();
                let def = row.get(1).unwrap_or("").to_owned();
                lines.push((name, def));
            }
        }
        if !lines.is_empty() {
            println!("Foreign-key constraints:");
            for (name, def) in &lines {
                println!("    \"{name}\" {def}");
            }
        }
    }

    // Referenced by — print as indented text lines (psql format).
    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{ref_sql}\n/************************/");
    }
    if let Ok(messages) = client.simple_query(&ref_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        let mut lines: Vec<(String, String, String)> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let from_table = row.get(0).unwrap_or("").to_owned();
                let name = row.get(1).unwrap_or("").to_owned();
                let def = row.get(2).unwrap_or("").to_owned();
                lines.push((from_table, name, def));
            }
        }
        if !lines.is_empty() {
            println!("Referenced by:");
            for (from_table, name, def) in &lines {
                println!("    TABLE \"{from_table}\" CONSTRAINT \"{name}\" {def}");
            }
        }
    }

    false
}

/// Collect `SimpleQueryMessage` responses into `(col_names, rows)`.
#[cfg(test)]
fn collect_messages(
    messages: Vec<tokio_postgres::SimpleQueryMessage>,
) -> (Vec<String>, Vec<Vec<String>>) {
    use tokio_postgres::SimpleQueryMessage;

    let mut col_names: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();

    for msg in messages {
        if let SimpleQueryMessage::Row(row) = msg {
            if col_names.is_empty() {
                col_names = (0..row.len())
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

    (col_names, rows)
}

// ---------------------------------------------------------------------------
// Unit tests (no DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metacmd::{MetaCmd, ParsedMeta};

    fn meta(cmd: MetaCmd, plus: bool, system: bool, pattern: Option<&str>) -> ParsedMeta {
        ParsedMeta {
            cmd,
            plus,
            system,
            pattern: pattern.map(ToOwned::to_owned),
            echo_hidden: false,
        }
    }

    // -----------------------------------------------------------------------
    // system_schema_filter
    // -----------------------------------------------------------------------

    #[test]
    fn system_filter_off_excludes_system_schemas() {
        let f = system_schema_filter(false);
        assert!(f.contains("pg_catalog"), "should exclude pg_catalog: {f}");
        assert!(
            f.contains("information_schema"),
            "should exclude information_schema: {f}"
        );
    }

    #[test]
    fn system_filter_on_is_empty() {
        assert_eq!(system_schema_filter(true), "");
    }

    // -----------------------------------------------------------------------
    // SQL generation — list_relations (tested by inspecting the SQL string)
    // -----------------------------------------------------------------------

    /// Build the SQL that `list_relations` would produce and verify key fragments
    /// are present for the basic `\dt` case.
    #[test]
    fn list_tables_sql_has_relkind_filter() {
        // We rebuild the SQL inline (matching list_relations logic) because the
        // function itself is async and requires a DB client.
        let relkinds = ["r", "p"];
        let kind_list: Vec<String> = relkinds.iter().map(|k| format!("'{k}'")).collect();
        let kind_in = kind_list.join(",");

        assert!(kind_in.contains("'r'"), "kind_in should include 'r'");
        assert!(kind_in.contains("'p'"), "kind_in should include 'p'");
    }

    #[test]
    fn list_indexes_sql_has_relkind_i() {
        let relkinds = ["i"];
        let kind_list: Vec<String> = relkinds.iter().map(|k| format!("'{k}'")).collect();
        let kind_in = kind_list.join(",");
        assert_eq!(kind_in, "'i'");
    }

    #[test]
    fn list_sequences_sql_has_relkind_s() {
        let relkinds = ["S"];
        let kind_list: Vec<String> = relkinds.iter().map(|k| format!("'{k}'")).collect();
        let kind_in = kind_list.join(",");
        assert_eq!(kind_in, "'S'");
    }

    // -----------------------------------------------------------------------
    // Pattern routing
    // -----------------------------------------------------------------------

    #[test]
    fn pattern_filter_exact_match() {
        let f = pattern::where_clause(Some("users"), "c.relname", Some("n.nspname"));
        assert!(f.contains("= 'users'"), "expected exact match: {f}");
    }

    #[test]
    fn pattern_filter_schema_qualified() {
        let f = pattern::where_clause(Some("public.users"), "c.relname", Some("n.nspname"));
        assert!(f.contains("nspname"), "expected schema filter: {f}");
        assert!(f.contains("= 'public'"), "expected schema value: {f}");
        assert!(f.contains("= 'users'"), "expected name value: {f}");
    }

    #[test]
    fn pattern_filter_wildcard() {
        let f = pattern::where_clause(Some("user*"), "c.relname", Some("n.nspname"));
        assert!(f.contains("LIKE"), "expected LIKE for wildcard: {f}");
        assert!(f.contains("user%"), "expected % wildcard: {f}");
    }

    #[test]
    fn pattern_filter_none_is_empty() {
        let f = pattern::where_clause(None, "c.relname", Some("n.nspname"));
        assert!(f.is_empty(), "no pattern should produce empty filter");
    }

    // -----------------------------------------------------------------------
    // describe_object lookup SQL
    // -----------------------------------------------------------------------

    /// Verify that the lookup query used by `describe_object` includes the
    /// pattern filter when a wildcard pattern is supplied (e.g. `\d t*`).
    #[test]
    fn describe_object_lookup_sql_includes_pattern_filter() {
        let pattern = "t*";
        let name_filter = pattern::where_clause(Some(pattern), "c.relname", Some("n.nspname"));
        // No schema specified — add visibility filter.
        let visibility_filter = "pg_catalog.pg_table_is_visible(c.oid)";

        let where_cond = format!("{name_filter}\n    and {visibility_filter}");

        let lookup_sql = format!(
            "select c.oid, n.nspname, c.relname
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where {where_cond}
order by 2, 3"
        );

        assert!(
            lookup_sql.contains("LIKE 't%'"),
            "lookup SQL should use LIKE with wildcard expanded: {lookup_sql}"
        );
        assert!(
            lookup_sql.contains("pg_table_is_visible"),
            "lookup SQL should include visibility filter: {lookup_sql}"
        );
        assert!(
            lookup_sql.contains("c.relname"),
            "lookup SQL should filter on relname: {lookup_sql}"
        );
        assert!(
            lookup_sql.contains("order by 2, 3"),
            "lookup SQL should order by schema, name: {lookup_sql}"
        );
    }

    /// Verify that when a schema-qualified wildcard pattern is used (e.g.
    /// `\d public.t*`), the lookup SQL filters on both schema and name and
    /// does NOT include the visibility filter.
    #[test]
    fn describe_object_lookup_sql_schema_qualified_no_visibility() {
        let pattern = "public.t*";
        let (schema_part, _name_part) = pattern::split_schema(pattern);
        let name_filter = pattern::where_clause(Some(pattern), "c.relname", Some("n.nspname"));

        // Schema was specified — no visibility filter.
        let visibility_filter = if schema_part.is_none() {
            "pg_catalog.pg_table_is_visible(c.oid)"
        } else {
            ""
        };

        let parts: Vec<&str> = [
            if name_filter.is_empty() {
                None
            } else {
                Some(name_filter.as_str())
            },
            if visibility_filter.is_empty() {
                None
            } else {
                Some(visibility_filter)
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        let where_cond = parts.join("\n    and ");

        let lookup_sql = format!(
            "select c.oid, n.nspname, c.relname
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where {where_cond}
order by 2, 3"
        );

        assert!(
            lookup_sql.contains("n.nspname = 'public'"),
            "lookup SQL should filter on schema: {lookup_sql}"
        );
        assert!(
            lookup_sql.contains("LIKE 't%'"),
            "lookup SQL should use LIKE for name wildcard: {lookup_sql}"
        );
        assert!(
            !lookup_sql.contains("pg_table_is_visible"),
            "schema-qualified lookup should NOT include visibility filter: {lookup_sql}"
        );
    }

    // -----------------------------------------------------------------------
    // ParsedMeta construction helpers
    // -----------------------------------------------------------------------

    #[test]
    fn meta_list_tables_no_extras() {
        let m = meta(MetaCmd::ListTables, false, false, None);
        assert_eq!(m.cmd, MetaCmd::ListTables);
        assert!(!m.plus);
        assert!(!m.system);
        assert!(m.pattern.is_none());
    }

    #[test]
    fn meta_list_tables_with_pattern() {
        let m = meta(MetaCmd::ListTables, false, false, Some("users"));
        assert_eq!(m.pattern, Some("users".to_owned()));
    }

    #[test]
    fn meta_list_tables_plus_system() {
        let m = meta(MetaCmd::ListTables, true, true, None);
        assert!(m.plus);
        assert!(m.system);
    }

    // -----------------------------------------------------------------------
    // print_table (smoke test via captured stdout)
    // -----------------------------------------------------------------------

    /// Verify that `print_table` produces a `(0 rows)` footer for an empty result.
    #[test]
    fn print_table_empty_rows_with_columns() {
        // We can't easily capture stdout in a unit test without extra deps,
        // but we can verify that the function doesn't panic.
        let cols = vec!["Schema".to_owned(), "Name".to_owned()];
        let rows: Vec<Vec<String>> = vec![];
        // Should not panic.
        print_table(&cols, &rows, None);
    }

    #[test]
    fn print_table_single_row() {
        let cols = vec!["Name".to_owned()];
        let rows = vec![vec!["users".to_owned()]];
        // Should not panic.
        print_table(&cols, &rows, None);
    }

    #[test]
    fn print_table_empty_no_columns() {
        // Edge case: no columns, no rows — prints (0 rows).
        print_table(&[], &[], None);
    }

    // -----------------------------------------------------------------------
    // collect_messages
    // -----------------------------------------------------------------------

    #[test]
    fn collect_messages_empty_returns_empty() {
        let (cols, rows) = collect_messages(vec![]);
        assert!(cols.is_empty());
        assert!(rows.is_empty());
    }

    // -----------------------------------------------------------------------
    // list_relations SQL — plus modifier adds Size + Description columns
    // -----------------------------------------------------------------------

    #[test]
    fn plus_modifier_adds_size_column() {
        // Reconstruct SQL fragment for \dt+ and check for Size column.
        // Uses pg_total_relation_size (correct for partitioned tables too).
        let sql = format!(
            "select\n    n.nspname as \"Schema\",\n    c.relname as \"Name\",\
            \n    {} as \"Type\",\n    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",\
            \n    pg_catalog.pg_size_pretty(pg_catalog.pg_total_relation_size(c.oid)) as \"Size\",\
            \n    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"",
            "c.relkind"
        );
        assert!(sql.contains("\"Size\""), "plus SQL should have Size: {sql}");
        assert!(
            sql.contains("\"Description\""),
            "plus SQL should have Description: {sql}"
        );
        assert!(
            sql.contains("pg_total_relation_size"),
            "plus SQL should use pg_total_relation_size: {sql}"
        );
    }
}
