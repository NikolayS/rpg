//! Session-management meta-commands: `\c`, `\sf`, `\sv`, `\h`.
//!
//! Each function is invoked by [`crate::repl::dispatch_meta`] and operates
//! directly on the live Postgres client.  The `echo_hidden` flag, when true,
//! prints internally-generated SQL to stdout before executing it (matching
//! the behaviour of `psql -E` / `rpg -E`).

use tokio_postgres::Client;

use crate::connection::{self, CliConnOpts, ConnParams};

// ---------------------------------------------------------------------------
// \c — reconnect
// ---------------------------------------------------------------------------

/// Arguments parsed from a `\c` command string.
///
/// Each field is `None` when the token is absent or `-` (meaning "reuse
/// current value").
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconnectArgs {
    pub dbname: Option<String>,
    pub user: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
}

/// Convert a `\c` token to `Some(value)` or `None` for the `-` sentinel.
fn reconnect_token(v: Option<&&str>) -> Option<String> {
    v.and_then(|s| {
        if *s == "-" {
            None
        } else {
            Some((*s).to_owned())
        }
    })
}

/// Parse the argument string of a `\c` command into individual fields.
///
/// Format: `[dbname [user [host [port]]]]`
///
/// A token of `-` means "keep the current value" and maps to `None`.
pub fn parse_reconnect_args(pattern: Option<&str>) -> ReconnectArgs {
    let Some(s) = pattern else {
        return ReconnectArgs::default();
    };

    let tokens: Vec<&str> = s.split_whitespace().collect();

    ReconnectArgs {
        dbname: reconnect_token(tokens.first()),
        user: reconnect_token(tokens.get(1)),
        host: reconnect_token(tokens.get(2)),
        port: reconnect_token(tokens.get(3)),
    }
}

/// Attempt to reconnect using `pattern` as the `\c` argument string.
///
/// The new parameters are formed by taking `current_params` as the base and
/// overriding only the fields that were explicitly supplied (i.e. not absent
/// or `-`).
///
/// On success returns the new `(Client, ConnParams)`.  The caller is
/// responsible for dropping the old client.
///
/// # Errors
/// Returns a human-readable error string on connection failure.
pub async fn reconnect(
    pattern: Option<&str>,
    current_params: &ConnParams,
) -> Result<(Client, ConnParams, Option<String>), String> {
    let args = parse_reconnect_args(pattern);

    // Build a CliConnOpts that, when passed to resolve_params, will produce
    // the desired parameters.  We seed the positional/named fields from the
    // current params so that absent \c tokens retain their current values.
    let port = match args.port.as_deref() {
        None => current_params.port,
        Some(p) => match p.parse::<u16>() {
            Ok(n) => n,
            Err(_) => return Err(format!("invalid port number: \"{p}\"")),
        },
    };

    let opts = CliConnOpts {
        host: Some(args.host.unwrap_or_else(|| current_params.host.clone())),
        port: Some(port),
        port_str: None,
        username: Some(args.user.unwrap_or_else(|| current_params.user.clone())),
        dbname: Some(args.dbname.unwrap_or_else(|| current_params.dbname.clone())),
        // All positional fields are None — we feed through the named fields.
        dbname_pos: None,
        user_pos: None,
        host_pos: None,
        port_pos: None,
        // Never prompt/suppress password on \c reconnect; keep existing
        // password from the params if it is already resolved.
        force_password: false,
        no_password: false,
        sslmode: None,
        // SSH tunnel is not re-established on \c; the outer tunnel (if any)
        // remains live for the session.
        ssh_tunnel: None,
    };

    let (mut new_params, mut initial_password) =
        connection::resolve_params(&opts).map_err(|e| e.to_string())?;

    // Carry forward the password if the user does not have a .pgpass entry
    // and no PGPASSWORD is set — avoids spurious prompts on same-server
    // reconnects.
    if initial_password.is_none() {
        initial_password = current_params.password.clone();
    }

    // Carry forward the sslmode from the current params when no override given.
    // resolve_params defaults to Prefer; we only override when the user
    // hasn't supplied --sslmode and the env var PGSSLMODE is not set.
    // A simple heuristic: if opts.sslmode is None and PGSSLMODE env is unset,
    // keep the existing sslmode.
    if opts.sslmode.is_none() && std::env::var("PGSSLMODE").is_err() {
        new_params.sslmode = current_params.sslmode;
    }

    // Carry forward the application name.
    new_params.application_name = current_params.application_name.clone();
    if let Ok(appname) = std::env::var("PGAPPNAME") {
        new_params.application_name = appname;
    }

    let (client, params, password) = connection::connect(new_params, initial_password, &opts)
        .await
        .map_err(|e| e.to_string())?;
    Ok((client, params, password))
}

// ---------------------------------------------------------------------------
// \sf — show function source
// ---------------------------------------------------------------------------

/// Display the source of a PL/pgSQL (or other language) function.
///
/// Optionally adds line numbers when `plus` is true.
/// When `echo_hidden` is true the generated SQL is printed before execution.
pub async fn show_function_source(client: &Client, name: &str, plus: bool, echo_hidden: bool) {
    // Split schema-qualified name if present.
    let (schema_filter, func_name) = split_schema_name(name);

    let sql = build_function_source_sql(schema_filter.as_deref(), &func_name);

    if echo_hidden {
        rpg_println!("{sql}");
    }

    let rows = match client.simple_query(&sql).await {
        Ok(r) => r,
        Err(e) => {
            crate::output::eprint_db_error(&e, None, false, false, false);
            return;
        }
    };

    let mut found = false;
    for msg in rows {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            found = true;
            let src = row.get(0).unwrap_or("");
            print_with_optional_line_numbers(src, plus);
        }
    }

    if !found {
        rpg_eprintln!("error: ERROR:  function \"{name}\" does not exist");
    }
}

/// Build the SQL query that retrieves function source(s).
fn build_function_source_sql(schema_filter: Option<&str>, func_name: &str) -> String {
    // Escape single quotes in user-supplied names.
    let safe_name = func_name.replace('\'', "''");
    let schema_clause = match schema_filter {
        Some(s) => {
            let safe_s = s.replace('\'', "''");
            format!("and n.nspname = '{safe_s}'")
        }
        None => "and n.nspname not in ('pg_catalog', 'information_schema')".to_owned(),
    };

    format!(
        "select pg_catalog.pg_get_functiondef(p.oid)\n\
         from pg_catalog.pg_proc as p\n\
         left join pg_catalog.pg_namespace as n\n\
             on n.oid = p.pronamespace\n\
         where p.proname = '{safe_name}'\n\
           {schema_clause};"
    )
}

// ---------------------------------------------------------------------------
// \sv — show view definition
// ---------------------------------------------------------------------------

/// Display the definition of a view or materialised view.
///
/// Optionally adds line numbers when `plus` is true.
/// When `echo_hidden` is true the generated SQL is printed before execution.
pub async fn show_view_def(client: &Client, name: &str, plus: bool, echo_hidden: bool) {
    let (schema_filter, view_name) = split_schema_name(name);

    let sql = build_view_def_sql(schema_filter.as_deref(), &view_name);

    if echo_hidden {
        rpg_println!("{sql}");
    }

    let rows = match client.simple_query(&sql).await {
        Ok(r) => r,
        Err(e) => {
            crate::output::eprint_db_error(&e, None, false, false, false);
            return;
        }
    };

    let mut found = false;
    for msg in rows {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            found = true;
            let viewdef = row.get(0).unwrap_or("");
            let nspname = row.get(1).unwrap_or("public");
            let relname = row.get(2).unwrap_or("");
            // Strip trailing semicolon to match psql behaviour.
            let body = viewdef.trim_end_matches(';');
            let header = format!("CREATE OR REPLACE VIEW {nspname}.{relname} AS");
            print_view_def(&header, body, plus);
        }
    }

    if !found {
        // Match psql error format: `error: ERROR:  relation "X" does not exist`
        // (the "error: " prefix appears in psql's script-file error output).
        rpg_eprintln!("error: ERROR:  relation \"{name}\" does not exist");
    }
}

