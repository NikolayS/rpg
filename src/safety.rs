//! Destructive statement detection and confirmation.
//!
//! Detects potentially dangerous SQL statements and prompts the user
//! for confirmation before execution in interactive sessions.

/// Check if the given SQL statement is potentially destructive.
///
/// Returns `Some(reason)` describing why the statement is dangerous, or
/// `None` if the statement appears safe. The check is case-insensitive
/// and handles leading whitespace.
///
/// Multi-statement input (e.g. `select 1; drop table foo;`) is scanned
/// for any destructive statement across all semicolon-separated segments.
///
/// # Detected patterns
///
/// - `drop table` / `drop database` / `drop schema`
/// - `truncate`
/// - `delete` without `where`
/// - `update` without `where`
/// - `alter table ... drop column`
pub fn is_destructive(sql: &str) -> Option<&'static str> {
    // Split on semicolons so we catch destructive statements in multi-statement
    // input like `select 1; drop table foo;`.
    for segment in sql.split(';') {
        if let Some(reason) = check_segment(segment) {
            return Some(reason);
        }
    }
    None
}

/// Check a single SQL statement (no semicolons) for destructive patterns.
fn check_segment(sql: &str) -> Option<&'static str> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_uppercase();
    let tokens: Vec<&str> = upper.split_whitespace().collect();

    if tokens.is_empty() {
        return None;
    }

    match tokens[0] {
        "DROP" => {
            if tokens.len() < 2 {
                return None;
            }
            match tokens[1] {
                "TABLE" => Some("drop table"),
                "DATABASE" => Some("drop database"),
                "SCHEMA" => Some("drop schema"),
                "INDEX" => Some("drop index"),
                "VIEW" => Some("drop view"),
                "FUNCTION" | "PROCEDURE" => Some("drop function/procedure"),
                "EXTENSION" => Some("drop extension"),
                "ROLE" | "USER" => Some("drop role/user"),
                _ => None,
            }
        }
        "TRUNCATE" => Some("truncate"),
        "DELETE" => {
            // `delete` without `where` affects all rows.
            if upper.contains(" WHERE ") {
                None
            } else {
                Some("delete without where clause")
            }
        }
        "UPDATE" => {
            // `update` without `where` affects all rows.
            if upper.contains(" WHERE ") {
                None
            } else {
                Some("update without where clause")
            }
        }
        "ALTER" => {
            // `alter table ... drop column` / `alter table ... drop constraint`
            if tokens.len() >= 4 && tokens[1] == "TABLE" && tokens.contains(&"DROP") {
                Some("alter table ... drop")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Prompt the user for confirmation before executing a destructive statement.
///
/// Prints a warning to stderr and reads a `y`/`n` response. Returns `true`
/// if the user confirms. In non-interactive mode (`-c`, `-f`, piped stdin)
/// always returns `true` so scripts are not interrupted.
///
/// On Unix the response is read from `/dev/tty` so the prompt works even
/// when stdin is redirected; other platforms fall back to stdin.
pub fn confirm_destructive(reason: &str) -> bool {
    use std::io::{self, BufRead, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        // Non-interactive: skip confirmation and proceed.
        return true;
    }

    eprint!("WARNING: {reason}\nAre you sure? [y/N] ");
    io::stderr().flush().ok();

    // Read from /dev/tty so the prompt works even when stdin is piped.
    #[cfg(unix)]
    {
        use std::fs::File;
        if let Ok(tty) = File::open("/dev/tty") {
            let mut input = String::new();
            if io::BufReader::new(tty).read_line(&mut input).is_ok() {
                let answer = input.trim().to_lowercase();
                return answer == "y" || answer == "yes";
            }
            return false;
        }
    }

    // Fallback for non-Unix platforms: read from stdin.
    let mut input = String::new();
    if io::stdin().lock().read_line(&mut input).is_ok() {
        let answer = input.trim().to_lowercase();
        answer == "y" || answer == "yes"
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- drop variants -------------------------------------------------------

    #[test]
    fn drop_table_detected() {
        assert_eq!(is_destructive("drop table my_table"), Some("drop table"));
    }

    #[test]
    fn drop_table_if_exists_detected() {
        // `drop table if exists` still starts with DROP TABLE.
        assert_eq!(
            is_destructive("drop table if exists foo"),
            Some("drop table")
        );
    }

    #[test]
    fn drop_database_detected() {
        assert_eq!(is_destructive("drop database mydb"), Some("drop database"));
    }

    #[test]
    fn drop_schema_detected() {
        assert_eq!(is_destructive("drop schema public"), Some("drop schema"));
    }

    #[test]
    fn drop_index_detected() {
        assert_eq!(is_destructive("drop index idx_name"), Some("drop index"));
    }

    #[test]
    fn drop_view_detected() {
        assert_eq!(is_destructive("drop view my_view"), Some("drop view"));
    }

    #[test]
    fn drop_function_detected() {
        assert_eq!(
            is_destructive("drop function my_func()"),
            Some("drop function/procedure")
        );
    }

    #[test]
    fn drop_procedure_detected() {
        assert_eq!(
            is_destructive("drop procedure my_proc()"),
            Some("drop function/procedure")
        );
    }

    #[test]
    fn drop_extension_detected() {
        assert_eq!(
            is_destructive("drop extension pg_stat_statements"),
            Some("drop extension")
        );
    }

    #[test]
    fn drop_role_detected() {
        assert_eq!(is_destructive("drop role my_role"), Some("drop role/user"));
    }

    #[test]
    fn drop_user_detected() {
        assert_eq!(is_destructive("drop user my_user"), Some("drop role/user"));
    }

    // -- truncate ------------------------------------------------------------

    #[test]
    fn truncate_detected() {
        assert_eq!(is_destructive("truncate my_table"), Some("truncate"));
    }

    #[test]
    fn truncate_table_keyword_detected() {
        // `truncate table` syntax is also detected.
        assert_eq!(is_destructive("truncate table my_table"), Some("truncate"));
    }

    // -- delete --------------------------------------------------------------

    #[test]
    fn delete_without_where_detected() {
        assert_eq!(
            is_destructive("delete from my_table"),
            Some("delete without where clause")
        );
    }

    #[test]
    fn delete_with_where_safe() {
        assert_eq!(is_destructive("delete from my_table where id = 1"), None);
    }

    // -- update --------------------------------------------------------------

    #[test]
    fn update_without_where_detected() {
        assert_eq!(
            is_destructive("update my_table set col = 'val'"),
            Some("update without where clause")
        );
    }

    #[test]
    fn update_with_where_safe() {
        assert_eq!(
            is_destructive("update my_table set col = 'val' where id = 1"),
            None
        );
    }

    // -- alter table drop ----------------------------------------------------

    #[test]
    fn alter_table_drop_column_detected() {
        assert_eq!(
            is_destructive("alter table my_table drop column col_name"),
            Some("alter table ... drop")
        );
    }

    #[test]
    fn alter_table_add_column_safe() {
        assert_eq!(
            is_destructive("alter table my_table add column new_col text"),
            None
        );
    }

    // -- safe statements -----------------------------------------------------

    #[test]
    fn select_safe() {
        assert_eq!(is_destructive("select * from my_table"), None);
    }

    #[test]
    fn insert_safe() {
        assert_eq!(
            is_destructive("insert into my_table (col) values ('val')"),
            None
        );
    }

    #[test]
    fn create_table_safe() {
        assert_eq!(is_destructive("create table new_table (id int8)"), None);
    }

    // -- edge cases ----------------------------------------------------------

    #[test]
    fn empty_input_safe() {
        assert_eq!(is_destructive(""), None);
        assert_eq!(is_destructive("   "), None);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(is_destructive("DROP TABLE my_table"), Some("drop table"));
        assert_eq!(is_destructive("Drop Table my_table"), Some("drop table"));
        assert_eq!(is_destructive("TRUNCATE my_table"), Some("truncate"));
        assert_eq!(
            is_destructive("DELETE FROM my_table"),
            Some("delete without where clause")
        );
        assert_eq!(
            is_destructive("UPDATE my_table SET col = 1"),
            Some("update without where clause")
        );
    }

    // -- multi-statement -----------------------------------------------------

    #[test]
    fn multi_statement_detects_drop() {
        // A safe statement followed by a destructive one is flagged.
        assert_eq!(
            is_destructive("select 1; drop table foo;"),
            Some("drop table")
        );
    }

    #[test]
    fn multi_statement_all_safe() {
        assert_eq!(is_destructive("select 1; select 2;"), None);
    }
}
