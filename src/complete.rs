//! Schema-aware tab completion for the Rpg REPL.
//!
//! Provides [`RpgHelper`], which implements rustyline's [`Helper`] trait,
//! and [`SchemaCache`], which holds `pg_catalog` metadata used during
//! completion.  The cache is loaded asynchronously via [`load_schema_cache`]
//! and shared through an `Arc<RwLock<SchemaCache>>` so the REPL can refresh
//! it without blocking completion.

use std::sync::{Arc, RwLock};

use rustyline::completion::{Completer, Pair};
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};

// ---------------------------------------------------------------------------
// Schema cache
// ---------------------------------------------------------------------------

/// A single table, view, materialised view, foreign table, or partition.
#[derive(Debug, Clone)]
pub struct TableInfo {
    /// Schema that owns the relation.
    pub schema: String,
    /// Relation name.
    pub name: String,
    /// `pg_class.relkind`: 'r' table, 'v' view, 'm' matview, 'f' foreign, 'p'
    /// partition root.
    #[allow(dead_code)]
    pub kind: char,
}

/// A single column within a relation.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    /// Schema that owns the relation.
    #[allow(dead_code)]
    pub schema: String,
    /// Relation name.
    pub table: String,
    /// Column name.
    pub name: String,
    /// Data type as returned by `format_type()`.
    #[allow(dead_code)]
    pub type_name: String,
    /// Ordinal position (1-based).
    #[allow(dead_code)]
    pub position: i16,
}

/// A single function or procedure.
#[derive(Debug, Clone)]
pub struct FuncInfo {
    /// Schema that owns the function.
    #[allow(dead_code)]
    pub schema: String,
    /// Function name.
    #[allow(dead_code)]
    pub name: String,
}

/// Cached `pg_catalog` metadata used for tab completion.
#[derive(Debug, Default)]
pub struct SchemaCache {
    /// All visible tables / views / matviews / foreign tables.
    pub tables: Vec<TableInfo>,
    /// All visible columns (non-system, non-dropped).
    pub columns: Vec<ColumnInfo>,
    /// All visible schemas.
    pub schemas: Vec<String>,
    /// All visible functions (excluding `pg_catalog` / `information_schema`).
    pub functions: Vec<FuncInfo>,
    /// All user-facing type names.
    #[allow(dead_code)]
    pub types: Vec<String>,
    /// All connectable databases.
    pub databases: Vec<String>,
    /// GUC parameter names from `pg_settings`.
    pub guc_params: Vec<String>,
}

// ---------------------------------------------------------------------------
// Cache loader
// ---------------------------------------------------------------------------

/// Extract a single string column from a [`tokio_postgres::SimpleQueryMessage`]
/// row at the given column index, returning an empty string if absent.
fn col(row: &tokio_postgres::SimpleQueryRow, idx: usize) -> String {
    row.get(idx).unwrap_or("").to_owned()
}

/// Load a fresh [`SchemaCache`] from the database.
///
/// Uses the simple-query protocol so no prepared statements are needed.
/// All errors are swallowed — an empty (but valid) cache is returned on
/// failure so completion degrades gracefully.
///
/// # Errors
///
/// Returns a `tokio_postgres::Error` if any query fails.
pub async fn load_schema_cache(
    client: &tokio_postgres::Client,
) -> Result<SchemaCache, tokio_postgres::Error> {
    let mut cache = SchemaCache::default();

    // ------------------------------------------------------------------
    // Tables / views / matviews / foreign tables / partition roots
    // ------------------------------------------------------------------
    let tables_sql = "\
        select n.nspname, c.relname, c.relkind::text \
        from pg_catalog.pg_class c \
        join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
        where c.relkind in ('r','v','m','f','p') \
          and n.nspname not in \
              ('pg_catalog', 'information_schema', 'pg_toast') \
        order by 1, 2";

    for msg in client.simple_query(tables_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let kind = col(&row, 2).chars().next().unwrap_or('r');
            cache.tables.push(TableInfo {
                schema: col(&row, 0),
                name: col(&row, 1),
                kind,
            });
        }
    }

    // ------------------------------------------------------------------
    // Columns
    // ------------------------------------------------------------------
    let columns_sql = "\
        select n.nspname, c.relname, a.attname, \
               pg_catalog.format_type(a.atttypid, a.atttypmod), \
               a.attnum \
        from pg_catalog.pg_attribute a \
        join pg_catalog.pg_class c on a.attrelid = c.oid \
        join pg_catalog.pg_namespace n on c.relnamespace = n.oid \
        where a.attnum > 0 \
          and not a.attisdropped \
          and c.relkind in ('r','v','m','f','p') \
          and n.nspname not in \
              ('pg_catalog', 'information_schema', 'pg_toast') \
        order by 1, 2, 5";

    for msg in client.simple_query(columns_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            let pos_str = col(&row, 4);
            let position = pos_str.parse::<i16>().unwrap_or(0);
            cache.columns.push(ColumnInfo {
                schema: col(&row, 0),
                table: col(&row, 1),
                name: col(&row, 2),
                type_name: col(&row, 3),
                position,
            });
        }
    }

    // ------------------------------------------------------------------
    // Schemas
    // ------------------------------------------------------------------
    let schemas_sql = "\
        select nspname \
        from pg_catalog.pg_namespace \
        where nspname not like 'pg_toast%' \
          and nspname not like 'pg_temp%' \
        order by 1";

    for msg in client.simple_query(schemas_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            cache.schemas.push(col(&row, 0));
        }
    }

    // ------------------------------------------------------------------
    // Functions
    // ------------------------------------------------------------------
    let functions_sql = "\
        select n.nspname, p.proname \
        from pg_catalog.pg_proc p \
        join pg_catalog.pg_namespace n on p.pronamespace = n.oid \
        where n.nspname not in ('pg_catalog', 'information_schema') \
        order by 1, 2";

    for msg in client.simple_query(functions_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            cache.functions.push(FuncInfo {
                schema: col(&row, 0),
                name: col(&row, 1),
            });
        }
    }

    // ------------------------------------------------------------------
    // Databases
    // ------------------------------------------------------------------
    let databases_sql = "select datname from pg_catalog.pg_database where datallowconn order by 1";

    for msg in client.simple_query(databases_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            cache.databases.push(col(&row, 0));
        }
    }

    // ------------------------------------------------------------------
    // GUC params
    // ------------------------------------------------------------------
    let guc_sql = "select name from pg_catalog.pg_settings order by 1";

    for msg in client.simple_query(guc_sql).await? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            cache.guc_params.push(col(&row, 0));
        }
    }

    Ok(cache)
}

