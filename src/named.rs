//! Named query storage and retrieval.
//!
//! Named queries are stored in a TOML file at
//! `~/.config/samo/named_queries.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Named query store.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NamedQueries {
    #[serde(default)]
    queries: BTreeMap<String, String>,
}

impl NamedQueries {
    /// Load from the default file path, or return an empty store.
    pub fn load() -> Self {
        let Some(path) = Self::file_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save to the default file path.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::file_path() else {
            return Err("could not determine config directory".to_owned());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let content = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, content).map_err(|e| e.to_string())
    }

    fn file_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("samo").join("named_queries.toml"))
    }

    /// Save a named query. Overwrites if name already exists.
    pub fn set(&mut self, name: &str, query: &str) {
        self.queries.insert(name.to_owned(), query.to_owned());
    }

    /// Get a named query by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.queries.get(name).map(String::as_str)
    }

    /// Delete a named query. Returns true if it existed.
    pub fn delete(&mut self, name: &str) -> bool {
        self.queries.remove(name).is_some()
    }

    /// List all named queries.
    pub fn list(&self) -> &BTreeMap<String, String> {
        &self.queries
    }

    /// Return `true` if `name` is a valid named-query identifier.
    ///
    /// Valid names consist only of ASCII alphanumeric characters and
    /// underscores, and must be non-empty.
    pub fn is_valid_name(name: &str) -> bool {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// Substitute positional parameters (`$1`, `$2`, …) in a query.
    ///
    /// Parameters are replaced in order: `$1` → `args[0]`, `$2` → `args[1]`,
    /// and so on. Placeholders for which no argument was supplied are left
    /// unchanged.
    pub fn substitute(query: &str, args: &[&str]) -> String {
        let mut result = query.to_owned();
        for (i, arg) in args.iter().enumerate() {
            let placeholder = format!("${}", i + 1);
            result = result.replace(&placeholder, arg);
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get() {
        let mut nq = NamedQueries::default();
        nq.set("foo", "select 1");
        assert_eq!(nq.get("foo"), Some("select 1"));
    }

    #[test]
    fn test_get_missing() {
        let nq = NamedQueries::default();
        assert_eq!(nq.get("nonexistent"), None);
    }

    #[test]
    fn test_delete_existing() {
        let mut nq = NamedQueries::default();
        nq.set("foo", "select 1");
        assert!(nq.delete("foo"));
        assert_eq!(nq.get("foo"), None);
    }

    #[test]
    fn test_delete_missing() {
        let mut nq = NamedQueries::default();
        assert!(!nq.delete("nonexistent"));
    }

    #[test]
    fn test_list_empty() {
        let nq = NamedQueries::default();
        assert!(nq.list().is_empty());
    }

    #[test]
    fn test_list_populated() {
        let mut nq = NamedQueries::default();
        nq.set("alpha", "select 1");
        nq.set("beta", "select 2");
        let list = nq.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list.get("alpha").map(String::as_str), Some("select 1"));
        assert_eq!(list.get("beta").map(String::as_str), Some("select 2"));
    }

    #[test]
    fn test_substitute_no_args() {
        let result = NamedQueries::substitute("select * from t", &[]);
        assert_eq!(result, "select * from t");
    }

    #[test]
    fn test_substitute_single() {
        let result = NamedQueries::substitute("select * from t order by $1", &["id"]);
        assert_eq!(result, "select * from t order by id");
    }

    #[test]
    fn test_substitute_multiple() {
        let result =
            NamedQueries::substitute("select * from t order by $1 limit $2", &["name", "10"]);
        assert_eq!(result, "select * from t order by name limit 10");
    }

    #[test]
    fn test_substitute_missing_arg() {
        // $3 left as-is when only 2 args provided
        let result = NamedQueries::substitute("$1 $2 $3", &["a", "b"]);
        assert_eq!(result, "a b $3");
    }

    // -- is_valid_name -------------------------------------------------------

    #[test]
    fn test_valid_name_alphanumeric() {
        assert!(NamedQueries::is_valid_name("top_tables"));
        assert!(NamedQueries::is_valid_name("query1"));
        assert!(NamedQueries::is_valid_name("a"));
    }

    #[test]
    fn test_valid_name_empty_rejected() {
        assert!(!NamedQueries::is_valid_name(""));
    }

    #[test]
    fn test_valid_name_hyphen_rejected() {
        assert!(!NamedQueries::is_valid_name("my-query"));
    }

    #[test]
    fn test_valid_name_space_rejected() {
        assert!(!NamedQueries::is_valid_name("my query"));
    }

    #[test]
    fn test_valid_name_dot_rejected() {
        assert!(!NamedQueries::is_valid_name("my.query"));
    }

    // -- TOML round-trip -----------------------------------------------------

    #[test]
    fn test_toml_round_trip() {
        let mut nq = NamedQueries::default();
        nq.set(
            "active",
            "select * from pg_stat_activity where state = 'active'",
        );
        nq.set(
            "top_tables",
            "select * from pg_stat_user_tables order by $1 desc limit $2",
        );

        let serialized = toml::to_string_pretty(&nq).expect("serialization failed");
        let deserialized: NamedQueries =
            toml::from_str(&serialized).expect("deserialization failed");

        assert_eq!(
            deserialized.get("active"),
            Some("select * from pg_stat_activity where state = 'active'")
        );
        assert_eq!(
            deserialized.get("top_tables"),
            Some("select * from pg_stat_user_tables order by $1 desc limit $2")
        );
        assert_eq!(deserialized.list().len(), 2);
    }

    #[test]
    fn test_toml_empty_round_trip() {
        let nq = NamedQueries::default();
        let serialized = toml::to_string_pretty(&nq).expect("serialization failed");
        let deserialized: NamedQueries =
            toml::from_str(&serialized).expect("deserialization failed");
        assert!(deserialized.list().is_empty());
    }
}
