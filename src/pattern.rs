//! Pattern matching helpers for psql-style `\d` command filters.
//!
//! Psql supports glob-style patterns in describe commands:
//! - `*` matches any sequence of characters (SQL `%`)
//! - `?` matches a single character (SQL `_`)
//! - `schema.name` is a schema-qualified pattern
//!
//! Because Samo uses `simple_query` (no parameterised queries), values must
//! be embedded directly in SQL strings.  All user-supplied strings are escaped
//! using standard SQL single-quote doubling (`'` → `''`) to prevent injection.

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Escape a string for embedding inside a SQL single-quoted literal.
///
/// Doubles any `'` characters already in `s`.  The caller is responsible for
/// wrapping the result in `'…'`.
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Convert a psql-style pattern to a SQL `LIKE` expression value.
///
/// - `*` → `%`
/// - `?` → `_`
/// - Existing `%` and `_` in the input are escaped with a backslash so they
///   are treated as literals: `%` → `\%`, `_` → `\_`.
/// - Single quotes are doubled for safe SQL embedding.
///
/// Returns the raw value (without enclosing quotes) ready to embed as a SQL
/// string literal.
pub fn to_like(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len() + 4);
    for ch in pattern.chars() {
        match ch {
            // Escape SQL LIKE special chars that are literal in psql patterns.
            '%' => out.push_str("\\%"),
            '_' => out.push_str("\\_"),
            // Map psql wildcards to SQL LIKE wildcards.
            '*' => out.push('%'),
            '?' => out.push('_'),
            // Escape single quotes for SQL embedding.
            '\'' => out.push_str("''"),
            other => out.push(other),
        }
    }
    out
}

/// Split a schema-qualified pattern into `(schema_pattern, name_pattern)`.
///
/// A single `.` is used as the delimiter.  Only the *first* dot is used; any
/// subsequent dots are part of the name portion.
///
/// - `"public.users"` → `(Some("public"), "users")`
/// - `"public.*"` → `(Some("public"), "*")`
/// - `"users"` → `(None, "users")`
/// - `"."` → `(Some(""), "")`
pub fn split_schema(pattern: &str) -> (Option<&str>, &str) {
    if let Some(dot) = pattern.find('.') {
        let schema = &pattern[..dot];
        let name = &pattern[dot + 1..];
        (Some(schema), name)
    } else {
        (None, pattern)
    }
}

/// Return `true` when a pattern string contains psql wildcard characters
/// (`*` or `?`).
fn has_wildcards(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?')
}

/// Build a SQL `WHERE` clause fragment for name-pattern filtering.
///
/// # Parameters
///
/// - `pattern` — optional psql-style pattern (may be schema-qualified).
/// - `column` — unqualified column name for the *object name* (e.g.
///   `"relname"`).
/// - `schema_column` — optional column name for the *schema* (e.g.
///   `"nspname"`).  Pass `None` when the query does not expose a schema
///   column.
///
/// # Return value
///
/// A SQL fragment suitable for appending after `WHERE` (or `AND`).  Returns
/// an empty string when `pattern` is `None` (no filter required).
///
/// When the pattern is schema-qualified and `schema_column` is provided, both
/// columns are filtered.
///
/// The fragment uses single-quoted SQL string literals with the value
/// SQL-escaped to prevent injection.  When wildcards are present a `LIKE`
/// comparison is used (with `ESCAPE '\'`); otherwise an equality check is
/// used.
pub fn where_clause(pattern: Option<&str>, column: &str, schema_column: Option<&str>) -> String {
    let Some(pat) = pattern else {
        return String::new();
    };

    // Only split on `.` when we have a schema column to filter on.
    // Otherwise treat the whole pattern (including any dot) as the name.
    if let Some(sc) = schema_column {
        let (schema_pat, name_pat) = split_schema(pat);

        if let Some(sp) = schema_pat {
            let schema_clause = build_name_clause(sp, sc);
            let name_clause = build_name_clause(name_pat, column);

            if schema_clause.is_empty() && name_clause.is_empty() {
                String::new()
            } else if schema_clause.is_empty() {
                name_clause
            } else if name_clause.is_empty() {
                schema_clause
            } else {
                format!("{schema_clause} AND {name_clause}")
            }
        } else {
            build_name_clause(name_pat, column)
        }
    } else {
        build_name_clause(pat, column)
    }
}