// ---------------------------------------------------------------------------
// SQL keyword list
// ---------------------------------------------------------------------------

/// Common SQL / PL/pgSQL keywords offered as fallback completions.
const SQL_KEYWORDS: &[&str] = &[
    // DDL / DML statements
    "ABORT",
    "ALTER",
    "ANALYZE",
    "BEGIN",
    "CALL",
    "CHECKPOINT",
    "CLOSE",
    "CLUSTER",
    "COMMENT",
    "COMMIT",
    "COPY",
    "CREATE",
    "DEALLOCATE",
    "DECLARE",
    "DELETE",
    "DISCARD",
    "DO",
    "DROP",
    "END",
    "EXECUTE",
    "EXPLAIN",
    "FETCH",
    "GRANT",
    "IMPORT",
    "INSERT",
    "LISTEN",
    "LOAD",
    "LOCK",
    "MOVE",
    "NOTIFY",
    "PREPARE",
    "REASSIGN",
    "REFRESH",
    "REINDEX",
    "RELEASE",
    "RESET",
    "REVOKE",
    "ROLLBACK",
    "SAVEPOINT",
    "SECURITY",
    "SELECT",
    "SET",
    "SHOW",
    "START",
    "TABLE",
    "TRUNCATE",
    "UNLISTEN",
    "UPDATE",
    "VACUUM",
    "VALUES",
    "WITH",
    // Clauses / operators
    "ALL",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BETWEEN",
    "BY",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "CONSTRAINT",
    "CROSS",
    "CURRENT",
    "DEFAULT",
    "DEFERRABLE",
    "DESC",
    "DISTINCT",
    "ELSE",
    "EXCEPT",
    "EXISTS",
    "FALSE",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IN",
    "INDEX",
    "INNER",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PRIMARY",
    "REFERENCES",
    "RETURNING",
    "RIGHT",
    "SCHEMA",
    "SIMILAR",
    "SOME",
    "THEN",
    "TO",
    "TRUE",
    "UNION",
    "UNIQUE",
    "USING",
    "WHEN",
    "WHERE",
    "WINDOW",
    // Types
    "BIGINT",
    "BOOLEAN",
    "CHAR",
    "CHARACTER",
    "DATE",
    "DECIMAL",
    "DOUBLE",
    "FLOAT",
    "INTEGER",
    "INTERVAL",
    "JSON",
    "JSONB",
    "NUMERIC",
    "REAL",
    "SERIAL",
    "SMALLINT",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "UUID",
    "VARCHAR",
    "XML",
];

// ---------------------------------------------------------------------------
// Backslash commands
// ---------------------------------------------------------------------------

/// All recognised backslash commands (the part after `\`).
const BACKSLASH_CMDS: &[&str] = &[
    "?",
    "!",
    "a",
    "b",
    "bind",
    "c",
    "C",
    "cd",
    "connect",
    "conninfo",
    "copy",
    "copyright",
    "crosstabview",
    "d",
    "da",
    "db",
    "dc",
    "dC",
    "dd",
    "dD",
    "dE",
    "df",
    "dF",
    "dg",
    "di",
    "dl",
    "dL",
    "dm",
    "dn",
    "do",
    "dp",
    "dP",
    "drds",
    "dRp",
    "dRs",
    "ds",
    "dS",
    "dt",
    "dT",
    "du",
    "dv",
    "dx",
    "dy",
    "e",
    "echo",
    "ef",
    "elif",
    "else",
    "encoding",
    "endif",
    "errverbose",
    "ev",
    "f",
    "g",
    "gdesc",
    "gexec",
    "gset",
    "gx",
    "H",
    "help",
    "i",
    "if",
    "ir",
    "l",
    "L",
    "list",
    "lo_export",
    "lo_import",
    "lo_list",
    "lo_unlink",
    "o",
    "p",
    "parse",
    "password",
    "pset",
    "q",
    "qecho",
    "quit",
    "r",
    "reset",
    "s",
    "set",
    "setenv",
    "sf",
    "sv",
    "t",
    "T",
    "timing",
    "unset",
    "w",
    "warn",
    "watch",
    "x",
    "z",
];

// ---------------------------------------------------------------------------
// Completion context
// ---------------------------------------------------------------------------

