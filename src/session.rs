//! Session-management meta-commands: `\c`, `\sf`, `\sv`, `\h`.
//!
//! Each function is invoked by [`crate::repl::dispatch_meta`] and operates
//! directly on the live Postgres client.  The `echo_hidden` flag, when true,
//! prints internally-generated SQL to stdout before executing it (matching
//! the behaviour of `psql -E` / `samo -E`).

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
) -> Result<(Client, ConnParams), String> {
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
    };

    let mut new_params = connection::resolve_params(&opts).map_err(|e| e.to_string())?;

    // Carry forward the password if the user does not have a .pgpass entry
    // and no PGPASSWORD is set — avoids spurious prompts on same-server
    // reconnects.
    if new_params.password.is_none() {
        new_params.password = current_params.password.clone();
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

    connection::connect(new_params, &opts)
        .await
        .map_err(|e| e.to_string())
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
        println!("{sql}");
    }

    let rows = match client.simple_query(&sql).await {
        Ok(r) => r,
        Err(e) => {
            crate::output::eprint_db_error(&e, None, false);
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
        eprintln!("ERROR:  function {name} does not exist");
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
        println!("{sql}");
    }

    let rows = match client.simple_query(&sql).await {
        Ok(r) => r,
        Err(e) => {
            crate::output::eprint_db_error(&e, None, false);
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
        eprintln!("ERROR:  view {name} does not exist");
    }
}

/// Build the SQL query that retrieves a view definition.
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
        "select pg_catalog.pg_get_viewdef(c.oid, true)\n\
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

/// Static help table: (`command_name`, synopsis).
///
/// Kept intentionally terse — just enough to be useful at the prompt.
static SQL_HELP: &[(&str, &str)] = &[
    (
        "SELECT",
        "select [distinct] <expressions>\n\
         from <table>\n\
         [where <condition>]\n\
         [group by <expressions>]\n\
         [having <condition>]\n\
         [order by <expressions> [asc|desc]]\n\
         [limit <n>] [offset <n>];",
    ),
    (
        "INSERT",
        "insert into <table> [(<columns>)]\n\
         values (<values>) [, ...];\n\
         -- or --\n\
         insert into <table> [(<columns>)] <select>;",
    ),
    (
        "UPDATE",
        "update <table>\n\
         set <column> = <value> [, ...]\n\
         [where <condition>];",
    ),
    (
        "DELETE",
        "delete from <table>\n\
         [where <condition>];",
    ),
    (
        "CREATE TABLE",
        "create [temp] table [if not exists] <table> (\n\
         <column> <type> [<constraints>] [, ...]\n\
         [, table_constraint [, ...]]\n\
         );",
    ),
    (
        "CREATE INDEX",
        "create [unique] index [concurrently] [<name>]\n\
         on <table> [using <method>] (<column> [asc|desc] [, ...]);",
    ),
    (
        "ALTER TABLE",
        "alter table <table>\n\
         <action> [, ...];\n\
         -- actions: add column, drop column, alter column,\n\
         --          add constraint, drop constraint, rename, …",
    ),
    (
        "DROP TABLE",
        "drop table [if exists] <table> [, ...]\n\
         [cascade | restrict];",
    ),
    (
        "BEGIN",
        "begin [isolation level <level>];\n\
         -- or: start transaction [isolation level <level>];",
    ),
    (
        "COMMIT",
        "commit;\n\
         -- or: end;",
    ),
    (
        "ROLLBACK",
        "rollback;\n\
         -- to a savepoint: rollback to [savepoint] <name>;",
    ),
    (
        "GRANT",
        "grant <privileges> on <object> to <role> [, ...];\n\
         -- privileges: select, insert, update, delete, all, …",
    ),
    (
        "REVOKE",
        "revoke <privileges> on <object> from <role> [, ...];",
    ),
    ("EXPLAIN", "explain [analyze] [verbose] [buffers] <query>;"),
    (
        "VACUUM",
        "vacuum [full] [analyze] [<table> [(<column> [, ...])]];",
    ),
    ("ANALYZE", "analyze [<table> [(<column> [, ...])]];"),
    (
        "COPY",
        "copy <table> [(<columns>)] from {{stdin | '<file>'}}\n\
         [with (format <fmt>, ...)];",
    ),
    (
        "CREATE FUNCTION",
        "create [or replace] function <name>(<args>)\n\
         returns <type>\n\
         language <lang>\n\
         as $$\n\
           <body>\n\
         $$;",
    ),
    (
        "CREATE VIEW",
        "create [or replace] view <name> [(<columns>)] as\n\
         <select>;",
    ),
];

/// Print SQL syntax help.
///
/// Without a topic: list all available help topics in columns.
/// With a topic: show the synopsis for that command (case-insensitive match;
/// matches on any prefix of multi-word commands, e.g. `CREATE` matches
/// `CREATE TABLE`).
pub fn sql_help(topic: Option<&str>) {
    match topic {
        None => print_help_topics(),
        Some(t) => print_help_topic(t),
    }
}

/// Print a two-column list of available SQL help topics.
fn print_help_topics() {
    println!("Available help:");
    println!();

    // Collect names and lay them out in two columns.
    let names: Vec<&str> = SQL_HELP.iter().map(|(n, _)| *n).collect();
    let half = names.len().div_ceil(2);
    let col_w = names.iter().map(|n| n.len()).max().unwrap_or(0) + 4;

    for i in 0..half {
        let left = names[i];
        let right = names.get(i + half).copied().unwrap_or("");
        println!("  {left:<col_w$}{right}");
    }

    println!();
    println!("Type \\h <command> for syntax. Example: \\h select");
}

/// Print the synopsis for a specific SQL command (case-insensitive).
fn print_help_topic(topic: &str) {
    let upper = topic.trim().to_uppercase();

    // Look for an exact match first, then a prefix match.
    let entry = SQL_HELP
        .iter()
        .find(|(name, _)| *name == upper)
        .or_else(|| SQL_HELP.iter().find(|(name, _)| name.starts_with(&upper)));

    if let Some((name, synopsis)) = entry {
        println!("Command:     {name}");
        println!("Description: SQL syntax help");
        println!("Syntax:");
        println!();
        for line in synopsis.lines() {
            println!("  {line}");
        }
        println!();
    } else {
        eprintln!("No help available for \"{topic}\".");
        eprintln!("Try \\h with no argument to list available topics.");
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

/// Print `text` to stdout, optionally prepending 1-based line numbers.
fn print_with_optional_line_numbers(text: &str, plus: bool) {
    if plus {
        for (i, line) in text.lines().enumerate() {
            println!("{:>4}\t{line}", i + 1);
        }
    } else {
        println!("{text}");
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

    // -- sql_help ------------------------------------------------------------

    #[test]
    fn sql_help_no_panic_no_topic() {
        // Should not panic.
        sql_help(None);
    }

    #[test]
    fn sql_help_select() {
        // Trust the output because SQL_HELP is static.
        let entry = SQL_HELP
            .iter()
            .find(|(n, _)| *n == "SELECT")
            .expect("SELECT entry missing");
        assert!(entry.1.contains("from"));
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
                SQL_HELP.iter().any(|(n, _)| *n == cmd),
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
            .find(|(n, _)| *n == upper)
            .or_else(|| SQL_HELP.iter().find(|(n, _)| n.starts_with(&upper)));
        assert!(entry.is_some(), "CREATE prefix should match something");
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
