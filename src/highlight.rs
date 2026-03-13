//! SQL syntax highlighting for the Samo REPL.
//!
//! Provides a simple tokenizer that classifies SQL tokens and emits ANSI
//! escape sequences for coloring.  This is used by [`SamoHelper`]'s
//! `Highlighter` implementation.
//!
//! [`SamoHelper`]: crate::complete::SamoHelper

/// Token categories for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// SQL keyword (SELECT, FROM, WHERE, etc.)
    Keyword,
    /// String literal (`'...'` or `$$...$$`)
    StringLiteral,
    /// Numeric literal (42, 3.14, 1e10)
    Number,
    /// Comment (`--` or `/* ... */`)
    Comment,
    /// Backslash command (`\dt`, `\l`, etc.)
    BackslashCmd,
    /// Operator (`+`, `-`, `*`, `/`, `=`, `<`, `>`, etc.)
    Operator,
    /// Regular identifier or other text
    Normal,
}

/// A token with its kind and byte range in the source.
#[derive(Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
}

// ---------------------------------------------------------------------------
// Keyword list
// ---------------------------------------------------------------------------

/// Sorted uppercase SQL keywords for binary search.
///
/// IMPORTANT: This array **must** remain in ascending lexicographic order so
/// that `binary_search` works correctly.
static SQL_KEYWORDS_UPPER: &[&str] = &[
    "ABORT",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BIGINT",
    "BOOLEAN",
    "BY",
    "CALL",
    "CASE",
    "CAST",
    "CHAR",
    "CHARACTER",
    "CHECK",
    "CHECKPOINT",
    "CLOSE",
    "CLUSTER",
    "COLLATE",
    "COLUMN",
    "COMMENT",
    "COMMIT",
    "CONSTRAINT",
    "COPY",
    "CREATE",
    "CROSS",
    "CURRENT",
    "DATE",
    "DEALLOCATE",
    "DECLARE",
    "DEFAULT",
    "DEFERRABLE",
    "DELETE",
    "DESC",
    "DISCARD",
    "DISTINCT",
    "DO",
    "DOUBLE",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXECUTE",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FLOAT",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IMPORT",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTEGER",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "JOIN",
    "JSON",
    "JSONB",
    "LATERAL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LISTEN",
    "LOAD",
    "LOCK",
    "MOVE",
    "NOT",
    "NOTIFY",
    "NULL",
    "NUMERIC",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PREPARE",
    "PRIMARY",
    "REAL",
    "REASSIGN",
    "REFERENCES",
    "REFRESH",
    "REINDEX",
    "RELEASE",
    "RESET",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "SAVEPOINT",
    "SCHEMA",
    "SECURITY",
    "SELECT",
    "SERIAL",
    "SET",
    "SHOW",
    "SIMILAR",
    "SMALLINT",
    "SOME",
    "START",
    "TABLE",
    "TEXT",
    "THEN",
    "TIME",
    "TIMESTAMP",
    "TO",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UNLISTEN",
    "UPDATE",
    "USING",
    "UUID",
    "VACUUM",
    "VALUES",
    "VARCHAR",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "XML",
];

/// Check if `word` is a SQL keyword (case-insensitive).
pub fn is_sql_keyword(word: &str) -> bool {
    // Fast path: avoid allocation when word is already ASCII uppercase.
    let upper = word.to_uppercase();
    SQL_KEYWORDS_UPPER.binary_search(&upper.as_str()).is_ok()
}

// ---------------------------------------------------------------------------
// ANSI colors
// ---------------------------------------------------------------------------

/// ANSI color/style escape for each token kind.
fn ansi_color(kind: TokenKind) -> &'static str {
    match kind {
        TokenKind::Keyword => "\x1b[1;34m",      // bold blue
        TokenKind::StringLiteral => "\x1b[32m",  // green
        TokenKind::Number => "\x1b[33m",         // yellow
        TokenKind::Comment => "\x1b[2;37m",      // dim gray
        TokenKind::BackslashCmd => "\x1b[1;35m", // bold magenta
        TokenKind::Operator => "\x1b[36m",       // cyan
        TokenKind::Normal => "",                 // no color
    }
}

