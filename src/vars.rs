//! psql-compatible variable store and SQL interpolation.
//!
//! Implements `\set` / `\unset` variable storage and the four interpolation
//! syntaxes that psql supports:
//!
//! | Syntax       | Expansion                                |
//! |--------------|------------------------------------------|
//! | `:name`      | raw value                                |
//! | `:'name'`    | single-quoted (SQL-safe)                 |
//! | `:"name"`    | double-quoted (identifier-safe)          |
//! | `:{?name}`   | `true` or `false` (existence test)       |

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/// Runtime variable store (`\set` / `\unset`).
#[derive(Debug, Clone)]
pub struct Variables {
    vars: HashMap<String, String>,
}

impl Default for Variables {
    fn default() -> Self {
        Self::new()
    }
}

impl Variables {
    /// Create a new store pre-populated with psql-compatible defaults.
    pub fn new() -> Self {
        let mut vars = HashMap::new();
        vars.insert("AUTOCOMMIT".to_owned(), "on".to_owned());
        vars.insert("ECHO".to_owned(), "none".to_owned());
        vars.insert("ECHO_HIDDEN".to_owned(), "off".to_owned());
        vars.insert("ON_ERROR_STOP".to_owned(), "off".to_owned());
        vars.insert("PROMPT1".to_owned(), "%/%R%x%# ".to_owned());
        vars.insert("PROMPT2".to_owned(), "%/%R%x%# ".to_owned());
        vars.insert("PROMPT3".to_owned(), ">> ".to_owned());
        Self { vars }
    }

    /// Set a variable to `value`.
    pub fn set(&mut self, name: &str, value: &str) {
        self.vars.insert(name.to_owned(), value.to_owned());
    }

    /// Get the value of a variable, or `None` if not set.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// Unset a variable.  Returns `true` if it was present.
    pub fn unset(&mut self, name: &str) -> bool {
        self.vars.remove(name).is_some()
    }

    /// Return the full variable map (for `\set` with no args).
    pub fn all(&self) -> &HashMap<String, String> {
        &self.vars
    }