/// Describes what category of identifier should be offered at the cursor.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CompletionContext {
    /// Default: offer SQL keywords.
    Keyword,
    /// After FROM / JOIN / INTO / UPDATE / TABLE / `\d` / ALTER TABLE /
    /// DROP TABLE / INSERT INTO / CREATE INDEX ON.
    TableName,
    /// After SELECT / WHERE / ON (in JOIN) / HAVING / ORDER BY / GROUP BY /
    /// UPDATE...SET, or after a `table.` prefix.
    ColumnName {
        /// Table names extracted from the FROM / UPDATE clause, if any.
        tables: Vec<String>,
    },
    /// After `schema_name.` — complete objects in that schema.
    SchemaObject {
        /// The schema name that precedes the dot.
        schema: String,
    },
    /// After `\c` / `\connect`.
    DatabaseName,
    /// After SET / RESET / SHOW (top-level GUC, not UPDATE...SET).
    GucParam,
    /// After a lone `\` — complete the command name.
    BackslashCmd,
    /// After `\i` / `\ir` / `\copy ... FROM` — complete file names.
    FileName,
}

// ---------------------------------------------------------------------------
// Tokeniser
// ---------------------------------------------------------------------------

/// Split `text` into whitespace-separated tokens, all uppercased.
fn tokens_upper(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_uppercase).collect()
}

/// Return the portion of `line[..pos]` that is the current incomplete word
/// (everything after the last whitespace / dot boundary), together with the
/// byte offset where that word starts.
///
/// A dot is **not** treated as a word boundary here — callers that need
/// schema-qualified handling look for `identifier.` themselves.
pub fn find_word_start(line: &str, pos: usize) -> (usize, String) {
    let before = &line[..pos];
    let start = before
        .rfind(|c: char| c.is_whitespace())
        .map_or(0, |i| i + 1);
    (start, before[start..].to_owned())
}

// ---------------------------------------------------------------------------
// Context detector
// ---------------------------------------------------------------------------

/// Extract bare table names referenced in the FROM clause of `sql_up_to_cursor`
/// (best-effort; handles simple cases).
fn extract_from_tables(sql_upper: &str) -> Vec<String> {
    let mut tables = Vec::new();
    // Find the token immediately after FROM and after each JOIN keyword.
    let toks: Vec<&str> = sql_upper.split_whitespace().collect();
    for (i, tok) in toks.iter().enumerate() {
        if matches!(*tok, "FROM" | "JOIN" | "UPDATE" | "INTO") {
            if let Some(next) = toks.get(i + 1) {
                // Strip schema qualification, aliases, trailing commas.
                let name = next.trim_end_matches(',');
                let bare = if let Some((_, after)) = name.split_once('.') {
                    after
                } else {
                    name
                };
                if !bare.is_empty() && bare.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    tables.push(bare.to_lowercase());
                }
            }
        }
    }
    tables
}

/// Return `true` when `toks` contains `UPDATE` before any intervening FROM or
/// JOIN or SELECT, scanning backward from `idx`.  Used to distinguish
/// `UPDATE t SET col` (`ColumnName`) from bare `SET guc` (`GucParam`).
fn preceded_by_update(toks: &[String], idx: usize) -> bool {
    for tok in toks[..idx].iter().rev() {
        match tok.as_str() {
            "UPDATE" => return true,
            // A SELECT/INSERT/DELETE/WITH resets scope.
            "SELECT" | "INSERT" | "DELETE" | "WITH" => return false,
            _ => {}
        }
    }
    false
}

/// Extract the table name from `UPDATE tablename SET ...` (best-effort).
fn extract_update_table(toks: &[String]) -> Option<String> {
    for (i, tok) in toks.iter().enumerate() {
        if tok == "UPDATE" {
            if let Some(next) = toks.get(i + 1) {
                let name = next.trim_end_matches(',');
                let bare = if let Some((_, after)) = name.split_once('.') {
                    after
                } else {
                    name
                };
                if !bare.is_empty() && bare.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return Some(bare.to_lowercase());
                }
            }
        }
    }
    None
}

