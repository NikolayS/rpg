//! Client-side `\copy` implementation for Samo.
//!
//! Unlike the SQL `COPY` command (which reads/writes on the *server*),
//! `\copy` transfers data between local files on the *client* machine and a
//! `PostgreSQL` table or query result.  The wire-level exchange always uses
//! `COPY … FROM STDIN` or `COPY … TO STDOUT`; Samo streams the file data.
//!
//! # Syntax
//!
//! ```text
//! \copy table [(col, …)] FROM 'file'|stdin  [options]
//! \copy table             TO   'file'|stdout [options]
//! \copy (query)           TO   'file'|stdout [options]
//! ```
//!
//! Options: `CSV`, `TEXT`, `DELIMITER 'x'`, `HEADER`, `NULL 'str'`

use std::io::{self, BufRead, Write};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Direction of the copy operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyDirection {
    From,
    To,
}

/// The table/query that is the source or destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyTarget {
    /// A named table, with an optional column list.
    Table { name: String, columns: Vec<String> },
    /// A parenthesised SELECT query (only valid for `TO`).
    Query(String),
}

/// Where data comes from or goes to on the client side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopySource {
    /// A local file path.
    File(String),
    /// Standard input (reads until `\.` on a line by itself).
    Stdin,
    /// Standard output.
    Stdout,
}

/// Wire format used for the copy data stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CopyFormat {
    /// `PostgreSQL` text format (tab-delimited by default).
    #[default]
    Text,
    /// Comma-separated values.
    Csv,
}

/// All parameters that define a `\copy` operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopySpec {
    pub direction: CopyDirection,
    pub target: CopyTarget,
    pub source: CopySource,
    pub format: CopyFormat,
    pub delimiter: Option<char>,
    pub header: bool,
    pub null_string: Option<String>,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse the argument string that follows `\copy`.
