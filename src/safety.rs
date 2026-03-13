//! Destructive statement detection and confirmation.
//!
//! Detects potentially dangerous SQL statements and prompts the user
//! for confirmation before execution.

/// Check if the given SQL statement is potentially destructive.
///
/// Returns `Some(description)` if destructive, `None` otherwise.
/// The check is case-insensitive and handles leading whitespace.
///
/// Multi-statement input (e.g. `SELECT 1; DROP TABLE foo;`) is scanned
/// for any destructive statement across all semicolon-separated segments.
pub fn check_destructive(sql: &str) -> Option<&'static str> {
    // Split on semicolons so we catch destructive statements in multi-statement
    // input like `SELECT 1; DROP TABLE foo;`.
    for segment in sql.split(';') {
        if let Some(desc) = check_single_statement(segment) {
            return Some(desc);
        }
    }
    None
}

/// Check a single SQL statement (no semicolons) for destructive patterns.
fn check_single_statement(sql: &str) -> Option<&'static str> {
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
            if tokens.len() >= 2 {
                match tokens[1] {
                    "TABLE" => Some("DROP TABLE"),
                    "DATABASE" => Some("DROP DATABASE"),
                    "SCHEMA" => Some("DROP SCHEMA"),
                    "INDEX" => Some("DROP INDEX"),
                    "VIEW" => Some("DROP VIEW"),
                    "FUNCTION" | "PROCEDURE" => Some("DROP FUNCTION/PROCEDURE"),
                    "EXTENSION" => Some("DROP EXTENSION"),
                    "ROLE" | "USER" => Some("DROP ROLE/USER"),
                    _ => None,
                }
            } else {
                None
            }
        }
        "TRUNCATE" => Some("TRUNCATE"),
        "DELETE" => {
            // DELETE without WHERE is dangerous.
            if upper.contains(" WHERE ") {
                None
            } else {
                Some("DELETE without WHERE clause")
            }
        }
        "UPDATE" => {
            // UPDATE without WHERE is dangerous.
            if upper.contains(" WHERE ") {
                None
            } else {
                Some("UPDATE without WHERE clause")
            }
        }
        "ALTER" => {
            if tokens.len() >= 4 && tokens[1] == "TABLE" && tokens.contains(&"DROP") {
                Some("ALTER TABLE ... DROP")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Prompt the user for confirmation before executing a destructive statement.
///
/// Prints a warning to stderr and reads a `y`/`n` response.
///
/// Returns `true` if the user confirms (or input is non-interactive).
/// In non-interactive mode (`-c`, `-f`, piped stdin), always returns `true`
/// so scripts are not interrupted.
pub fn confirm_destructive(description: &str) -> bool {
    use std::io::{self, BufRead, IsTerminal, Write};

    if !io::stdin().is_terminal() {
        // Non-interactive: skip confirmation and proceed.
        return true;
    }

    eprint!("WARNING: {description} — are you sure? [y/N] ");
    io::stderr().flush().ok();

    let mut input = String::new();
    if io::stdin().lock().read_line(&mut input).is_ok() {
        let trimmed = input.trim().to_lowercase();
        trimmed == "y" || trimmed == "yes"
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

    #[test]
    fn test_drop_table() {
        assert_eq!(check_destructive("DROP TABLE my_table"), Some("DROP TABLE"));
    }

    #[test]
    fn test_drop_database() {
        assert_eq!(
            check_destructive("DROP DATABASE mydb"),
            Some("DROP DATABASE")
        );
    }

    #[test]
    fn test_drop_schema() {
        assert_eq!(check_destructive("DROP SCHEMA public"), Some("DROP SCHEMA"));
    }

    #[test]
    fn test_drop_index() {
        assert_eq!(check_destructive("DROP INDEX idx_name"), Some("DROP INDEX"));
    }

    #[test]
    fn test_drop_view() {
        assert_eq!(check_destructive("DROP VIEW my_view"), Some("DROP VIEW"));
    }

    #[test]
    fn test_drop_function() {
        assert_eq!(
            check_destructive("DROP FUNCTION my_func()"),
            Some("DROP FUNCTION/PROCEDURE")
        );
    }

    #[test]
    fn test_drop_procedure() {
        assert_eq!(
            check_destructive("DROP PROCEDURE my_proc()"),
            Some("DROP FUNCTION/PROCEDURE")
        );
    }

    #[test]
    fn test_drop_extension() {
        assert_eq!(
            check_destructive("DROP EXTENSION pg_stat_statements"),
            Some("DROP EXTENSION")
        );
    }

    #[test]
    fn test_drop_role() {
        assert_eq!(
            check_destructive("DROP ROLE my_role"),
            Some("DROP ROLE/USER")
        );
    }

    #[test]
    fn test_drop_user() {
        assert_eq!(
            check_destructive("DROP USER my_user"),
            Some("DROP ROLE/USER")
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(check_destructive("TRUNCATE my_table"), Some("TRUNCATE"));
    }

    #[test]
    fn test_delete_without_where() {
        assert_eq!(
            check_destructive("DELETE FROM my_table"),
            Some("DELETE without WHERE clause")
        );
    }

    #[test]
    fn test_delete_with_where() {
        assert_eq!(check_destructive("DELETE FROM my_table WHERE id = 1"), None);
    }

    #[test]
    fn test_update_without_where() {
        assert_eq!(
            check_destructive("UPDATE my_table SET col = 'val'"),
            Some("UPDATE without WHERE clause")
        );
    }

    #[test]
    fn test_update_with_where() {
        assert_eq!(
            check_destructive("UPDATE my_table SET col = 'val' WHERE id = 1"),
            None
        );
    }

    #[test]
    fn test_alter_table_drop_column() {
        assert_eq!(
            check_destructive("ALTER TABLE my_table DROP COLUMN col_name"),
            Some("ALTER TABLE ... DROP")
        );
    }

    #[test]
    fn test_alter_table_add_column_is_safe() {
        assert_eq!(
            check_destructive("ALTER TABLE my_table ADD COLUMN new_col text"),
            None
        );
    }

    #[test]
    fn test_select_is_safe() {
        assert_eq!(check_destructive("SELECT * FROM my_table"), None);
    }

    #[test]
    fn test_insert_is_safe() {
        assert_eq!(
            check_destructive("INSERT INTO my_table (col) VALUES ('val')"),
            None
        );
    }

    #[test]
    fn test_create_table_is_safe() {
        assert_eq!(check_destructive("CREATE TABLE new_table (id int8)"), None);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(check_destructive(""), None);
        assert_eq!(check_destructive("   "), None);
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(check_destructive("drop table my_table"), Some("DROP TABLE"));
        assert_eq!(check_destructive("Drop Table my_table"), Some("DROP TABLE"));
        assert_eq!(check_destructive("truncate my_table"), Some("TRUNCATE"));
    }

    #[test]
    fn test_drop_if_exists() {
        // DROP TABLE IF EXISTS should still be detected.
        assert_eq!(
            check_destructive("DROP TABLE IF EXISTS foo"),
            Some("DROP TABLE")
        );
    }

    #[test]
    fn test_delete_from_with_where() {
        assert_eq!(check_destructive("DELETE FROM t WHERE id = 1"), None);
    }

    #[test]
    fn test_multi_statement_detects_drop() {
        // Multi-statement input: SELECT is safe, but DROP TABLE is not.
        assert_eq!(
            check_destructive("SELECT 1; DROP TABLE foo;"),
            Some("DROP TABLE")
        );
    }

    #[test]
    fn test_multi_statement_all_safe() {
        assert_eq!(check_destructive("SELECT 1; SELECT 2;"), None);
    }

    #[test]
    fn test_truncate_with_table_keyword() {
        // TRUNCATE TABLE syntax also detected.
        assert_eq!(
            check_destructive("TRUNCATE TABLE my_table"),
            Some("TRUNCATE")
        );
    }
}
