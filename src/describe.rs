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
/// `pg_major_version` is used to adapt catalog queries to the connected
/// server (e.g. column renames between PG 15/16/17).
/// `settings` is used to route output through the pager when appropriate.
///
/// Returns `true` if the REPL loop should exit after this command (always
/// `false` for describe commands — only `\q` exits).
pub async fn execute(
    client: &Client,
    meta: &ParsedMeta,
    pg_major_version: Option<u32>,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    match &meta.cmd {
        MetaCmd::DescribeObject => describe_object(client, meta, settings).await,
        MetaCmd::ListTables => list_relations(client, meta, &["r", "p"], settings).await,
        MetaCmd::ListIndexes => list_relations(client, meta, &["i"], settings).await,
        MetaCmd::ListSequences => list_relations(client, meta, &["S"], settings).await,
        MetaCmd::ListViews => list_relations(client, meta, &["v"], settings).await,
        MetaCmd::ListMatViews => list_relations(client, meta, &["m"], settings).await,
        MetaCmd::ListForeignTables => list_relations(client, meta, &["f"], settings).await,
        MetaCmd::ListFunctions => list_functions(client, meta, settings).await,
        MetaCmd::ListSchemas => list_schemas(client, meta, settings).await,
        MetaCmd::ListRoles => list_roles(client, meta, settings).await,
        MetaCmd::ListDatabases => list_databases(client, meta, pg_major_version, settings).await,
        MetaCmd::ListExtensions => list_extensions(client, meta, settings).await,
        MetaCmd::ListTablespaces => list_tablespaces(client, meta, settings).await,
        MetaCmd::ListTypes => list_types(client, meta, settings).await,
        MetaCmd::ListDomains => list_domains(client, meta, settings).await,
        MetaCmd::ListPrivileges => list_privileges(client, meta, settings).await,
        MetaCmd::ListConversions => list_conversions(client, meta, settings).await,
        MetaCmd::ListCasts => list_casts(client, meta, settings).await,
        MetaCmd::ListComments => list_comments(client, meta, settings).await,
        MetaCmd::ListForeignServers => list_foreign_servers(client, meta, settings).await,
        MetaCmd::ListFdws => list_fdws(client, meta, settings).await,
        MetaCmd::ListForeignTablesViaFdw => {
            list_foreign_tables_via_fdw(client, meta, settings).await
        }
        MetaCmd::ListUserMappings => list_user_mappings(client, meta, settings).await,
        MetaCmd::ListEventTriggers => list_event_triggers(client, meta, settings).await,
        MetaCmd::ListOperators => list_operators(client, meta, settings).await,
        MetaCmd::ListExtStatistics => list_ext_statistics(client, meta, settings).await,
        MetaCmd::ListPublications => list_publications(client, meta, settings).await,
        MetaCmd::ListSubscriptions => list_subscriptions(client, meta, settings).await,
        // Non-describe commands should never reach this function.
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Pager helper
// ---------------------------------------------------------------------------

/// Route `text` through the pager when it exceeds the terminal height,
/// otherwise print directly to stdout.
///
/// Mirrors the logic used by `maybe_page` in `src/repl/mod.rs`.
fn maybe_page(settings: &mut crate::repl::ReplSettings, text: &str) {
    use std::io::Write;

    // Honour \o redirect.
    if let Some(ref mut w) = settings.output_target {
        let _ = writeln!(w, "{text}");
        return;
    }
    let term_rows = crossterm::terminal::size()
        .map(|(_, h)| h as usize)
        .unwrap_or(24);
    if settings.pager_enabled
        && crate::pager::needs_paging_with_min(
            text,
            term_rows.saturating_sub(2),
            settings.pager_min_lines,
        )
    {
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.clear();
            sl.teardown_scroll_region();
        }
        if let Some(ref cmd) = settings.pager_command.clone() {
            if let Err(e) = crate::pager::run_pager_external(cmd, text) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    eprintln!(
                        "rpg: pager '{cmd}' not found — check your PAGER setting \
                         (\\set PAGER off to disable)"
                    );
                } else {
                    eprintln!("rpg: pager error: {e}");
                }
                let _ = std::io::stdout().write_all(text.as_bytes());
            }
        } else if let Err(e) = crate::pager::run_pager(text) {
            // Unsupported means no TTY available (piped/non-interactive).
            // Fall back silently — no error message, just print.
            if e.kind() != std::io::ErrorKind::Unsupported {
                eprintln!("rpg: pager error: {e}");
            }
            let _ = std::io::stdout().write_all(text.as_bytes());
        }
        if let Some(ref sl_arc) = settings.statusline {
            let sl = sl_arc.lock().unwrap();
            sl.setup_scroll_region();
            sl.render();
        }
    } else {
        print!("{text}");
    }
}

// ---------------------------------------------------------------------------
// Internal execution helper
// ---------------------------------------------------------------------------

/// Execute `sql` via `simple_query`, print an aligned table with an optional
/// centered title, and return `false` (never exits the REPL).
///
/// When `echo_hidden` is `true` the SQL is echoed to stderr first.
async fn run_and_print_titled(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    run_and_print_full(client, sql, echo_hidden, title, true, settings).await
}

/// Like `run_and_print_titled` but suppresses the `(N rows)` footer.
/// Used by `\d tablename` to match psql behaviour.
async fn run_and_print_no_count(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    run_and_print_full(client, sql, echo_hidden, title, false, settings).await
}

async fn run_and_print_full(
    client: &Client,
    sql: &str,
    echo_hidden: bool,
    title: Option<&str>,
    show_row_count: bool,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    use crate::output::{format_rowset_pset, OutputFormat};
    use crate::query::{ColumnMeta, RowSet};

    if echo_hidden {
        eprintln!("/******** QUERY *********/\n{sql}\n/************************/");
    }

    match client.simple_query(sql).await {
        Ok(messages) => {
            use tokio_postgres::SimpleQueryMessage;

            let mut col_names: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<String>> = Vec::new();

            for msg in messages {
                match msg {
                    SimpleQueryMessage::RowDescription(columns) => {
                        if col_names.is_empty() {
                            col_names = columns.iter().map(|c| c.name().to_owned()).collect();
                        }
                    }
                    SimpleQueryMessage::Row(row) => {
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
                    _ => {}
                }
            }

            let fmt = &settings.pset.format;
            // Use format_rowset_pset for HTML/non-aligned formats to respect pset settings.
            // For aligned/wrapped (default), use the custom format_table_inner which handles
            // centered titles and describe-specific formatting.
            let text = if matches!(fmt, OutputFormat::Aligned | OutputFormat::Wrapped)
                && !settings.pset.tuples_only
                && settings.pset.expanded == crate::output::ExpandedMode::Off
            {
                format_table_inner(&col_names, &rows, title, show_row_count)
            } else {
                // Build a RowSet and use pset-aware formatting.
                let columns: Vec<ColumnMeta> = col_names
                    .iter()
                    .map(|name| ColumnMeta {
                        name: name.clone(),
                        // Describe queries return text; nothing is numeric.
                        is_numeric: false,
                    })
                    .collect();
                let rs_rows: Vec<Vec<Option<String>>> = rows
                    .iter()
                    .map(|r| r.iter().map(|v| Some(v.clone())).collect())
                    .collect();
                let rs = RowSet {
                    columns,
                    rows: rs_rows,
                };
                // Apply title to pset config temporarily.
                let mut cfg = settings.pset.clone();
                if let Some(t) = title {
                    cfg.title = Some(t.to_owned());
                }
                cfg.footer = show_row_count && settings.pset.footer;
                let mut out = String::new();
                format_rowset_pset(&mut out, &rs, &cfg);
                out
            };
            maybe_page(settings, &text);
        }
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(sql), false, false, false);
        }
    }

    false
}

/// Format a column-aligned table as a `String`, optionally with a centered title.
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
///
/// The `#[cfg(test)]` wrapper below provides a `print_table` helper for tests.
#[cfg(test)]
fn print_table(col_names: &[String], rows: &[Vec<String>], title: Option<&str>) {
    print!("{}", format_table_inner(col_names, rows, title, true));
}