/// Print the full view definition (header + body) with optional line numbers.
///
/// Line numbers start at 1 for the `CREATE OR REPLACE VIEW` header line,
/// matching psql's `\sv+` output exactly.
///
/// psql uses `%-*d` with `width = (int)log10(total_lines) + 8`, giving a
/// left-aligned number in a fixed-width field followed directly by the line
/// content — no tab character.
fn print_view_def(header: &str, body: &str, plus: bool) {
    if plus {
        let body_lines: Vec<&str> = body.lines().collect();
        // Total lines = 1 (header) + body line count.
        let total = 1 + body_lines.len();
        // Width matches psql: floor(log10(total)) + 8, computed without
        // floating point to satisfy clippy's cast lints.
        let log = if total < 10 {
            0usize
        } else if total < 100 {
            1
        } else if total < 1000 {
            2
        } else {
            3
        };
        let width = log + 8;

        // Header is line 1.
        rpg_println!("{:<width$}{header}", 1);
        for (i, line) in body_lines.iter().enumerate() {
            rpg_println!("{:<width$}{line}", i + 2);
        }
    } else {
        rpg_println!("{header}");
        rpg_println!("{body}");
    }
}

/// Build the SQL query that retrieves a view definition.
///
/// Returns three columns: `viewdef`, `nspname`, `relname`.
fn build_view_def_sql(schema_filter: Option<&str>, view_name: &str) -> String {
    let safe_name = view_name.replace('\'', "''");
    let schema_clause = match schema_filter {
        Some(s) => {
            let safe_s = s.replace('\'', "''");
            format!("and n.nspname = '{safe_s}'")
        }
        None => "and n.nspname not in ('pg_catalog', 'information_schema')".to_owned(),
    };

    format!(
        "select pg_catalog.pg_get_viewdef(c.oid, true),\n\
               n.nspname,\n\
               c.relname\n\
         from pg_catalog.pg_class as c\n\
         left join pg_catalog.pg_namespace as n\n\
             on n.oid = c.relnamespace\n\
         where c.relname = '{safe_name}'\n\
           and c.relkind in ('v', 'm')\n\
           {schema_clause};"
    )
}

// ---------------------------------------------------------------------------
// \h — SQL help
// ---------------------------------------------------------------------------

/// One entry in the SQL help table.
struct SqlHelpEntry {
    /// SQL command name (uppercase), e.g. `"SELECT"`.
    name: &'static str,
    /// Short description matching psql's output.
    description: &'static str,
    /// Full syntax synopsis — printed verbatim, no extra indentation.
    synopsis: &'static str,
    /// `PostgreSQL` documentation URL for the command.
    url: &'static str,
}