/// Decide what kind of completion is appropriate given the text before the
/// cursor.
///
/// Context detection is token-based (no full SQL parse).  The algorithm walks
/// the token list backward to find the most recent "context keyword" and uses
/// one look-back token to resolve ambiguous multi-word patterns:
///
/// | Pattern before cursor | Context |
/// |---|---|
/// | `SELECT` / `WHERE` / `HAVING` / `ON` (JOIN) | ColumnName |
/// | `ORDER BY` / `GROUP BY` | ColumnName |
/// | `UPDATE t SET` | ColumnName (from `t`) |
/// | `FROM` / `JOIN` / `INTO` / `TABLE` / `UPDATE` | TableName |
/// | `ALTER TABLE` / `DROP TABLE` | TableName |
/// | `INSERT INTO` | TableName |
/// | `CREATE INDEX ON` | TableName |
/// | `SET` / `RESET` / `SHOW` (top-level) | GucParam |
/// | `\c` / `\connect` | DatabaseName |
/// | `\d` / `\dt` / `\dv` / `\di` / `\dm` / `\dE` | TableName |
/// | `\i` / `\ir` | FileName |
/// | `\` (partial command) | BackslashCmd |
/// | otherwise | Keyword |
#[allow(clippy::too_many_lines)]
pub fn detect_context(line: &str, pos: usize) -> CompletionContext {
    let before = &line[..pos];

    // -----------------------------------------------------------------------
    // Backslash handling
    // -----------------------------------------------------------------------

    // `\` at very start (possibly followed by partial command name).
    if before.trim_start().starts_with('\\') {
        let after_slash = before.trim_start().trim_start_matches('\\');
        // Check for \c / \connect → DatabaseName
        if after_slash.starts_with("c ")
            || after_slash.starts_with("connect ")
            || after_slash == "c"
            || after_slash == "connect"
        {
            return CompletionContext::DatabaseName;
        }
        // \d / \dt / \dv / \di / \dm / \ds / \dE / \dS / \dT / \du / \dg /
        // \dn / \df / \da / \db / \dp / \dF / \dC / \dD → TableName
        if after_slash.starts_with('d') {
            let rest = after_slash.trim_start_matches(|c: char| c.is_alphanumeric());
            // rest should be empty (still typing) or start with a space (arg).
            if rest.is_empty() || rest.starts_with(' ') {
                return CompletionContext::TableName;
            }
        }
        // \i / \ir → FileName
        if after_slash.starts_with("ir ") || after_slash.starts_with("i ") {
            return CompletionContext::FileName;
        }
        // Still typing the command letter itself.
        return CompletionContext::BackslashCmd;
    }

    // -----------------------------------------------------------------------
    // schema_name. prefix
    // -----------------------------------------------------------------------

    // If the current word contains a dot, the part before the dot might be a
    // schema name.
    let word_start_idx = before
        .rfind(|c: char| c.is_whitespace())
        .map_or(0, |i| i + 1);
    let current_word = &before[word_start_idx..];

    if let Some(dot_pos) = current_word.rfind('.') {
        let candidate_schema = &current_word[..dot_pos];
        // Return SchemaObject if candidate_schema looks like a plain identifier.
        if !candidate_schema.is_empty()
            && candidate_schema
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
        {
            return CompletionContext::SchemaObject {
                schema: candidate_schema.to_lowercase(),
            };
        }
    }

    // -----------------------------------------------------------------------
    // SQL keyword context
    // -----------------------------------------------------------------------

    let upper = before.to_uppercase();
    let toks = tokens_upper(before);

    // Walk backward with explicit index so we can peek at earlier tokens for
    // multi-word patterns like "CREATE INDEX ON" and "UPDATE t SET".
    let mut kw_idx: Option<usize> = None;
    let mut last_kw_upper: Option<&str> = None;

    for i in (0..toks.len()).rev() {
        match toks[i].trim_end_matches(',') {
            // ---------------------------------------------------------------
            // Table-name triggers
            // ---------------------------------------------------------------
            "FROM" | "JOIN" | "INNER" | "LEFT" | "RIGHT" | "FULL" | "CROSS" | "OUTER"
            | "LATERAL" | "INTO" | "UPDATE" => {
                kw_idx = Some(i);
                last_kw_upper = Some("FROM");
                break;
            }
            // TABLE alone triggers TableName only when preceded by ALTER, DROP,
            // or as a standalone `TABLE tablename` shorthand.
            "TABLE" => {
                kw_idx = Some(i);
                last_kw_upper = Some("FROM"); // TABLE → suggest table names
                break;
            }
            // ON is ambiguous: it's ColumnName in JOIN conditions but TableName
            // in "CREATE INDEX ON".  Peek at earlier tokens.
            "ON" => {
                // Check whether this ON follows "INDEX" (possibly with an index
                // name between them): CREATE [UNIQUE] INDEX [name] ON.
                let is_index_on = toks[..i]
                    .iter()
                    .rev()
                    .any(|t| t == "INDEX" || t == "REINDEX");
                if is_index_on {
                    kw_idx = Some(i);
                    last_kw_upper = Some("FROM"); // CREATE INDEX ON → TableName
                } else {
                    kw_idx = Some(i);
                    last_kw_upper = Some("SELECT"); // JOIN ... ON → ColumnName
                }
                break;
            }
            // ---------------------------------------------------------------
            // Column-name triggers
            // ---------------------------------------------------------------
            "SELECT" | "WHERE" | "HAVING" | "BY" => {
                kw_idx = Some(i);
                last_kw_upper = Some("SELECT");
                break;
            }
            // SET is a column trigger in UPDATE context, otherwise GUC.
            "SET" => {
                if preceded_by_update(&toks, i) {
                    kw_idx = Some(i);
                    last_kw_upper = Some("SET_UPDATE"); // UPDATE...SET → ColumnName
                } else {
                    kw_idx = Some(i);
                    last_kw_upper = Some("SET"); // bare SET → GucParam
                }
                break;
            }
            // ---------------------------------------------------------------
            // GUC triggers
            // ---------------------------------------------------------------
            "RESET" | "SHOW" => {
                kw_idx = Some(i);
                last_kw_upper = Some("SET");
                break;
            }
            _ => {}
        }
    }

    let _ = kw_idx; // stored for potential future use (e.g. range-based lookups)

    match last_kw_upper {
        Some("FROM") => CompletionContext::TableName,
        Some("SELECT") => {
            let tables = extract_from_tables(&upper);
            CompletionContext::ColumnName { tables }
        }
        Some("SET_UPDATE") => {
            // Columns from the UPDATE target table.
            let tables = extract_update_table(&toks)
                .map(|t| vec![t])
                .unwrap_or_default();
            CompletionContext::ColumnName { tables }
        }
        Some("SET") => CompletionContext::GucParam,
        _ => CompletionContext::Keyword,
    }
}

// ---------------------------------------------------------------------------
// Fuzzy matcher
// ---------------------------------------------------------------------------

/// Try to match `input` as a subsequence of `candidate` (case-insensitive).
///
/// Returns `None` when `input` is not a subsequence of `candidate`, or
/// `Some(score)` otherwise.  Higher scores indicate better matches:
/// - +100 for an exact match
/// - +50 for an exact prefix match
/// - +5 per consecutive matched character
/// - +2 per matched character
/// - -1 per skipped character in the candidate
pub fn fuzzy_match(input: &str, candidate: &str) -> Option<i32> {
    if input.is_empty() {
        return Some(0);
    }

    let input_lower = input.to_lowercase();
    let cand_lower = candidate.to_lowercase();

    // Fast paths.
    if cand_lower == input_lower {
        return Some(100);
    }
    if cand_lower.starts_with(&input_lower) {
        return Some(50);
    }

    // Subsequence match.
    let input_chars: Vec<char> = input_lower.chars().collect();
    let cand_chars: Vec<char> = cand_lower.chars().collect();

    let mut score: i32 = 0;
    let mut inp_idx = 0;
    let mut prev_matched = false;

    for ch in &cand_chars {
        if inp_idx < input_chars.len() && *ch == input_chars[inp_idx] {
            score += 2;
            if prev_matched {
                score += 5; // consecutive bonus
            }
            prev_matched = true;
            inp_idx += 1;
        } else {
            score -= 1; // gap penalty
            prev_matched = false;
        }
    }

    if inp_idx == input_chars.len() {
        Some(score)
    } else {
        None // not all input chars matched
    }
}