    /// Interpolate variable references in `sql`.
    ///
    /// The four syntaxes recognised:
    ///
    /// - `:{?name}` → `true` or `false` (existence test, no quotes in output)
    /// - `:'name'`  → `'value'` with SQL single-quote escaping
    /// - `:"name"`  → `"value"` with double-quote escaping
    /// - `:name`    → raw value
    ///
    /// A double colon `::` is an escape (Postgres cast syntax): the first `:`
    /// is consumed and one `:` is emitted, no interpolation takes place.
    /// This prevents `::<cast>` from being mangled.
    ///
    /// References to undefined variables are left verbatim.
    pub fn interpolate(&self, sql: &str) -> String {
        let chars: Vec<char> = sql.chars().collect();
        let len = chars.len();
        let mut out = String::with_capacity(sql.len());
        let mut i = 0;

        while i < len {
            // Not a colon — pass through.
            if chars[i] != ':' {
                out.push(chars[i]);
                i += 1;
                continue;
            }

            // Double colon `::` — Postgres cast operator, pass both through.
            if i + 1 < len && chars[i + 1] == ':' {
                out.push(':');
                out.push(':');
                i += 2;
                continue;
            }

            // Existence test: `:{?name}`.
            if i + 2 < len && chars[i + 1] == '{' && chars[i + 2] == '?' {
                let start = i + 3;
                if let Some(close) = chars[start..].iter().position(|&c| c == '}') {
                    let name: String = chars[start..start + close].iter().collect();
                    if !name.is_empty() {
                        let exists = self.vars.contains_key(&name);
                        out.push_str(if exists { "true" } else { "false" });
                        i = start + close + 1;
                        continue;
                    }
                }
                // Malformed — emit verbatim.
                out.push(':');
                i += 1;
                continue;
            }

            // Single-quoted: `:'name'`
            if i + 1 < len && chars[i + 1] == '\'' {
                let start = i + 2;
                if let Some(end) = chars[start..].iter().position(|&c| c == '\'') {
                    let name: String = chars[start..start + end].iter().collect();
                    if !name.is_empty() {
                        if let Some(val) = self.vars.get(&name) {
                            out.push('\'');
                            out.push_str(&sql_quote_single(val));
                            out.push('\'');
                            i = start + end + 1;
                            continue;
                        }
                    }
                }
                // No closing quote or variable not found — emit verbatim.
                out.push(':');
                i += 1;
                continue;
            }

            // Double-quoted: `:"name"`
            if i + 1 < len && chars[i + 1] == '"' {
                let start = i + 2;
                if let Some(end) = chars[start..].iter().position(|&c| c == '"') {
                    let name: String = chars[start..start + end].iter().collect();
                    if !name.is_empty() {
                        if let Some(val) = self.vars.get(&name) {
                            out.push('"');
                            out.push_str(&sql_quote_double(val));
                            out.push('"');
                            i = start + end + 1;
                            continue;
                        }
                    }
                }
                // No closing quote or variable not found — emit verbatim.
                out.push(':');
                i += 1;
                continue;
            }

            // Bare identifier: `:name` (must start with letter or underscore).
            if i + 1 < len && (chars[i + 1].is_alphabetic() || chars[i + 1] == '_') {
                let start = i + 1;
                let mut end = start;
                while end < len && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                let name: String = chars[start..end].iter().collect();
                if let Some(val) = self.vars.get(&name) {
                    out.push_str(val);
                    i = end;
                    continue;
                }
                // Variable not defined — emit verbatim.
                out.push(':');
                i += 1;
                continue;
            }

            // Lone colon with no recognised following syntax — pass through.
            out.push(':');
            i += 1;
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Quoting helpers
// ---------------------------------------------------------------------------

/// Escape a value for embedding inside `'...'` (SQL string literal).
///
/// Each `'` in the value is doubled to `''` per SQL standard.
fn sql_quote_single(val: &str) -> String {
    val.replace('\'', "''")
}

/// Escape a value for embedding inside `"..."` (SQL quoted identifier).
///
/// Each `"` in the value is doubled to `""` per SQL standard.
fn sql_quote_double(val: &str) -> String {
    val.replace('"', "\"\"")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn vars_with(pairs: &[(&str, &str)]) -> Variables {
        let mut v = Variables::new();
        for (k, val) in pairs {
            v.set(k, val);
        }
        v
    }

    // -- set / get / unset ---------------------------------------------------

    #[test]
    fn set_and_get() {
        let mut v = Variables::new();
        v.set("FOO", "bar");
        assert_eq!(v.get("FOO"), Some("bar"));
    }

    #[test]
    fn get_missing_returns_none() {
        let v = Variables::new();
        assert_eq!(v.get("MISSING"), None);
    }

    #[test]
    fn unset_existing_returns_true() {
        let mut v = Variables::new();
        v.set("FOO", "bar");
        assert!(v.unset("FOO"));
        assert_eq!(v.get("FOO"), None);
    }

    #[test]
    fn unset_missing_returns_false() {
        let mut v = Variables::new();
        assert!(!v.unset("NOPE"));
    }

    #[test]
    fn defaults_include_autocommit() {
        let v = Variables::new();
        assert_eq!(v.get("AUTOCOMMIT"), Some("on"));
    }

    // -- interpolation: bare :name -------------------------------------------

    #[test]
    fn interp_bare_variable() {
        let v = vars_with(&[("myvar", "hello")]);
        assert_eq!(v.interpolate("select :myvar"), "select hello");
    }

    #[test]
    fn interp_bare_variable_in_middle() {
        let v = vars_with(&[("tbl", "users")]);
        assert_eq!(
            v.interpolate("select * from :tbl where 1=1"),
            "select * from users where 1=1"
        );
    }

    #[test]
    fn interp_bare_undefined_passthrough() {
        let v = Variables::new();
        assert_eq!(v.interpolate("select :unknown"), "select :unknown");
    }

    // -- interpolation: :'name' ----------------------------------------------

    #[test]
    fn interp_single_quoted_variable() {
        let v = vars_with(&[("name", "alice")]);
        assert_eq!(v.interpolate("where x = :'name'"), "where x = 'alice'");
    }

    #[test]
    fn interp_single_quoted_escapes_quotes() {
        let v = vars_with(&[("val", "it's")]);
        assert_eq!(v.interpolate("where x = :'val'"), "where x = 'it''s'");
    }

    #[test]
    fn interp_single_quoted_undefined_passthrough() {
        let v = Variables::new();
        assert_eq!(v.interpolate(":'missing'"), ":'missing'");
    }

    // -- interpolation: :"name" ----------------------------------------------

    #[test]
    fn interp_double_quoted_variable() {
        let v = vars_with(&[("col", "my_col")]);
        assert_eq!(v.interpolate("select :\"col\""), "select \"my_col\"");
    }

    #[test]
    fn interp_double_quoted_escapes_quotes() {
        let v = vars_with(&[("col", "my\"col")]);
        assert_eq!(v.interpolate(":\"col\""), "\"my\"\"col\"");
    }

    #[test]
    fn interp_double_quoted_undefined_passthrough() {
        let v = Variables::new();
        assert_eq!(v.interpolate(":\"missing\""), ":\"missing\"");
    }

    // -- interpolation: :{?name} ---------------------------------------------

    #[test]
    fn interp_existence_test_true() {
        let v = vars_with(&[("FOO", "1")]);
        assert_eq!(v.interpolate(":{?FOO}"), "true");
    }

    #[test]
    fn interp_existence_test_false() {
        let v = Variables::new();
        // MISSING is not a default variable.
        assert_eq!(v.interpolate(":{?MISSING}"), "false");
    }

    // -- double colon (Postgres cast) ----------------------------------------

    #[test]
    fn interp_cast_syntax_passthrough() {
        let v = Variables::new();
        assert_eq!(v.interpolate("select 1::int"), "select 1::int");
    }

    #[test]
    fn interp_cast_in_complex_query() {
        let v = vars_with(&[("val", "42")]);
        assert_eq!(v.interpolate("select :val::int"), "select 42::int");
    }

    // -- no interpolation when no colon --------------------------------------

    #[test]
    fn interp_no_colon_unchanged() {
        let v = Variables::new();
        let sql = "select 1, 'hello world'";
        assert_eq!(v.interpolate(sql), sql);
    }

    // -- all() ---------------------------------------------------------------

    #[test]
    fn all_contains_defaults() {
        let v = Variables::new();
        assert!(v.all().contains_key("ECHO_HIDDEN"));
        assert!(v.all().contains_key("ON_ERROR_STOP"));
    }
}