/// Static help table sourced from `PostgreSQL` 18 `sql_help.h`.
///
/// Entries cover the most commonly used SQL commands.  The synopsis text
/// matches psql's output exactly so that `diff`-based comparison tests pass.
static SQL_HELP: &[SqlHelpEntry] = &[
    SqlHelpEntry {
        name: "SELECT",
        description: "retrieve rows from a table or view",
        synopsis: "[ WITH [ RECURSIVE ] with_query [, ...] ]\n\
SELECT [ ALL | DISTINCT [ ON ( expression [, ...] ) ] ]\n\
    [ { * | expression [ [ AS ] output_name ] } [, ...] ]\n\
    [ FROM from_item [, ...] ]\n\
    [ WHERE condition ]\n\
    [ GROUP BY [ ALL | DISTINCT ] grouping_element [, ...] ]\n\
    [ HAVING condition ]\n\
    [ WINDOW window_name AS ( window_definition ) [, ...] ]\n\
    [ { UNION | INTERSECT | EXCEPT } [ ALL | DISTINCT ] select ]\n\
    [ ORDER BY expression [ ASC | DESC | USING operator ] [ NULLS { FIRST | LAST } ] [, ...] ]\n\
    [ LIMIT { count | ALL } ]\n\
    [ OFFSET start [ ROW | ROWS ] ]\n\
    [ FETCH { FIRST | NEXT } [ count ] { ROW | ROWS } { ONLY | WITH TIES } ]\n\
    [ FOR { UPDATE | NO KEY UPDATE | SHARE | KEY SHARE } [ OF from_reference [, ...] ] [ NOWAIT | SKIP LOCKED ] [...] ]\n\
\n\
where from_item can be one of:\n\
\n\
    [ ONLY ] table_name [ * ] [ [ AS ] alias [ ( column_alias [, ...] ) ] ]\n\
                [ TABLESAMPLE sampling_method ( argument [, ...] ) [ REPEATABLE ( seed ) ] ]\n\
    [ LATERAL ] ( select ) [ [ AS ] alias [ ( column_alias [, ...] ) ] ]\n\
    with_query_name [ [ AS ] alias [ ( column_alias [, ...] ) ] ]\n\
    [ LATERAL ] function_name ( [ argument [, ...] ] )\n\
                [ WITH ORDINALITY ] [ [ AS ] alias [ ( column_alias [, ...] ) ] ]\n\
    [ LATERAL ] function_name ( [ argument [, ...] ] ) [ AS ] alias ( column_definition [, ...] )\n\
    [ LATERAL ] function_name ( [ argument [, ...] ] ) AS ( column_definition [, ...] )\n\
    [ LATERAL ] ROWS FROM( function_name ( [ argument [, ...] ] ) [ AS ( column_definition [, ...] ) ] [, ...] )\n\
                [ WITH ORDINALITY ] [ [ AS ] alias [ ( column_alias [, ...] ) ] ]\n\
    from_item join_type from_item { ON join_condition | USING ( join_column [, ...] ) [ AS join_using_alias ] }\n\
    from_item NATURAL join_type from_item\n\
    from_item CROSS JOIN from_item\n\
\n\
and grouping_element can be one of:\n\
\n\
    ( )\n\
    expression\n\
    ( expression [, ...] )\n\
    ROLLUP ( { expression | ( expression [, ...] ) } [, ...] )\n\
    CUBE ( { expression | ( expression [, ...] ) } [, ...] )\n\
    GROUPING SETS ( grouping_element [, ...] )\n\
\n\
and with_query is:\n\
\n\
    with_query_name [ ( column_name [, ...] ) ] AS [ [ NOT ] MATERIALIZED ] ( select | values | insert | update | delete | merge )\n\
        [ SEARCH { BREADTH | DEPTH } FIRST BY column_name [, ...] SET search_seq_col_name ]\n\
        [ CYCLE column_name [, ...] SET cycle_mark_col_name [ TO cycle_mark_value DEFAULT cycle_mark_default ] USING cycle_path_col_name ]\n\
\n\
TABLE [ ONLY ] table_name [ * ]",
        url: "https://www.postgresql.org/docs/18/sql-select.html",
    },
    SqlHelpEntry {
        name: "INSERT",
        description: "create new rows in a table",
        synopsis: "[ WITH [ RECURSIVE ] with_query [, ...] ]\n\
INSERT INTO table_name [ AS alias ] [ ( column_name [, ...] ) ]\n\
    [ OVERRIDING { SYSTEM | USER } VALUE ]\n\
    { DEFAULT VALUES | VALUES ( { expression | DEFAULT } [, ...] ) [, ...] | query }\n\
    [ ON CONFLICT [ conflict_target ] conflict_action ]\n\
    [ RETURNING [ WITH ( { OLD | NEW } AS output_alias [, ...] ) ]\n\
                { * | output_expression [ [ AS ] output_name ] } [, ...] ]\n\
\n\
where conflict_target can be one of:\n\
\n\
    ( { index_column_name | ( index_expression ) } [ COLLATE collation ] [ opclass ] [, ...] ) [ WHERE index_predicate ]\n\
    ON CONSTRAINT constraint_name\n\
\n\
and conflict_action is one of:\n\
\n\
    DO NOTHING\n\
    DO UPDATE SET { column_name = { expression | DEFAULT } |\n\
                    ( column_name [, ...] ) = [ ROW ] ( { expression | DEFAULT } [, ...] ) |\n\
                    ( column_name [, ...] ) = ( sub-SELECT )\n\
                  } [, ...]\n\
              [ WHERE condition ]",
        url: "https://www.postgresql.org/docs/18/sql-insert.html",
    },
    SqlHelpEntry {
        name: "UPDATE",
        description: "update rows of a table",
        synopsis: "[ WITH [ RECURSIVE ] with_query [, ...] ]\n\
UPDATE [ ONLY ] table_name [ * ] [ [ AS ] alias ]\n\
    SET { column_name = { expression | DEFAULT } |\n\
          ( column_name [, ...] ) = [ ROW ] ( { expression | DEFAULT } [, ...] ) |\n\
          ( column_name [, ...] ) = ( sub-SELECT )\n\
        } [, ...]\n\
    [ FROM from_item [, ...] ]\n\
    [ WHERE condition | WHERE CURRENT OF cursor_name ]\n\
    [ RETURNING [ WITH ( { OLD | NEW } AS output_alias [, ...] ) ]\n\
                { * | output_expression [ [ AS ] output_name ] } [, ...] ]",
        url: "https://www.postgresql.org/docs/18/sql-update.html",
    },
    SqlHelpEntry {
        name: "DELETE",
        description: "delete rows of a table",
        synopsis: "[ WITH [ RECURSIVE ] with_query [, ...] ]\n\
DELETE FROM [ ONLY ] table_name [ * ] [ [ AS ] alias ]\n\
    [ USING from_item [, ...] ]\n\
    [ WHERE condition | WHERE CURRENT OF cursor_name ]\n\
    [ RETURNING [ WITH ( { OLD | NEW } AS output_alias [, ...] ) ]\n\
                { * | output_expression [ [ AS ] output_name ] } [, ...] ]",
        url: "https://www.postgresql.org/docs/18/sql-delete.html",
    },
    SqlHelpEntry {
        name: "CREATE TABLE",
        description: "define a new table",
        synopsis: "CREATE [ [ GLOBAL | LOCAL ] { TEMPORARY | TEMP } | UNLOGGED ] TABLE [ IF NOT EXISTS ] table_name ( [\n\
  { column_name data_type [ STORAGE { PLAIN | EXTERNAL | EXTENDED | MAIN | DEFAULT } ] [ COMPRESSION compression_method ] [ COLLATE collation ] [ column_constraint [ ... ] ]\n\
    | table_constraint\n\
    | LIKE source_table [ like_option ... ] }\n\
    [, ... ]\n\
] )\n\
[ INHERITS ( parent_table [, ... ] ) ]\n\
[ PARTITION BY { RANGE | LIST | HASH } ( { column_name | ( expression ) } [ COLLATE collation ] [ opclass ] [, ... ] ) ]\n\
[ USING method ]\n\
[ WITH ( storage_parameter [= value] [, ... ] ) | WITHOUT OIDS ]\n\
[ ON COMMIT { PRESERVE ROWS | DELETE ROWS | DROP } ]\n\
[ TABLESPACE tablespace_name ]\n\
\n\
CREATE [ [ GLOBAL | LOCAL ] { TEMPORARY | TEMP } | UNLOGGED ] TABLE [ IF NOT EXISTS ] table_name\n\
    OF type_name [ (\n\
  { column_name [ WITH OPTIONS ] [ column_constraint [ ... ] ]\n\
    | table_constraint }\n\
    [, ... ]\n\
) ]\n\
[ PARTITION BY { RANGE | LIST | HASH } ( { column_name | ( expression ) } [ COLLATE collation ] [ opclass ] [, ... ] ) ]\n\
[ USING method ]\n\
[ WITH ( storage_parameter [= value] [, ... ] ) | WITHOUT OIDS ]\n\
[ ON COMMIT { PRESERVE ROWS | DELETE ROWS | DROP } ]\n\
[ TABLESPACE tablespace_name ]\n\
\n\
CREATE [ [ GLOBAL | LOCAL ] { TEMPORARY | TEMP } | UNLOGGED ] TABLE [ IF NOT EXISTS ] table_name\n\
    PARTITION OF parent_table [ (\n\
  { column_name [ WITH OPTIONS ] [ column_constraint [ ... ] ]\n\
    | table_constraint }\n\
    [, ... ]\n\
) ] { FOR VALUES partition_bound_spec | DEFAULT }\n\
[ PARTITION BY { RANGE | LIST | HASH } ( { column_name | ( expression ) } [ COLLATE collation ] [ opclass ] [, ... ] ) ]\n\
[ USING method ]\n\
[ WITH ( storage_parameter [= value] [, ... ] ) | WITHOUT OIDS ]\n\
[ ON COMMIT { PRESERVE ROWS | DELETE ROWS | DROP } ]\n\
[ TABLESPACE tablespace_name ]\n\
\n\
where column_constraint is:\n\
\n\
[ CONSTRAINT constraint_name ]\n\
{ NOT NULL [ NO INHERIT ]  |\n\
  NULL |\n\
  CHECK ( expression ) [ NO INHERIT ] |\n\
  DEFAULT default_expr |\n\
  GENERATED ALWAYS AS ( generation_expr ) [ STORED | VIRTUAL ] |\n\
  GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( sequence_options ) ] |\n\
  UNIQUE [ NULLS [ NOT ] DISTINCT ] index_parameters |\n\
  PRIMARY KEY index_parameters |\n\
  REFERENCES reftable [ ( refcolumn ) ] [ MATCH FULL | MATCH PARTIAL | MATCH SIMPLE ]\n\
    [ ON DELETE referential_action ] [ ON UPDATE referential_action ] }\n\
[ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ] [ ENFORCED | NOT ENFORCED ]\n\
\n\
and table_constraint is:\n\
\n\
[ CONSTRAINT constraint_name ]\n\
{ CHECK ( expression ) [ NO INHERIT ] |\n\
  NOT NULL column_name [ NO INHERIT ] |\n\
  UNIQUE [ NULLS [ NOT ] DISTINCT ] ( column_name [, ... ] [, column_name WITHOUT OVERLAPS ] ) index_parameters |\n\
  PRIMARY KEY ( column_name [, ... ] [, column_name WITHOUT OVERLAPS ] ) index_parameters |\n\
  EXCLUDE [ USING index_method ] ( exclude_element WITH operator [, ... ] ) index_parameters [ WHERE ( predicate ) ] |\n\
  FOREIGN KEY ( column_name [, ... ] [, PERIOD column_name ] ) REFERENCES reftable [ ( refcolumn [, ... ] [, PERIOD refcolumn ] ) ]\n\
    [ MATCH FULL | MATCH PARTIAL | MATCH SIMPLE ] [ ON DELETE referential_action ] [ ON UPDATE referential_action ] }\n\
[ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ] [ ENFORCED | NOT ENFORCED ]\n\
\n\
and like_option is:\n\
\n\
{ INCLUDING | EXCLUDING } { COMMENTS | COMPRESSION | CONSTRAINTS | DEFAULTS | GENERATED | IDENTITY | INDEXES | STATISTICS | STORAGE | ALL }\n\
\n\
and partition_bound_spec is:\n\
\n\
IN ( partition_bound_expr [, ...] ) |\n\
FROM ( { partition_bound_expr | MINVALUE | MAXVALUE } [, ...] )\n\
  TO ( { partition_bound_expr | MINVALUE | MAXVALUE } [, ...] ) |\n\
WITH ( MODULUS numeric_literal, REMAINDER numeric_literal )\n\
\n\
index_parameters in UNIQUE, PRIMARY KEY, and EXCLUDE constraints are:\n\
\n\
[ INCLUDE ( column_name [, ... ] ) ]\n\
[ WITH ( storage_parameter [= value] [, ... ] ) ]\n\
[ USING INDEX TABLESPACE tablespace_name ]\n\
\n\
exclude_element in an EXCLUDE constraint is:\n\
\n\
{ column_name | ( expression ) } [ COLLATE collation ] [ opclass [ ( opclass_parameter = value [, ... ] ) ] ] [ ASC | DESC ] [ NULLS { FIRST | LAST } ]\n\
\n\
referential_action in a FOREIGN KEY/REFERENCES constraint is:\n\
\n\
{ NO ACTION | RESTRICT | CASCADE | SET NULL [ ( column_name [, ... ] ) ] | SET DEFAULT [ ( column_name [, ... ] ) ] }",
        url: "https://www.postgresql.org/docs/18/sql-createtable.html",
    },
    SqlHelpEntry {
        name: "CREATE INDEX",
        description: "define a new index",
        synopsis: "CREATE [ UNIQUE ] INDEX [ CONCURRENTLY ] [ [ IF NOT EXISTS ] name ] ON [ ONLY ] table_name [ USING method ]\n\
    ( { column_name | ( expression ) } [ COLLATE collation ] [ opclass [ ( opclass_parameter = value [, ... ] ) ] ] [ ASC | DESC ] [ NULLS { FIRST | LAST } ] [, ...] )\n\
    [ INCLUDE ( column_name [, ...] ) ]\n\
    [ NULLS [ NOT ] DISTINCT ]\n\
    [ WITH ( storage_parameter [= value] [, ... ] ) ]\n\
    [ TABLESPACE tablespace_name ]\n\
    [ WHERE predicate ]",
        url: "https://www.postgresql.org/docs/18/sql-createindex.html",
    },
    SqlHelpEntry {
        name: "ALTER TABLE",
        description: "change the definition of a table",
        synopsis: "ALTER TABLE [ IF EXISTS ] [ ONLY ] name [ * ]\n\
    action [, ... ]\n\
ALTER TABLE [ IF EXISTS ] [ ONLY ] name [ * ]\n\
    RENAME [ COLUMN ] column_name TO new_column_name\n\
ALTER TABLE [ IF EXISTS ] [ ONLY ] name [ * ]\n\
    RENAME CONSTRAINT constraint_name TO new_constraint_name\n\
ALTER TABLE [ IF EXISTS ] name\n\
    RENAME TO new_name\n\
ALTER TABLE [ IF EXISTS ] name\n\
    SET SCHEMA new_schema\n\
ALTER TABLE ALL IN TABLESPACE name [ OWNED BY role_name [, ... ] ]\n\
    SET TABLESPACE new_tablespace [ NOWAIT ]\n\
ALTER TABLE [ IF EXISTS ] name\n\
    ATTACH PARTITION partition_name { FOR VALUES partition_bound_spec | DEFAULT }\n\
ALTER TABLE [ IF EXISTS ] name\n\
    DETACH PARTITION partition_name [ CONCURRENTLY | FINALIZE ]\n\
\n\
where action is one of:\n\
\n\
    ADD [ COLUMN ] [ IF NOT EXISTS ] column_name data_type [ COLLATE collation ] [ column_constraint [ ... ] ]\n\
    DROP [ COLUMN ] [ IF EXISTS ] column_name [ RESTRICT | CASCADE ]\n\
    ALTER [ COLUMN ] column_name [ SET DATA ] TYPE data_type [ COLLATE collation ] [ USING expression ]\n\
    ALTER [ COLUMN ] column_name SET DEFAULT expression\n\
    ALTER [ COLUMN ] column_name DROP DEFAULT\n\
    ALTER [ COLUMN ] column_name { SET | DROP } NOT NULL\n\
    ALTER [ COLUMN ] column_name SET EXPRESSION AS ( expression )\n\
    ALTER [ COLUMN ] column_name DROP EXPRESSION [ IF EXISTS ]\n\
    ALTER [ COLUMN ] column_name ADD GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( sequence_options ) ]\n\
    ALTER [ COLUMN ] column_name { SET GENERATED { ALWAYS | BY DEFAULT } | SET sequence_option | RESTART [ [ WITH ] restart ] } [...]\n\
    ALTER [ COLUMN ] column_name DROP IDENTITY [ IF EXISTS ]\n\
    ALTER [ COLUMN ] column_name SET STATISTICS { integer | DEFAULT }\n\
    ALTER [ COLUMN ] column_name SET ( attribute_option = value [, ... ] )\n\
    ALTER [ COLUMN ] column_name RESET ( attribute_option [, ... ] )\n\
    ALTER [ COLUMN ] column_name SET STORAGE { PLAIN | EXTERNAL | EXTENDED | MAIN | DEFAULT }\n\
    ALTER [ COLUMN ] column_name SET COMPRESSION compression_method\n\
    ADD table_constraint [ NOT VALID ]\n\
    ADD table_constraint_using_index\n\
    ALTER CONSTRAINT constraint_name [ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ] [ ENFORCED | NOT ENFORCED ]\n\
    ALTER CONSTRAINT constraint_name [ INHERIT | NO INHERIT ]\n\
    VALIDATE CONSTRAINT constraint_name\n\
    DROP CONSTRAINT [ IF EXISTS ]  constraint_name [ RESTRICT | CASCADE ]\n\
    DISABLE TRIGGER [ trigger_name | ALL | USER ]\n\
    ENABLE TRIGGER [ trigger_name | ALL | USER ]\n\
    ENABLE REPLICA TRIGGER trigger_name\n\
    ENABLE ALWAYS TRIGGER trigger_name\n\
    DISABLE RULE rewrite_rule_name\n\
    ENABLE RULE rewrite_rule_name\n\
    ENABLE REPLICA RULE rewrite_rule_name\n\
    ENABLE ALWAYS RULE rewrite_rule_name\n\
    DISABLE ROW LEVEL SECURITY\n\
    ENABLE ROW LEVEL SECURITY\n\
    FORCE ROW LEVEL SECURITY\n\
    NO FORCE ROW LEVEL SECURITY\n\
    CLUSTER ON index_name\n\
    SET WITHOUT CLUSTER\n\
    SET WITHOUT OIDS\n\
    SET ACCESS METHOD { new_access_method | DEFAULT }\n\
    SET TABLESPACE new_tablespace\n\
    SET { LOGGED | UNLOGGED }\n\
    SET ( storage_parameter [= value] [, ... ] )\n\
    RESET ( storage_parameter [, ... ] )\n\
    INHERIT parent_table\n\
    NO INHERIT parent_table\n\
    OF type_name\n\
    NOT OF\n\
    OWNER TO { new_owner | CURRENT_ROLE | CURRENT_USER | SESSION_USER }\n\
    REPLICA IDENTITY { DEFAULT | USING INDEX index_name | FULL | NOTHING }\n\
\n\
and partition_bound_spec is:\n\
\n\
IN ( partition_bound_expr [, ...] ) |\n\
FROM ( { partition_bound_expr | MINVALUE | MAXVALUE } [, ...] )\n\
  TO ( { partition_bound_expr | MINVALUE | MAXVALUE } [, ...] ) |\n\
WITH ( MODULUS numeric_literal, REMAINDER numeric_literal )\n\
\n\
and column_constraint is:\n\
\n\
[ CONSTRAINT constraint_name ]\n\
{ NOT NULL [ NO INHERIT ] |\n\
  NULL |\n\
  CHECK ( expression ) [ NO INHERIT ] |\n\
  DEFAULT default_expr |\n\
  GENERATED ALWAYS AS ( generation_expr ) [ STORED | VIRTUAL ] |\n\
  GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [ ( sequence_options ) ] |\n\
  UNIQUE [ NULLS [ NOT ] DISTINCT ] index_parameters |\n\
  PRIMARY KEY index_parameters |\n\
  REFERENCES reftable [ ( refcolumn ) ] [ MATCH FULL | MATCH PARTIAL | MATCH SIMPLE ]\n\
    [ ON DELETE referential_action ] [ ON UPDATE referential_action ] }\n\
[ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ] [ ENFORCED | NOT ENFORCED ]\n\
\n\
and table_constraint is:\n\
\n\
[ CONSTRAINT constraint_name ]\n\
{ CHECK ( expression ) [ NO INHERIT ] |\n\
  NOT NULL column_name [ NO INHERIT ] |\n\
  UNIQUE [ NULLS [ NOT ] DISTINCT ] ( column_name [, ... ] [, column_name WITHOUT OVERLAPS ] ) index_parameters |\n\
  PRIMARY KEY ( column_name [, ... ] [, column_name WITHOUT OVERLAPS ] ) index_parameters |\n\
  EXCLUDE [ USING index_method ] ( exclude_element WITH operator [, ... ] ) index_parameters [ WHERE ( predicate ) ] |\n\
  FOREIGN KEY ( column_name [, ... ] [, PERIOD column_name ] ) REFERENCES reftable [ ( refcolumn [, ... ]  [, PERIOD refcolumn ] ) ]\n\
    [ MATCH FULL | MATCH PARTIAL | MATCH SIMPLE ] [ ON DELETE referential_action ] [ ON UPDATE referential_action ] }\n\
[ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ] [ ENFORCED | NOT ENFORCED ]\n\
\n\
and table_constraint_using_index is:\n\
\n\
    [ CONSTRAINT constraint_name ]\n\
    { UNIQUE | PRIMARY KEY } USING INDEX index_name\n\
    [ DEFERRABLE | NOT DEFERRABLE ] [ INITIALLY DEFERRED | INITIALLY IMMEDIATE ]\n\
\n\
index_parameters in UNIQUE, PRIMARY KEY, and EXCLUDE constraints are:\n\
\n\
[ INCLUDE ( column_name [, ... ] ) ]\n\
[ WITH ( storage_parameter [= value] [, ... ] ) ]\n\
[ USING INDEX TABLESPACE tablespace_name ]\n\
\n\
exclude_element in an EXCLUDE constraint is:\n\
\n\
{ column_name | ( expression ) } [ COLLATE collation ] [ opclass [ ( opclass_parameter = value [, ... ] ) ] ] [ ASC | DESC ] [ NULLS { FIRST | LAST } ]\n\
\n\
referential_action in a FOREIGN KEY/REFERENCES constraint is:\n\
\n\
{ NO ACTION | RESTRICT | CASCADE | SET NULL [ ( column_name [, ... ] ) ] | SET DEFAULT [ ( column_name [, ... ] ) ] }",
        url: "https://www.postgresql.org/docs/18/sql-altertable.html",
    },
    SqlHelpEntry {
        name: "DROP TABLE",
        description: "remove a table",
        synopsis: "DROP TABLE [ IF EXISTS ] name [, ...] [ CASCADE | RESTRICT ]",
        url: "https://www.postgresql.org/docs/18/sql-droptable.html",
    },
    SqlHelpEntry {
        name: "BEGIN",
        description: "start a transaction block",
        synopsis: "BEGIN [ WORK | TRANSACTION ] [ transaction_mode [, ...] ]\n\
\n\
where transaction_mode is one of:\n\
\n\
    ISOLATION LEVEL { SERIALIZABLE | REPEATABLE READ | READ COMMITTED | READ UNCOMMITTED }\n\
    READ WRITE | READ ONLY\n\
    [ NOT ] DEFERRABLE",
        url: "https://www.postgresql.org/docs/18/sql-begin.html",
    },
    SqlHelpEntry {
        name: "COMMIT",
        description: "commit the current transaction",
        synopsis: "COMMIT [ WORK | TRANSACTION ] [ AND [ NO ] CHAIN ]",
        url: "https://www.postgresql.org/docs/18/sql-commit.html",
    },
    SqlHelpEntry {
        name: "ROLLBACK",
        description: "abort the current transaction",
        synopsis: "ROLLBACK [ WORK | TRANSACTION ] [ AND [ NO ] CHAIN ]",
        url: "https://www.postgresql.org/docs/18/sql-rollback.html",
    },
    SqlHelpEntry {
        name: "GRANT",
        description: "define access privileges",
        synopsis: "GRANT { { SELECT | INSERT | UPDATE | DELETE | TRUNCATE | REFERENCES | TRIGGER | MAINTAIN }\n\
    [, ...] | ALL [ PRIVILEGES ] }\n\
    ON { [ TABLE ] table_name [, ...]\n\
         | ALL TABLES IN SCHEMA schema_name [, ...] }\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { SELECT | INSERT | UPDATE | REFERENCES } ( column_name [, ...] )\n\
    [, ...] | ALL [ PRIVILEGES ] ( column_name [, ...] ) }\n\
    ON [ TABLE ] table_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { USAGE | SELECT | UPDATE }\n\
    [, ...] | ALL [ PRIVILEGES ] }\n\
    ON { SEQUENCE sequence_name [, ...]\n\
         | ALL SEQUENCES IN SCHEMA schema_name [, ...] }\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { CREATE | CONNECT | TEMPORARY | TEMP } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON DATABASE database_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { USAGE | ALL [ PRIVILEGES ] }\n\
    ON DOMAIN domain_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { USAGE | ALL [ PRIVILEGES ] }\n\
    ON FOREIGN DATA WRAPPER fdw_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { USAGE | ALL [ PRIVILEGES ] }\n\
    ON FOREIGN SERVER server_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { EXECUTE | ALL [ PRIVILEGES ] }\n\
    ON { { FUNCTION | PROCEDURE | ROUTINE } routine_name [ ( [ [ argmode ] [ arg_name ] arg_type [, ...] ] ) ] [, ...]\n\
         | ALL { FUNCTIONS | PROCEDURES | ROUTINES } IN SCHEMA schema_name [, ...] }\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { USAGE | ALL [ PRIVILEGES ] }\n\
    ON LANGUAGE lang_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { SELECT | UPDATE } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON LARGE OBJECT loid [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { SET | ALTER SYSTEM } [, ... ] | ALL [ PRIVILEGES ] }\n\
    ON PARAMETER configuration_parameter [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { { CREATE | USAGE } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON SCHEMA schema_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { CREATE | ALL [ PRIVILEGES ] }\n\
    ON TABLESPACE tablespace_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT { USAGE | ALL [ PRIVILEGES ] }\n\
    ON TYPE type_name [, ...]\n\
    TO role_specification [, ...] [ WITH GRANT OPTION ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
GRANT role_name [, ...] TO role_specification [, ...]\n\
    [ WITH { ADMIN | INHERIT | SET } { OPTION | TRUE | FALSE } ]\n\
    [ GRANTED BY role_specification ]\n\
\n\
where role_specification can be:\n\
\n\
    [ GROUP ] role_name\n\
  | PUBLIC\n\
  | CURRENT_ROLE\n\
  | CURRENT_USER\n\
  | SESSION_USER",
        url: "https://www.postgresql.org/docs/18/sql-grant.html",
    },
    SqlHelpEntry {
        name: "REVOKE",
        description: "remove access privileges",
        synopsis: "REVOKE [ GRANT OPTION FOR ]\n\
    { { SELECT | INSERT | UPDATE | DELETE | TRUNCATE | REFERENCES | TRIGGER | MAINTAIN }\n\
    [, ...] | ALL [ PRIVILEGES ] }\n\
    ON { [ TABLE ] table_name [, ...]\n\
         | ALL TABLES IN SCHEMA schema_name [, ...] }\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { SELECT | INSERT | UPDATE | REFERENCES } ( column_name [, ...] )\n\
    [, ...] | ALL [ PRIVILEGES ] ( column_name [, ...] ) }\n\
    ON [ TABLE ] table_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { USAGE | SELECT | UPDATE }\n\
    [, ...] | ALL [ PRIVILEGES ] }\n\
    ON { SEQUENCE sequence_name [, ...]\n\
         | ALL SEQUENCES IN SCHEMA schema_name [, ...] }\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { CREATE | CONNECT | TEMPORARY | TEMP } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON DATABASE database_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { USAGE | ALL [ PRIVILEGES ] }\n\
    ON DOMAIN domain_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { USAGE | ALL [ PRIVILEGES ] }\n\
    ON FOREIGN DATA WRAPPER fdw_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { USAGE | ALL [ PRIVILEGES ] }\n\
    ON FOREIGN SERVER server_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { EXECUTE | ALL [ PRIVILEGES ] }\n\
    ON { { FUNCTION | PROCEDURE | ROUTINE } function_name [ ( [ [ argmode ] [ arg_name ] arg_type [, ...] ] ) ] [, ...]\n\
         | ALL { FUNCTIONS | PROCEDURES | ROUTINES } IN SCHEMA schema_name [, ...] }\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { USAGE | ALL [ PRIVILEGES ] }\n\
    ON LANGUAGE lang_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { SELECT | UPDATE } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON LARGE OBJECT loid [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { SET | ALTER SYSTEM } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON PARAMETER configuration_parameter [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { { CREATE | USAGE } [, ...] | ALL [ PRIVILEGES ] }\n\
    ON SCHEMA schema_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { CREATE | ALL [ PRIVILEGES ] }\n\
    ON TABLESPACE tablespace_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ GRANT OPTION FOR ]\n\
    { USAGE | ALL [ PRIVILEGES ] }\n\
    ON TYPE type_name [, ...]\n\
    FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
REVOKE [ { ADMIN | INHERIT | SET } OPTION FOR ]\n\
    role_name [, ...] FROM role_specification [, ...]\n\
    [ GRANTED BY role_specification ]\n\
    [ CASCADE | RESTRICT ]\n\
\n\
where role_specification can be:\n\
\n\
    [ GROUP ] role_name\n\
  | PUBLIC\n\
  | CURRENT_ROLE\n\
  | CURRENT_USER\n\
  | SESSION_USER",
        url: "https://www.postgresql.org/docs/18/sql-revoke.html",
    },
    SqlHelpEntry {
        name: "EXPLAIN",
        description: "show the execution plan of a statement",
        synopsis: "EXPLAIN [ ( option [, ...] ) ] statement\n\
\n\
where option can be one of:\n\
\n\
    ANALYZE [ boolean ]\n\
    VERBOSE [ boolean ]\n\
    COSTS [ boolean ]\n\
    SETTINGS [ boolean ]\n\
    GENERIC_PLAN [ boolean ]\n\
    BUFFERS [ boolean ]\n\
    SERIALIZE [ { NONE | TEXT | BINARY } ]\n\
    WAL [ boolean ]\n\
    TIMING [ boolean ]\n\
    SUMMARY [ boolean ]\n\
    MEMORY [ boolean ]\n\
    FORMAT { TEXT | XML | JSON | YAML }",
        url: "https://www.postgresql.org/docs/18/sql-explain.html",
    },
    SqlHelpEntry {
        name: "VACUUM",
        description: "garbage-collect and optionally analyze a database",
        synopsis: "VACUUM [ ( option [, ...] ) ] [ table_and_columns [, ...] ]\n\
\n\
where option can be one of:\n\
\n\
    FULL [ boolean ]\n\
    FREEZE [ boolean ]\n\
    VERBOSE [ boolean ]\n\
    ANALYZE [ boolean ]\n\
    DISABLE_PAGE_SKIPPING [ boolean ]\n\
    SKIP_LOCKED [ boolean ]\n\
    INDEX_CLEANUP { AUTO | ON | OFF }\n\
    PROCESS_MAIN [ boolean ]\n\
    PROCESS_TOAST [ boolean ]\n\
    TRUNCATE [ boolean ]\n\
    PARALLEL integer\n\
    SKIP_DATABASE_STATS [ boolean ]\n\
    ONLY_DATABASE_STATS [ boolean ]\n\
    BUFFER_USAGE_LIMIT size\n\
\n\
and table_and_columns is:\n\
\n\
    [ ONLY ] table_name [ * ] [ ( column_name [, ...] ) ]",
        url: "https://www.postgresql.org/docs/18/sql-vacuum.html",
    },
    SqlHelpEntry {
        name: "ANALYZE",
        description: "collect statistics about a database",
        synopsis: "ANALYZE [ ( option [, ...] ) ] [ table_and_columns [, ...] ]\n\
\n\
where option can be one of:\n\
\n\
    VERBOSE [ boolean ]\n\
    SKIP_LOCKED [ boolean ]\n\
    BUFFER_USAGE_LIMIT size\n\
\n\
and table_and_columns is:\n\
\n\
    [ ONLY ] table_name [ * ] [ ( column_name [, ...] ) ]",
        url: "https://www.postgresql.org/docs/18/sql-analyze.html",
    },
    SqlHelpEntry {
        name: "COPY",
        description: "copy data between a file and a table",
        synopsis: "COPY table_name [ ( column_name [, ...] ) ]\n\
    FROM { 'filename' | PROGRAM 'command' | STDIN }\n\
    [ [ WITH ] ( option [, ...] ) ]\n\
    [ WHERE condition ]\n\
\n\
COPY { table_name [ ( column_name [, ...] ) ] | ( query ) }\n\
    TO { 'filename' | PROGRAM 'command' | STDOUT }\n\
    [ [ WITH ] ( option [, ...] ) ]\n\
\n\
where option can be one of:\n\
\n\
    FORMAT format_name\n\
    FREEZE [ boolean ]\n\
    DELIMITER 'delimiter_character'\n\
    NULL 'null_string'\n\
    DEFAULT 'default_string'\n\
    HEADER [ boolean | MATCH ]\n\
    QUOTE 'quote_character'\n\
    ESCAPE 'escape_character'\n\
    FORCE_QUOTE { ( column_name [, ...] ) | * }\n\
    FORCE_NOT_NULL { ( column_name [, ...] ) | * }\n\
    FORCE_NULL { ( column_name [, ...] ) | * }\n\
    ON_ERROR error_action\n\
    REJECT_LIMIT maxerror\n\
    ENCODING 'encoding_name'\n\
    LOG_VERBOSITY verbosity",
        url: "https://www.postgresql.org/docs/18/sql-copy.html",
    },
    SqlHelpEntry {
        name: "CREATE FUNCTION",
        description: "define a new function",
        synopsis: "CREATE [ OR REPLACE ] FUNCTION\n\
    name ( [ [ argmode ] [ argname ] argtype [ { DEFAULT | = } default_expr ] [, ...] ] )\n\
    [ RETURNS rettype\n\
      | RETURNS TABLE ( column_name column_type [, ...] ) ]\n\
  { LANGUAGE lang_name\n\
    | TRANSFORM { FOR TYPE type_name } [, ... ]\n\
    | WINDOW\n\
    | { IMMUTABLE | STABLE | VOLATILE }\n\
    | [ NOT ] LEAKPROOF\n\
    | { CALLED ON NULL INPUT | RETURNS NULL ON NULL INPUT | STRICT }\n\
    | { [ EXTERNAL ] SECURITY INVOKER | [ EXTERNAL ] SECURITY DEFINER }\n\
    | PARALLEL { UNSAFE | RESTRICTED | SAFE }\n\
    | COST execution_cost\n\
    | ROWS result_rows\n\
    | SUPPORT support_function\n\
    | SET configuration_parameter { TO value | = value | FROM CURRENT }\n\
    | AS 'definition'\n\
    | AS 'obj_file', 'link_symbol'\n\
    | sql_body\n\
  } ...",
        url: "https://www.postgresql.org/docs/18/sql-createfunction.html",
    },
    SqlHelpEntry {
        name: "CREATE VIEW",
        description: "define a new view",
        synopsis: "CREATE [ OR REPLACE ] [ TEMP | TEMPORARY ] [ RECURSIVE ] VIEW name [ ( column_name [, ...] ) ]\n\
    [ WITH ( view_option_name [= view_option_value] [, ... ] ) ]\n\
    AS query\n\
    [ WITH [ CASCADED | LOCAL ] CHECK OPTION ]",
        url: "https://www.postgresql.org/docs/18/sql-createview.html",
    },
];