// ---------------------------------------------------------------------------
// RpgHelper
// ---------------------------------------------------------------------------

/// rustyline [`Helper`] implementation for Rpg.
///
/// Wraps an `Arc<RwLock<SchemaCache>>` so the cache can be refreshed from
/// the async REPL without blocking readline.
pub struct RpgHelper {
    cache: Arc<RwLock<SchemaCache>>,
    /// Whether syntax highlighting is active.
    highlight: bool,
    /// Whether schema-aware tab completion is active.
    ///
    /// When `false`, `complete()` returns no candidates (toggled by F2).
    completion_enabled: bool,
}

impl RpgHelper {
    /// Create a new helper backed by the given cache.
    ///
    /// `highlight` enables ANSI syntax highlighting.  Pass `false` when
    /// stdout is not a terminal or `$TERM` is `dumb`.
    pub fn new(cache: Arc<RwLock<SchemaCache>>, highlight: bool) -> Self {
        Self {
            cache,
            highlight,
            completion_enabled: true,
        }
    }

    /// Return `true` when syntax highlighting is enabled.
    fn highlight_enabled(&self) -> bool {
        self.highlight
    }

    /// Enable or disable syntax highlighting at runtime.
    pub fn set_highlight(&mut self, enabled: bool) {
        self.highlight = enabled;
    }

    /// Enable or disable schema-aware tab completion at runtime.
    pub fn set_completion(&mut self, enabled: bool) {
        self.completion_enabled = enabled;
    }
}

impl Completer for RpgHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        if !self.completion_enabled {
            return Ok((pos, vec![]));
        }
        let context = detect_context(line, pos);
        let (start, prefix) = find_word_start(line, pos);

        // For schema-qualified or dot-preceded words, the replacement should
        // start after the dot, not at the beginning of the whole word.
        let (completion_start, completion_prefix) =
            if let CompletionContext::SchemaObject { ref schema } = context {
                // "public.us" → start after the dot
                let dot_offset = start + schema.len() + 1;
                (dot_offset, prefix[schema.len() + 1..].to_owned())
            } else {
                (start, prefix.clone())
            };

        // Acquire a read lock on the cache.  If the lock is poisoned we just
        // return no completions rather than panicking.
        let Ok(cache) = self.cache.read() else {
            return Ok((completion_start, vec![]));
        };

        let mut candidates: Vec<(String, i32)> = match context {
            CompletionContext::TableName => cache
                .tables
                .iter()
                .map(|t| &t.name)
                .chain(cache.schemas.iter())
                .filter_map(|name| fuzzy_match(&completion_prefix, name).map(|s| (name.clone(), s)))
                .collect(),

            CompletionContext::ColumnName { ref tables } => cache
                .columns
                .iter()
                .filter(|c| tables.is_empty() || tables.contains(&c.table))
                .filter_map(|c| {
                    fuzzy_match(&completion_prefix, &c.name).map(|s| (c.name.clone(), s))
                })
                .collect(),

            CompletionContext::SchemaObject { ref schema } => cache
                .tables
                .iter()
                .filter(|t| t.schema == *schema)
                .filter_map(|t| {
                    fuzzy_match(&completion_prefix, &t.name).map(|s| (t.name.clone(), s))
                })
                .collect(),

            CompletionContext::GucParam => cache
                .guc_params
                .iter()
                .filter_map(|g| fuzzy_match(&completion_prefix, g).map(|s| (g.clone(), s)))
                .collect(),

            CompletionContext::DatabaseName => cache
                .databases
                .iter()
                .filter_map(|d| fuzzy_match(&completion_prefix, d).map(|s| (d.clone(), s)))
                .collect(),

            CompletionContext::BackslashCmd => BACKSLASH_CMDS
                .iter()
                .filter_map(|cmd| {
                    fuzzy_match(&completion_prefix, cmd).map(|s| ((*cmd).to_owned(), s))
                })
                .collect(),

            // FileName: delegate to the OS (no DB lookup needed).  For now we
            // return nothing and let the user type the path manually.
            // A future PR can plug in a filesystem completer here.
            CompletionContext::FileName => vec![],

            CompletionContext::Keyword => SQL_KEYWORDS
                .iter()
                .filter_map(|kw| {
                    let kw_lower = kw.to_lowercase();
                    fuzzy_match(&completion_prefix, &kw_lower).map(|s| (kw_lower, s))
                })
                .collect(),
        };

        // Sort by score descending, then alphabetically for tie-breaking.
        candidates.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        candidates.dedup_by(|a, b| a.0 == b.0);

        let pairs = candidates
            .into_iter()
            .map(|(name, _)| Pair {
                display: name.clone(),
                replacement: name,
            })
            .collect();

        Ok((completion_start, pairs))
    }
}