///
/// Returns a [`CopySpec`] on success, or an error string describing what
/// went wrong.
///
/// # Examples
///
/// ```
/// use samo::copy::parse_copy_args;
/// let spec = parse_copy_args("my_table FROM '/tmp/data.csv' CSV HEADER").unwrap();
/// ```
pub fn parse_copy_args(args: &str) -> Result<CopySpec, String> {
    let mut tokens = Tokenizer::new(args);

    // -----------------------------------------------------------------------
    // Step 1 — table name / column list  or  (query)
    // -----------------------------------------------------------------------
    let target = parse_target(&mut tokens)?;

    // -----------------------------------------------------------------------
    // Step 2 — FROM | TO
    // -----------------------------------------------------------------------
    let direction_tok = tokens
        .next()
        .ok_or_else(|| "\\copy: missing FROM or TO keyword".to_owned())?;
    let direction = match direction_tok.to_uppercase().as_str() {
        "FROM" => CopyDirection::From,
        "TO" => CopyDirection::To,
        other => return Err(format!("\\copy: expected FROM or TO, got '{other}'")),
    };

    // -----------------------------------------------------------------------
    // Step 3 — file path / stdin / stdout
    // -----------------------------------------------------------------------
    let src_tok = tokens
        .next()
        .ok_or_else(|| "\\copy: missing file path or stdin/stdout".to_owned())?;

    let source = match src_tok.to_lowercase().as_str() {
        "stdin" => CopySource::Stdin,
        "stdout" => CopySource::Stdout,
        _ => {
            // Strip surrounding single quotes if present.
            let path = src_tok
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .unwrap_or(&src_tok)
                .to_owned();
            CopySource::File(path)
        }
    };

    // Validate direction / source combinations.
    match (&direction, &source) {
        (CopyDirection::From, CopySource::Stdout) => {
            return Err("\\copy: STDOUT is not valid for FROM direction".to_owned());
        }
        (CopyDirection::To, CopySource::Stdin) => {
            return Err("\\copy: STDIN is not valid for TO direction".to_owned());
        }
        _ => {}
    }
    if direction == CopyDirection::From {
        if let CopyTarget::Query(_) = &target {
            return Err("\\copy: query form is only valid for TO direction".to_owned());
        }
    }

    // -----------------------------------------------------------------------
    // Step 4 — options (CSV, TEXT, DELIMITER 'x', HEADER, NULL 'str')
    // -----------------------------------------------------------------------
    let mut format = CopyFormat::default();
    let mut delimiter: Option<char> = None;
    let mut header = false;
    let mut null_string: Option<String> = None;

    while let Some(opt) = tokens.next() {
        match opt.to_uppercase().as_str() {
            "TEXT" => format = CopyFormat::Text,
            "CSV" => format = CopyFormat::Csv,
            "HEADER" => header = true,
            "DELIMITER" => {
                let val = tokens
                    .next()
                    .ok_or_else(|| "\\copy: DELIMITER requires a value".to_owned())?;
                let ch = unquote_char(&val)?;
                delimiter = Some(ch);
            }
            "NULL" => {
                let val = tokens
                    .next()
                    .ok_or_else(|| "\\copy: NULL requires a value".to_owned())?;
                let s = val
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .unwrap_or(&val)
                    .to_owned();
                null_string = Some(s);
            }
            unknown => {
                return Err(format!("\\copy: unknown option '{unknown}'"));
            }
        }
    }

    Ok(CopySpec {
        direction,
        target,
        source,
        format,
        delimiter,
        header,
        null_string,
    })
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// Execute a parsed `\copy` specification against the given client.
///
/// Prints `COPY N` on success (where N is the number of rows transferred).
/// Returns an error string on failure.
pub async fn execute_copy(client: &tokio_postgres::Client, spec: &CopySpec) -> Result<(), String> {
    match spec.direction {
        CopyDirection::From => execute_copy_from(client, spec).await,
        CopyDirection::To => execute_copy_to(client, spec).await,
    }
}

/// Build the `COPY … FROM STDIN` SQL statement string.
fn build_copy_from_sql(spec: &CopySpec) -> String {
    let mut sql = String::from("copy ");
    append_target(&mut sql, &spec.target);
    sql.push_str(" from stdin");
    append_options(&mut sql, spec);
    sql
}

/// Build the `COPY … TO STDOUT` SQL statement string.
fn build_copy_to_sql(spec: &CopySpec) -> String {
    let mut sql = String::from("copy ");
    append_target(&mut sql, &spec.target);
    sql.push_str(" to stdout");
    append_options(&mut sql, spec);
    sql
}

/// Append the `target` clause to a SQL string being built.
fn append_target(sql: &mut String, target: &CopyTarget) {
    match target {
        CopyTarget::Table { name, columns } => {
            sql.push_str(name);
            if !columns.is_empty() {
                sql.push_str(" (");
                sql.push_str(&columns.join(", "));
                sql.push(')');
            }
        }
        CopyTarget::Query(q) => {
            sql.push('(');
            sql.push_str(q);
            sql.push(')');
        }
    }
}

/// Append `WITH (FORMAT …, DELIMITER …, HEADER, NULL …)` options.
fn append_options(sql: &mut String, spec: &CopySpec) {
    let mut opts: Vec<String> = Vec::new();

    match spec.format {
        CopyFormat::Text => {} // TEXT is the default; omit for brevity.
        CopyFormat::Csv => opts.push("format csv".to_owned()),
    }

    if let Some(delim) = spec.delimiter {
        // Escape single quote if the delimiter happens to be one.
        let escaped = if delim == '\'' {
            "''''".to_owned()
        } else {
            format!("'{delim}'")
        };
        opts.push(format!("delimiter {escaped}"));
    }

    if spec.header {
        opts.push("header".to_owned());
    }

    if let Some(ref ns) = spec.null_string {
        let escaped = ns.replace('\'', "''");
        opts.push(format!("null '{escaped}'"));
    }

    if !opts.is_empty() {
        sql.push_str(" with (");
        sql.push_str(&opts.join(", "));
        sql.push(')');
    }
}

/// Execute `COPY FROM STDIN` — stream local file data to the server.
async fn execute_copy_from(client: &tokio_postgres::Client, spec: &CopySpec) -> Result<(), String> {
    use futures::SinkExt;

    let sql = build_copy_from_sql(spec);

    // Read data bytes from the source.
    let data: Vec<u8> = match &spec.source {
        CopySource::File(path) => {
            std::fs::read(path).map_err(|e| format!("\\copy: could not read file '{path}': {e}"))?
        }
        CopySource::Stdin => read_stdin_until_terminator()?,
        CopySource::Stdout => {
            // Validated earlier; this branch is unreachable in practice.
            return Err("\\copy: STDOUT is not valid for FROM direction".to_owned());
        }
    };

    let sink = client
        .copy_in(&sql)
        .await
        .map_err(|e| format!("\\copy: {e}"))?;

    // CopyInSink is !Unpin; we must pin it on the stack before using SinkExt.
    tokio::pin!(sink);

    sink.send(bytes::Bytes::from(data))
        .await
        .map_err(|e| format!("\\copy: {e}"))?;

    // finish() flushes, sends CopyDone, and returns the number of rows copied.
    let rows = sink.finish().await.map_err(|e| format!("\\copy: {e}"))?;

    println!("COPY {rows}");
    Ok(())
}

/// Execute `COPY TO STDOUT` — stream server data to a local file.
async fn execute_copy_to(client: &tokio_postgres::Client, spec: &CopySpec) -> Result<(), String> {
    use futures::StreamExt;

    let sql = build_copy_to_sql(spec);

    let stream = client
        .copy_out(&sql)
        .await
        .map_err(|e| format!("\\copy: {e}"))?;

    // CopyOutStream is !Unpin; pin it before using StreamExt.
    tokio::pin!(stream);

    match &spec.source {
        CopySource::File(path) => {
            let mut file = std::fs::File::create(path)
                .map_err(|e| format!("\\copy: could not create file '{path}': {e}"))?;
            let mut row_count = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("\\copy: {e}"))?;
                // Count newlines as a proxy for row count (text/csv format).
                row_count += chunk.iter().fold(0u64, |n, &b| n + u64::from(b == b'\n'));
                file.write_all(&chunk)
                    .map_err(|e| format!("\\copy: write error: {e}"))?;
            }
            println!("COPY {row_count}");
        }
        CopySource::Stdout => {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let mut row_count = 0u64;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("\\copy: {e}"))?;
                row_count += chunk.iter().fold(0u64, |n, &b| n + u64::from(b == b'\n'));
                out.write_all(&chunk)
                    .map_err(|e| format!("\\copy: write error: {e}"))?;
            }
            println!("COPY {row_count}");
        }
        CopySource::Stdin => {
            // Validated earlier; this branch is unreachable in practice.
            return Err("\\copy: STDIN is not valid for TO direction".to_owned());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Stdin reader
// ---------------------------------------------------------------------------

/// Read lines from stdin until a line that is exactly `\.` (the copy
/// terminator), then return all preceding lines as UTF-8 bytes.
fn read_stdin_until_terminator() -> Result<Vec<u8>, String> {
    let stdin = io::stdin();
    let mut buf = Vec::new();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("\\copy: read error: {e}"))?;
        if line == "\\." {
            break;
        }
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse the target (table or parenthesised query) from the token stream.
fn parse_target(tokens: &mut Tokenizer) -> Result<CopyTarget, String> {
    let first = tokens
        .peek()
        .ok_or_else(|| "\\copy: missing table name or query".to_owned())?;

    if first.starts_with('(') {
        // Parenthesised query — consume tokens until closing ')'.
        let raw = tokens.next().unwrap(); // consume the peeked token
        let query = if raw.starts_with('(') && raw.ends_with(')') {
            // The whole query was returned as one token (e.g. it contained
            // no internal spaces that weren't inside quotes).
            raw[1..raw.len() - 1].trim().to_owned()
        } else {
            // Multi-token query: gather until we see a token ending with ')'.
            let mut parts = vec![raw.trim_start_matches('(').to_owned()];
            loop {
                let tok = tokens
                    .next()
                    .ok_or_else(|| "\\copy: unclosed parenthesis in query".to_owned())?;
                if tok.ends_with(')') {
                    parts.push(tok.trim_end_matches(')').to_owned());
                    break;
                }
                parts.push(tok);
            }
            parts.join(" ")
        };
        return Ok(CopyTarget::Query(query.trim().to_owned()));
    }

    // Table name.
    let name = tokens.next().unwrap(); // consume the peeked token

    // Optional column list `(col, col2, …)`.
    let mut columns: Vec<String> = Vec::new();
    if let Some(next) = tokens.peek() {
        if next.starts_with('(') {
            let col_tok = tokens.next().unwrap();
            // Strip surrounding parens and split by comma.
            let inner = col_tok.trim_start_matches('(').trim_end_matches(')').trim();
            for col in inner.split(',') {
                let c = col.trim().to_owned();
                if !c.is_empty() {
                    columns.push(c);
                }
            }
        }
    }

    Ok(CopyTarget::Table { name, columns })
}

/// Strip single quotes around a one-character value like `','`.
fn unquote_char(s: &str) -> Result<char, String> {
    let inner = s
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .unwrap_or(s);
    let mut chars = inner.chars();
    let ch = chars
        .next()
        .ok_or_else(|| "\\copy: DELIMITER value must be a single character".to_owned())?;
    if chars.next().is_some() {
        return Err("\\copy: DELIMITER value must be a single character".to_owned());
    }
    Ok(ch)
}

// ---------------------------------------------------------------------------
// Simple tokenizer
// ---------------------------------------------------------------------------

/// A minimal tokenizer that yields whitespace-separated tokens, treating
/// single-quoted strings as single tokens and parenthesised groups as tokens
/// only when `(` appears at the start of an argument.
struct Tokenizer<'a> {
    input: &'a str,
    pos: usize,
    peeked: Option<String>,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            pos: 0,
            peeked: None,
        }
    }

    /// Return the next token without consuming it.
    fn peek(&mut self) -> Option<&str> {
        if self.peeked.is_none() {
            self.peeked = self.advance();
        }
        self.peeked.as_deref()
    }

    /// Consume and return the next token.
    fn next(&mut self) -> Option<String> {
        if let Some(tok) = self.peeked.take() {
            return Some(tok);
        }
        self.advance()
    }

    fn advance(&mut self) -> Option<String> {
        // Skip leading whitespace.
        let bytes = self.input.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.pos >= bytes.len() {
            return None;
        }

        let start = self.pos;

        if bytes[start] == b'\'' {
            // Single-quoted string: consume until closing quote ('' escapes a
            // literal single quote inside the string).
            self.pos += 1;
            loop {
                if self.pos >= bytes.len() {
                    break;
                }
                if bytes[self.pos] == b'\'' {
                    self.pos += 1;
                    if self.pos < bytes.len() && bytes[self.pos] == b'\'' {
                        // Escaped quote inside string.
                        self.pos += 1;
                    } else {
                        break;
                    }
                } else {
                    self.pos += 1;
                }
            }
        } else if bytes[start] == b'(' {
            // Parenthesised expression: consume until matching ')'.
            let mut depth = 0usize;
            while self.pos < bytes.len() {
                match bytes[self.pos] {
                    b'(' => {
                        depth += 1;
                        self.pos += 1;
                    }
                    b')' => {
                        self.pos += 1;
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    b'\'' => {
                        // Skip quoted string inside query.
                        self.pos += 1;
                        while self.pos < bytes.len() {
                            if bytes[self.pos] == b'\'' {
                                self.pos += 1;
                                if self.pos < bytes.len() && bytes[self.pos] == b'\'' {
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            } else {
                                self.pos += 1;
                            }
                        }
                    }
                    _ => {
                        self.pos += 1;
                    }
                }
            }
        } else {
            // Regular token: read until whitespace.
            while self.pos < bytes.len() && !bytes[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
        }

        Some(self.input[start..self.pos].to_owned())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_copy_args ----------------------------------------------------

    #[test]
    fn test_parse_from_file() {
        let spec = parse_copy_args("my_table FROM '/tmp/data.txt'").unwrap();
        assert_eq!(spec.direction, CopyDirection::From);
        assert_eq!(
            spec.target,
            CopyTarget::Table {
                name: "my_table".to_owned(),
                columns: vec![],
            }
        );
        assert_eq!(spec.source, CopySource::File("/tmp/data.txt".to_owned()));
        assert_eq!(spec.format, CopyFormat::Text);
        assert!(!spec.header);
    }

    #[test]
    fn test_parse_to_file() {
        let spec = parse_copy_args("orders TO '/tmp/out.csv' CSV HEADER").unwrap();
        assert_eq!(spec.direction, CopyDirection::To);
        assert_eq!(
            spec.target,
            CopyTarget::Table {
                name: "orders".to_owned(),
                columns: vec![],
            }
        );
        assert_eq!(spec.source, CopySource::File("/tmp/out.csv".to_owned()));
        assert_eq!(spec.format, CopyFormat::Csv);
        assert!(spec.header);
    }

    #[test]
    fn test_parse_query_to_stdout() {
        let spec = parse_copy_args("(select id, name from users) TO stdout CSV").unwrap();
        assert_eq!(spec.direction, CopyDirection::To);
        assert!(matches!(spec.target, CopyTarget::Query(_)));
        assert_eq!(spec.source, CopySource::Stdout);
        assert_eq!(spec.format, CopyFormat::Csv);
    }

    #[test]
    fn test_parse_from_stdin() {
        let spec = parse_copy_args("my_table FROM stdin").unwrap();
        assert_eq!(spec.direction, CopyDirection::From);
        assert_eq!(spec.source, CopySource::Stdin);
    }

    #[test]
    fn test_parse_with_delimiter() {
        let spec = parse_copy_args("t FROM '/f' DELIMITER ','").unwrap();
        assert_eq!(spec.delimiter, Some(','));
    }

    #[test]
    fn test_parse_with_null_string() {
        let spec = parse_copy_args("t FROM '/f' NULL 'NULL'").unwrap();
        assert_eq!(spec.null_string.as_deref(), Some("NULL"));
    }

    #[test]
    fn test_parse_columns() {
        let spec = parse_copy_args("t (a, b, c) FROM stdin").unwrap();
        assert_eq!(
            spec.target,
            CopyTarget::Table {
                name: "t".to_owned(),
                columns: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            }
        );
    }

    #[test]
    fn test_parse_missing_from_to() {
        assert!(parse_copy_args("my_table").is_err());
    }

    #[test]
    fn test_parse_invalid_direction() {
        assert!(parse_copy_args("my_table INTO stdin").is_err());
    }

    #[test]
    fn test_parse_stdout_with_from_is_error() {
        assert!(parse_copy_args("my_table FROM stdout").is_err());
    }

    #[test]
    fn test_parse_stdin_with_to_is_error() {
        assert!(parse_copy_args("my_table TO stdin").is_err());
    }

    #[test]
    fn test_parse_query_with_from_is_error() {
        assert!(parse_copy_args("(select 1) FROM stdin").is_err());
    }

    // --- SQL builder --------------------------------------------------------

    #[test]
    fn test_build_from_sql_text() {
        let spec = CopySpec {
            direction: CopyDirection::From,
            target: CopyTarget::Table {
                name: "t".to_owned(),
                columns: vec![],
            },
            source: CopySource::File("/f".to_owned()),
            format: CopyFormat::Text,
            delimiter: None,
            header: false,
            null_string: None,
        };
        assert_eq!(build_copy_from_sql(&spec), "copy t from stdin");
    }

    #[test]
    fn test_build_to_sql_csv_header() {
        let spec = CopySpec {
            direction: CopyDirection::To,
            target: CopyTarget::Table {
                name: "orders".to_owned(),
                columns: vec![],
            },
            source: CopySource::File("/tmp/out.csv".to_owned()),
            format: CopyFormat::Csv,
            delimiter: None,
            header: true,
            null_string: None,
        };
        assert_eq!(
            build_copy_to_sql(&spec),
            "copy orders to stdout with (format csv, header)"
        );
    }

    #[test]
    fn test_build_to_sql_query() {
        let spec = CopySpec {
            direction: CopyDirection::To,
            target: CopyTarget::Query("select 1".to_owned()),
            source: CopySource::Stdout,
            format: CopyFormat::Text,
            delimiter: None,
            header: false,
            null_string: None,
        };
        assert_eq!(build_copy_to_sql(&spec), "copy (select 1) to stdout");
    }

    #[test]
    fn test_build_sql_with_columns() {
        let spec = CopySpec {
            direction: CopyDirection::From,
            target: CopyTarget::Table {
                name: "t".to_owned(),
                columns: vec!["a".to_owned(), "b".to_owned()],
            },
            source: CopySource::Stdin,
            format: CopyFormat::Text,
            delimiter: None,
            header: false,
            null_string: None,
        };
        assert_eq!(build_copy_from_sql(&spec), "copy t (a, b) from stdin");
    }

    #[test]
    fn test_build_sql_all_options() {
        let spec = CopySpec {
            direction: CopyDirection::To,
            target: CopyTarget::Table {
                name: "t".to_owned(),
                columns: vec![],
            },
            source: CopySource::Stdout,
            format: CopyFormat::Csv,
            delimiter: Some(';'),
            header: true,
            null_string: Some("\\N".to_owned()),
        };
        assert_eq!(
            build_copy_to_sql(&spec),
            "copy t to stdout with (format csv, delimiter ';', header, null '\\N')"
        );
    }

    #[test]
    fn test_null_string_with_single_quote_is_escaped() {
        let spec = CopySpec {
            direction: CopyDirection::From,
            target: CopyTarget::Table {
                name: "t".to_owned(),
                columns: vec![],
            },
            source: CopySource::Stdin,
            format: CopyFormat::Text,
            delimiter: None,
            header: false,
            null_string: Some("it's".to_owned()),
        };
        assert_eq!(
            build_copy_from_sql(&spec),
            "copy t from stdin with (null 'it''s')"
        );
    }
}