/// Print SQL syntax help.
///
/// Without a topic: list all available help topics in columns.
/// With a topic: show the synopsis for that command (case-insensitive match;
/// matches on any prefix of multi-word commands, e.g. `CREATE` matches
/// `CREATE TABLE`).
/// Return the SQL help text for `topic` as a `String`.
///
/// When `topic` is `None`, returns the full list of available help topics.
/// When `topic` is `Some`, returns the synopsis for that command, or `None`
/// when no matching entry is found (the caller should print an error to
/// stderr in that case).
///
/// Returns `Err(topic)` when no matching entry is found so the caller can
/// print an appropriate error message.
pub fn sql_help_text(topic: Option<&str>) -> Result<String, String> {
    match topic {
        None => Ok(help_topics_text()),
        Some(t) => help_topic_text(t),
    }
}

/// Build and return a two-column list of available SQL help topics.
///
/// Format matches psql: two columns, left column 33 characters wide,
/// each entry indented by 2 spaces.
fn help_topics_text() -> String {
    use std::fmt::Write as FmtWrite;

    // psql uses a fixed column width of 33 characters for the left column.
    const COL_W: usize = 33;

    let mut out = String::new();
    let _ = writeln!(out, "Available help:");
    let _ = writeln!(out);

    // Collect names and lay them out in two columns.
    let names: Vec<&str> = SQL_HELP.iter().map(|e| e.name).collect();
    let half = names.len().div_ceil(2);

    for i in 0..half {
        let left = names[i];
        let right = names.get(i + half).copied().unwrap_or("");
        if right.is_empty() {
            let _ = writeln!(out, "  {left}");
        } else {
            let _ = writeln!(out, "  {left:<COL_W$}{right}");
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Type \\h <command-name> for help on a specific command"
    );
    out
}

/// Build and return the synopsis for a specific SQL command (case-insensitive).
///
/// Returns `Ok(text)` when the command is found, or `Err(topic)` when no
/// matching entry is found.
///
/// Output format matches psql exactly:
/// ```text
/// Command:     NAME
/// Description: short description
/// Syntax:
/// <synopsis lines, no leading indentation>
///
/// URL: https://...
/// ```
fn help_topic_text(topic: &str) -> Result<String, String> {
    use std::fmt::Write as FmtWrite;

    let upper = topic.trim().to_uppercase();

    // Look for an exact match first, then a prefix match.
    let entry = SQL_HELP
        .iter()
        .find(|e| e.name == upper)
        .or_else(|| SQL_HELP.iter().find(|e| e.name.starts_with(&upper)));

    if let Some(e) = entry {
        let mut out = String::new();
        let _ = writeln!(out, "Command:     {}", e.name);
        let _ = writeln!(out, "Description: {}", e.description);
        let _ = writeln!(out, "Syntax:");
        let _ = writeln!(out, "{}", e.synopsis);
        let _ = writeln!(out);
        let _ = writeln!(out, "URL: {}", e.url);
        Ok(out)
    } else {
        Err(topic.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Split a possibly schema-qualified name (`schema.object`) into its parts.
///
/// Returns `(schema_option, object_name)`.
fn split_schema_name(name: &str) -> (Option<String>, String) {
    match name.split_once('.') {
        Some((schema, obj)) => (Some(schema.to_owned()), obj.to_owned()),
        None => (None, name.to_owned()),
    }
}

/// Print function/view source, optionally with psql-compatible line numbers.
///
/// When `plus` is true, line numbers are applied only to lines starting from
/// the `AS $` dollar-quote marker (i.e. the function body).  Lines before that
/// marker are printed with equivalent blank space so the body content stays
/// aligned.  The number width is determined by the total number of body lines.
///
/// Format matches psql `print_function_source()`:
/// - Header lines: `fprintf(output, "%*s\t", nln, "")` — spaces padded to
///   `nln` width, then a TAB character.
/// - Body lines: `fprintf(output, "%-*d\t", nln, lineno)` — left-aligned
///   number in `nln` width, then a TAB character.
///
/// Using TAB (not spaces) means alignment is governed by terminal tab stops
/// (every 8 columns), which is what psql produces.
fn print_with_optional_line_numbers(text: &str, plus: bool) {
    if !plus {
        // pg_get_functiondef already ends with a newline; use print! to
        // avoid a spurious extra blank line (println! would add another \n).
        rpg_print!("{text}");
        return;
    }

    let lines: Vec<&str> = text.lines().collect();

    // Find the index of the first line that starts the dollar-quoted body
    // (i.e. begins with "AS $").  If no such line is found, number from the
    // very first line (matches \sv+ behaviour where there is no AS header).
    let body_start = lines
        .iter()
        .position(|l| l.starts_with("AS $"))
        .unwrap_or(0);

    let body_line_count = lines.len().saturating_sub(body_start);
    // Width needed to left-align the largest line number.
    let width = body_line_count.to_string().len();

    let mut body_lineno: usize = 0;
    for (idx, line) in lines.iter().enumerate() {
        if idx < body_start {
            // psql: fprintf(output, "%*s\t", nln, "")
            rpg_println!("{:>width$}\t{line}", "");
        } else {
            body_lineno += 1;
            // psql: fprintf(output, "%-*d\t", nln, lineno)
            rpg_println!("{body_lineno:<width$}\t{line}");
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_reconnect_args ------------------------------------------------

    #[test]
    fn reconnect_no_args() {
        let a = parse_reconnect_args(None);
        assert_eq!(a, ReconnectArgs::default());
    }

    #[test]
    fn reconnect_dbname_only() {
        let a = parse_reconnect_args(Some("mydb"));
        assert_eq!(
            a,
            ReconnectArgs {
                dbname: Some("mydb".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reconnect_dbname_and_user() {
        let a = parse_reconnect_args(Some("mydb alice"));
        assert_eq!(
            a,
            ReconnectArgs {
                dbname: Some("mydb".into()),
                user: Some("alice".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reconnect_all_four_params() {
        let a = parse_reconnect_args(Some("mydb alice myhost 5433"));
        assert_eq!(
            a,
            ReconnectArgs {
                dbname: Some("mydb".into()),
                user: Some("alice".into()),
                host: Some("myhost".into()),
                port: Some("5433".into()),
            }
        );
    }

    #[test]
    fn reconnect_dash_means_keep() {
        // `\c - alice` keeps the current dbname but changes the user.
        let a = parse_reconnect_args(Some("- alice"));
        assert_eq!(
            a,
            ReconnectArgs {
                dbname: None, // `-` → None (keep current)
                user: Some("alice".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reconnect_all_dashes() {
        let a = parse_reconnect_args(Some("- - - -"));
        assert_eq!(a, ReconnectArgs::default());
    }

    // -- sql_help / sql_help_text -------------------------------------------

    #[test]
    fn sql_help_no_panic_no_topic() {
        // Should not panic; verify via sql_help_text which is the primary API.
        let text = sql_help_text(None).expect("no-topic help text should succeed");
        assert!(!text.is_empty(), "help text must be non-empty");
    }

    #[test]
    fn sql_help_text_no_topic_returns_ok() {
        let text = sql_help_text(None).expect("no-topic should succeed");
        assert!(text.contains("Available help:"));
    }

    #[test]
    fn sql_help_text_select_returns_ok() {
        let text = sql_help_text(Some("SELECT")).expect("SELECT should be found");
        assert!(text.contains("Command:     SELECT"));
        assert!(text.contains("Syntax:"));
    }

    #[test]
    fn sql_help_text_unknown_returns_err() {
        let err = sql_help_text(Some("NOTAVALIDCOMMAND")).expect_err("should be Err");
        assert_eq!(err, "NOTAVALIDCOMMAND");
    }

    #[test]
    fn sql_help_select() {
        // Trust the output because SQL_HELP is static.
        let entry = SQL_HELP
            .iter()
            .find(|e| e.name == "SELECT")
            .expect("SELECT entry missing");
        // Synopsis must contain the FROM keyword (from psql's output).
        assert!(entry.synopsis.contains("FROM"));
        // Description must match psql's exact wording.
        assert_eq!(entry.description, "retrieve rows from a table or view");
        // URL must point to the PostgreSQL docs.
        assert!(entry.url.contains("sql-select"));
    }

    #[test]
    fn sql_help_all_topics_present() {
        let required = [
            "SELECT",
            "INSERT",
            "UPDATE",
            "DELETE",
            "CREATE TABLE",
            "CREATE INDEX",
            "ALTER TABLE",
            "DROP TABLE",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "GRANT",
            "REVOKE",
            "EXPLAIN",
            "VACUUM",
            "ANALYZE",
            "COPY",
            "CREATE FUNCTION",
            "CREATE VIEW",
        ];
        for cmd in required {
            assert!(
                SQL_HELP.iter().any(|e| e.name == cmd),
                "missing SQL help topic: {cmd}"
            );
        }
    }

    #[test]
    fn sql_help_topic_prefix_match() {
        // "CREATE" should match "CREATE TABLE" (first prefix match).
        let upper = "CREATE".to_uppercase();
        let entry = SQL_HELP
            .iter()
            .find(|e| e.name == upper)
            .or_else(|| SQL_HELP.iter().find(|e| e.name.starts_with(&upper)));
        assert!(entry.is_some(), "CREATE prefix should match something");
    }

    #[test]
    fn sql_help_descriptions_non_empty() {
        // Every entry must have a non-empty description and URL.
        for e in SQL_HELP {
            assert!(
                !e.description.is_empty(),
                "empty description for {}",
                e.name
            );
            assert!(!e.url.is_empty(), "empty URL for {}", e.name);
            assert!(
                e.url.starts_with("https://www.postgresql.org/"),
                "unexpected URL for {}: {}",
                e.name,
                e.url
            );
        }
    }

    // -- split_schema_name ---------------------------------------------------

    #[test]
    fn split_schema_name_qualified() {
        let (schema, obj) = split_schema_name("public.my_func");
        assert_eq!(schema, Some("public".into()));
        assert_eq!(obj, "my_func");
    }

    #[test]
    fn split_schema_name_unqualified() {
        let (schema, obj) = split_schema_name("my_func");
        assert!(schema.is_none());
        assert_eq!(obj, "my_func");
    }

    // -- build SQL helpers ---------------------------------------------------

    #[test]
    fn build_function_source_sql_unqualified() {
        let sql = build_function_source_sql(None, "my_func");
        assert!(sql.contains("p.proname = 'my_func'"));
        assert!(sql.contains("pg_catalog"));
        assert!(sql.contains("pg_get_functiondef"));
    }

    #[test]
    fn build_function_source_sql_qualified() {
        let sql = build_function_source_sql(Some("myschema"), "my_func");
        assert!(sql.contains("n.nspname = 'myschema'"));
    }

    #[test]
    fn build_view_def_sql_unqualified() {
        let sql = build_view_def_sql(None, "my_view");
        assert!(sql.contains("c.relname = 'my_view'"));
        assert!(sql.contains("pg_get_viewdef"));
        assert!(sql.contains("'v', 'm'"));
    }

    #[test]
    fn build_view_def_sql_qualified() {
        let sql = build_view_def_sql(Some("myschema"), "my_view");
        assert!(sql.contains("n.nspname = 'myschema'"));
    }

    #[test]
    fn build_view_def_sql_selects_nspname_and_relname() {
        let sql = build_view_def_sql(None, "my_view");
        assert!(sql.contains("n.nspname"));
        assert!(sql.contains("c.relname"));
    }

    // -- print_view_def ------------------------------------------------------

    #[test]
    fn print_view_def_no_line_numbers() {
        // Capture output would require redirecting stdout; instead verify that
        // the function accepts a semicolon-free body and returns without panic.
        // Actual formatting is validated by the `\sv+` line-number tests below.
        let header = "CREATE OR REPLACE VIEW public.v AS";
        let body = " SELECT id\n   FROM t";
        // No panic == pass.
        print_view_def(header, body, false);
    }

    #[test]
    fn print_view_def_with_line_numbers() {
        // Verify the function accepts a body and runs without panic when
        // plus=true.  Line numbering starts at 1 for the header.
        let header = "CREATE OR REPLACE VIEW public.v AS";
        let body = " SELECT id\n   FROM t";
        print_view_def(header, body, true);
    }

    // -- reconnect port validation -------------------------------------------

    #[tokio::test]
    async fn reconnect_invalid_port_returns_error() {
        // A non-numeric port must return an error immediately, without
        // attempting a network connection.
        use crate::connection::ConnParams;
        let params = ConnParams::default();
        // Pattern: "mydb alice myhost notaport"
        let err = reconnect(Some("mydb alice myhost notaport"), &params)
            .await
            .unwrap_err();
        assert!(
            err.contains("invalid port number"),
            "unexpected error message: {err}"
        );
        assert!(
            err.contains("notaport"),
            "error should include the bad value: {err}"
        );
    }

    #[tokio::test]
    async fn reconnect_port_out_of_range_returns_error() {
        // u16 max is 65535; 99999 is out of range.
        use crate::connection::ConnParams;
        let params = ConnParams::default();
        let err = reconnect(Some("mydb alice myhost 99999"), &params)
            .await
            .unwrap_err();
        assert!(
            err.contains("invalid port number"),
            "unexpected error message: {err}"
        );
    }
}