impl Hinter for RpgHelper {
    type Hint = String;

    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Validator for RpgHelper {}

impl Highlighter for RpgHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> std::borrow::Cow<'l, str> {
        if self.highlight_enabled() {
            // Build a set of known schema object names (tables + columns) for
            // schema-aware identifier colouring.  We do this inline on each
            // keystroke so it stays fresh after a cache reload; the lock is
            // held only for the duration of the set construction.
            let schema_names: Option<std::collections::HashSet<String>> =
                self.cache.read().ok().map(|cache| {
                    cache
                        .tables
                        .iter()
                        .map(|t| t.name.to_lowercase())
                        .chain(cache.columns.iter().map(|c| c.name.to_lowercase()))
                        .collect()
                });
            crate::highlight::highlight_sql(line, schema_names.as_ref())
        } else {
            std::borrow::Cow::Borrowed(line)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        // Return true to trigger re-highlighting on every keystroke.
        self.highlight_enabled()
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> std::borrow::Cow<'b, str> {
        std::borrow::Cow::Borrowed(prompt)
    }
}

impl Helper for RpgHelper {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // detect_context tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_context_from_keyword() {
        let line = "SELECT * FROM ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_join_keyword() {
        let line = "SELECT * FROM foo JOIN ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_update_keyword() {
        let line = "UPDATE ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_select_no_from() {
        // SELECT with no FROM yet → ColumnName with empty tables list.
        let line = "SELECT ";
        let ctx = detect_context(line, line.len());
        assert!(matches!(ctx, CompletionContext::ColumnName { ref tables } if tables.is_empty()));
    }

    #[test]
    fn test_detect_context_where_clause() {
        let line = "SELECT * FROM users WHERE ";
        let ctx = detect_context(line, line.len());
        // WHERE falls under SELECT context → ColumnName.
        assert!(matches!(ctx, CompletionContext::ColumnName { .. }));
    }

    #[test]
    fn test_detect_context_set_keyword() {
        let line = "SET ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::GucParam);
    }

    #[test]
    fn test_detect_context_reset_keyword() {
        let line = "RESET ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::GucParam);
    }

    #[test]
    fn test_detect_context_show_keyword() {
        let line = "SHOW ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::GucParam);
    }

    #[test]
    fn test_detect_context_backslash_d() {
        let line = r"\d ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_backslash_connect() {
        let line = r"\c ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::DatabaseName);
    }

    #[test]
    fn test_detect_context_backslash_connect_long() {
        let line = r"\connect ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::DatabaseName);
    }

    #[test]
    fn test_detect_context_backslash_i() {
        let line = r"\i ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::FileName);
    }

    #[test]
    fn test_detect_context_backslash_ir() {
        let line = r"\ir ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::FileName);
    }

    #[test]
    fn test_detect_context_bare_backslash() {
        // Just `\` — the user is typing a command name.
        let line = "\\";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::BackslashCmd);
    }

    #[test]
    fn test_detect_context_schema_qualified() {
        let line = "SELECT * FROM public.";
        let ctx = detect_context(line, line.len());
        assert_eq!(
            ctx,
            CompletionContext::SchemaObject {
                schema: "public".to_owned()
            }
        );
    }

    #[test]
    fn test_detect_context_keyword_default() {
        // Plain start of line.
        let line = "SE";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::Keyword);
    }

    // -----------------------------------------------------------------------
    // find_word_start tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_word_start_middle() {
        let line = "SELECT * FROM users";
        let pos = line.len();
        let (start, word) = find_word_start(line, pos);
        assert_eq!(start, 14);
        assert_eq!(word, "users");
    }

    #[test]
    fn test_find_word_start_at_space() {
        let line = "SELECT * FROM ";
        let pos = line.len();
        let (start, word) = find_word_start(line, pos);
        assert_eq!(start, pos);
        assert_eq!(word, "");
    }

    #[test]
    fn test_find_word_start_beginning() {
        let line = "SELECT";
        let pos = line.len();
        let (start, word) = find_word_start(line, pos);
        assert_eq!(start, 0);
        assert_eq!(word, "SELECT");
    }

    #[test]
    fn test_find_word_start_mid_cursor() {
        let line = "SELECT * FROM users";
        let pos = 8; // cursor after "SELECT *"
        let (start, word) = find_word_start(line, pos);
        assert_eq!(start, 7);
        assert_eq!(word, "*");
    }

    // -----------------------------------------------------------------------
    // fuzzy_match tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_fuzzy_match_exact() {
        let score = fuzzy_match("select", "select");
        assert_eq!(score, Some(100));
    }

    #[test]
    fn test_fuzzy_match_prefix() {
        let score = fuzzy_match("sel", "select");
        assert!(score.is_some());
        // Prefix match should score >= 50.
        assert!(score.unwrap() >= 50);
    }

    #[test]
    fn test_fuzzy_match_subsequence() {
        // "djm" matches "django_migrations"
        let score = fuzzy_match("djm", "django_migrations");
        assert!(score.is_some());
    }

    #[test]
    fn test_fuzzy_match_case_insensitive() {
        let score = fuzzy_match("SEL", "select");
        assert!(score.is_some());
    }

    #[test]
    fn test_fuzzy_match_no_match() {
        let score = fuzzy_match("xyz", "select");
        assert_eq!(score, None);
    }

    #[test]
    fn test_fuzzy_match_empty_input() {
        // Empty input matches everything with score 0.
        assert_eq!(fuzzy_match("", "select"), Some(0));
        assert_eq!(fuzzy_match("", ""), Some(0));
    }

    #[test]
    fn test_fuzzy_match_prefix_beats_subsequence() {
        // A prefix match should score higher than a scattered subsequence
        // match.
        let prefix_score = fuzzy_match("sel", "select").unwrap();
        let subseq_score = fuzzy_match("sel", "server_elog").unwrap_or(i32::MIN);
        assert!(prefix_score >= subseq_score);
    }

    // -----------------------------------------------------------------------
    // RpgHelper trait bounds
    // -----------------------------------------------------------------------

    #[test]
    fn test_rpg_helper_implements_helper() {
        // Compile-time check: RpgHelper must implement Helper.
        fn assert_helper<T: Helper>(_: &T) {}
        let cache = Arc::new(RwLock::new(SchemaCache::default()));
        let helper = RpgHelper::new(cache, false);
        assert_helper(&helper);
    }

    // -----------------------------------------------------------------------
    // extract_from_tables
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_from_tables_simple() {
        let tables = extract_from_tables("SELECT * FROM USERS WHERE ID = 1");
        assert_eq!(tables, vec!["users"]);
    }

    #[test]
    fn test_extract_from_tables_join() {
        let tables =
            extract_from_tables("SELECT * FROM ORDERS JOIN USERS ON ORDERS.USER_ID = USERS.ID");
        assert!(tables.contains(&"orders".to_owned()));
        assert!(tables.contains(&"users".to_owned()));
    }

    #[test]
    fn test_extract_from_tables_schema_qualified() {
        let tables = extract_from_tables("SELECT * FROM PUBLIC.ORDERS");
        assert_eq!(tables, vec!["orders"]);
    }

    // -----------------------------------------------------------------------
    // RpgHelper::complete integration smoke tests (no DB required)
    // -----------------------------------------------------------------------

    #[test]
    fn test_complete_keywords_from_empty() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(SchemaCache::default()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let (start, candidates) = helper.complete("SE", 2, &ctx).unwrap();
        assert_eq!(start, 0);
        // "select", "set", "security", "serial", "sequence"... should appear.
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"select"), "expected 'select' in {names:?}");
    }

    #[test]
    fn test_complete_table_names_from_cache() {
        use rustyline::history::DefaultHistory;

        let mut cache = SchemaCache::default();
        cache.tables.push(TableInfo {
            schema: "public".to_owned(),
            name: "users".to_owned(),
            kind: 'r',
        });
        cache.tables.push(TableInfo {
            schema: "public".to_owned(),
            name: "orders".to_owned(),
            kind: 'r',
        });

        let cache = Arc::new(RwLock::new(cache));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT * FROM ";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        assert_eq!(start, line.len());
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
        assert!(names.contains(&"orders"), "expected 'orders' in {names:?}");
    }

    #[test]
    fn test_complete_schema_object() {
        use rustyline::history::DefaultHistory;

        let mut cache = SchemaCache::default();
        cache.tables.push(TableInfo {
            schema: "public".to_owned(),
            name: "users".to_owned(),
            kind: 'r',
        });

        let cache = Arc::new(RwLock::new(cache));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT * FROM public.us";
        let (start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        // Start should be after the dot.
        let dot_pos = line.find('.').unwrap() + 1;
        assert_eq!(start, dot_pos);
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
    }

    // -----------------------------------------------------------------------
    // New context scenarios — pgcli-style
    // -----------------------------------------------------------------------

    // --- INSERT INTO → TableName ---

    #[test]
    fn test_detect_context_insert_into() {
        let line = "INSERT INTO ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_insert_into_lowercase() {
        let line = "insert into ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // --- ALTER TABLE / DROP TABLE → TableName ---

    #[test]
    fn test_detect_context_alter_table() {
        let line = "ALTER TABLE ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_drop_table() {
        let line = "DROP TABLE ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_drop_table_lowercase() {
        let line = "drop table ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // --- CREATE INDEX ON → TableName ---

    #[test]
    fn test_detect_context_create_index_on() {
        let line = "CREATE INDEX ON ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_create_index_named_on() {
        // CREATE INDEX idx_name ON → should still be TableName
        let line = "CREATE INDEX idx_users_email ON ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_create_unique_index_on() {
        let line = "CREATE UNIQUE INDEX ON ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_create_index_lowercase() {
        let line = "create index on ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // --- JOIN ... ON → ColumnName (not TableName) ---

    #[test]
    fn test_detect_context_join_on_is_column() {
        // "JOIN foo ON" → should be ColumnName (join condition), not TableName
        let line = "SELECT * FROM orders JOIN users ON ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName, got {ctx:?}"
        );
    }

    // --- UPDATE ... SET → ColumnName (from UPDATE target) ---

    #[test]
    fn test_detect_context_update_set_is_column() {
        let line = "UPDATE users SET ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName after UPDATE...SET, got {ctx:?}"
        );
    }

    #[test]
    fn test_detect_context_update_set_carries_table() {
        // The ColumnName context should include the UPDATE target table.
        let line = "UPDATE users SET ";
        let ctx = detect_context(line, line.len());
        match ctx {
            CompletionContext::ColumnName { tables } => {
                assert!(
                    tables.contains(&"users".to_owned()),
                    "expected 'users' in tables, got {tables:?}"
                );
            }
            other => panic!("expected ColumnName, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_context_update_set_lowercase() {
        let line = "update orders set ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName after update...set, got {ctx:?}"
        );
    }

    // --- Bare SET / RESET / SHOW → GucParam (not ColumnName) ---

    #[test]
    fn test_detect_context_bare_set_is_guc() {
        // Top-level SET without UPDATE → GUC parameter.
        let line = "SET ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::GucParam);
    }

    // --- ORDER BY / GROUP BY → ColumnName ---

    #[test]
    fn test_detect_context_order_by() {
        let line = "SELECT id FROM users ORDER BY ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName after ORDER BY, got {ctx:?}"
        );
    }

    #[test]
    fn test_detect_context_group_by() {
        let line = "SELECT status, count(*) FROM orders GROUP BY ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName after GROUP BY, got {ctx:?}"
        );
    }

    #[test]
    fn test_detect_context_order_by_carries_from_tables() {
        // Tables from FROM clause should be available in ORDER BY context.
        let line = "SELECT id FROM users ORDER BY ";
        let ctx = detect_context(line, line.len());
        match ctx {
            CompletionContext::ColumnName { tables } => {
                assert!(
                    tables.contains(&"users".to_owned()),
                    "expected 'users' in tables, got {tables:?}"
                );
            }
            other => panic!("expected ColumnName, got {other:?}"),
        }
    }

    // --- \dt / \dv / \di → TableName ---

    #[test]
    fn test_detect_context_backslash_dt() {
        let line = r"\dt ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_backslash_dv() {
        let line = r"\dv ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_backslash_di() {
        let line = r"\di ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    #[test]
    fn test_detect_context_backslash_dm() {
        let line = r"\dm ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // --- Case insensitivity ---

    #[test]
    fn test_detect_context_mixed_case_select() {
        let line = "Select * From users Where ";
        let ctx = detect_context(line, line.len());
        assert!(
            matches!(ctx, CompletionContext::ColumnName { .. }),
            "expected ColumnName (mixed case), got {ctx:?}"
        );
    }

    #[test]
    fn test_detect_context_mixed_case_from() {
        let line = "select * From ";
        let ctx = detect_context(line, line.len());
        assert_eq!(ctx, CompletionContext::TableName);
    }

    // --- Schema-qualified table in INSERT INTO ---

    #[test]
    fn test_detect_context_schema_qualified_insert() {
        let line = "INSERT INTO public.";
        let ctx = detect_context(line, line.len());
        assert_eq!(
            ctx,
            CompletionContext::SchemaObject {
                schema: "public".to_owned()
            }
        );
    }

    // -----------------------------------------------------------------------
    // extract_update_table tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_update_table_simple() {
        let toks = tokens_upper("UPDATE users SET");
        let result = extract_update_table(&toks);
        assert_eq!(result, Some("users".to_owned()));
    }

    #[test]
    fn test_extract_update_table_schema_qualified() {
        let toks = tokens_upper("UPDATE public.orders SET");
        let result = extract_update_table(&toks);
        assert_eq!(result, Some("orders".to_owned()));
    }

    #[test]
    fn test_extract_update_table_none_when_absent() {
        let toks = tokens_upper("SELECT * FROM users");
        let result = extract_update_table(&toks);
        assert_eq!(result, None);
    }

    // -----------------------------------------------------------------------
    // preceded_by_update tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_preceded_by_update_true() {
        let toks = tokens_upper("UPDATE users SET");
        // SET is index 2; check toks[..2]
        assert!(preceded_by_update(&toks, 2));
    }

    #[test]
    fn test_preceded_by_update_false_bare_set() {
        let toks = tokens_upper("SET work_mem");
        assert!(!preceded_by_update(&toks, 0));
    }

    #[test]
    fn test_preceded_by_update_false_select_intervenes() {
        // "SELECT ... SET" (hypothetical) — SELECT should reset scope.
        let toks = tokens_upper("SELECT 1 FROM t SET");
        let set_idx = toks.iter().position(|t| t == "SET").unwrap();
        assert!(!preceded_by_update(&toks, set_idx));
    }

    // -----------------------------------------------------------------------
    // RpgHelper::complete — new scenario smoke tests
    // -----------------------------------------------------------------------

    fn make_cache_with_table_and_columns() -> SchemaCache {
        let mut cache = SchemaCache::default();
        cache.tables.push(TableInfo {
            schema: "public".to_owned(),
            name: "users".to_owned(),
            kind: 'r',
        });
        for col in ["id", "email", "created_at"] {
            cache.columns.push(ColumnInfo {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                name: col.to_owned(),
                type_name: "text".to_owned(),
                position: 1,
            });
        }
        cache
    }

    #[test]
    fn test_complete_columns_after_select() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        // With no FROM, all columns should be offered.
        assert!(names.contains(&"id"), "expected 'id' in {names:?}");
        assert!(names.contains(&"email"), "expected 'email' in {names:?}");
    }

    #[test]
    fn test_complete_columns_after_where() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT * FROM users WHERE ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"id"), "expected 'id' in {names:?}");
        assert!(names.contains(&"email"), "expected 'email' in {names:?}");
    }

    #[test]
    fn test_complete_columns_after_update_set() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "UPDATE users SET ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"email"), "expected 'email' in {names:?}");
        assert!(names.contains(&"id"), "expected 'id' in {names:?}");
    }

    #[test]
    fn test_complete_tables_after_insert_into() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "INSERT INTO ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
    }

    #[test]
    fn test_complete_tables_after_drop_table() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "DROP TABLE ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
    }

    #[test]
    fn test_complete_tables_after_alter_table() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "ALTER TABLE ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
    }

    #[test]
    fn test_complete_tables_after_create_index_on() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "CREATE INDEX ON ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"users"), "expected 'users' in {names:?}");
    }

    #[test]
    fn test_complete_columns_order_by() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT id FROM users ORDER BY ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"id"), "expected 'id' in {names:?}");
        assert!(names.contains(&"email"), "expected 'email' in {names:?}");
    }

    #[test]
    fn test_complete_columns_group_by() {
        use rustyline::history::DefaultHistory;

        let cache = Arc::new(RwLock::new(make_cache_with_table_and_columns()));
        let helper = RpgHelper::new(cache, false);
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);

        let line = "SELECT status FROM users GROUP BY ";
        let (_start, candidates) = helper.complete(line, line.len(), &ctx).unwrap();
        let names: Vec<&str> = candidates.iter().map(|p| p.display.as_str()).collect();
        assert!(names.contains(&"id"), "expected 'id' in {names:?}");
    }
}