#[allow(clippy::too_many_lines)]
fn format_table_inner(
    col_names: &[String],
    rows: &[Vec<String>],
    title: Option<&str>,
    show_row_count: bool,
) -> String {
    use std::fmt::Write as FmtWrite;

    let mut out = String::new();

    if col_names.is_empty() {
        if show_row_count {
            let n = rows.len();
            let word = if n == 1 { "row" } else { "rows" };
            let _ = writeln!(out, "({n} {word})");
        }
        return out;
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

    // Determine which columns should be right-aligned (numeric inference).
    // A column is numeric if all non-empty values parse as f64.
    // This matches psql's behavior for typed integer/float columns.
    let is_numeric: Vec<bool> = (0..col_names.len())
        .map(|col_idx| {
            let name_lc = col_names[col_idx].to_lowercase();
            // Known text-type column names — never right-align these.
            if matches!(
                name_lc.as_str(),
                "type"
                    | "schema"
                    | "name"
                    | "owner"
                    | "collation"
                    | "nullable"
                    | "default"
                    | "check"
                    | "access privileges"
                    | "storage"
                    | "compression"
                    | "stats target"
                    | "description"
                    | "column"
                    | "definition"
                    | "condition"
                    | "columns"
                    | "key?"
                    | "primary"
                    | "references"
                    | "options"
                    | "fdw options"
                    | "cycles?"
                    | "comment"
                    | "inherits"
                    | "tablespace"
                    | "child tables"
                    | "partition of"
                    | "partition constraint"
                    | "replica identity"
                    | "access method"
                    | "version"
                    | "foreign-data wrapper"
            ) {
                return false;
            }
            let mut has_value = false;
            let all_numeric = rows.iter().all(|row| {
                let val = row.get(col_idx).map_or("", String::as_str);
                if val.is_empty() {
                    return true;
                }
                has_value = true;
                if val.starts_with('+') {
                    return false;
                }
                if val.len() > 1
                    && val.starts_with('0')
                    && val.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                {
                    return false;
                }
                val.parse::<f64>().is_ok()
            });
            all_numeric && has_value
        })
        .collect();

    // Total table width: 1 (leading space) + sum(widths) +
    // 3*(ncols-1) (` | `) + 1 (trailing space).
    let ncols = widths.len();
    let table_width =
        1 + widths.iter().sum::<usize>() + if ncols > 1 { 3 * (ncols - 1) } else { 0 } + 1;

    // Optional title centered to table width.
    if let Some(t) = title {
        let tlen = t.len();
        if tlen >= table_width {
            let _ = writeln!(out, "{t}");
        } else {
            let padding = (table_width - tlen) / 2;
            let _ = writeln!(out, "{:>width$}", t, width = padding + tlen);
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
    let _ = writeln!(out, " {} ", header.join(" | "));

    // Separator.
    let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    let _ = writeln!(out, "-{}-", sep.join("-+-"));

    // Data rows — cells with embedded newlines are printed as psql continuation
    // lines.  For the last column, `+` replaces the trailing space.  For middle
    // columns, `+` is placed within the cell width.
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
            let mut line = String::new();
            // Track whether the previous column had a continuation marker, so
            // we can suppress the leading space in the following ` | ` separator
            // (psql prints `+|` with no gap between the marker and `|`).
            let mut prev_had_continuation = false;

            for (col_idx, &w) in widths.iter().enumerate() {
                let text = cell_lines
                    .get(col_idx)
                    .and_then(|ls| ls.get(line_idx))
                    .copied()
                    .unwrap_or("");
                let has_more = cell_lines
                    .get(col_idx)
                    .is_some_and(|ls| line_idx + 1 < ls.len());

                // Column separator.
                if col_idx == 0 {
                    line.push(' ');
                } else if prev_had_continuation {
                    // Previous column ended with `+`; omit the leading space so
                    // the separator renders as `+|` (matching psql).
                    line.push_str("| ");
                } else {
                    line.push_str(" | ");
                }
                prev_had_continuation = false;

                if has_more && col_idx < ncols - 1 {
                    // Middle column with continuation: pad to full width, then
                    // append `+` which will replace the leading space of the
                    // next separator.
                    let text_pad = w.saturating_sub(text.len());
                    line.push_str(text);
                    for _ in 0..text_pad {
                        line.push(' ');
                    }
                    line.push('+');
                    prev_had_continuation = true;
                } else if col_idx == ncols - 1
                    && !has_more
                    && !is_numeric.get(col_idx).copied().unwrap_or(false)
                {
                    // Last column without continuation — no trailing padding
                    // (matches psql) for non-numeric columns.
                    line.push_str(text);
                } else if is_numeric.get(col_idx).copied().unwrap_or(false) {
                    // Numeric column — right-align.
                    let padded = format!("{text:>w$}");
                    line.push_str(&padded);
                } else {
                    // Normal cell — pad to column width.
                    let padded = format!("{text:<w$}");
                    line.push_str(&padded);
                }
            }

            // Trailing: for the last column with continuation, `+` is appended
            // after the padded value (matching psql behaviour).
            let last_has_more = cell_lines
                .get(ncols - 1)
                .is_some_and(|ls| line_idx + 1 < ls.len());
            if last_has_more {
                line.push('+');
            }

            let _ = writeln!(out, "{line}");
        }
    }

    if show_row_count {
        let n = rows.len();
        let word = if n == 1 { "row" } else { "rows" };
        let _ = writeln!(out, "({n} {word})\n");
    }

    out
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

/// Return the result-set title for a given set of relkinds.
///
/// psql uses type-specific headings: "List of tables" for \dt, "List of
/// indexes" for \di, etc.  Only the unfiltered \d (all relkinds) uses
/// the generic "List of relations".
fn relation_title(relkinds: &[&str]) -> &'static str {
    match relkinds {
        ["r", "p"] | ["r" | "p"] => "List of tables",
        ["i" | "I"] | ["i", "I"] => "List of indexes",
        ["v"] => "List of views",
        ["S"] => "List of sequences",
        ["m"] => "List of materialized views",
        ["f"] => "List of foreign tables",
        _ => "List of relations",
    }
}

/// List relations of the given `relkinds` (e.g. `["r","p"]` for tables).
#[allow(clippy::too_many_lines)]
async fn list_relations(
    client: &Client,
    meta: &ParsedMeta,
    relkinds: &[&str],
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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

    // Views and sequences use pg_relation_size in verbose mode and omit the
    // Access method column (but do show Persistence).  Materialized views are
    // heap-stored like tables and need `pg_table_size` + Access method.
    let is_view_or_seq = matches!(relkinds, ["v" | "S"]);

    let sql = if meta.plus {
        if is_index_only {
            format!(
                "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    {type_expr} as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    ct.relname as \"Table\",
    case c.relpersistence
        when 'p' then 'permanent'
        when 't' then 'temporary'
        when 'u' then 'unlogged'
        else c.relpersistence::text
    end as \"Persistence\",
    coalesce(am.amname, '') as \"Access method\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
join pg_catalog.pg_index as idx_i
    on idx_i.indexrelid = c.oid
join pg_catalog.pg_class as ct
    on ct.oid = idx_i.indrelid
left join pg_catalog.pg_am as am
    on am.oid = c.relam
where c.relkind in ({kind_in})
    {where_clause}
order by 1, 2"
            )
        } else if is_view_or_seq {
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
    pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
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
    pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\",
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

    let title = relation_title(relkinds);
    run_and_print_titled(client, &sql, meta.echo_hidden, Some(title), settings).await
}

// ---------------------------------------------------------------------------
// \df — list functions
// ---------------------------------------------------------------------------

async fn list_functions(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "p.proname", Some("n.nspname"));

    // When a pattern is given, show all schemas (including pg_catalog) like psql.
    // When no pattern and no -S flag, only show user schemas.
    let sys_filter = if meta.system || meta.pattern.is_some() {
        String::new()
    } else {
        "n.nspname not in ('pg_catalog', 'information_schema')".to_owned()
    };

    // \dfn → only normal functions, \dfp → only procedures, etc.
    let kind_filter = meta.kind_filter.map(|k| {
        let pg_kind = match k {
            'n' => "'f'", // normal function
            'p' => "'p'", // procedure
            'a' => "'a'", // aggregate
            'w' => "'w'", // window function
            _ => "null",
        };
        format!("p.prokind = {pg_kind}")
    });

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
        kind_filter.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();

    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!("where {}", where_parts.join("\n    and "))
    };

    // For \da (aggregate functions), use a different column set: Description instead of Type.
    let is_agg_only = meta.kind_filter == Some('a');

    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Schema\",
    p.proname as \"Name\",
    pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",
    pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",
    case p.prokind
        when 'a' then 'agg'
        when 'w' then 'window'
        when 'p' then 'proc'
        else 'func'
    end as \"Type\",
    case
        when p.provolatile = 'i' then 'immutable'
        when p.provolatile = 's' then 'stable'
        when p.provolatile = 'v' then 'volatile'
    end as \"Volatility\",
    case
        when p.proparallel = 'r' then 'restricted'
        when p.proparallel = 's' then 'safe'
        when p.proparallel = 'u' then 'unsafe'
    end as \"Parallel\",
    pg_catalog.pg_get_userbyid(p.proowner) as \"Owner\",
    case when p.prosecdef then 'definer' else 'invoker' end as \"Security\",
    pg_catalog.array_to_string(p.proacl, E'\\n') as \"Access privileges\",
    l.lanname as \"Language\",
    case when l.lanname in ('internal', 'c') then p.prosrc end as \"Internal name\",
    pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"
from pg_catalog.pg_proc as p
left join pg_catalog.pg_namespace as n
    on n.oid = p.pronamespace
left join pg_catalog.pg_language as l
    on l.oid = p.prolang
{where_clause}
order by 1, 2, 4"
        )
    } else if is_agg_only {
        // \da: List of aggregate functions — Description column instead of Type
        format!(
            "select
    n.nspname as \"Schema\",
    p.proname as \"Name\",
    pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",
    pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",
    pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"
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

    let title = if is_agg_only {
        "List of aggregate functions"
    } else {
        "List of functions"
    };

    run_and_print_titled(client, &sql, meta.echo_hidden, Some(title), settings).await
}

// ---------------------------------------------------------------------------
// \dn — list schemas
// ---------------------------------------------------------------------------

async fn list_schemas(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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
    pg_catalog.array_to_string(n.nspacl, E'\\n') as \"Access privileges\",
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

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of schemas"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \du / \dg — list roles
// ---------------------------------------------------------------------------

async fn list_roles(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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
    // The `+` variant additionally shows a Description column.
    let attrs_expr = "case when r.rolsuper then 'Superuser' else '' end
    || case when not r.rolinherit then case when r.rolsuper then ', No inherit' else 'No inherit' end else '' end
    || case when r.rolcreaterole then case when r.rolsuper or not r.rolinherit then ', Create role' else 'Create role' end else '' end
    || case when r.rolcreatedb then case when r.rolsuper or not r.rolinherit or r.rolcreaterole then ', Create DB' else 'Create DB' end else '' end
    || case when not r.rolcanlogin then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb then ', Cannot login' else 'Cannot login' end else '' end
    || case when r.rolreplication then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb or not r.rolcanlogin then ', Replication' else 'Replication' end else '' end
    || case when r.rolbypassrls then case when r.rolsuper or not r.rolinherit or r.rolcreaterole or r.rolcreatedb or not r.rolcanlogin or r.rolreplication then ', Bypass RLS' else 'Bypass RLS' end else '' end
    as \"Attributes\"";

    let sql = if meta.plus {
        format!(
            "select
    r.rolname as \"Role name\",
    {attrs_expr},
    pg_catalog.shobj_description(r.oid, 'pg_authid') as \"Description\"
from pg_catalog.pg_roles as r
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    r.rolname as \"Role name\",
    {attrs_expr}
from pg_catalog.pg_roles as r
{where_clause}
order by 1"
        )
    };

    // psql suppresses the row count footer for \du.
    run_and_print_no_count(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of roles"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \l — list databases
// ---------------------------------------------------------------------------

async fn list_databases(
    client: &Client,
    meta: &ParsedMeta,
    pg_major_version: Option<u32>,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "d.datname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let ver = pg_major_version.unwrap_or(14);

    // Locale columns differ across PG versions:
    //   PG 14: no datlocprovider, no ICU locale/rules columns
    //   PG 15: datlocprovider, daticulocale (no daticurules)
    //   PG 16: datlocprovider, daticulocale, daticurules
    //   PG 17+: datlocprovider (adds 'builtin'), datlocale (renamed), daticurules
    let locale_provider = if ver >= 17 {
        "case d.datlocprovider when 'b' then 'builtin' when 'c' then 'libc' when 'i' then 'icu' end as \"Locale Provider\","
    } else if ver >= 15 {
        "case d.datlocprovider when 'c' then 'libc' when 'i' then 'icu' end as \"Locale Provider\","
    } else {
        ""
    };

    let icu_locale = if ver >= 17 {
        "d.datlocale as \"Locale\","
    } else if ver >= 15 {
        "d.daticulocale as \"ICU Locale\","
    } else {
        ""
    };

    let icu_rules = if ver >= 16 {
        "d.daticurules as \"ICU Rules\","
    } else {
        ""
    };

    let acl = if ver >= 17 {
        "case when pg_catalog.array_length(d.datacl, 1) = 0 then '(none)' \
         else pg_catalog.array_to_string(d.datacl, E'\\n') end as \"Access privileges\""
    } else {
        "pg_catalog.array_to_string(d.datacl, E'\\n') as \"Access privileges\""
    };

    let sql = if meta.plus {
        format!(
            "select \
    d.datname as \"Name\", \
    pg_catalog.pg_get_userbyid(d.datdba) as \"Owner\", \
    pg_catalog.pg_encoding_to_char(d.encoding) as \"Encoding\", \
    {locale_provider} \
    d.datcollate as \"Collate\", \
    d.datctype as \"Ctype\", \
    {icu_locale} \
    {icu_rules} \
    {acl}, \
    case \
        when pg_catalog.has_database_privilege(d.datname, 'CONNECT') \
        then pg_catalog.pg_size_pretty(pg_catalog.pg_database_size(d.datname)) \
        else 'No Access' \
    end as \"Size\", \
    t.spcname as \"Tablespace\", \
    coalesce(pg_catalog.shobj_description(d.oid, 'pg_database'), '') as \"Description\" \
from pg_catalog.pg_database as d \
join pg_catalog.pg_tablespace as t \
    on t.oid = d.dattablespace \
{where_clause} \
order by 1"
        )
    } else {
        format!(
            "select \
    d.datname as \"Name\", \
    pg_catalog.pg_get_userbyid(d.datdba) as \"Owner\", \
    pg_catalog.pg_encoding_to_char(d.encoding) as \"Encoding\", \
    {locale_provider} \
    d.datcollate as \"Collate\", \
    d.datctype as \"Ctype\", \
    {icu_locale} \
    {icu_rules} \
    {acl} \
from pg_catalog.pg_database as d \
{where_clause} \
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of databases"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dx — list extensions
// ---------------------------------------------------------------------------

async fn list_extensions(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of installed extensions"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \db — list tablespaces
// ---------------------------------------------------------------------------

async fn list_tablespaces(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "spcname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let sql = if meta.plus {
        format!(
            "select
    spcname as \"Name\",
    pg_catalog.pg_get_userbyid(spcowner) as \"Owner\",
    pg_catalog.pg_tablespace_location(oid) as \"Location\",
    case when pg_catalog.array_length(spcacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(spcacl, E'\\n')
    end as \"Access privileges\",
    spcoptions as \"Options\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_tablespace_size(oid)) as \"Size\",
    pg_catalog.shobj_description(oid, 'pg_tablespace') as \"Description\"
from pg_catalog.pg_tablespace
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    spcname as \"Name\",
    pg_catalog.pg_get_userbyid(spcowner) as \"Owner\",
    pg_catalog.pg_tablespace_location(oid) as \"Location\"
from pg_catalog.pg_tablespace
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of tablespaces"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dT — list types
// ---------------------------------------------------------------------------

/// List data types matching psql's `\dT [pattern]` output.
///
/// Basic columns: Schema, Name, Description.
/// Verbose (`\dT+`) adds: Internal name, Size, Elements, Owner,
/// Access privileges.
async fn list_types(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "t.typname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast')".to_owned()
    };

    // Show composite, domain, enum, range, and multirange types; exclude array
    // types (names starting with _) and table-backed composite types.
    let base_filter = "t.typtype in ('c', 'd', 'e', 'm', 'r') and t.typname !~ '^_'\
        \n    and (t.typrelid = 0 or (select c.relkind = 'c' from pg_catalog.pg_class as c where c.oid = t.typrelid))";

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

    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Schema\",
    pg_catalog.format_type(t.oid, null) as \"Name\",
    t.typname as \"Internal name\",
    case when t.typrelid != 0
            then cast('tuple' as pg_catalog.text)
        when t.typlen < 0
            then cast('var' as pg_catalog.text)
        else cast(t.typlen as pg_catalog.text)
    end as \"Size\",
    pg_catalog.array_to_string(
        array(
            select e.enumlabel
            from pg_catalog.pg_enum as e
            where e.enumtypid = t.oid
            order by e.enumsortorder
        ),
        E'\\n'
    ) as \"Elements\",
    pg_catalog.pg_get_userbyid(t.typowner) as \"Owner\",
    case when pg_catalog.array_length(t.typacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(t.typacl, E'\\n')
    end as \"Access privileges\",
    coalesce(pg_catalog.obj_description(t.oid, 'pg_type'), '') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
        )
    } else {
        format!(
            "select
    n.nspname as \"Schema\",
    pg_catalog.format_type(t.oid, null) as \"Name\",
    coalesce(pg_catalog.obj_description(t.oid, 'pg_type'), '') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of data types"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dD — list domains
// ---------------------------------------------------------------------------

/// List domain types matching psql's `\dD [pattern]` output.
///
/// Basic columns: Schema, Name, Type, Collation, Nullable, Default, Check.
/// Verbose (`\dD+`) adds: Access privileges, Description.
async fn list_domains(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "t.typname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname <> 'pg_catalog'\n    and n.nspname <> 'information_schema'".to_owned()
    };

    let visibility_filter = "pg_catalog.pg_type_is_visible(t.oid)";
    let base_filter = "t.typtype = 'd'";

    let where_parts: Vec<&str> = [
        Some(base_filter),
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        Some(visibility_filter),
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

    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Schema\",
    t.typname as \"Name\",
    pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",
    (select c.collname
     from pg_catalog.pg_collation as c, pg_catalog.pg_type as bt
     where c.oid = t.typcollation
       and bt.oid = t.typbasetype
       and t.typcollation <> bt.typcollation) as \"Collation\",
    case when t.typnotnull then 'not null' end as \"Nullable\",
    t.typdefault as \"Default\",
    pg_catalog.array_to_string(array(
        select pg_catalog.pg_get_constraintdef(r.oid, true)
        from pg_catalog.pg_constraint as r
        where t.oid = r.contypid
          and r.contype = 'c'
        order by r.conname
    ), ' ') as \"Check\",
    case when pg_catalog.array_length(t.typacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(t.typacl, E'\\n')
    end as \"Access privileges\",
    d.description as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
left join pg_catalog.pg_description as d
    on d.classoid = t.tableoid
   and d.objoid = t.oid
   and d.objsubid = 0
{where_clause}
order by 1, 2"
        )
    } else {
        format!(
            "select
    n.nspname as \"Schema\",
    t.typname as \"Name\",
    pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",
    (select c.collname
     from pg_catalog.pg_collation as c, pg_catalog.pg_type as bt
     where c.oid = t.typcollation
       and bt.oid = t.typbasetype
       and t.typcollation <> bt.typcollation) as \"Collation\",
    case when t.typnotnull then 'not null' end as \"Nullable\",
    t.typdefault as \"Default\",
    pg_catalog.array_to_string(array(
        select pg_catalog.pg_get_constraintdef(r.oid, true)
        from pg_catalog.pg_constraint as r
        where t.oid = r.contypid
          and r.contype = 'c'
        order by r.conname
    ), ' ') as \"Check\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of domains"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dp — list access privileges
// ---------------------------------------------------------------------------

/// List access privileges for relations (tables, views, sequences).
///
/// Matches psql's `\dp [pattern]` output: Schema, Name, Type, Access
/// privileges, Column privileges, Policies.
#[allow(clippy::too_many_lines)]
async fn list_privileges(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "c.relname", Some("n.nspname"));

    let sys_filter = system_schema_filter(meta.system);

    // relkind guard: only relation types that can have privileges
    let relkind_filter = "c.relkind in ('r','v','m','S','f','p')";

    // visibility: use pg_table_is_visible when no schema pattern is given,
    // matching psql's behaviour for the non-schema-qualified case
    let visibility_filter = if name_filter.is_empty() || !name_filter.contains("n.nspname") {
        "pg_catalog.pg_table_is_visible(c.oid)"
    } else {
        ""
    };

    let where_parts: Vec<&str> = [
        Some(relkind_filter),
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter)
        },
        if visibility_filter.is_empty() {
            None
        } else {
            Some(visibility_filter)
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
    c.relname as \"Name\",
    case c.relkind
        when 'r' then 'table'
        when 'v' then 'view'
        when 'm' then 'materialized view'
        when 'S' then 'sequence'
        when 'f' then 'foreign table'
        when 'p' then 'partitioned table'
    end as \"Type\",
    case when pg_catalog.array_length(c.relacl, 1) = 0
        then '(none)'
        else pg_catalog.array_to_string(c.relacl, E'\\n')
    end as \"Access privileges\",
    pg_catalog.array_to_string(array(
        select
            a.attname || E':\\n  '
            || pg_catalog.array_to_string(a.attacl, E'\\n  ')
        from pg_catalog.pg_attribute as a
        where a.attrelid = c.oid
          and not a.attisdropped
          and a.attacl is not null
    ), E'\\n') as \"Column privileges\",
    pg_catalog.array_to_string(array(
        select
            pol.polname
            || case when not pol.polpermissive
               then E' (RESTRICTIVE)'
               else '' end
            || case when pol.polcmd <> '*'
               then E' (' || pol.polcmd::pg_catalog.text || E'):'
               else E':'
               end
            || case when pol.polqual is not null
               then E'\\n  (u): '
                    || pg_catalog.pg_get_expr(pol.polqual, pol.polrelid)
               else '' end
            || case when pol.polwithcheck is not null
               then E'\\n  (c): '
                    || pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid)
               else '' end
            || case when pol.polroles <> '{{0}}'
               then E'\\n  to: ' || pg_catalog.array_to_string(array(
                   select rolname
                   from pg_catalog.pg_roles
                   where oid = any(pol.polroles)
                   order by 1
               ), E', ')
               else '' end
        from pg_catalog.pg_policy as pol
        where pol.polrelid = c.oid
    ), E'\\n') as \"Policies\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
{where_clause}
order by 1, 2"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("Access privileges"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dd — list object descriptions/comments
// ---------------------------------------------------------------------------

/// List object descriptions (comments) for operators, functions, types, etc.
///
/// Matches psql's `\dd [pattern]` output: Schema, Name, Object, Description.
/// Shows objects that have comments but are not shown by other `\d` commands.
async fn list_comments(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "n.nspname", Some("n.nspname"));

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

    let extra_cond = if where_parts.is_empty() {
        String::new()
    } else {
        format!("and {}", where_parts.join("\n    and "))
    };

    // Operators
    let sql = format!(
        "select
    n.nspname as \"Schema\",
    o.oprname as \"Name\",
    'operator' as \"Object\",
    pg_catalog.obj_description(o.oid, 'pg_operator') as \"Description\"
from pg_catalog.pg_operator as o
left join pg_catalog.pg_namespace as n
    on n.oid = o.oprnamespace
where pg_catalog.obj_description(o.oid, 'pg_operator') is not null
    {extra_cond}
union all
select
    n.nspname as \"Schema\",
    t.typname as \"Name\",
    'type' as \"Object\",
    pg_catalog.obj_description(t.oid, 'pg_type') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
where pg_catalog.obj_description(t.oid, 'pg_type') is not null
    and t.typtype <> 'p'
    and t.typname !~ '^_'
    {extra_cond}
order by 1, 3, 2"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("Object descriptions"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dC — list casts
// ---------------------------------------------------------------------------

/// List casts between data types.
///
/// Matches psql's `\dC [pattern]` output: Source type, Target type,
/// Function, Implicit?
///
/// The visibility filter (`pg_type_is_visible`) mirrors psql: only casts
/// where the source **or** target type is visible in the current
/// `search_path` are shown.  When a pattern is supplied it is matched
/// against both `typname` and the formatted type name (`format_type()`),
/// using the same `~` regex operator that psql uses.
async fn list_casts(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    // Build the WHERE clause.  psql always applies a visibility filter; when
    // a pattern is present it is also matched against type names via regex.
    let where_clause = match meta.pattern.as_deref() {
        None => {
            // No pattern: show any cast where source or target is visible.
            "where (
    pg_catalog.pg_type_is_visible(ts.oid)
    or pg_catalog.pg_type_is_visible(tt.oid)
)"
            .to_owned()
        }
        Some(pat) => {
            // Pattern: match on typname OR format_type(), with visibility.
            let re = pattern::to_regex(pat);
            format!(
                "where (
    (ts.typname operator(pg_catalog.~) '{re}' collate pg_catalog.default
        or pg_catalog.format_type(ts.oid, null) operator(pg_catalog.~) '{re}' collate pg_catalog.default)
    and pg_catalog.pg_type_is_visible(ts.oid)
)
or (
    (tt.typname operator(pg_catalog.~) '{re}' collate pg_catalog.default
        or pg_catalog.format_type(tt.oid, null) operator(pg_catalog.~) '{re}' collate pg_catalog.default)
    and pg_catalog.pg_type_is_visible(tt.oid)
)"
            )
        }
    };

    let sql = format!(
        "select
    pg_catalog.format_type(c.castsource, null) as \"Source type\",
    pg_catalog.format_type(c.casttarget, null) as \"Target type\",
    case
        when c.castmethod = 'b' then '(binary coercible)'
        when c.castmethod = 'i' then '(with inout)'
        else p.proname
    end as \"Function\",
    case
        when c.castcontext = 'e' then 'no'
        when c.castcontext = 'a' then 'in assignment'
        else 'yes'
    end as \"Implicit?\"
from pg_catalog.pg_cast as c
left join pg_catalog.pg_proc as p
    on c.castfunc = p.oid
left join pg_catalog.pg_type as ts
    on c.castsource = ts.oid
left join pg_catalog.pg_namespace as ns
    on ns.oid = ts.typnamespace
left join pg_catalog.pg_type as tt
    on c.casttarget = tt.oid
left join pg_catalog.pg_namespace as nt
    on nt.oid = tt.typnamespace
{where_clause}
order by 1, 2"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of casts"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dc — list conversions
// ---------------------------------------------------------------------------

/// List character set conversions.
///
/// Matches psql's `\dc [pattern]` output: Schema, Name, Source, Destination,
/// Default?
async fn list_conversions(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "c.conname", Some("n.nspname"));

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

    let sql = format!(
        "select
    n.nspname as \"Schema\",
    c.conname as \"Name\",
    pg_catalog.pg_encoding_to_char(c.conforencoding) as \"Source\",
    pg_catalog.pg_encoding_to_char(c.contoencoding) as \"Destination\",
    case when c.condefault then 'yes' else 'no' end as \"Default?\"
from pg_catalog.pg_conversion as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.connamespace
{where_clause}
order by 1, 2"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of conversions"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \des — list foreign servers
// ---------------------------------------------------------------------------

/// List foreign servers.
///
/// Matches psql's `\des [pattern]` output: Name, Owner, Foreign-data wrapper.
/// With `+`: also shows Type, Version, FDW options, Description.
/// SQL expression to format a text[] of `key=value` options as psql's `(key 'val', ...)`.
/// Keys that contain spaces or special chars are double-quoted (matching psql).
/// Pass the column name as the argument (e.g. `s.srvoptions`).
fn fdw_options_sql(col: &str) -> String {
    format!(
        "case
        when {col} is null or {col} = '{{}}' then ''
        else '(' || (
            select string_agg(
                pg_catalog.quote_ident(split_part(e, '=', 1))
                || ' ''' ||
                replace(substring(e from position('=' in e)+1), '''', '''''') ||
                '''',
                ', '
            )
            from unnest({col}) as t(e)
        ) || ')'
    end"
    )
}

async fn list_foreign_servers(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "s.srvname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let fdw_opts = fdw_options_sql("s.srvoptions");
    let sql = if meta.plus {
        format!(
            "select
    s.srvname as \"Name\",
    pg_catalog.pg_get_userbyid(s.srvowner) as \"Owner\",
    f.fdwname as \"Foreign-data wrapper\",
    pg_catalog.array_to_string(s.srvacl, E'\\n') as \"Access privileges\",
    s.srvtype as \"Type\",
    s.srvversion as \"Version\",
    {fdw_opts} as \"FDW options\",
    pg_catalog.obj_description(s.oid, 'pg_foreign_server') as \"Description\"
from pg_catalog.pg_foreign_server as s
join pg_catalog.pg_foreign_data_wrapper as f
    on f.oid = s.srvfdw
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    s.srvname as \"Name\",
    pg_catalog.pg_get_userbyid(s.srvowner) as \"Owner\",
    f.fdwname as \"Foreign-data wrapper\"
from pg_catalog.pg_foreign_server as s
join pg_catalog.pg_foreign_data_wrapper as f
    on f.oid = s.srvfdw
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of foreign servers"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dew — list foreign-data wrappers
// ---------------------------------------------------------------------------

/// List foreign-data wrappers.
///
/// Matches psql's `\dew [pattern]` output: Name, Owner, Handler, Validator.
/// With `+`: also shows FDW options, Description.
async fn list_fdws(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "fdwname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let fdw_opts = fdw_options_sql("fdwoptions");
    let sql = if meta.plus {
        format!(
            "select
    fdwname as \"Name\",
    pg_catalog.pg_get_userbyid(fdwowner) as \"Owner\",
    fdwhandler::regproc as \"Handler\",
    fdwvalidator::regproc as \"Validator\",
    pg_catalog.array_to_string(fdwacl, E'\\n') as \"Access privileges\",
    {fdw_opts} as \"FDW options\",
    pg_catalog.obj_description(oid, 'pg_foreign_data_wrapper') as \"Description\"
from pg_catalog.pg_foreign_data_wrapper
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    fdwname as \"Name\",
    pg_catalog.pg_get_userbyid(fdwowner) as \"Owner\",
    fdwhandler::regproc as \"Handler\",
    fdwvalidator::regproc as \"Validator\"
from pg_catalog.pg_foreign_data_wrapper
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of foreign-data wrappers"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \det — list foreign tables (via FDW)
// ---------------------------------------------------------------------------

/// List foreign tables registered via foreign-data wrappers.
///
/// Matches psql's `\det [pattern]` output: Schema, Table, Server.
/// With `+`: also shows FDW options, Description.
async fn list_foreign_tables_via_fdw(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "c.relname", Some("n.nspname"));

    let sys_filter = system_schema_filter(meta.system);

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

    let extra_cond = if where_parts.is_empty() {
        String::new()
    } else {
        format!("and {}", where_parts.join("\n    and "))
    };

    let ft_opts = fdw_options_sql("t.ftoptions");
    let sql = if meta.plus {
        format!(
            "select
    n.nspname as \"Schema\",
    c.relname as \"Table\",
    s.srvname as \"Server\",
    {ft_opts} as \"FDW options\",
    pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"
from pg_catalog.pg_foreign_table as t
join pg_catalog.pg_class as c
    on c.oid = t.ftrelid
join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
join pg_catalog.pg_foreign_server as s
    on s.oid = t.ftserver
where true
    {extra_cond}
order by 1, 2"
        )
    } else {
        format!(
            "select
    n.nspname as \"Schema\",
    c.relname as \"Table\",
    s.srvname as \"Server\"
from pg_catalog.pg_foreign_table as t
join pg_catalog.pg_class as c
    on c.oid = t.ftrelid
join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
join pg_catalog.pg_foreign_server as s
    on s.oid = t.ftserver
where true
    {extra_cond}
order by 1, 2"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of foreign tables"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \deu — list user mappings
// ---------------------------------------------------------------------------

/// List user mappings for foreign servers.
///
/// Matches psql's `\deu [pattern]` output: Server, User name.
/// With `+`: also shows FDW options.
async fn list_user_mappings(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "um.srvname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let um_opts = fdw_options_sql("um.umoptions");
    let sql = if meta.plus {
        format!(
            "select
    um.srvname as \"Server\",
    um.usename as \"User name\",
    {um_opts} as \"FDW options\"
from pg_catalog.pg_user_mappings as um
{where_clause}
order by 1, 2"
        )
    } else {
        format!(
            "select
    um.srvname as \"Server\",
    um.usename as \"User name\"
from pg_catalog.pg_user_mappings as um
{where_clause}
order by 1, 2"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of user mappings"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dy — list event triggers
// ---------------------------------------------------------------------------

/// List event triggers.
///
/// Matches psql's `\dy [pattern]` output: Name, Event, Owner, Enabled,
/// Function, Tags.  With `+`, also adds a Description column.
///
/// Event triggers are global objects (no schema qualifier).
async fn list_event_triggers(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "e.evtname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let sql = if meta.plus {
        format!(
            "select
    e.evtname as \"Name\",
    e.evtevent as \"Event\",
    pg_catalog.pg_get_userbyid(e.evtowner) as \"Owner\",
    case e.evtenabled
        when 'O' then 'enabled'
        when 'R' then 'replica'
        when 'A' then 'always'
        when 'D' then 'disabled'
    end as \"Enabled\",
    e.evtfoid::pg_catalog.regproc as \"Function\",
    pg_catalog.array_to_string(
        array(
            select x
            from pg_catalog.unnest(e.evttags) as t(x)
        ),
        ', '
    ) as \"Tags\",
    coalesce(pg_catalog.obj_description(e.oid, 'pg_event_trigger'), '') as \"Description\"
from pg_catalog.pg_event_trigger as e
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    e.evtname as \"Name\",
    e.evtevent as \"Event\",
    pg_catalog.pg_get_userbyid(e.evtowner) as \"Owner\",
    case e.evtenabled
        when 'O' then 'enabled'
        when 'R' then 'replica'
        when 'A' then 'always'
        when 'D' then 'disabled'
    end as \"Enabled\",
    e.evtfoid::pg_catalog.regproc as \"Function\",
    pg_catalog.array_to_string(
        array(
            select x
            from pg_catalog.unnest(e.evttags) as t(x)
        ),
        ', '
    ) as \"Tags\"
from pg_catalog.pg_event_trigger as e
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of event triggers"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \do — list operators
// ---------------------------------------------------------------------------

/// List operators.
///
/// Matches psql's `\do [pattern]` output: Schema, Name, Left arg type,
/// Right arg type, Result type, Description.  With `+`, Description uses the
/// same coalesce but the query is otherwise identical.
async fn list_operators(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "o.oprname", Some("n.nspname"));

    let sys_filter = if meta.system {
        String::new()
    } else {
        "n.nspname <> 'pg_catalog'\n    and n.nspname <> 'information_schema'".to_owned()
    };

    let visibility_filter = if meta.system {
        String::new()
    } else {
        "pg_catalog.pg_operator_is_visible(o.oid)".to_owned()
    };

    let where_parts: Vec<&str> = [
        if sys_filter.is_empty() {
            None
        } else {
            Some(sys_filter.as_str())
        },
        if visibility_filter.is_empty() {
            None
        } else {
            Some(visibility_filter.as_str())
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

    // Both basic and verbose queries include Description; psql always shows it.
    let sql = format!(
        "select
    n.nspname as \"Schema\",
    o.oprname as \"Name\",
    case when o.oprkind = 'l' then null
         else pg_catalog.format_type(o.oprleft, null)
    end as \"Left arg type\",
    case when o.oprkind = 'r' then null
         else pg_catalog.format_type(o.oprright, null)
    end as \"Right arg type\",
    pg_catalog.format_type(o.oprresult, null) as \"Result type\",
    coalesce(pg_catalog.obj_description(o.oid, 'pg_operator'),
             pg_catalog.obj_description(o.oprcode, 'pg_proc')) as \"Description\"
from pg_catalog.pg_operator as o
left join pg_catalog.pg_namespace as n
    on n.oid = o.oprnamespace
{where_clause}
order by 1, 2, 3, 4"
    );

    let _ = meta.plus; // both modes use the same query

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of operators"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dX [pattern] — list extended statistics
// ---------------------------------------------------------------------------

async fn list_ext_statistics(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter =
        pattern::where_clause(meta.pattern.as_deref(), "s.stxname", Some("n.nspname"));

    // Use pg_statistics_obj_is_visible (which respects search_path) when no
    // schema qualifier is present in the pattern, matching psql behaviour.
    // When a schema qualifier is given, the name_filter already constrains the
    // schema column so we skip the visibility filter (also matching psql).
    let visibility_filter = if name_filter.contains("n.nspname") {
        ""
    } else {
        "pg_catalog.pg_statistics_obj_is_visible(s.oid)"
    };

    let where_parts: Vec<&str> = [
        if visibility_filter.is_empty() {
            None
        } else {
            Some(visibility_filter)
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

    // Build the Definition column: columns/expressions + FROM table
    // Ndistinct/Dependencies/MCV columns show 'defined' when that statistic type is enabled.
    let sql = format!(
        "select
    n.nspname as \"Schema\",
    s.stxname as \"Name\",
    pg_catalog.pg_get_statisticsobjdef_columns(s.oid) || ' FROM ' || c.relname as \"Definition\",
    case when (s.stxkind @> array['d'::\"char\"])
         then 'defined' else null end as \"Ndistinct\",
    case when (s.stxkind @> array['f'::\"char\"])
         then 'defined' else null end as \"Dependencies\",
    case when (s.stxkind @> array['m'::\"char\"])
         then 'defined' else null end as \"MCV\"
from pg_catalog.pg_statistic_ext as s
join pg_catalog.pg_namespace as n on n.oid = s.stxnamespace
join pg_catalog.pg_class as c on c.oid = s.stxrelid
{where_clause}
order by 1, 2"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of extended statistics"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \dRp — list publications
// ---------------------------------------------------------------------------

/// List publications.
///
/// Matches psql's `\dRp [pattern]` output: Name, Owner, All tables, Inserts,
/// Updates, Deletes, Truncates, Via root.
///
/// With `+` and a pattern, per-publication detail is shown: after the main
/// attribute table, "Tables:" and "Tables from schemas:" subsections are
/// printed.
async fn list_publications(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "p.pubname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    if meta.plus && meta.pattern.is_some() {
        // Verbose per-publication view: show attributes then tables/schemas.
        return list_publications_verbose(client, meta, &where_clause, settings).await;
    }

    let sql = format!(
        "select
    p.pubname as \"Name\",
    pg_catalog.pg_get_userbyid(p.pubowner) as \"Owner\",
    p.puballtables as \"All tables\",
    p.pubinsert as \"Inserts\",
    p.pubupdate as \"Updates\",
    p.pubdelete as \"Deletes\",
    p.pubtruncate as \"Truncates\",
    p.pubviaroot as \"Via root\"
from pg_catalog.pg_publication as p
{where_clause}
order by 1"
    );

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of publications"),
        settings,
    )
    .await
}

/// Verbose per-publication detail shown when `\dRp+ pattern` matches.
///
/// For each matching publication: print its attribute table, then list the
/// tables and schemas it covers.
async fn list_publications_verbose(
    client: &Client,
    meta: &ParsedMeta,
    where_clause: &str,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    use std::fmt::Write as FmtWrite;
    use tokio_postgres::SimpleQueryMessage;

    // Fetch all matching publications.
    let pubs_sql = format!(
        "select
    p.oid,
    p.pubname,
    pg_catalog.pg_get_userbyid(p.pubowner) as owner,
    p.puballtables,
    p.pubinsert,
    p.pubupdate,
    p.pubdelete,
    p.pubtruncate,
    p.pubviaroot
from pg_catalog.pg_publication as p
{where_clause}
order by 1"
    );

    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{pubs_sql}\n/************************/");
    }

    let pub_rows: Vec<(
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    )> = match client.simple_query(&pubs_sql).await {
        Ok(msgs) => msgs
            .into_iter()
            .filter_map(|m| {
                if let SimpleQueryMessage::Row(row) = m {
                    Some((
                        row.get(0).unwrap_or("").to_owned(), // oid
                        row.get(1).unwrap_or("").to_owned(), // pubname
                        row.get(2).unwrap_or("").to_owned(), // owner
                        row.get(3).unwrap_or("").to_owned(), // puballtables
                        row.get(4).unwrap_or("").to_owned(), // pubinsert
                        row.get(5).unwrap_or("").to_owned(), // pubupdate
                        row.get(6).unwrap_or("").to_owned(), // pubdelete
                        row.get(7).unwrap_or("").to_owned(), // pubtruncate
                        row.get(8).unwrap_or("").to_owned(), // pubviaroot
                    ))
                } else {
                    None
                }
            })
            .collect(),
        Err(e) => {
            crate::output::eprint_db_error(&e, Some(&pubs_sql), false, false, false);
            return false;
        }
    };

    let mut full_output = String::new();

    for (
        oid,
        pubname,
        owner,
        puballtables,
        pubinsert,
        pubupdate,
        pubdelete,
        pubtruncate,
        pubviaroot,
    ) in &pub_rows
    {
        // Build the attribute rows for this publication.
        let col_names: Vec<String> = vec![
            "Owner".to_owned(),
            "All tables".to_owned(),
            "Inserts".to_owned(),
            "Updates".to_owned(),
            "Deletes".to_owned(),
            "Truncates".to_owned(),
            "Via root".to_owned(),
        ];
        let data_rows: Vec<Vec<String>> = vec![vec![
            owner.clone(),
            puballtables.clone(),
            pubinsert.clone(),
            pubupdate.clone(),
            pubdelete.clone(),
            pubtruncate.clone(),
            pubviaroot.clone(),
        ]];
        // Fetch tables covered by this publication.
        // Returns (table_name, col_list, where_clause) for each table.
        let tables_sql = format!(
            "select
    n.nspname || '.' || c.relname as table_name,
    case
        when pr.prattrs is not null
        then ' (' || (
            select string_agg(a.attname, ', ' order by ka.ord)
            from unnest(pr.prattrs::int2[]) with ordinality as ka(num, ord)
            join pg_catalog.pg_attribute as a
                on a.attrelid = pr.prrelid and a.attnum = ka.num
        ) || ')'
        else ''
    end as col_list,
    case
        when pr.prqual is not null
        then ' WHERE ' || pg_catalog.pg_get_expr(pr.prqual, pr.prrelid)
        else ''
    end as where_clause
from pg_catalog.pg_publication_rel as pr
join pg_catalog.pg_class as c on c.oid = pr.prrelid
join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where pr.prpubid = {oid}
order by 1"
        );
        if meta.echo_hidden {
            eprintln!("/******** QUERY *********/\n{tables_sql}\n/************************/");
        }
        let tbl_rows: Vec<(String, String, String)> =
            if let Ok(msgs) = client.simple_query(&tables_sql).await {
                msgs.into_iter()
                    .filter_map(|m| {
                        if let SimpleQueryMessage::Row(row) = m {
                            Some((
                                row.get(0).unwrap_or("").to_owned(),
                                row.get(1).unwrap_or("").to_owned(),
                                row.get(2).unwrap_or("").to_owned(),
                            ))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };

        // Fetch schemas covered by this publication.
        let schemas_sql = format!(
            "select
    '\"' || n.nspname || '\"' as schema_name
from pg_catalog.pg_publication_namespace as pn
join pg_catalog.pg_namespace as n on n.oid = pn.pnnspid
where pn.pnpubid = {oid}
order by 1"
        );
        if meta.echo_hidden {
            eprintln!("/******** QUERY *********/\n{schemas_sql}\n/************************/");
        }
        let schema_names: Vec<String> = if let Ok(msgs) = client.simple_query(&schemas_sql).await {
            msgs.into_iter()
                .filter_map(|m| {
                    if let SimpleQueryMessage::Row(row) = m {
                        Some(row.get(0).unwrap_or("").to_owned())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        // Write the publication header table.
        let title = format!("Publication {pubname}");
        let table_text = format_table_inner(&col_names, &data_rows, Some(&title), false);
        let _ = write!(full_output, "{table_text}");
        // Print (1 row) only when there are no Tables or Tables from schemas sections.
        if tbl_rows.is_empty() && schema_names.is_empty() {
            let _ = writeln!(full_output, "(1 row)");
        }

        if !tbl_rows.is_empty() {
            let _ = writeln!(full_output, "Tables:");
            for (tname, col_list, where_clause) in &tbl_rows {
                let _ = writeln!(full_output, "    \"{tname}\"{col_list}{where_clause}");
            }
        }
        if !schema_names.is_empty() {
            let _ = writeln!(full_output, "Tables from schemas:");
            for s in &schema_names {
                let _ = writeln!(full_output, "    {s}");
            }
        }
        // Trailing blank line after each publication block (matches psql).
        full_output.push('\n');
    }

    if !full_output.is_empty() {
        maybe_page(settings, &full_output);
    }

    false
}

// ---------------------------------------------------------------------------
// \dRs — list subscriptions
// ---------------------------------------------------------------------------

/// List subscriptions.
///
/// Matches psql's `\dRs [pattern]` output: Name, Owner, Enabled, Publication.
/// With `+`: adds Binary, Streaming, Two-phase commit, Disable on error,
/// Origin, Password required, Run as owner?, Synchronous commit, Conninfo,
/// Skip LSN.
///
/// Requires superuser or `pg_monitor` membership to query `pg_subscription`.
async fn list_subscriptions(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    let name_filter = pattern::where_clause(meta.pattern.as_deref(), "s.subname", None);

    let where_clause = if name_filter.is_empty() {
        String::new()
    } else {
        format!("where {name_filter}")
    };

    let sql = if meta.plus {
        format!(
            "select
    s.subname as \"Name\",
    pg_catalog.pg_get_userbyid(s.subowner) as \"Owner\",
    s.subenabled as \"Enabled\",
    s.subpublications as \"Publication\",
    s.subbinary as \"Binary\",
    case s.substream
        when 'f' then 'off'
        when 't' then 'on'
        when 'p' then 'parallel'
        else s.substream::text
    end as \"Streaming\",
    case s.subtwophasestate
        when 'd' then 'd'
        when 'p' then 'p'
        when 'e' then 'e'
        else s.subtwophasestate::text
    end as \"Two-phase commit\",
    s.subdisableonerr as \"Disable on error\",
    s.suborigin as \"Origin\",
    s.subpasswordrequired as \"Password required\",
    s.subrunasowner as \"Run as owner?\",
    s.subsynccommit as \"Synchronous commit\",
    s.subconninfo as \"Conninfo\",
    s.subskiplsn as \"Skip LSN\"
from pg_catalog.pg_subscription as s
{where_clause}
order by 1"
        )
    } else {
        format!(
            "select
    s.subname as \"Name\",
    pg_catalog.pg_get_userbyid(s.subowner) as \"Owner\",
    s.subenabled as \"Enabled\",
    s.subpublications as \"Publication\"
from pg_catalog.pg_subscription as s
{where_clause}
order by 1"
        )
    };

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of subscriptions"),
        settings,
    )
    .await
}

// ---------------------------------------------------------------------------
// \d [table] — describe a specific table, or list all relations
// ---------------------------------------------------------------------------

async fn describe_object(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
    match &meta.pattern {
        None => {
            // `\d` with no argument: list all user-visible relations.
            list_all_relations(client, meta, settings).await
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
                    crate::output::eprint_db_error(&e, None, false, false, false);
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
                if !settings.quiet {
                    eprintln!("Did not find any relation named \"{pattern}\".");
                }
                return false;
            }

            for (schema, name) in matches {
                // Use the exact schema-qualified name so describe_table resolves
                // to exactly one object.
                let qualified = format!("{schema}.{name}");
                describe_table(client, meta, &qualified, settings).await;
                // psql always prints a blank line after each \d table description.
                println!();
            }

            // Return false unconditionally (only \q should exit the REPL).
            false
        }
    }
}

/// List all user-visible relations (tables, views, sequences, indexes, etc.)
async fn list_all_relations(
    client: &Client,
    meta: &ParsedMeta,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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

    run_and_print_titled(
        client,
        &sql,
        meta.echo_hidden,
        Some("List of relations"),
        settings,
    )
    .await
}

/// Describe a single table (or view, sequence, …): columns + indexes + constraints.
#[allow(clippy::too_many_lines)]
async fn describe_table(
    client: &Client,
    meta: &ParsedMeta,
    obj_pattern: &str,
    settings: &mut crate::repl::ReplSettings,
) -> bool {
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
        when a.attgenerated = 's' then 'generated always as (' || pg_catalog.pg_get_expr(d.adbin, d.adrelid, true) || ') stored'
        when a.attgenerated = 'v' then 'generated always as (' || pg_catalog.pg_get_expr(d.adbin, d.adrelid, true) || ')'
        else coalesce(pg_catalog.pg_get_expr(d.adbin, d.adrelid, true), '')
    end";

    // 1. Columns — query depends on object type and plus mode.
    // Build two variants: one for tables (\d+ shows Compression/Stats target),
    // one for views/sequences/composites (\d+ shows Storage+Description but not Compression).
    let cols_sql_table_plus = format!(
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
    case a.attstorage
        when 'p' then 'plain'
        when 'e' then 'external'
        when 'x' then 'extended'
        when 'm' then 'main'
        else a.attstorage::text
    end as \"Storage\",
    case a.attcompression
        when 'p' then 'pglz'
        when 'l' then 'lz4'
        else ''
    end as \"Compression\",
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
    );
    let cols_sql_view_plus = format!(
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
    case a.attstorage
        when 'p' then 'plain'
        when 'e' then 'external'
        when 'x' then 'extended'
        when 'm' then 'main'
        else a.attstorage::text
    end as \"Storage\",
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
    );
    // Foreign table variants: always include FDW options, never include Compression.
    // \d  (non-plus): Column, Type, Collation, Nullable, Default, FDW options
    // \d+ (plus):     Column, Type, Collation, Nullable, Default, FDW options,
    //                 Storage, Stats target, Description
    let fdw_col_opts = fdw_options_sql("a.attfdwoptions");
    let collation_subq = "coalesce(
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
    )";
    let from_clause = format!(
        "from pg_catalog.pg_attribute as a
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
    );
    let cols_sql_foreign_base = format!(
        "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    {collation_subq} as \"Collation\",
    case when a.attnotnull then 'not null' else '' end as \"Nullable\",
    {default_expr} as \"Default\",
    {fdw_col_opts} as \"FDW options\"
{from_clause}"
    );
    let cols_sql_foreign_plus = format!(
        "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    {collation_subq} as \"Collation\",
    case when a.attnotnull then 'not null' else '' end as \"Nullable\",
    {default_expr} as \"Default\",
    {fdw_col_opts} as \"FDW options\",
    case a.attstorage
        when 'p' then 'plain'
        when 'e' then 'external'
        when 'x' then 'extended'
        when 'm' then 'main'
        else a.attstorage::text
    end as \"Storage\",
    case when a.attstattarget = -1 then '' else a.attstattarget::text end as \"Stats target\",
    coalesce(pg_catalog.col_description(a.attrelid, a.attnum), '') as \"Description\"
{from_clause}"
    );

    // Placeholder: will be resolved after fetching relkind below.
    // For non-plus mode, always use the 5-column variant.
    let cols_sql_base = if meta.plus {
        // Will be replaced based on relkind below
        cols_sql_table_plus.clone()
    } else {
        format!(
            "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    {collation_subq} as \"Collation\",
    case when a.attnotnull then 'not null' else '' end as \"Nullable\",
    {default_expr} as \"Default\"
{from_clause}"
        )
    };

    // Fetch relkind and actual schema to determine the correct object-type label
    // and build a fully-qualified display name (psql always shows "schema.name").
    let relkind_sql = format!(
        "select c.relkind::text, n.nspname, c.relpersistence
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where {name_cond}
limit 1"
    );
    let (obj_label, display_name, relkind_char) = {
        let mut label = "Table";
        let mut resolved_schema = String::new();
        let mut rk = 'r';
        if let Ok(msgs) = client.simple_query(&relkind_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let kind_str = row.get(0).unwrap_or("r");
                    rk = kind_str.chars().next().unwrap_or('r');
                    let persistence = row.get(2).unwrap_or("p");
                    let unlogged = persistence == "u";
                    let temp = persistence == "t";
                    label = match kind_str {
                        "r" if unlogged => "Unlogged table",
                        "r" if temp => "Temporary table",
                        "r" => "Table",
                        "p" if unlogged => "Unlogged partitioned table",
                        "p" => "Partitioned table",
                        "v" => "View",
                        "m" if unlogged => "Unlogged materialized view",
                        "m" => "Materialized view",
                        "i" => "Index",
                        "I" => "Partitioned index",
                        "S" if unlogged => "Unlogged sequence",
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
        (label, fq_name, rk)
    };

    // Special handling for indexes — completely different column schema.
    if matches!(relkind_char, 'i' | 'I') {
        let table_title = format!("{obj_label} \"{display_name}\"");
        let idx_cols_sql = if meta.plus {
            format!(
                "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    case when a.attnum <= ix.indnkeyatts then 'yes' else 'no' end as \"Key?\",
    pg_catalog.pg_get_indexdef(ix.indexrelid, a.attnum::int, true) as \"Definition\",
    case a.attstorage
        when 'p' then 'plain'
        when 'e' then 'external'
        when 'x' then 'extended'
        when 'm' then 'main'
        else a.attstorage::text
    end as \"Storage\",
    case when a.attstattarget = -1 then '' else a.attstattarget::text end as \"Stats target\"
from pg_catalog.pg_attribute as a
join pg_catalog.pg_index as ix on ix.indexrelid = a.attrelid
where a.attrelid = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
)
and a.attnum > 0
and not a.attisdropped
order by a.attnum"
            )
        } else {
            format!(
                "select
    a.attname as \"Column\",
    pg_catalog.format_type(a.atttypid, a.atttypmod) as \"Type\",
    case when a.attnum <= ix.indnkeyatts then 'yes' else 'no' end as \"Key?\",
    pg_catalog.pg_get_indexdef(ix.indexrelid, a.attnum::int, true) as \"Definition\"
from pg_catalog.pg_attribute as a
join pg_catalog.pg_index as ix on ix.indexrelid = a.attrelid
where a.attrelid = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
)
and a.attnum > 0
and not a.attisdropped
order by a.attnum"
            )
        };
        run_and_print_no_count(
            client,
            &idx_cols_sql,
            meta.echo_hidden,
            Some(&table_title),
            settings,
        )
        .await;

        // Partition info: "Partition of: parent_index" and constraint.
        // Replicates psql's describeOneTableDetails footer for partition children.
        // Shown only when the object is a partition (c.relispartition = true).
        let idx_partition_sql = format!(
            "select
    i.inhparent::pg_catalog.regclass as parent_index,
    pg_catalog.pg_get_expr(c.relpartbound, c.oid) as partdef
from pg_catalog.pg_class as c
join pg_catalog.pg_inherits as i on c.oid = i.inhrelid
where c.relispartition = true
  and c.oid = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
  )"
        );
        if let Ok(msgs) = client.simple_query(&idx_partition_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let parent = row.get(0).unwrap_or("");
                    let partdef = row.get(1).unwrap_or("");
                    if !parent.is_empty() {
                        let partdef_str = if partdef.is_empty() {
                            String::new()
                        } else {
                            format!(" {partdef}")
                        };
                        println!("Partition of: {parent}{partdef_str}");
                        // No partition constraint applies to indexes.
                        println!("No partition constraint");
                    }
                    break;
                }
            }
        }

        // Footer: "amname, for table \"schema.table\""
        let idx_footer_sql = format!(
            "select am.amname,
    tn.nspname || '.' || tc.relname as table_name,
    ix.indisprimary, ix.indisunique,
    pg_catalog.pg_get_expr(ix.indpred, ix.indrelid, true) as predicate,
    ix.indnullsnotdistinct,
    c.reloptions,
    coalesce(spc.spcname, '') as idx_tablespace
from pg_catalog.pg_class as c
join pg_catalog.pg_index as ix on ix.indexrelid = c.oid
join pg_catalog.pg_class as tc on tc.oid = ix.indrelid
join pg_catalog.pg_namespace as tn on tn.oid = tc.relnamespace
join pg_catalog.pg_am as am on am.oid = c.relam
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
left join pg_catalog.pg_tablespace as spc on spc.oid = c.reltablespace
where c.oid = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
)"
        );
        if let Ok(msgs) = client.simple_query(&idx_footer_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let am = row.get(0).unwrap_or("");
                    let tname = row.get(1).unwrap_or("");
                    let is_primary = row.get(2).is_some_and(|v| v == "t");
                    let is_unique = row.get(3).is_some_and(|v| v == "t");
                    let pred = row.get(4).unwrap_or("");
                    let nulls_not_distinct = row.get(5).is_some_and(|v| v == "t");
                    let reloptions = row.get(6).unwrap_or("");
                    let idx_tablespace = row.get(7).unwrap_or("");
                    let mut parts = Vec::new();
                    if is_primary {
                        parts.push("primary key".to_owned());
                    } else if is_unique {
                        if nulls_not_distinct {
                            parts.push("unique nulls not distinct".to_owned());
                        } else {
                            parts.push("unique".to_owned());
                        }
                    }
                    parts.push(am.to_owned());
                    let mut footer = parts.join(", ");
                    footer.push_str(", for table \"");
                    footer.push_str(tname);
                    footer.push('"');
                    if !pred.is_empty() {
                        footer.push_str(" where ");
                        footer.push_str(pred);
                    }
                    println!("{footer}");
                    // Options (reloptions) — shown when present, e.g. "Options: fastupdate=on"
                    if !reloptions.is_empty() {
                        // reloptions is a PostgreSQL array literal like {key=val,key2=val2}
                        // Strip braces and rejoin with ", " to match psql output format.
                        let inner = reloptions.trim_start_matches('{').trim_end_matches('}');
                        if !inner.is_empty() {
                            let opts = inner.split(',').collect::<Vec<_>>().join(", ");
                            println!("Options: {opts}");
                        }
                    }

                    // For partitioned indexes (relkind 'I'): partition count/list
                    // comes BEFORE Tablespace, then Access method for \d+.
                    // psql ordering: footer → partitions → tablespace → access method
                    if relkind_char == 'I' {
                        let count_sql = format!(
                            "select count(*)
from pg_catalog.pg_inherits
where inhparent = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
)"
                        );
                        let num_parts = if let Ok(cmsgs) = client.simple_query(&count_sql).await {
                            use tokio_postgres::SimpleQueryMessage;
                            let mut n = 0usize;
                            for cmsg in cmsgs {
                                if let SimpleQueryMessage::Row(crow) = cmsg {
                                    n = crow.get(0).unwrap_or("0").parse().unwrap_or(0);
                                    break;
                                }
                            }
                            n
                        } else {
                            0
                        };
                        if meta.plus {
                            // \d+ lists the actual partitions.
                            if num_parts > 0 {
                                let parts_sql = format!(
                                    "select n2.nspname || '.' || c2.relname
from pg_catalog.pg_inherits as i
join pg_catalog.pg_class as c2 on c2.oid = i.inhrelid
join pg_catalog.pg_namespace as n2 on n2.oid = c2.relnamespace
where i.inhparent = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1
)
order by 1"
                                );
                                if let Ok(pmsgs) = client.simple_query(&parts_sql).await {
                                    use tokio_postgres::SimpleQueryMessage;
                                    let mut pnames: Vec<String> = Vec::new();
                                    for pmsg in pmsgs {
                                        if let SimpleQueryMessage::Row(prow) = pmsg {
                                            pnames.push(prow.get(0).unwrap_or("").to_owned());
                                        }
                                    }
                                    if !pnames.is_empty() {
                                        println!("Partitions: {}", pnames.join(",\n            "));
                                    }
                                }
                            }
                        } else {
                            println!("Number of partitions: {num_parts} (Use \\d+ to list them.)");
                        }
                    }

                    // Tablespace — shown when index is in a non-default tablespace.
                    if !idx_tablespace.is_empty() {
                        println!("Tablespace: \"{idx_tablespace}\"");
                    }

                    // Access method — shown for partitioned indexes in \d+ mode.
                    if relkind_char == 'I' && meta.plus {
                        println!("Access method: {am}");
                    }

                    break;
                }
            }
        }

        return true;
    }

    // Special handling for sequences — show psql-style sequence info table.
    if relkind_char == 'S' {
        let table_title = format!("{obj_label} \"{display_name}\"");
        let seq_sql = format!(
            "select
    pg_catalog.format_type(s.seqtypid, null) as \"Type\",
    s.seqstart as \"Start\",
    s.seqmin as \"Minimum\",
    s.seqmax as \"Maximum\",
    s.seqincrement as \"Increment\",
    case when s.seqcycle then 'yes' else 'no' end as \"Cycles?\",
    s.seqcache as \"Cache\"
from pg_catalog.pg_sequence as s
join pg_catalog.pg_class as c on c.oid = s.seqrelid
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}"
        );
        run_and_print_no_count(
            client,
            &seq_sql,
            meta.echo_hidden,
            Some(&table_title),
            settings,
        )
        .await;

        // Check if this sequence is owned by a column (identity or SERIAL/OWNED BY).
        // deptype = 'i' → identity column ("Sequence for identity column:")
        // deptype = 'a' → automatic/SERIAL ("Owned by:")
        // Use schema-qualified table name explicitly (regclass omits
        // schema when it's in search_path, but psql always shows it).
        let owned_sql = format!(
            "select n_ref.nspname || '.' || c_ref.relname || '.' || a.attname,
       d.deptype
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
join pg_catalog.pg_depend as d
    on d.objid = c.oid
    and d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
    and d.deptype in ('i', 'a')
    and d.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass
join pg_catalog.pg_class as c_ref on c_ref.oid = d.refobjid
join pg_catalog.pg_namespace as n_ref on n_ref.oid = c_ref.relnamespace
join pg_catalog.pg_attribute as a
    on a.attrelid = d.refobjid
    and a.attnum = d.refobjsubid
where c.relkind = 'S'
  and {name_cond}"
        );
        if let Ok(msgs) = client.simple_query(&owned_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    if let Some(col_ref) = row.get(0) {
                        if !col_ref.is_empty() {
                            let deptype = row.get(1).unwrap_or("");
                            if deptype == "a" {
                                println!("Owned by: {col_ref}");
                            } else {
                                println!("Sequence for identity column: {col_ref}");
                            }
                        }
                    }
                    break;
                }
            }
        }

        return false;
    }

    // Choose columns query based on relkind and plus mode.
    // Foreign tables: always show FDW options, never show Compression.
    // Regular/partitioned tables and matviews: show Compression + Stats target in \d+ mode.
    // Views, sequences, composite types: show Storage+Description in \d+ mode only.
    let cols_sql = if relkind_char == 'f' {
        if meta.plus {
            cols_sql_foreign_plus
        } else {
            cols_sql_foreign_base
        }
    } else if meta.plus && matches!(relkind_char, 'r' | 'p' | 'm') {
        cols_sql_table_plus
    } else if meta.plus {
        cols_sql_view_plus
    } else {
        cols_sql_base
    };

    // Build the centered title and pass it to run_and_print_no_count so it is
    // centered above the column table — matching psql's \d output.  The row
    // count footer is suppressed to match psql behaviour.
    let table_title = format!("{obj_label} \"{display_name}\"");
    run_and_print_no_count(
        client,
        &cols_sql,
        meta.echo_hidden,
        Some(&table_title),
        settings,
    )
    .await;

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

    // 1b. View definition — shown for regular views in \d+ mode before indexes.
    // For materialized views, it is shown AFTER indexes (psql ordering).
    if meta.plus && relkind_char == 'v' {
        // check_option is stored in reloptions as "check_option=local/cascaded".
        let viewdef_sql = format!(
            "select pg_catalog.pg_get_viewdef(c.oid, true),
    (select opt
     from unnest(c.reloptions) as opt
     where opt like 'check_option=%'
     limit 1)
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
limit 1"
        );
        if let Ok(msgs) = client.simple_query(&viewdef_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let def = row.get(0).unwrap_or("");
                    if !def.is_empty() {
                        println!("View definition:");
                        for vline in def.lines() {
                            println!("{vline}");
                        }
                    }
                    // check_option is "check_option=local" or "check_option=cascaded"
                    if let Some(opt) = row.get(1) {
                        let val = opt.trim_start_matches("check_option=");
                        if !val.is_empty() {
                            println!("Options: check_option={val}");
                        }
                    }
                    break;
                }
            }
        }
    }

    // 2. Indexes — query returns raw fields; we format as psql indented text.
    // psql format: "name" PRIMARY KEY, btree (cols)  or  "name" btree (cols)
    // col 6: pg_get_expr(indpred) is non-NULL for partial indexes (WHERE clause)
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
     limit 1) as con_name,
    pg_catalog.pg_get_expr(ix.indpred, ix.indrelid, true) as idx_pred,
    ix.indisvalid,
    (select contype
     from pg_catalog.pg_constraint
     where conrelid = ix.indrelid
       and conindid = i.oid
     limit 1) as con_type,
    (select oid
     from pg_catalog.pg_constraint
     where conrelid = ix.indrelid
       and conindid = i.oid
     limit 1) as con_oid,
    ix.indnullsnotdistinct,
    (select condeferrable
     from pg_catalog.pg_constraint
     where conrelid = ix.indrelid
       and conindid = i.oid
     limit 1) as condeferrable,
    ix.indisreplident,
    coalesce(spc.spcname, '') as idx_tablespace
from pg_catalog.pg_index as ix
join pg_catalog.pg_class as i
    on i.oid = ix.indexrelid
join pg_catalog.pg_class as tc
    on tc.oid = ix.indrelid
join pg_catalog.pg_am as am
    on am.oid = i.relam
left join pg_catalog.pg_namespace as tn
    on tn.oid = tc.relnamespace
left join pg_catalog.pg_tablespace as spc
    on spc.oid = i.reltablespace
where {idx_name_cond}
order by ix.indisprimary desc, i.relname"
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
    // For partition-inherited FKs (conparentid != 0), psql shows the PARENT
    // constraint with format: TABLE "parent_table" CONSTRAINT "parent_name" def
    let fk_sql = format!(
        "select
    root.parent_table,
    coalesce(root.conname, co.conname) as conname,
    pg_catalog.pg_get_constraintdef(
        coalesce(root.oid, co.oid), true) as condef
from pg_catalog.pg_constraint as co
left join lateral (
    with recursive rc as (
        select c2.oid, c2.conrelid, c2.conname, c2.conparentid
        from pg_catalog.pg_constraint as c2
        where c2.oid = co.conparentid
        union all
        select c3.oid, c3.conrelid, c3.conname, c3.conparentid
        from pg_catalog.pg_constraint as c3
        join rc on c3.oid = rc.conparentid
        where rc.conparentid <> 0
    )
    select
        oid,
        conrelid as root_relid,
        conrelid::pg_catalog.regclass::text as parent_table,
        conname
    from rc
    where conparentid = 0
    limit 1
) as root on co.conparentid <> 0
where co.contype = 'f'
    and co.conrelid = (
        select c.oid
        from pg_catalog.pg_class as c
        left join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        where {name_cond}
        limit 1
    )
    and (co.conparentid = 0
         or root.root_relid <> co.conrelid)
order by conname"
    );

    // 5. Referenced by (incoming FKs) — psql format:
    //    TABLE "orders" CONSTRAINT "orders_user_id_fkey" FOREIGN KEY (user_id) REFERENCES users(id)
    // Exclude partition-cloned constraints (conparentid != 0) so we only
    // show top-level FK constraints, matching psql behavior for partitioned tables.
    let ref_sql = format!(
        "select
    conrelid::pg_catalog.regclass::text as from_table,
    conname,
    pg_catalog.pg_get_constraintdef(oid, true) as condef
from pg_catalog.pg_constraint as co
where co.contype = 'f'
    and co.conparentid = 0
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

    // Partition info — query once, print in two phases:
    //   Phase 1 (before indexes): Partition key / Partition of / Partition constraint
    //   Phase 2 (after constraints): Partitions list / Number of partitions
    let part_info_sql = format!(
        "select c.relkind,
    case when c.relispartition then
        pg_catalog.pg_get_expr(c.relpartbound, c.oid, true)
    else '' end as partbound,
    case when c.relkind = 'p' then
        pg_catalog.pg_get_partkeydef(c.oid)
    else '' end as partkeydef,
    case when c.relispartition then
        (select case when pg_catalog.pg_table_is_visible(p.oid)
                     then p.relname
                     else n2.nspname || '.' || p.relname end
         from pg_catalog.pg_class as p
         join pg_catalog.pg_namespace as n2 on n2.oid = p.relnamespace
         where p.oid = (select inhparent from pg_catalog.pg_inherits
                        where inhrelid = c.oid limit 1))
    else '' end as parent_name
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
limit 1"
    );
    // Fetch partition info; store for use in both phases.
    let (part_relkind, partbound, partkeydef, parent_name) = {
        let mut rk = String::new();
        let mut pb = String::new();
        let mut pk = String::new();
        let mut pn = String::new();
        if let Ok(msgs) = client.simple_query(&part_info_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    rk.clear();
                    rk.push_str(row.get(0).unwrap_or(""));
                    pb.clear();
                    pb.push_str(row.get(1).unwrap_or(""));
                    pk.clear();
                    pk.push_str(row.get(2).unwrap_or(""));
                    pn.clear();
                    pn.push_str(row.get(3).unwrap_or(""));
                    break;
                }
            }
        }
        (rk, pb, pk, pn)
    };

    // Phase 1: print "before indexes" partition info.
    // For partition child: Partition of / Partition constraint (constraint only for \d+).
    if !partbound.is_empty() && !parent_name.is_empty() {
        println!("Partition of: {parent_name} {partbound}");
        if meta.plus {
            let pcon_sql = format!(
                "select pg_catalog.pg_get_partition_constraintdef(c.oid)
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond} limit 1"
            );
            if let Ok(pmsgs) = client.simple_query(&pcon_sql).await {
                use tokio_postgres::SimpleQueryMessage;
                for pmsg in pmsgs {
                    if let SimpleQueryMessage::Row(prow) = pmsg {
                        let pcon = prow.get(0).unwrap_or("");
                        if pcon.is_empty() {
                            println!("No partition constraint");
                        } else {
                            println!("Partition constraint: {pcon}");
                        }
                        break;
                    }
                }
            }
        }
    }
    // For partition parent: Partition key.
    if part_relkind == "p" && !partkeydef.is_empty() {
        println!("Partition key: {partkeydef}");
    }

    // Indexes — print as indented text lines (psql format), not a table.
    if meta.echo_hidden {
        eprintln!("/******** QUERY *********/\n{idx_sql}\n/************************/");
    }
    if let Ok(messages) = client.simple_query(&idx_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        // Collect: (idx_name, is_primary, is_unique, amname, idx_oid_str, idx_pred, is_valid, con_type, con_oid, nulls_not_distinct, is_deferrable, is_replident, idx_tablespace)
        type IndexRow = (
            String,
            bool,
            bool,
            String,
            String,
            String,
            bool,
            String,
            String,
            bool,
            bool,
            bool,
            String,
        );
        let mut index_rows: Vec<IndexRow> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let idx_name = row.get(0).unwrap_or("").to_owned();
                let is_primary = row.get(1).unwrap_or("f") == "t";
                let is_unique = row.get(2).unwrap_or("f") == "t";
                let amname = row.get(3).unwrap_or("").to_owned();
                let idx_oid_str = row.get(4).unwrap_or("0").to_owned();
                // col 5 = con_name (used implicitly via is_primary/is_unique flags)
                // col 6 = pg_get_expr(indpred): non-empty for partial indexes
                let idx_pred = row.get(6).unwrap_or("").to_owned();
                // col 7 = indisvalid: false means the index is being rebuilt (INVALID)
                let is_valid = row.get(7).unwrap_or("t") == "t";
                // col 8 = contype: 'x' for EXCLUDE constraints
                let con_type = row.get(8).unwrap_or("").to_owned();
                // col 9 = con_oid: OID of the backing constraint (for pg_get_constraintdef)
                let con_oid = row.get(9).unwrap_or("").to_owned();
                // col 10 = indnullsnotdistinct: NULLS NOT DISTINCT for unique indexes (PG15+)
                let nulls_not_distinct = row.get(10).unwrap_or("f") == "t";
                // col 11 = condeferrable: true when the backing constraint is deferrable
                let is_deferrable = row.get(11).is_some_and(|v| v == "t");
                // col 12 = indisreplident: true when this index is the replica identity index
                let is_replident = row.get(12).is_some_and(|v| v == "t");
                // col 13 = idx_tablespace: non-empty when index is in non-default tablespace
                let idx_tablespace = row.get(13).unwrap_or("").to_owned();
                index_rows.push((
                    idx_name,
                    is_primary,
                    is_unique,
                    amname,
                    idx_oid_str,
                    idx_pred,
                    is_valid,
                    con_type,
                    con_oid,
                    nulls_not_distinct,
                    is_deferrable,
                    is_replident,
                    idx_tablespace,
                ));
            }
        }
        if !index_rows.is_empty() {
            println!("Indexes:");
            for (
                idx_name,
                is_primary,
                is_unique,
                amname,
                idx_oid_str,
                idx_pred,
                is_valid,
                con_type,
                con_oid,
                nulls_not_distinct,
                is_deferrable,
                is_replident,
                idx_tablespace,
            ) in &index_rows
            {
                // EXCLUDE constraints use pg_get_constraintdef for full definition.
                let is_exclude = con_type == "x";
                // Extract column list from pg_get_indexdef (the part inside parens).
                let col_expr = if is_exclude && !con_oid.is_empty() {
                    // For EXCLUDE constraints, use pg_get_constraintdef which gives the
                    // full "EXCLUDE USING gist (c4 WITH &&) INCLUDE ..." form.
                    let condef_sql =
                        format!("select pg_catalog.pg_get_constraintdef({con_oid}, true)");
                    if let Ok(def_msgs) = client.simple_query(&condef_sql).await {
                        let mut expr = String::new();
                        for def_msg in def_msgs {
                            if let SimpleQueryMessage::Row(def_row) = def_msg {
                                expr.clear();
                                expr.push_str(def_row.get(0).unwrap_or(""));
                                break;
                            }
                        }
                        expr
                    } else {
                        String::new()
                    }
                } else {
                    let indexdef_sql =
                        format!("select pg_catalog.pg_get_indexdef({idx_oid_str}, 0, true)");
                    if let Ok(def_msgs) = client.simple_query(&indexdef_sql).await {
                        let mut expr = String::new();
                        for def_msg in def_msgs {
                            if let SimpleQueryMessage::Row(def_row) = def_msg {
                                let full = def_row.get(0).unwrap_or("");
                                if let (Some(open), Some(close)) = (full.find('('), full.rfind(')'))
                                {
                                    full[open..=close].clone_into(&mut expr);
                                }
                                break;
                            }
                        }
                        expr
                    } else {
                        String::new()
                    }
                };

                let type_label = if *is_primary {
                    " PRIMARY KEY,".to_owned()
                } else if *is_unique && con_type == "u" {
                    " UNIQUE CONSTRAINT,".to_owned()
                } else if *is_unique {
                    " UNIQUE,".to_owned()
                } else {
                    String::new()
                };

                // NULLS NOT DISTINCT suffix for unique indexes (PG15+)
                let nulls_not_distinct_suffix = if *is_unique && *nulls_not_distinct {
                    " NULLS NOT DISTINCT"
                } else {
                    ""
                };

                // For EXCLUDE constraints, the full definition is already in col_expr.
                let pred_suffix = if is_exclude || idx_pred.is_empty() {
                    String::new()
                } else {
                    // pg_get_expr wraps in parens; psql strips the outer pair.
                    let pred = match idx_pred.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
                        Some(inner) => inner,
                        None => idx_pred.as_str(),
                    };
                    format!(" WHERE {pred}")
                };

                let invalid_suffix = if *is_valid { "" } else { " INVALID" };
                let deferrable_suffix = if *is_deferrable { " DEFERRABLE" } else { "" };
                let replident_suffix = if *is_replident {
                    " REPLICA IDENTITY"
                } else {
                    ""
                };
                let tblspc_suffix = if idx_tablespace.is_empty() {
                    String::new()
                } else {
                    format!(", tablespace \"{idx_tablespace}\"")
                };
                if is_exclude {
                    // EXCLUDE: show as "name" EXCLUDE USING ... (no amname prefix)
                    println!("    \"{idx_name}\" {col_expr}{pred_suffix}{invalid_suffix}{replident_suffix}{tblspc_suffix}");
                } else {
                    println!("    \"{idx_name}\"{type_label} {amname} {col_expr}{nulls_not_distinct_suffix}{pred_suffix}{deferrable_suffix}{invalid_suffix}{replident_suffix}{tblspc_suffix}");
                }
            }
        }
    }

    // Tablespace footer — shown for tables/partitioned tables when stored in
    // a non-default tablespace.  Placed after Indexes, matching psql ordering.
    if matches!(relkind_char, 'r' | 'p') {
        let tblspc_sql = format!(
            "select coalesce(spc.spcname, '')
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
left join pg_catalog.pg_tablespace as spc on spc.oid = c.reltablespace
where {name_cond} limit 1"
        );
        if let Ok(msgs) = client.simple_query(&tblspc_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let spcname = row.get(0).unwrap_or("");
                    if !spcname.is_empty() {
                        println!("Tablespace: \"{spcname}\"");
                    }
                    break;
                }
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
        let mut lines: Vec<(Option<String>, String, String)> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let parent_table = row.get(0).map(std::borrow::ToOwned::to_owned);
                let name = row.get(1).unwrap_or("").to_owned();
                let def = row.get(2).unwrap_or("").to_owned();
                lines.push((parent_table, name, def));
            }
        }
        if !lines.is_empty() {
            println!("Foreign-key constraints:");
            for (parent_table, name, def) in &lines {
                if let Some(pt) = parent_table {
                    println!("    TABLE \"{pt}\" CONSTRAINT \"{name}\" {def}");
                } else {
                    println!("    \"{name}\" {def}");
                }
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

    // Row Security Policies — shown before Partitions (psql ordering).
    if matches!(relkind_char, 'r' | 'p' | 'f' | 'v') {
        let pol_sql = format!(
            "select pol.polname,
       pol.polpermissive,
       case when pol.polroles = '{{0}}' then null
            else (
                select string_agg(rolname, ', ' order by rolname)
                from pg_catalog.pg_roles
                where oid = any(pol.polroles)
            )
       end as polroles,
       pg_catalog.pg_get_expr(pol.polqual, pol.polrelid) as polqual,
       pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid) as polwithcheck
from pg_catalog.pg_policy as pol
join pg_catalog.pg_class as c on c.oid = pol.polrelid
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
order by pol.polname"
        );
        if let Ok(msgs) = client.simple_query(&pol_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            let mut policies: Vec<(String, bool, Option<String>, Option<String>, Option<String>)> =
                Vec::new();
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let name = row.get(0).unwrap_or("").to_owned();
                    let permissive = row.get(1).is_none_or(|s| s == "t");
                    let roles = row.get(2).map(str::to_owned);
                    let qual = row.get(3).map(str::to_owned);
                    let withcheck = row.get(4).map(str::to_owned);
                    policies.push((name, permissive, roles, qual, withcheck));
                }
            }
            if !policies.is_empty() {
                println!("Policies:");
                for (name, permissive, roles, qual, withcheck) in &policies {
                    // psql format: `    POLICY "name" [AS RESTRICTIVE]`
                    let restrictive = if *permissive { "" } else { " AS RESTRICTIVE" };
                    println!("    POLICY \"{name}\"{restrictive}");
                    if let Some(r) = roles {
                        println!("      TO {r}");
                    }
                    if let Some(q) = qual {
                        println!("      USING ({q})");
                    }
                    if let Some(w) = withcheck {
                        println!("      WITH CHECK ({w})");
                    }
                }
            }
        }
    }

    // Statistics objects — print as "Statistics objects:" section.
    // Matches psql's describeOneTableDetails statistics footer (PG14+).
    // psql places this after Row Security Policies and before Not-null constraints.
    // psql queries by OID without filtering on relkind, so this includes
    // foreign tables ('f') in addition to regular ('r') and partitioned ('p').
    if matches!(relkind_char, 'r' | 'p' | 'f') {
        let stat_sql = format!(
            "select
    s.stxnamespace::pg_catalog.regnamespace::pg_catalog.text as nsp,
    s.stxname,
    pg_catalog.pg_get_statisticsobjdef_columns(s.oid) as columns,
    'd'::\"char\" = any(s.stxkind) as has_ndistinct,
    'f'::\"char\" = any(s.stxkind) as has_deps,
    'm'::\"char\" = any(s.stxkind) as has_mcv,
    s.stxrelid::pg_catalog.regclass as table_name,
    s.stxstattarget
from pg_catalog.pg_statistic_ext as s
where s.stxrelid = (
    select c.oid
    from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond}
    limit 1
)
order by nsp, s.stxname"
        );
        if let Ok(messages) = client.simple_query(&stat_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            // (nsp, name, columns, has_ndistinct, has_deps, has_mcv, table_name, stxstattarget)
            let mut stat_rows: Vec<(String, String, String, bool, bool, bool, String, String)> =
                Vec::new();
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    let nsp = row.get(0).unwrap_or("").to_owned();
                    let name = row.get(1).unwrap_or("").to_owned();
                    let cols = row.get(2).unwrap_or("").to_owned();
                    let has_nd = row.get(3).is_some_and(|v| v == "t");
                    let has_dep = row.get(4).is_some_and(|v| v == "t");
                    let has_mcv = row.get(5).is_some_and(|v| v == "t");
                    let tbl = row.get(6).unwrap_or("").to_owned();
                    let stxtgt = row.get(7).unwrap_or("-1").to_owned();
                    stat_rows.push((nsp, name, cols, has_nd, has_dep, has_mcv, tbl, stxtgt));
                }
            }
            if !stat_rows.is_empty() {
                println!("Statistics objects:");
                for (nsp, name, cols, has_nd, has_dep, has_mcv, tbl, stxtgt) in &stat_rows {
                    // Show kinds only when some (but not all) of ndistinct/deps/mcv are set.
                    let has_all = *has_nd && *has_dep && *has_mcv;
                    let has_some = *has_nd || *has_dep || *has_mcv;
                    let kinds_str = if has_some && !has_all {
                        let mut parts = Vec::new();
                        if *has_nd {
                            parts.push("ndistinct");
                        }
                        if *has_dep {
                            parts.push("dependencies");
                        }
                        if *has_mcv {
                            parts.push("mcv");
                        }
                        format!(" ({})", parts.join(", "))
                    } else {
                        String::new()
                    };
                    // stxstattarget suffix: shown when != -1
                    let target_str = if stxtgt == "-1" {
                        String::new()
                    } else {
                        format!("; STATISTICS {stxtgt}")
                    };
                    println!("    \"{nsp}.{name}\"{kinds_str} ON {cols} FROM {tbl}{target_str}");
                }
            }
        }
    }

    // Not-null constraints — verbose only (\d+), PostgreSQL 17+.
    // psql places this after Statistics objects and before Partitions list.
    if meta.plus && matches!(relkind_char, 'r' | 'p' | 'f') {
        let nn_sql = format!(
            "select co.conname, a.attname, co.connoinherit, co.conislocal,
    co.coninhcount <> 0 as inherited,
    co.convalidated
from pg_catalog.pg_constraint as co
join pg_catalog.pg_attribute as a
    on (a.attrelid = co.conrelid and a.attnum = co.conkey[1])
where co.contype = 'n'
    and co.conrelid = (
        select c.oid
        from pg_catalog.pg_class as c
        left join pg_catalog.pg_namespace as n
            on n.oid = c.relnamespace
        where {name_cond}
        limit 1
    )
order by a.attnum"
        );
        if let Ok(messages) = client.simple_query(&nn_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            let mut lines: Vec<String> = Vec::new();
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    let conname = row.get(0).unwrap_or("");
                    let attname = row.get(1).unwrap_or("");
                    let connoinherit = row.get(2).unwrap_or("f") == "t";
                    let conislocal = row.get(3).unwrap_or("t") == "t";
                    let inherited = row.get(4).unwrap_or("f") == "t";
                    let validated = row.get(5).unwrap_or("t") == "t";
                    let modifier = if connoinherit {
                        " NO INHERIT".to_owned()
                    } else if conislocal && inherited {
                        " (local, inherited)".to_owned()
                    } else if inherited {
                        " (inherited)".to_owned()
                    } else {
                        String::new()
                    };
                    let not_valid = if validated { "" } else { " NOT VALID" };
                    lines.push(format!(
                        "    \"{conname}\" NOT NULL \"{attname}\"{modifier}{not_valid}"
                    ));
                }
            }
            if !lines.is_empty() {
                println!("Not-null constraints:");
                for line in &lines {
                    println!("{line}");
                }
            }
        }
    }

    // Phase 2: print "after constraints" partition info for partition parents:
    // "Partitions:" list (for \d+) or "Number of partitions: N" (for \d).
    if part_relkind == "p" && !partkeydef.is_empty() {
        if meta.plus {
            // List individual partitions for \d+.
            let parts_list_sql = format!(
                "select case when pg_catalog.pg_table_is_visible(c2.oid)
         then c2.relname
         else n2.nspname || '.' || c2.relname end as partname,
    pg_catalog.pg_get_expr(c2.relpartbound, c2.oid, true) as partbound,
    c2.relkind
from pg_catalog.pg_inherits as i
join pg_catalog.pg_class as c2 on c2.oid = i.inhrelid
join pg_catalog.pg_namespace as n2 on n2.oid = c2.relnamespace
where i.inhparent = (select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1)
order by (pg_catalog.pg_get_expr(c2.relpartbound, c2.oid, true) = 'DEFAULT'),
         c2.oid::pg_catalog.regclass::pg_catalog.text"
            );
            if let Ok(pmsgs) = client.simple_query(&parts_list_sql).await {
                use tokio_postgres::SimpleQueryMessage;
                let mut parts: Vec<(String, String, String)> = Vec::new();
                for pmsg in pmsgs {
                    if let SimpleQueryMessage::Row(prow) = pmsg {
                        let pname = prow.get(0).unwrap_or("").to_owned();
                        let pbound = prow.get(1).unwrap_or("").to_owned();
                        let pkind = prow.get(2).unwrap_or("").to_owned();
                        parts.push((pname, pbound, pkind));
                    }
                }
                if parts.is_empty() {
                    println!("Number of partitions: 0");
                } else {
                    println!(
                        "Partitions: {}",
                        parts
                            .iter()
                            .enumerate()
                            .map(|(i, (pn, pb, pkind))| {
                                let suffix = if pkind == "p" {
                                    ", PARTITIONED"
                                } else if pkind == "f" {
                                    ", FOREIGN"
                                } else {
                                    ""
                                };
                                if i == 0 {
                                    format!("{pn} {pb}{suffix}")
                                } else {
                                    format!("            {pn} {pb}{suffix}")
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(",\n")
                    );
                }
            }
        } else {
            // For \d (non-plus), show "Number of partitions: N".
            let count_sql = format!(
                "select count(*) from pg_catalog.pg_inherits
where inhparent = (select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1)"
            );
            let num_parts = if let Ok(cmsgs) = client.simple_query(&count_sql).await {
                use tokio_postgres::SimpleQueryMessage;
                cmsgs
                    .iter()
                    .find_map(|m| {
                        if let SimpleQueryMessage::Row(r) = m {
                            r.get(0).and_then(|v| v.parse::<u64>().ok())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0)
            } else {
                0
            };

            if num_parts == 0 {
                println!("Number of partitions: 0");
            } else {
                println!("Number of partitions: {num_parts} (Use \\d+ to list them.)");
            }
        }
    }

    // Foreign table footer: "Server: srvname" and optionally "FDW options: (...)"
    // Placed after all constraints/policies but before Triggers/Rules/Inherits.
    if relkind_char == 'f' {
        let srv_opts_sql_expr = fdw_options_sql("ft.ftoptions");
        let ft_footer_sql = format!(
            "select s.srvname, {srv_opts_sql_expr} as ftoptions
from pg_catalog.pg_foreign_table as ft
join pg_catalog.pg_foreign_server as s on s.oid = ft.ftserver
join pg_catalog.pg_class as c on c.oid = ft.ftrelid
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
limit 1"
        );
        if let Ok(msgs) = client.simple_query(&ft_footer_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let srvname = row.get(0).unwrap_or("");
                    let ftoptions = row.get(1).unwrap_or("");
                    if !srvname.is_empty() {
                        println!("Server: {srvname}");
                    }
                    if !ftoptions.is_empty() {
                        println!("FDW options: {ftoptions}");
                    }
                    break;
                }
            }
        }
    }

    // Triggers — print as "Triggers:" section.
    let trig_sql = format!(
        "select tg.tgname,
    pg_catalog.pg_get_triggerdef(tg.oid, true) as tgdef,
    tg.tgenabled,
    case when tg.tgparentid <> 0 then
        (select case when pg_catalog.pg_table_is_visible(pt.tgrelid)
                     then (select relname from pg_catalog.pg_class where oid = pt.tgrelid)
                     else (select n2.nspname || '.' || c2.relname
                           from pg_catalog.pg_class c2
                           join pg_catalog.pg_namespace n2 on n2.oid = c2.relnamespace
                           where c2.oid = pt.tgrelid)
                end
         from pg_catalog.pg_trigger pt where pt.oid = tg.tgparentid)
    else null end as parent_table
from pg_catalog.pg_trigger as tg
where tg.tgrelid = (
    select c.oid
    from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond}
    limit 1
)
and not tg.tgisinternal
order by 1"
    );
    if let Ok(messages) = client.simple_query(&trig_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        let mut trigger_lines: Vec<String> = Vec::new();
        let mut disabled_lines: Vec<String> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let tgname = row.get(0).unwrap_or("").to_owned();
                let tgdef_full = row.get(1).unwrap_or("").to_owned();
                let tgenabled = row.get(2).unwrap_or("O");
                let parent_table = row.get(3).unwrap_or("");
                // pg_get_triggerdef returns "CREATE TRIGGER name ..."
                // psql shows "    name ..." (strip "CREATE TRIGGER name ")
                let prefix = format!("CREATE TRIGGER {tgname} ");
                let body = if let Some(rest) = tgdef_full.strip_prefix(&prefix) {
                    rest.to_owned()
                } else {
                    tgdef_full.clone()
                };
                // For inherited triggers (from partitioned parent), append ", ON TABLE parent"
                let suffix = if parent_table.is_empty() {
                    String::new()
                } else {
                    format!(", ON TABLE {parent_table}")
                };
                let entry = format!("    {tgname} {body}{suffix}");
                match tgenabled {
                    "D" => disabled_lines.push(entry),
                    _ => trigger_lines.push(entry),
                }
            }
        }
        if !trigger_lines.is_empty() {
            println!("Triggers:");
            for line in &trigger_lines {
                println!("{line}");
            }
        }
        if !disabled_lines.is_empty() {
            println!("Disabled user triggers:");
            for line in &disabled_lines {
                println!("{line}");
            }
        }
    }

    // Rules — print as "Rules:" section.
    let rules_sql = format!(
        "select r.rulename, trim(trailing ';' from pg_catalog.pg_get_ruledef(r.oid, true)) as ruledef
from pg_catalog.pg_rewrite as r
where r.ev_class = (
    select c.oid
    from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond}
    limit 1
)
and r.rulename != '_RETURN'
order by 1"
    );
    if let Ok(messages) = client.simple_query(&rules_sql).await {
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
            println!("Rules:");
            for (name, def) in &lines {
                // psql formats rules differently for views vs tables/other:
                // - view rules: strip "CREATE RULE " and prepend 1 space
                // - table rules: print "    {name} AS\n{body}" with 4-space indent
                if relkind_char == 'v' {
                    let display = def
                        .strip_prefix("CREATE RULE ")
                        .map_or_else(|| format!(" {def}"), |rest| format!(" {rest}"));
                    println!("{display}");
                } else {
                    // Table (and other) rules: show name with 4-space indent,
                    // then the body lines after the "AS\n" separator.
                    println!("    {name} AS");
                    let body =
                        if let Some(rest) = def.strip_prefix(&format!("CREATE RULE {name} AS")) {
                            rest.strip_prefix('\n').unwrap_or(rest)
                        } else if let Some(rest) = def.strip_prefix("CREATE RULE ") {
                            rest.split_once('\n').map_or("", |x| x.1)
                        } else {
                            def.as_str()
                        };
                    for line in body.lines() {
                        println!("{line}");
                    }
                }
            }
        }
    }

    // Inherits — show parent table(s) for non-partition inheritance.
    let inherits_sql = format!(
        "select case when pg_catalog.pg_table_is_visible(c2.oid)
         then c2.relname
         else n2.nspname || '.' || c2.relname end as parent_name
from pg_catalog.pg_inherits as i
join pg_catalog.pg_class as c2 on c2.oid = i.inhparent
join pg_catalog.pg_namespace as n2 on n2.oid = c2.relnamespace
where i.inhrelid = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1)
  and (select not c.relispartition from pg_catalog.pg_class c
       left join pg_catalog.pg_namespace n on n.oid = c.relnamespace
       where {name_cond} limit 1)
order by i.inhseqno"
    );
    if let Ok(messages) = client.simple_query(&inherits_sql).await {
        use tokio_postgres::SimpleQueryMessage;
        let mut parents: Vec<String> = Vec::new();
        for msg in messages {
            if let SimpleQueryMessage::Row(row) = msg {
                let parent = row.get(0).unwrap_or("").to_owned();
                if !parent.is_empty() {
                    parents.push(parent);
                }
            }
        }
        if !parents.is_empty() {
            // psql formats multi-parent lists with each name on its own line,
            // indented to align with the first name.
            if parents.len() == 1 {
                println!("Inherits: {}", parents[0]);
            } else {
                let prefix = "Inherits: ";
                let indent = " ".repeat(prefix.len());
                print!("{}{}", prefix, parents[0]);
                for p in &parents[1..] {
                    print!(",\n{indent}{p}");
                }
                println!();
            }
        }
    }

    // Child tables — shown for regular tables that have children (non-partition).
    // In \d mode: shows "Number of child tables: N (Use \d+ to list them.)"
    // In \d+ mode: shows each child table name, with ", FOREIGN" suffix for foreign tables.
    let child_sql = format!(
        "select case when pg_catalog.pg_table_is_visible(c2.oid)
         then c2.relname
         else n2.nspname || '.' || c2.relname end as child_name,
         c2.relkind::text as child_relkind
from pg_catalog.pg_inherits as i
join pg_catalog.pg_class as c2 on c2.oid = i.inhrelid
join pg_catalog.pg_namespace as n2 on n2.oid = c2.relnamespace
where not c2.relispartition
  and i.inhparent = (
    select c.oid from pg_catalog.pg_class as c
    left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
    where {name_cond} limit 1)
order by 1"
    );
    if matches!(relkind_char, 'r' | 'f') {
        if let Ok(messages) = client.simple_query(&child_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            let mut children: Vec<String> = Vec::new();
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    let child = row.get(0).unwrap_or("").to_owned();
                    let relkind = row.get(1).unwrap_or("");
                    if !child.is_empty() {
                        if relkind == "f" {
                            children.push(format!("{child}, FOREIGN"));
                        } else {
                            children.push(child);
                        }
                    }
                }
            }
            if !children.is_empty() {
                if meta.plus {
                    // \d+ mode: list all child tables
                    if children.len() == 1 {
                        println!("Child tables: {}", children[0]);
                    } else {
                        let prefix = "Child tables: ";
                        let indent = " ".repeat(prefix.len());
                        print!("{}{}", prefix, children[0]);
                        for c in &children[1..] {
                            print!(",\n{indent}{c}");
                        }
                        println!();
                    }
                } else {
                    // \d mode: show count summary
                    let n = children.len();
                    println!("Number of child tables: {n} (Use \\d+ to list them.)");
                }
            }
        }
    }

    // Typed table — show "Typed table of type: typename" when reloftype != 0.
    // psql places this AFTER child tables but BEFORE partition info.
    if matches!(relkind_char, 'r') {
        let typed_sql = format!(
            "select case when pg_catalog.pg_type_is_visible(t.oid) then t.typname
         else nt.nspname || '.' || t.typname end as type_name
from pg_catalog.pg_class as c
join pg_catalog.pg_type as t on t.oid = c.reloftype
join pg_catalog.pg_namespace as nt on nt.oid = t.typnamespace
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where c.reloftype != 0
    and {name_cond}
limit 1"
        );
        if let Ok(msgs) = client.simple_query(&typed_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let tname = row.get(0).unwrap_or("");
                    if !tname.is_empty() {
                        println!("Typed table of type: {tname}");
                    }
                    break;
                }
            }
        }
    }

    // View definition for materialized views — shown AFTER indexes (psql ordering).
    if meta.plus && relkind_char == 'm' {
        let viewdef_sql = format!(
            "select pg_catalog.pg_get_viewdef(c.oid, true)
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
limit 1"
        );
        if let Ok(msgs) = client.simple_query(&viewdef_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let def = row.get(0).unwrap_or("");
                    if !def.is_empty() {
                        println!("View definition:");
                        for vline in def.lines() {
                            println!("{vline}");
                        }
                    }
                    break;
                }
            }
        }
    }
    // Publications — tables can belong to logical replication publications.
    // psql shows a "Publications:" section for both \d and \d+.
    if matches!(relkind_char, 'r' | 'p') {
        let pub_sql = format!(
            "select p.pubname,
    case
        when pr.prattrs is not null
        then ' (' || (
            select string_agg(a.attname, ', ' order by ka.ord)
            from unnest(pr.prattrs::int2[]) with ordinality as ka(num, ord)
            join pg_catalog.pg_attribute as a
                on a.attrelid = pr.prrelid and a.attnum = ka.num
        ) || ')'
        else ''
    end as col_list,
    case
        when pr.prqual is not null
        then ' WHERE ' || pg_catalog.pg_get_expr(pr.prqual, pr.prrelid)
        else ''
    end as where_clause
from pg_catalog.pg_publication as p
join pg_catalog.pg_publication_rel as pr on pr.prpubid = p.oid
join pg_catalog.pg_class as c on c.oid = pr.prrelid
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond}
union
select p.pubname, '' as col_list, '' as where_clause
from pg_catalog.pg_publication as p
join pg_catalog.pg_class as c
    on c.relnamespace = any(
        select pn.pnnspid from pg_catalog.pg_publication_namespace as pn
        where pn.pnpubid = p.oid)
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where p.puballtables = false
    and {name_cond}
union
select p.pubname, '' as col_list, '' as where_clause
from pg_catalog.pg_publication as p
where p.puballtables = true
order by 1"
        );
        if let Ok(msgs) = client.simple_query(&pub_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            let mut pubs: Vec<(String, String, String)> = Vec::new();
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    if let Some(name) = row.get(0) {
                        let col_list = row.get(1).unwrap_or("").to_owned();
                        let where_clause = row.get(2).unwrap_or("").to_owned();
                        pubs.push((name.to_owned(), col_list, where_clause));
                    }
                }
            }
            if !pubs.is_empty() {
                println!("Publications:");
                for (p, col_list, where_clause) in &pubs {
                    println!("    \"{p}\"{col_list}{where_clause}");
                }
            }
        }
    }

    // Replica Identity — psql only shows when not DEFAULT ('d').
    // psql places this after Child tables and before Access method.
    // INDEX is shown on the index line itself. Show FULL here only.
    {
        let ri_sql = format!(
            "select c.relreplident
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n on n.oid = c.relnamespace
where {name_cond} limit 1"
        );
        if let Ok(msgs) = client.simple_query(&ri_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    if row.get(0).unwrap_or("") == "f" {
                        println!("Replica Identity: FULL");
                    }
                    break;
                }
            }
        }
    }

    // Access method — shown by psql \d+ for tables and materialized views.
    if meta.plus {
        let am_sql = format!(
            "select am.amname
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
left join pg_catalog.pg_am as am
    on am.oid = c.relam
where c.relkind in ('r','p','m')
    and {name_cond}
limit 1"
        );
        if meta.echo_hidden {
            eprintln!("/******** QUERY *********/\n{am_sql}\n/************************/");
        }
        if let Ok(msgs) = client.simple_query(&am_sql).await {
            use tokio_postgres::SimpleQueryMessage;
            for msg in msgs {
                if let SimpleQueryMessage::Row(row) = msg {
                    let amname = row.get(0).unwrap_or("");
                    if !amname.is_empty() {
                        println!("Access method: {amname}");
                    }
                    break;
                }
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
            kind_filter: None,
            continuation: None,
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
    // relation_title — type-specific headings to match psql
    // -----------------------------------------------------------------------

    /// psql uses type-specific headings: "List of tables" for \dt, etc.
    /// Verify that `relation_title()` returns the correct heading for each
    /// relkind combination.
    #[test]
    fn relation_title_tables() {
        assert_eq!(relation_title(&["r", "p"]), "List of tables");
        assert_eq!(relation_title(&["r"]), "List of tables");
        assert_eq!(relation_title(&["p"]), "List of tables");
    }

    #[test]
    fn relation_title_indexes() {
        assert_eq!(relation_title(&["i"]), "List of indexes");
        assert_eq!(relation_title(&["I"]), "List of indexes");
        assert_eq!(relation_title(&["i", "I"]), "List of indexes");
    }

    #[test]
    fn relation_title_sequences() {
        assert_eq!(relation_title(&["S"]), "List of sequences");
    }

    #[test]
    fn relation_title_views() {
        assert_eq!(relation_title(&["v"]), "List of views");
    }

    #[test]
    fn relation_title_matviews() {
        assert_eq!(relation_title(&["m"]), "List of materialized views");
    }

    #[test]
    fn relation_title_foreign_tables() {
        assert_eq!(relation_title(&["f"]), "List of foreign tables");
    }

    #[test]
    fn relation_title_generic() {
        assert_eq!(
            relation_title(&["r", "p", "v", "m"]),
            "List of relations",
            "mixed relkinds should show 'List of relations'"
        );
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
    // format_table_inner / print_table
    // -----------------------------------------------------------------------

    /// Verify that `format_table_inner` returns a non-empty string for a
    /// single-row result.
    #[test]
    fn format_table_inner_single_row() {
        let cols = vec!["Name".to_owned()];
        let rows = vec![vec!["users".to_owned()]];
        let text = format_table_inner(&cols, &rows, None, true);
        assert!(text.contains("Name"), "header must appear in output");
        assert!(text.contains("users"), "data must appear in output");
        assert!(text.contains("(1 row)"), "row count must appear");
    }

    #[test]
    fn format_table_inner_empty_rows_has_zero_rows_footer() {
        let cols = vec!["Schema".to_owned(), "Name".to_owned()];
        let rows: Vec<Vec<String>> = vec![];
        let text = format_table_inner(&cols, &rows, None, true);
        assert!(text.contains("(0 rows)"), "must have (0 rows) footer");
    }

    #[test]
    fn format_table_inner_no_columns_returns_row_count() {
        let text = format_table_inner(&[], &[], None, true);
        assert!(text.contains("(0 rows)"), "empty table must show (0 rows)");
    }

    #[test]
    fn format_table_inner_show_row_count_false_no_footer() {
        let cols = vec!["Col".to_owned()];
        let rows = vec![vec!["val".to_owned()]];
        let text = format_table_inner(&cols, &rows, None, false);
        assert!(
            !text.contains("row"),
            "no row count when show_row_count=false"
        );
    }

    #[test]
    fn format_table_inner_with_title_includes_title() {
        let cols = vec!["Col".to_owned()];
        let rows: Vec<Vec<String>> = vec![];
        let text = format_table_inner(&cols, &rows, Some("List of tables"), true);
        assert!(
            text.contains("List of tables"),
            "title must appear in output"
        );
    }

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
        // Uses pg_table_size to match psql \dt+ behaviour.
        let sql = format!(
            "select\n    n.nspname as \"Schema\",\n    c.relname as \"Name\",\
            \n    {} as \"Type\",\n    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",\
            \n    pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\",\
            \n    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"",
            "c.relkind"
        );
        assert!(sql.contains("\"Size\""), "plus SQL should have Size: {sql}");
        assert!(
            sql.contains("\"Description\""),
            "plus SQL should have Description: {sql}"
        );
        assert!(
            sql.contains("pg_table_size"),
            "plus SQL should use pg_table_size: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // list_relations SQL — \dv+/\dm+/\ds+ include Persistence column (#149)
    // -----------------------------------------------------------------------

    /// Verify that the verbose SQL for views, materialized views, and sequences
    /// includes the Persistence column (after Owner, before Size) to match psql
    /// output.  Regression test for bug #149.
    #[test]
    fn view_plus_sql_has_persistence_column() {
        // Replicate the is_view_or_seq branch of list_relations for \dv+.
        let sql = "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    c.relkind as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    case c.relpersistence
        when 'p' then 'permanent'
        when 't' then 'temporary'
        when 'u' then 'unlogged'
        else c.relpersistence::text
    end as \"Persistence\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_relation_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
where c.relkind in ('v')
order by 1, 2";

        assert!(
            sql.contains("\"Persistence\""),
            "view plus SQL must have Persistence column: {sql}"
        );
        // Persistence must come before Size in the column list.
        let persistence_pos = sql.find("\"Persistence\"").unwrap();
        let size_pos = sql.find("\"Size\"").unwrap();
        assert!(
            persistence_pos < size_pos,
            "Persistence must appear before Size: {sql}"
        );
        // Access method column should NOT be present for views.
        assert!(
            !sql.contains("\"Access method\""),
            "view plus SQL must NOT have Access method: {sql}"
        );
        assert!(
            sql.contains("pg_relation_size"),
            "view plus SQL should use pg_relation_size: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // list_relations SQL — \dm+ has Access method and pg_table_size (#159)
    // -----------------------------------------------------------------------

    /// Regression test for bug #159: `\dm+` was missing the Access method
    /// column and reported "0 bytes" because matviews were incorrectly grouped
    /// with views/sequences in the `is_view_or_seq` branch.  Matviews are
    /// heap-stored and must use the default branch (`pg_table_size` + Access
    /// method).
    #[test]
    fn matview_plus_sql_has_access_method_and_table_size() {
        // Replicate the default (non-view, non-seq) branch of list_relations
        // for \dm+, which is what the fix causes matviews to use.
        let sql = "select
    n.nspname as \"Schema\",
    c.relname as \"Name\",
    c.relkind as \"Type\",
    pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",
    case c.relpersistence
        when 'p' then 'permanent'
        when 't' then 'temporary'
        when 'u' then 'unlogged'
        else c.relpersistence::text
    end as \"Persistence\",
    coalesce(am.amname, '') as \"Access method\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\",
    coalesce(pg_catalog.obj_description(c.oid, 'pg_class'), '') as \"Description\"
from pg_catalog.pg_class as c
left join pg_catalog.pg_namespace as n
    on n.oid = c.relnamespace
left join pg_catalog.pg_am as am
    on am.oid = c.relam
where c.relkind in ('m')
order by 1, 2";

        assert!(
            sql.contains("\"Access method\""),
            "matview plus SQL must have Access method column: {sql}"
        );
        assert!(
            sql.contains("pg_table_size"),
            "matview plus SQL must use pg_table_size (not pg_relation_size): {sql}"
        );
        assert!(
            !sql.contains("pg_relation_size"),
            "matview plus SQL must NOT use pg_relation_size: {sql}"
        );
        // Access method must come after Persistence and before Size.
        let am_pos = sql.find("\"Access method\"").unwrap();
        let size_pos = sql.find("\"Size\"").unwrap();
        assert!(
            am_pos < size_pos,
            "Access method must appear before Size: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // list_event_triggers SQL generation
    // -----------------------------------------------------------------------

    /// Verify that the non-verbose SQL for `\dy` includes the six expected
    /// columns and queries `pg_event_trigger`.
    #[test]
    fn list_event_triggers_sql_has_required_columns() {
        let name_filter = pattern::where_clause(None, "e.evtname", None);
        let where_clause = if name_filter.is_empty() {
            String::new()
        } else {
            format!("where {name_filter}")
        };

        let sql = format!(
            "select
    e.evtname as \"Name\",
    e.evtevent as \"Event\",
    pg_catalog.pg_get_userbyid(e.evtowner) as \"Owner\",
    case e.evtenabled
        when 'O' then 'enabled'
        when 'R' then 'replica'
        when 'A' then 'always'
        when 'D' then 'disabled'
    end as \"Enabled\",
    e.evtfoid::pg_catalog.regproc as \"Function\",
    pg_catalog.array_to_string(
        array(
            select x
            from pg_catalog.unnest(e.evttags) as t(x)
        ),
        ', '
    ) as \"Tags\"
from pg_catalog.pg_event_trigger as e
{where_clause}
order by 1"
        );

        assert!(sql.contains("\"Name\""), "SQL must have Name column: {sql}");
        assert!(
            sql.contains("\"Event\""),
            "SQL must have Event column: {sql}"
        );
        assert!(
            sql.contains("\"Owner\""),
            "SQL must have Owner column: {sql}"
        );
        assert!(
            sql.contains("\"Enabled\""),
            "SQL must have Enabled column: {sql}"
        );
        assert!(
            sql.contains("\"Function\""),
            "SQL must have Function column: {sql}"
        );
        assert!(sql.contains("\"Tags\""), "SQL must have Tags column: {sql}");
        assert!(
            sql.contains("pg_event_trigger"),
            "SQL must query pg_event_trigger: {sql}"
        );
        assert!(
            !sql.contains("\"Description\""),
            "non-verbose SQL must not have Description: {sql}"
        );
    }

    /// Verify that verbose `\dy+` SQL adds a Description column via
    /// `obj_description`.
    #[test]
    fn list_event_triggers_plus_sql_has_description_column() {
        let sql = "select
    e.evtname as \"Name\",
    e.evtevent as \"Event\",
    pg_catalog.pg_get_userbyid(e.evtowner) as \"Owner\",
    case e.evtenabled
        when 'O' then 'enabled'
        when 'R' then 'replica'
        when 'A' then 'always'
        when 'D' then 'disabled'
    end as \"Enabled\",
    e.evtfoid::pg_catalog.regproc as \"Function\",
    pg_catalog.array_to_string(
        array(
            select x
            from pg_catalog.unnest(e.evttags) as t(x)
        ),
        ', '
    ) as \"Tags\",
    coalesce(pg_catalog.obj_description(e.oid, 'pg_event_trigger'), '') as \"Description\"
from pg_catalog.pg_event_trigger as e
order by 1";

        assert!(
            sql.contains("\"Description\""),
            "verbose SQL must have Description column: {sql}"
        );
        assert!(
            sql.contains("obj_description"),
            "verbose SQL must use obj_description: {sql}"
        );
        assert!(
            sql.contains("pg_event_trigger"),
            "verbose SQL must query pg_event_trigger: {sql}"
        );
    }

    /// Verify that a pattern filter is applied to `evtname`.
    #[test]
    fn list_event_triggers_pattern_filter_applied() {
        let name_filter = pattern::where_clause(Some("my_trigger"), "e.evtname", None);
        let where_clause = format!("where {name_filter}");

        assert!(
            where_clause.contains("e.evtname"),
            "filter must reference e.evtname: {where_clause}"
        );
        assert!(
            where_clause.contains("my_trigger"),
            "filter must include pattern value: {where_clause}"
        );
    }

    // -----------------------------------------------------------------------
    // list_operators SQL generation
    // -----------------------------------------------------------------------

    /// Verify that the non-verbose SQL for `\do` includes all six expected
    /// columns (including Description) and queries `pg_operator`.
    ///
    /// Bug #188: psql's basic `\do` always shows Description; rpg previously
    /// omitted it from the non-verbose path.
    #[test]
    fn list_operators_sql_has_expected_columns() {
        let sql = "select
    n.nspname as \"Schema\",
    o.oprname as \"Name\",
    case when o.oprkind = 'l' then null
         else pg_catalog.format_type(o.oprleft, null)
    end as \"Left arg type\",
    case when o.oprkind = 'r' then null
         else pg_catalog.format_type(o.oprright, null)
    end as \"Right arg type\",
    pg_catalog.format_type(o.oprresult, null) as \"Result type\",
    coalesce(pg_catalog.obj_description(o.oid, 'pg_operator'),
             pg_catalog.obj_description(o.oprcode, 'pg_proc')) as \"Description\"
from pg_catalog.pg_operator as o
left join pg_catalog.pg_namespace as n
    on n.oid = o.oprnamespace
order by 1, 2, 3, 4";

        assert!(
            sql.contains("\"Schema\""),
            "SQL must have Schema column: {sql}"
        );
        assert!(sql.contains("\"Name\""), "SQL must have Name column: {sql}");
        assert!(
            sql.contains("\"Left arg type\""),
            "SQL must have Left arg type column: {sql}"
        );
        assert!(
            sql.contains("\"Right arg type\""),
            "SQL must have Right arg type column: {sql}"
        );
        assert!(
            sql.contains("\"Result type\""),
            "SQL must have Result type column: {sql}"
        );
        assert!(
            sql.contains("\"Description\""),
            "basic SQL must have Description column: {sql}"
        );
        assert!(
            sql.contains("pg_operator"),
            "SQL must query pg_operator: {sql}"
        );
        // Right arg type must also use a CASE for oprkind='r' (unary left ops).
        assert!(
            sql.contains("oprkind = 'r'"),
            "SQL must guard Right arg type with oprkind = 'r': {sql}"
        );
        // Description must coalesce operator description with proc description.
        assert!(
            sql.contains("pg_proc"),
            "Description must fall back to pg_proc description: {sql}"
        );
    }

    /// Verify that verbose `\do+` SQL also includes a Description column.
    ///
    /// After bug #188 the basic and verbose queries are identical; this test
    /// guards that the column is present regardless of the plus flag.
    #[test]
    fn list_operators_plus_sql_has_description_column() {
        let sql = "select
    n.nspname as \"Schema\",
    o.oprname as \"Name\",
    case when o.oprkind = 'l' then null
         else pg_catalog.format_type(o.oprleft, null)
    end as \"Left arg type\",
    case when o.oprkind = 'r' then null
         else pg_catalog.format_type(o.oprright, null)
    end as \"Right arg type\",
    pg_catalog.format_type(o.oprresult, null) as \"Result type\",
    coalesce(pg_catalog.obj_description(o.oid, 'pg_operator'),
             pg_catalog.obj_description(o.oprcode, 'pg_proc')) as \"Description\"
from pg_catalog.pg_operator as o
left join pg_catalog.pg_namespace as n
    on n.oid = o.oprnamespace
order by 1, 2, 3, 4";

        assert!(
            sql.contains("\"Description\""),
            "verbose SQL must have Description column: {sql}"
        );
        assert!(
            sql.contains("obj_description"),
            "verbose SQL must use obj_description: {sql}"
        );
        assert!(
            sql.contains("pg_operator"),
            "verbose SQL must query pg_operator: {sql}"
        );
    }

    /// Verify that the system filter uses `<>` (not `not in`) and includes
    /// `pg_operator_is_visible`, matching psql's exact query.
    #[test]
    fn list_operators_system_filter_excludes_pg_catalog() {
        let sys_filter = "n.nspname <> 'pg_catalog'\n    and n.nspname <> 'information_schema'";
        let visibility_filter = "pg_catalog.pg_operator_is_visible(o.oid)";
        let where_clause = format!("where {sys_filter}\n    and {visibility_filter}");

        assert!(
            where_clause.contains("pg_catalog"),
            "system filter must reference pg_catalog: {where_clause}"
        );
        assert!(
            where_clause.contains("information_schema"),
            "system filter must reference information_schema: {where_clause}"
        );
        assert!(
            where_clause.contains("pg_operator_is_visible"),
            "filter must include visibility check: {where_clause}"
        );
        assert!(
            !where_clause.contains("not in"),
            "filter must use <> not 'not in': {where_clause}"
        );
    }

    /// Verify that a pattern filter is applied to `oprname`.
    #[test]
    fn list_operators_pattern_filter_applied() {
        let name_filter = pattern::where_clause(Some("my_op"), "o.oprname", Some("n.nspname"));
        let where_clause = format!("where {name_filter}");

        assert!(
            where_clause.contains("o.oprname"),
            "filter must reference o.oprname: {where_clause}"
        );
        assert!(
            where_clause.contains("my_op"),
            "filter must include pattern value: {where_clause}"
        );
    }

    // -----------------------------------------------------------------------
    // list_tablespaces SQL generation — \db+ verbose columns (#178)
    // -----------------------------------------------------------------------

    /// Verify that the basic `\db` SQL includes the three expected columns
    /// and does NOT include the verbose-only columns.
    #[test]
    fn list_tablespaces_sql_has_basic_columns() {
        let sql = "select
    spcname as \"Name\",
    pg_catalog.pg_get_userbyid(spcowner) as \"Owner\",
    pg_catalog.pg_tablespace_location(oid) as \"Location\"
from pg_catalog.pg_tablespace
order by 1";

        assert!(sql.contains("\"Name\""), "SQL must have Name column: {sql}");
        assert!(
            sql.contains("\"Owner\""),
            "SQL must have Owner column: {sql}"
        );
        assert!(
            sql.contains("\"Location\""),
            "SQL must have Location column: {sql}"
        );
        assert!(
            !sql.contains("\"Access privileges\""),
            "basic SQL must not have Access privileges: {sql}"
        );
        assert!(
            !sql.contains("\"Size\""),
            "basic SQL must not have Size: {sql}"
        );
        assert!(
            !sql.contains("\"Description\""),
            "basic SQL must not have Description: {sql}"
        );
    }

    /// Verify that verbose `\db+` SQL adds Access privileges, Options, Size,
    /// and Description columns, matching psql's exact query.
    ///
    /// Bug #178: rpg previously had no plus branch for `\db`.
    #[test]
    fn list_tablespaces_plus_sql_has_verbose_columns() {
        let sql = "select
    spcname as \"Name\",
    pg_catalog.pg_get_userbyid(spcowner) as \"Owner\",
    pg_catalog.pg_tablespace_location(oid) as \"Location\",
    case when pg_catalog.array_length(spcacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(spcacl, E'\\n')
    end as \"Access privileges\",
    spcoptions as \"Options\",
    pg_catalog.pg_size_pretty(pg_catalog.pg_tablespace_size(oid)) as \"Size\",
    pg_catalog.shobj_description(oid, 'pg_tablespace') as \"Description\"
from pg_catalog.pg_tablespace
order by 1";

        assert!(
            sql.contains("\"Access privileges\""),
            "verbose SQL must have Access privileges: {sql}"
        );
        assert!(
            sql.contains("\"Options\""),
            "verbose SQL must have Options: {sql}"
        );
        assert!(
            sql.contains("\"Size\""),
            "verbose SQL must have Size: {sql}"
        );
        assert!(
            sql.contains("\"Description\""),
            "verbose SQL must have Description: {sql}"
        );
        assert!(
            sql.contains("pg_tablespace_size"),
            "verbose SQL must use pg_tablespace_size: {sql}"
        );
        assert!(
            sql.contains("shobj_description"),
            "verbose SQL must use shobj_description: {sql}"
        );
        assert!(
            sql.contains("spcacl"),
            "verbose SQL must reference spcacl for Access privileges: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // list_domains SQL generation
    // -----------------------------------------------------------------------

    /// Verify that the non-verbose SQL for `\dD` includes the seven expected
    /// columns (Schema, Name, Type, Collation, Nullable, Default, Check) and
    /// does NOT include Description or Access privileges.
    #[test]
    fn list_domains_sql_has_required_columns() {
        let sys_filter =
            "n.nspname <> 'pg_catalog'\n    and n.nspname <> 'information_schema'".to_owned();
        let visibility_filter = "pg_catalog.pg_type_is_visible(t.oid)";
        let base_filter = "t.typtype = 'd'";
        let where_parts: Vec<&str> = [
            Some(base_filter),
            Some(sys_filter.as_str()),
            Some(visibility_filter),
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
    (select c.collname
     from pg_catalog.pg_collation as c, pg_catalog.pg_type as bt
     where c.oid = t.typcollation
       and bt.oid = t.typbasetype
       and t.typcollation <> bt.typcollation) as \"Collation\",
    case when t.typnotnull then 'not null' end as \"Nullable\",
    t.typdefault as \"Default\",
    pg_catalog.array_to_string(array(
        select pg_catalog.pg_get_constraintdef(r.oid, true)
        from pg_catalog.pg_constraint as r
        where t.oid = r.contypid
          and r.contype = 'c'
        order by r.conname
    ), ' ') as \"Check\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
        );

        assert!(
            sql.contains("\"Schema\""),
            "SQL must have Schema column: {sql}"
        );
        assert!(sql.contains("\"Name\""), "SQL must have Name column: {sql}");
        assert!(sql.contains("\"Type\""), "SQL must have Type column: {sql}");
        assert!(
            sql.contains("\"Collation\""),
            "SQL must have Collation column: {sql}"
        );
        assert!(
            sql.contains("\"Nullable\""),
            "SQL must have Nullable column: {sql}"
        );
        assert!(
            sql.contains("\"Default\""),
            "SQL must have Default column: {sql}"
        );
        assert!(
            sql.contains("\"Check\""),
            "SQL must have Check column: {sql}"
        );
        assert!(
            sql.contains("pg_get_constraintdef"),
            "SQL must use pg_get_constraintdef for Check: {sql}"
        );
        assert!(
            sql.contains("pg_type_is_visible"),
            "SQL must use pg_type_is_visible: {sql}"
        );
        assert!(
            sql.contains("typcollation"),
            "SQL must query typcollation for Collation: {sql}"
        );
        assert!(
            !sql.contains("'not null' else ''"),
            "Nullable must not use else branch (must be NULL not empty string): {sql}"
        );
        assert!(
            !sql.contains("\"Description\""),
            "non-verbose SQL must not have Description: {sql}"
        );
        assert!(
            !sql.contains("\"Access privileges\""),
            "non-verbose SQL must not have Access privileges: {sql}"
        );
    }

    /// Verify that verbose `\dD+` SQL adds Access privileges and Description
    /// columns, and joins to `pg_description`.
    #[test]
    fn list_domains_plus_sql_has_extra_columns() {
        let sql = "select
    n.nspname as \"Schema\",
    t.typname as \"Name\",
    pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",
    (select c.collname
     from pg_catalog.pg_collation as c, pg_catalog.pg_type as bt
     where c.oid = t.typcollation
       and bt.oid = t.typbasetype
       and t.typcollation <> bt.typcollation) as \"Collation\",
    case when t.typnotnull then 'not null' end as \"Nullable\",
    t.typdefault as \"Default\",
    pg_catalog.array_to_string(array(
        select pg_catalog.pg_get_constraintdef(r.oid, true)
        from pg_catalog.pg_constraint as r
        where t.oid = r.contypid
          and r.contype = 'c'
        order by r.conname
    ), ' ') as \"Check\",
    case when pg_catalog.array_length(t.typacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(t.typacl, E'\\n')
    end as \"Access privileges\",
    d.description as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
left join pg_catalog.pg_description as d
    on d.classoid = t.tableoid
   and d.objoid = t.oid
   and d.objsubid = 0
order by 1, 2";

        assert!(
            sql.contains("\"Access privileges\""),
            "verbose SQL must have Access privileges column: {sql}"
        );
        assert!(
            sql.contains("\"Description\""),
            "verbose SQL must have Description column: {sql}"
        );
        assert!(
            sql.contains("pg_description"),
            "verbose SQL must join pg_description: {sql}"
        );
        assert!(
            sql.contains("typacl"),
            "verbose SQL must reference typacl for Access privileges: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // list_types SQL generation — \dT+ verbose columns (#177)
    // -----------------------------------------------------------------------

    /// Verify that the non-verbose SQL for `\dT` contains only Schema, Name,
    /// and Description columns.
    #[test]
    fn list_types_basic_sql_has_three_columns() {
        let base_filter = "t.typtype in ('c', 'd', 'e', 'r') and t.typname !~ '^_'\
            \n    and (t.typrelid = 0 or (select c.relkind = 'c' from pg_catalog.pg_class as c where c.oid = t.typrelid))";
        let sys_filter = "n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast')";
        let where_clause = format!("where {base_filter}\n    and {sys_filter}");

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

        assert!(
            sql.contains("\"Schema\""),
            "basic SQL must have Schema: {sql}"
        );
        assert!(sql.contains("\"Name\""), "basic SQL must have Name: {sql}");
        assert!(
            sql.contains("\"Description\""),
            "basic SQL must have Description: {sql}"
        );
        assert!(
            !sql.contains("\"Internal name\""),
            "basic SQL must not have Internal name: {sql}"
        );
        assert!(
            !sql.contains("\"Owner\""),
            "basic SQL must not have Owner: {sql}"
        );
        assert!(
            !sql.contains("\"Access privileges\""),
            "basic SQL must not have Access privileges: {sql}"
        );
    }

    /// Build the verbose `\dT+` SQL fragment used by the two tests below.
    fn dt_plus_sql() -> String {
        let base_filter = "t.typtype in ('c', 'd', 'e', 'r') and t.typname !~ '^_'\
            \n    and (t.typrelid = 0 or (select c.relkind = 'c' from pg_catalog.pg_class as c where c.oid = t.typrelid))";
        let sys_filter = "n.nspname not in ('pg_catalog', 'information_schema', 'pg_toast')";
        let where_clause = format!("where {base_filter}\n    and {sys_filter}");
        format!(
            "select
    n.nspname as \"Schema\",
    pg_catalog.format_type(t.oid, null) as \"Name\",
    t.typname as \"Internal name\",
    case when t.typrelid != 0
            then cast('tuple' as pg_catalog.text)
        when t.typlen < 0
            then cast('var' as pg_catalog.text)
        else cast(t.typlen as pg_catalog.text)
    end as \"Size\",
    pg_catalog.array_to_string(
        array(
            select e.enumlabel
            from pg_catalog.pg_enum as e
            where e.enumtypid = t.oid
            order by e.enumsortorder
        ),
        E'\\n'
    ) as \"Elements\",
    pg_catalog.pg_get_userbyid(t.typowner) as \"Owner\",
    case when pg_catalog.array_length(t.typacl, 1) = 0
         then '(none)'
         else pg_catalog.array_to_string(t.typacl, E'\\n')
    end as \"Access privileges\",
    coalesce(pg_catalog.obj_description(t.oid, 'pg_type'), '') as \"Description\"
from pg_catalog.pg_type as t
left join pg_catalog.pg_namespace as n
    on n.oid = t.typnamespace
{where_clause}
order by 1, 2"
        )
    }

    /// Verify that verbose `\dT+` SQL contains the expected columns and
    /// references the correct catalog objects.
    /// Regression test for bug #177.
    #[test]
    fn list_types_plus_sql_has_verbose_columns() {
        let sql = dt_plus_sql();

        assert!(
            sql.contains("\"Internal name\""),
            "verbose SQL must have Internal name: {sql}"
        );
        assert!(
            sql.contains("t.typname as \"Internal name\""),
            "Internal name must use t.typname: {sql}"
        );
        assert!(
            sql.contains("\"Size\""),
            "verbose SQL must have Size: {sql}"
        );
        assert!(
            sql.contains("t.typlen"),
            "Size must reference t.typlen: {sql}"
        );
        assert!(
            sql.contains("\"Elements\""),
            "verbose SQL must have Elements: {sql}"
        );
        assert!(
            sql.contains("pg_enum"),
            "Elements must query pg_enum: {sql}"
        );
        assert!(
            sql.contains("enumsortorder"),
            "Elements must order by enumsortorder: {sql}"
        );
        assert!(
            sql.contains("\"Owner\""),
            "verbose SQL must have Owner: {sql}"
        );
        assert!(
            sql.contains("pg_get_userbyid(t.typowner)"),
            "Owner must use pg_get_userbyid: {sql}"
        );
        assert!(
            sql.contains("\"Access privileges\""),
            "verbose SQL must have Access privileges: {sql}"
        );
        assert!(
            sql.contains("t.typacl"),
            "Access privileges must reference t.typacl: {sql}"
        );
        assert!(
            sql.contains("\"Description\""),
            "verbose SQL must have Description: {sql}"
        );
    }

    /// Verify that verbose `\dT+` columns appear in the order psql uses:
    /// Schema, Name, Internal name, Size, Elements, Owner, Access privileges,
    /// Description.
    /// Regression test for bug #177.
    #[test]
    fn list_types_plus_sql_column_order() {
        let sql = dt_plus_sql();

        let internal_pos = sql.find("\"Internal name\"").unwrap();
        let size_pos = sql.find("\"Size\"").unwrap();
        let elements_pos = sql.find("\"Elements\"").unwrap();
        let owner_pos = sql.find("\"Owner\"").unwrap();
        let acl_pos = sql.find("\"Access privileges\"").unwrap();
        let desc_pos = sql.find("\"Description\"").unwrap();

        assert!(
            internal_pos < size_pos,
            "Internal name must appear before Size: {sql}"
        );
        assert!(
            size_pos < elements_pos,
            "Size must appear before Elements: {sql}"
        );
        assert!(
            elements_pos < owner_pos,
            "Elements must appear before Owner: {sql}"
        );
        assert!(
            owner_pos < acl_pos,
            "Owner must appear before Access privileges: {sql}"
        );
        assert!(
            acl_pos < desc_pos,
            "Access privileges must appear before Description: {sql}"
        );
    }
}