const ANSI_RESET: &str = "\x1b[0m";

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Tokenize `input` for syntax highlighting purposes.
///
/// Returns a list of non-overlapping [`Token`]s that together cover the
/// entire input (including whitespace, which is emitted as `Normal`).
#[allow(clippy::too_many_lines)]
pub fn tokenize(input: &str) -> Vec<Token> {
    let bytes = input.as_bytes();
    let len = input.len();
    let mut tokens = Vec::new();
    let mut pos = 0_usize;

    while pos < len {
        // ------------------------------------------------------------------
        // Whitespace → Normal
        // ------------------------------------------------------------------
        if bytes[pos].is_ascii_whitespace() {
            let start = pos;
            while pos < len && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Normal,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Line comment: -- …
        // ------------------------------------------------------------------
        if pos + 1 < len && bytes[pos] == b'-' && bytes[pos + 1] == b'-' {
            let start = pos;
            while pos < len && bytes[pos] != b'\n' {
                pos += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Block comment: /* … */ (nested)
        // ------------------------------------------------------------------
        if pos + 1 < len && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
            let start = pos;
            pos += 2; // consume '/*'
            let mut depth: u32 = 1;
            while pos + 1 < len && depth > 0 {
                if bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
                    depth += 1;
                    pos += 2;
                } else if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                    depth -= 1;
                    pos += 2;
                } else {
                    pos += 1;
                }
            }
            // Consume trailing '*/' for depth==0 already handled above, but
            // handle unclosed comment (consume to end).
            if depth > 0 {
                pos = len;
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Single-quoted string: '…' ('' escape inside)
        // ------------------------------------------------------------------
        if bytes[pos] == b'\'' {
            let start = pos;
            pos += 1;
            loop {
                if pos >= len {
                    break;
                }
                if bytes[pos] == b'\'' {
                    pos += 1;
                    // '' is an escaped quote — continue inside the string.
                    if pos < len && bytes[pos] == b'\'' {
                        pos += 1;
                    } else {
                        break;
                    }
                } else {
                    pos += 1;
                }
            }
            tokens.push(Token {
                kind: TokenKind::StringLiteral,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Dollar-quoted string: $tag$…$tag$ or $$…$$
        // ------------------------------------------------------------------
        if bytes[pos] == b'$' {
            // Find the closing '$' that ends the tag.
            let tag_start = pos;
            let mut tag_end = pos + 1;
            // Tag can contain letters, digits, underscores (no spaces).
            while tag_end < len
                && (bytes[tag_end].is_ascii_alphanumeric() || bytes[tag_end] == b'_')
            {
                tag_end += 1;
            }
            if tag_end < len && bytes[tag_end] == b'$' {
                tag_end += 1; // include closing '$'
                let tag = &input[tag_start..tag_end];
                pos = tag_end;
                // Scan for the matching closing delimiter.
                if let Some(close_idx) = input[pos..].find(tag) {
                    pos += close_idx + tag.len();
                } else {
                    // Unclosed dollar quote — consume to end.
                    pos = len;
                }
                tokens.push(Token {
                    kind: TokenKind::StringLiteral,
                    start: tag_start,
                    end: pos,
                });
                continue;
            }
            // Not a dollar quote (e.g. bare '$1' parameter) — fall through
            // to the identifier/operator handling below.
        }

        // ------------------------------------------------------------------
        // Backslash command: \ at start or after whitespace
        // ------------------------------------------------------------------
        if bytes[pos] == b'\\' {
            let start = pos;
            pos += 1;
            // Consume the command name (letters, digits, '+', '*', '?', etc.)
            while pos < len && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            tokens.push(Token {
                kind: TokenKind::BackslashCmd,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Numeric literal: digit or .digit (e.g. 42, 3.14, .5, 1e10)
        // ------------------------------------------------------------------
        if bytes[pos].is_ascii_digit()
            || (bytes[pos] == b'.' && pos + 1 < len && bytes[pos + 1].is_ascii_digit())
        {
            let start = pos;
            // Integer / decimal part.
            while pos < len && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos < len && bytes[pos] == b'.' {
                pos += 1;
                while pos < len && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
            }
            // Exponent.
            if pos < len && (bytes[pos] == b'e' || bytes[pos] == b'E') {
                let saved = pos;
                pos += 1;
                if pos < len && (bytes[pos] == b'+' || bytes[pos] == b'-') {
                    pos += 1;
                }
                if pos < len && bytes[pos].is_ascii_digit() {
                    while pos < len && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                } else {
                    // Not a valid exponent — backtrack.
                    pos = saved;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Quoted identifier: "…"
        // ------------------------------------------------------------------
        if bytes[pos] == b'"' {
            let start = pos;
            pos += 1;
            while pos < len {
                if bytes[pos] == b'"' {
                    pos += 1;
                    // "" is an escaped double-quote inside a quoted identifier.
                    if pos < len && bytes[pos] == b'"' {
                        pos += 1;
                    } else {
                        break;
                    }
                } else {
                    pos += 1;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Normal,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Identifier or keyword
        // ------------------------------------------------------------------
        if bytes[pos].is_ascii_alphabetic() || bytes[pos] == b'_' {
            let start = pos;
            while pos < len
                && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'$')
            {
                pos += 1;
            }
            let word = &input[start..pos];
            let kind = if is_sql_keyword(word) {
                TokenKind::Keyword
            } else {
                TokenKind::Normal
            };
            tokens.push(Token {
                kind,
                start,
                end: pos,
            });
            continue;
        }

        // ------------------------------------------------------------------
        // Operators and punctuation
        // ------------------------------------------------------------------
        // Multi-character operators first.
        let op_end = consume_operator(bytes, pos);
        if op_end > pos {
            tokens.push(Token {
                kind: TokenKind::Operator,
                start: pos,
                end: op_end,
            });
            pos = op_end;
            continue;
        }

        // ------------------------------------------------------------------
        // Fallback: consume one byte as Normal (e.g. '(', ')', ',', ';')
        // ------------------------------------------------------------------
        tokens.push(Token {
            kind: TokenKind::Normal,
            start: pos,
            end: pos + 1,
        });
        pos += 1;
    }

    tokens
}

/// Try to consume an operator sequence starting at `pos` in `bytes`.
///
/// Returns the byte offset *past* the operator, or `pos` if no operator
/// starts here.
fn consume_operator(bytes: &[u8], pos: usize) -> usize {
    let len = bytes.len();
    if pos >= len {
        return pos;
    }

    // Two-character operators.
    if pos + 1 < len {
        #[allow(clippy::unnested_or_patterns)]
        match (bytes[pos], bytes[pos + 1]) {
            (b'!', b'=')
            | (b'<', b'>')
            | (b'<', b'=')
            | (b'>', b'=')
            | (b'|', b'|')
            | (b':', b':')
            | (b'-', b'>')
            | (b'~', b'~')
            | (b'!', b'~')
            | (b'<', b'<')
            | (b'>', b'>') => return pos + 2,
            _ => {}
        }
    }

    // Single-character operators.
    match bytes[pos] {
        b'+' | b'-' | b'*' | b'/' | b'=' | b'<' | b'>' | b'%' | b'^' | b'@' | b'~' | b'&'
        | b'|' | b'#' => pos + 1,
        _ => pos,
    }
}

// ---------------------------------------------------------------------------
// Highlighter
// ---------------------------------------------------------------------------

/// Highlight SQL text by wrapping tokens in ANSI escape sequences.
///
/// Returns a [`std::borrow::Cow`] — borrows the original if no highlighting
/// is needed (all tokens are `Normal`), or returns an owned `String`
/// otherwise.
pub fn highlight_sql(input: &str) -> std::borrow::Cow<'_, str> {
    let tokens = tokenize(input);
    if tokens.iter().all(|t| t.kind == TokenKind::Normal) {
        return std::borrow::Cow::Borrowed(input);
    }

    // Reserve extra capacity for the ANSI codes.
    let mut out = String::with_capacity(input.len() + tokens.len() * 10);
    for token in &tokens {
        let text = &input[token.start..token.end];
        let color = ansi_color(token.kind);
        if color.is_empty() {
            out.push_str(text);
        } else {
            out.push_str(color);
            out.push_str(text);
            out.push_str(ANSI_RESET);
        }
    }
    std::borrow::Cow::Owned(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper
    // -----------------------------------------------------------------------

    fn token_kinds(input: &str) -> Vec<(TokenKind, &str)> {
        tokenize(input)
            .into_iter()
            .map(|t| (t.kind, &input[t.start..t.end]))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Basic tokenizer tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_tokenize_empty() {
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn test_tokenize_whitespace_only() {
        let tokens = token_kinds("   \t\n");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::Normal);
        assert_eq!(tokens[0].1, "   \t\n");
    }

    #[test]
    fn test_tokenize_select_query() {
        let tokens = token_kinds("SELECT 1");
        // [Keyword("SELECT"), Normal(" "), Number("1")]
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0], (TokenKind::Keyword, "SELECT"));
        assert_eq!(tokens[1], (TokenKind::Normal, " "));
        assert_eq!(tokens[2], (TokenKind::Number, "1"));
    }

    #[test]
    fn test_tokenize_string_literal() {
        let tokens = token_kinds("'hello'");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::StringLiteral, "'hello'"));
    }

    #[test]
    fn test_tokenize_string_with_escape() {
        // 'it''s' is a single string literal.
        let tokens = token_kinds("'it''s'");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::StringLiteral);
        assert_eq!(tokens[0].1, "'it''s'");
    }

    #[test]
    fn test_tokenize_dollar_quoting() {
        let tokens = token_kinds("$$body$$");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::StringLiteral);
        assert_eq!(tokens[0].1, "$$body$$");
    }

    #[test]
    fn test_tokenize_dollar_quoting_with_tag() {
        let tokens = token_kinds("$func$body$func$");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::StringLiteral);
        assert_eq!(tokens[0].1, "$func$body$func$");
    }

    #[test]
    fn test_tokenize_line_comment() {
        let tokens = token_kinds("-- comment");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Comment, "-- comment"));
    }

    #[test]
    fn test_tokenize_line_comment_stops_at_newline() {
        let input = "-- comment\nSELECT";
        let tokens = token_kinds(input);
        assert_eq!(tokens[0], (TokenKind::Comment, "-- comment"));
        assert_eq!(tokens[1], (TokenKind::Normal, "\n"));
        assert_eq!(tokens[2], (TokenKind::Keyword, "SELECT"));
    }

    #[test]
    fn test_tokenize_block_comment() {
        let tokens = token_kinds("/* block */");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Comment, "/* block */"));
    }

    #[test]
    fn test_tokenize_nested_comment() {
        let tokens = token_kinds("/* /* nested */ */");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::Comment);
        assert_eq!(tokens[0].1, "/* /* nested */ */");
    }

    #[test]
    fn test_tokenize_number_integer() {
        let tokens = token_kinds("42");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Number, "42"));
    }

    #[test]
    fn test_tokenize_number_decimal() {
        let tokens = token_kinds("3.14");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Number, "3.14"));
    }

    #[test]
    fn test_tokenize_number_leading_dot() {
        let tokens = token_kinds(".5");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Number, ".5"));
    }

    #[test]
    fn test_tokenize_number_scientific() {
        let tokens = token_kinds("1e10");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Number, "1e10"));
    }

    #[test]
    fn test_tokenize_number_scientific_signed() {
        let tokens = token_kinds("1.5e-3");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::Number, "1.5e-3"));
    }

    #[test]
    fn test_tokenize_backslash_cmd() {
        let tokens = token_kinds(r"\dt");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], (TokenKind::BackslashCmd, r"\dt"));
    }

    #[test]
    fn test_tokenize_backslash_cmd_with_plus() {
        let tokens = token_kinds(r"\d+");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].0, TokenKind::BackslashCmd);
    }

    #[test]
    fn test_tokenize_operators() {
        // SELECT 1 + 2 should contain Operator("+").
        let tokens = token_kinds("SELECT 1 + 2");
        let op = tokens.iter().find(|(k, _)| *k == TokenKind::Operator);
        assert!(op.is_some(), "expected an Operator token");
        assert_eq!(op.unwrap().1, "+");
    }

    #[test]
    fn test_tokenize_multi_char_operator() {
        let tokens = token_kinds("a != b");
        let op = tokens
            .iter()
            .find(|(k, t)| *k == TokenKind::Operator && *t == "!=");
        assert!(op.is_some(), "expected '!=' operator token");
    }

    #[test]
    fn test_tokenize_cast_operator() {
        let tokens = token_kinds("42::int");
        let op = tokens
            .iter()
            .find(|(k, t)| *k == TokenKind::Operator && *t == "::");
        assert!(op.is_some(), "expected '::' operator token");
    }

    #[test]
    fn test_tokenize_complete_query() {
        let input = "SELECT name, age FROM users WHERE id = 1";
        let tokens = token_kinds(input);

        let keywords: Vec<&str> = tokens
            .iter()
            .filter(|(k, _)| *k == TokenKind::Keyword)
            .map(|(_, t)| *t)
            .collect();
        assert!(keywords.contains(&"SELECT"), "missing SELECT");
        assert!(keywords.contains(&"FROM"), "missing FROM");
        assert!(keywords.contains(&"WHERE"), "missing WHERE");

        let numbers: Vec<&str> = tokens
            .iter()
            .filter(|(k, _)| *k == TokenKind::Number)
            .map(|(_, t)| *t)
            .collect();
        assert!(numbers.contains(&"1"), "missing number '1'");
    }

    #[test]
    fn test_tokenize_keyword_case_insensitive_lower() {
        // The tokenizer should recognise lowercase keywords too.
        let tokens = token_kinds("select * from users");
        assert_eq!(tokens[0], (TokenKind::Keyword, "select"));
    }

    #[test]
    fn test_tokenize_keyword_mixed_case() {
        let tokens = token_kinds("Select");
        assert_eq!(tokens[0].0, TokenKind::Keyword);
    }

    // -----------------------------------------------------------------------
    // highlight_sql tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_highlight_sql_no_color_for_plain() {
        // A plain identifier (no keywords) should return Cow::Borrowed.
        let result = highlight_sql("identifier");
        assert!(
            matches!(result, std::borrow::Cow::Borrowed(_)),
            "expected Borrowed for plain identifier"
        );
    }

    #[test]
    fn test_highlight_sql_keywords_colored() {
        let result = highlight_sql("SELECT 1");
        // Result must contain ANSI bold-blue escape for SELECT.
        assert!(
            result.contains("\x1b[1;34m"),
            "expected bold-blue ANSI code for keyword"
        );
        assert!(result.contains("SELECT"), "expected SELECT in output");
    }

    #[test]
    fn test_highlight_sql_reset_after_token() {
        let result = highlight_sql("SELECT 1");
        // After each colored token there must be a reset.
        assert!(result.contains(ANSI_RESET), "expected ANSI reset code");
    }

    #[test]
    fn test_highlight_sql_covers_all_input() {
        // Strip ANSI codes from output; result must equal the original input.
        let input = "SELECT name FROM users WHERE id = 1";
        let colored = highlight_sql(input);
        let stripped = strip_ansi(colored.as_ref());
        assert_eq!(stripped, input);
    }

    // -----------------------------------------------------------------------
    // is_sql_keyword tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_sql_keyword_upper() {
        assert!(is_sql_keyword("SELECT"));
        assert!(is_sql_keyword("FROM"));
        assert!(is_sql_keyword("WHERE"));
    }

    #[test]
    fn test_is_sql_keyword_lower() {
        assert!(is_sql_keyword("select"));
        assert!(is_sql_keyword("from"));
    }

    #[test]
    fn test_is_sql_keyword_mixed() {
        assert!(is_sql_keyword("Select"));
        assert!(is_sql_keyword("WheRe"));
    }

    #[test]
    fn test_is_sql_keyword_false() {
        assert!(!is_sql_keyword("users"));
        assert!(!is_sql_keyword("foobar"));
        assert!(!is_sql_keyword(""));
    }

    // -----------------------------------------------------------------------
    // Coverage tests for edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_tokenize_semicolon_is_normal() {
        let tokens = token_kinds("SELECT 1;");
        let last = tokens.last().unwrap();
        assert_eq!(last.0, TokenKind::Normal);
        assert_eq!(last.1, ";");
    }

    #[test]
    fn test_tokenize_paren_is_normal() {
        let tokens = token_kinds("count(*)");
        let kinds: Vec<TokenKind> = tokens.iter().map(|t| t.0).collect();
        // '(' and ')' and '*' should all appear.
        assert!(kinds.contains(&TokenKind::Normal));
    }

    #[test]
    fn test_tokenize_entire_coverage() {
        // Ensure the full token text is exactly the input reconstructed.
        let input =
            "SELECT t.id, 'hello' AS greeting -- comment\nFROM public.tbl WHERE val > 3.14;";
        let tokens = tokenize(input);
        let reconstructed: String = tokens.iter().map(|t| &input[t.start..t.end]).collect();
        assert_eq!(reconstructed, input);
    }

    // -----------------------------------------------------------------------
    // Utility
    // -----------------------------------------------------------------------

    /// Remove ANSI escape sequences for assertion helpers.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip until 'm'.
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