/// Build a single-column filter clause (helper for [`where_clause`]).
fn build_name_clause(pattern: &str, column: &str) -> String {
    if pattern.is_empty() {
        return String::new();
    }

    if has_wildcards(pattern) {
        let like_val = to_like(pattern);
        format!("{column} LIKE '{like_val}' ESCAPE '\\'")
    } else {
        let escaped = sql_escape(pattern);
        format!("{column} = '{escaped}'")
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- to_like ---------------------------------------------------------------

    #[test]
    fn to_like_star_becomes_percent() {
        assert_eq!(to_like("public.*"), "public.%");
    }

    #[test]
    fn to_like_question_becomes_underscore() {
        assert_eq!(to_like("foo?bar"), "foo_bar");
    }

    #[test]
    fn to_like_escapes_existing_percent() {
        assert_eq!(to_like("100%"), "100\\%");
    }

    #[test]
    fn to_like_escapes_existing_underscore() {
        assert_eq!(to_like("my_table"), "my\\_table");
    }

    #[test]
    fn to_like_mixed_wildcards() {
        assert_eq!(to_like("pub*._t?"), "pub%.\\_t_");
    }

    #[test]
    fn to_like_escapes_single_quote() {
        assert_eq!(to_like("o'reilly"), "o''reilly");
    }

    #[test]
    fn to_like_no_wildcards() {
        assert_eq!(to_like("users"), "users");
    }

    // -- split_schema ----------------------------------------------------------

    #[test]
    fn split_schema_qualified() {
        assert_eq!(split_schema("public.users"), (Some("public"), "users"));
    }

    #[test]
    fn split_schema_with_wildcard() {
        assert_eq!(split_schema("public.*"), (Some("public"), "*"));
    }

    #[test]
    fn split_schema_unqualified() {
        assert_eq!(split_schema("users"), (None, "users"));
    }

    #[test]
    fn split_schema_empty_schema() {
        assert_eq!(split_schema(".users"), (Some(""), "users"));
    }

    #[test]
    fn split_schema_only_dot() {
        assert_eq!(split_schema("."), (Some(""), ""));
    }

    // -- where_clause ----------------------------------------------------------

    #[test]
    fn where_clause_none_returns_empty() {
        assert_eq!(where_clause(None, "relname", Some("nspname")), "");
    }

    #[test]
    fn where_clause_exact_match_no_schema() {
        assert_eq!(
            where_clause(Some("users"), "relname", None),
            "relname = 'users'"
        );
    }

    #[test]
    fn where_clause_wildcard_like() {
        assert_eq!(
            where_clause(Some("user*"), "relname", None),
            "relname LIKE 'user%' ESCAPE '\\'"
        );
    }

    #[test]
    fn where_clause_schema_qualified_exact() {
        assert_eq!(
            where_clause(Some("public.users"), "relname", Some("nspname")),
            "nspname = 'public' AND relname = 'users'"
        );
    }

    #[test]
    fn where_clause_schema_qualified_wildcard_name() {
        assert_eq!(
            where_clause(Some("public.*"), "relname", Some("nspname")),
            "nspname = 'public' AND relname LIKE '%' ESCAPE '\\'"
        );
    }

    #[test]
    fn where_clause_schema_wildcard_no_schema_column() {
        // When schema_column is None, dot is part of the name filter.
        assert_eq!(
            where_clause(Some("public.*"), "relname", None),
            "relname LIKE 'public.%' ESCAPE '\\'"
        );
    }

    #[test]
    fn where_clause_sql_escape_in_literal() {
        assert_eq!(
            where_clause(Some("o'reilly"), "relname", None),
            "relname = 'o''reilly'"
        );
    }

    #[test]
    fn where_clause_empty_schema_part() {
        // ".users" — schema part is empty so only name is filtered.
        assert_eq!(
            where_clause(Some(".users"), "relname", Some("nspname")),
            "relname = 'users'"
        );
    }
}
