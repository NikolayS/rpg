//! Lightweight markdown-to-ANSI terminal renderer for AI output.
//!
//! Renders the common markdown patterns that LLMs produce into styled
//! terminal output using ANSI escape codes.  Not a full `CommonMark` parser —
//! designed to handle the patterns AI assistants actually emit.
//!
//! Supported constructs:
//! - Headers (`#`, `##`, `###`) → bold coloured text
//! - Bold (`**text**`) → ANSI bold
//! - Italic (`*text*` or `_text_`) → ANSI dim/italic
//! - Code spans (`` `code` ``) → green text
//! - Code fences (` ```lang … ``` `) → distinct style; SQL fences use the
//!   existing [`crate::highlight::highlight_sql`] highlighter
//! - Bullet lists (`-` or `*` at line start) → `•` bullet with indent
//! - Numbered lists (`1.`, `2.` …) → kept as-is, indented
//! - Horizontal rules (`---`, `***`, `___`) → drawn across terminal width
//! - Tables (`| col | col |`) → pipe-delimited with bold header row
//! - Links (`[text](url)`) → `text (url)`
//!
//! All rendering is skipped when `no_highlight` is `true`.

// ---------------------------------------------------------------------------
// ANSI constants
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const BOLD_CYAN: &str = "\x1b[1;36m";
const BOLD_YELLOW: &str = "\x1b[1;33m";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render `text` (which may contain markdown) to a terminal-styled string.
///
/// When `no_highlight` is `true` the input is returned unchanged.
pub fn render_markdown(text: &str, no_highlight: bool) -> String {
    if no_highlight {
        return text.to_owned();
    }
    render(text)
}

// ---------------------------------------------------------------------------
// Core renderer
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
fn render(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out = String::with_capacity(input.len() + input.len() / 4);
    let term_width = terminal_width();

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        // ---- Code fence  --------------------------------------------------
        if line.trim_start().starts_with("```") {
            let fence_prefix = line.trim_start();
            let lang = fence_prefix.trim_start_matches('`').trim().to_lowercase();
            let mut code_lines: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            // Skip the closing fence line.
            if i < lines.len() {
                i += 1;
            }

            render_code_fence(&mut out, &lang, &code_lines);
            continue;
        }

        // ---- Horizontal rule  ---------------------------------------------
        if is_horizontal_rule(line) {
            let width = term_width.min(80);
            out.push_str(DIM);
            for _ in 0..width {
                out.push('─');
            }
            out.push_str(RESET);
            out.push('\n');
            i += 1;
            continue;
        }

        // ---- Table row  ---------------------------------------------------
        let trimmed = line.trim();
        if trimmed.starts_with('|') && trimmed.ends_with('|') {
            // Peek ahead: if next line is a separator row, current is header.
            let next_is_sep = lines.get(i + 1).is_some_and(|l| is_table_separator(l));
            let is_header = next_is_sep;

            render_table_row(&mut out, line, is_header);

            // Skip the separator row that follows the header.
            if is_header {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // ---- ATX headers  -------------------------------------------------
        if let Some(rest) = trimmed.strip_prefix("### ") {
            out.push_str(BOLD_CYAN);
            out.push_str(&inline(rest));
            out.push_str(RESET);
            out.push('\n');
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            out.push_str(BOLD_YELLOW);
            out.push_str(&inline(rest));
            out.push_str(RESET);
            out.push('\n');
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            out.push_str(BOLD);
            out.push_str(YELLOW);
            out.push_str(&inline(rest));
            out.push_str(RESET);
            out.push('\n');
            i += 1;
            continue;
        }

        // ---- Bullet list  -------------------------------------------------
        if let Some(rest) = parse_bullet(line) {
            let indent = leading_spaces(line);
            let extra = "  ".repeat(indent / 2);
            out.push_str(&extra);
            out.push_str(CYAN);
            out.push('•');
            out.push_str(RESET);
            out.push(' ');
            out.push_str(&inline(rest));
            out.push('\n');
            i += 1;
            continue;
        }

        // ---- Numbered list  -----------------------------------------------
        if let Some((num, rest)) = parse_numbered(line) {
            let indent = leading_spaces(line);
            let extra = "  ".repeat(indent / 2);
            out.push_str(&extra);
            out.push_str(BOLD);
            out.push_str(&num);
            out.push_str(RESET);
            out.push(' ');
            out.push_str(&inline(rest));
            out.push('\n');
            i += 1;
            continue;
        }

        // ---- Plain paragraph line  ----------------------------------------
        out.push_str(&inline(line));
        out.push('\n');
        i += 1;
    }

    out
}

// ---------------------------------------------------------------------------
// Code fence rendering
// ---------------------------------------------------------------------------

fn render_code_fence(out: &mut String, lang: &str, code_lines: &[&str]) {
    use std::fmt::Write as _;

    let is_sql = matches!(lang, "sql" | "pgsql" | "postgresql" | "psql");

    if is_sql {
        // Join the code lines and run through the SQL highlighter.
        let code = code_lines.join("\n");
        let highlighted = crate::highlight::highlight_sql(&code, None);
        out.push_str(DIM);
        out.push_str("┌── sql ");
        out.push_str(RESET);
        out.push('\n');
        out.push_str(&highlighted);
        out.push('\n');
        out.push_str(DIM);
        out.push_str("└───────");
        out.push_str(RESET);
    } else {
        // Non-SQL code blocks: dim with a language label if present.
        let label = if lang.is_empty() {
            String::new()
        } else {
            format!("── {lang} ")
        };
        out.push_str(DIM);
        let _ = write!(out, "┌{label}");
        out.push_str(RESET);
        out.push('\n');
        for &code_line in code_lines {
            out.push_str(GREEN);
            out.push_str(code_line);
            out.push_str(RESET);
            out.push('\n');
        }
        out.push_str(DIM);
        out.push_str("└───────");
        out.push_str(RESET);
    }
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Table rendering
// ---------------------------------------------------------------------------

fn render_table_row(out: &mut String, line: &str, is_header: bool) {
    // Split on '|', skip first and last empty tokens.
    let mut cells: Vec<&str> = line
        .split('|')
        .skip(1) // leading '|' produces empty first token
        .collect();
    // Last cell may be empty from trailing '|'.
    if cells.last().is_some_and(|s: &&str| s.trim().is_empty()) {
        cells.pop();
    }

    for (idx, cell) in cells.iter().enumerate() {
        if idx == 0 {
            out.push_str("  ");
        } else {
            out.push_str(" │ ");
        }
        if is_header {
            out.push_str(BOLD);
            out.push_str(&inline(cell.trim()));
            out.push_str(RESET);
        } else {
            out.push_str(&inline(cell.trim()));
        }
    }
    out.push('\n');
}

// ---------------------------------------------------------------------------
// Inline markdown rendering
// ---------------------------------------------------------------------------

/// Process inline markdown (`**bold**`, `*italic*`, `` `code` ``, `[t](url)`)
/// within a single line of text.
fn inline(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 32);
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Link: [text](url)
        if chars[i] == '[' {
            if let Some((text, url, end)) = parse_link(&chars, i) {
                out.push_str(CYAN);
                out.push_str(&text);
                out.push_str(RESET);
                out.push_str(" (");
                out.push_str(DIM);
                out.push_str(&url);
                out.push_str(RESET);
                out.push(')');
                i = end;
                continue;
            }
        }

        // Bold: **text** or __text__
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let delim = chars[i];
            if let Some((text, end)) = parse_delimited(&chars, i, &[delim, delim]) {
                out.push_str(BOLD);
                out.push_str(&text);
                out.push_str(RESET);
                i = end;
                continue;
            }
        }

        // Italic: *text* or _text_
        if chars[i] == '*' || chars[i] == '_' {
            let delim = chars[i];
            // Make sure it's not a standalone or double delimiter.
            let not_double = i + 1 >= len || chars[i + 1] != delim;
            let not_escaped = i == 0 || chars[i - 1] != '\\';
            if not_double && not_escaped {
                if let Some((text, end)) = parse_delimited(&chars, i, &[delim]) {
                    out.push_str(DIM);
                    out.push_str(&text);
                    out.push_str(RESET);
                    i = end;
                    continue;
                }
            }
        }

        // Code span: `code`
        if chars[i] == '`' {
            // Support double-backtick spans too: ``code``
            let (delim_len, delim_char) = if i + 1 < len && chars[i + 1] == '`' {
                (2usize, "``")
            } else {
                (1usize, "`")
            };
            let delim_chars: Vec<char> = delim_char.chars().collect();
            if let Some((text, end)) = parse_delimited(&chars, i, &delim_chars) {
                out.push_str(GREEN);
                out.push_str(&text);
                out.push_str(RESET);
                i = end;
                continue;
            }
            // Unmatched backtick — emit literally and move past the delimiters.
            for _ in 0..delim_len {
                if i < len {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            continue;
        }

        // Escaped character: \*
        if chars[i] == '\\' && i + 1 < len {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

// ---------------------------------------------------------------------------
// Inline helpers
// ---------------------------------------------------------------------------

/// Parse `[text](url)` starting at `pos`.  Returns `(text, url, end_pos)`.
fn parse_link(chars: &[char], pos: usize) -> Option<(String, String, usize)> {
    let len = chars.len();
    // Find closing ']'.
    let mut j = pos + 1;
    while j < len && chars[j] != ']' {
        j += 1;
    }
    if j >= len {
        return None;
    }
    let text: String = chars[pos + 1..j].iter().collect();
    // Expect '(' immediately after ']'.
    if j + 1 >= len || chars[j + 1] != '(' {
        return None;
    }
    let url_start = j + 2;
    let mut k = url_start;
    while k < len && chars[k] != ')' {
        k += 1;
    }
    if k >= len {
        return None;
    }
    let url: String = chars[url_start..k].iter().collect();
    Some((text, url, k + 1))
}

/// Parse a delimited span (`delim … delim`) starting at `pos`.
///
/// Returns the text content (without delimiters) and the position *after*
/// the closing delimiter, or `None` if no closing delimiter is found on the
/// same line.
fn parse_delimited(chars: &[char], pos: usize, delim: &[char]) -> Option<(String, usize)> {
    let len = chars.len();
    let dlen = delim.len();

    // Skip opening delimiter.
    let start = pos + dlen;
    if start >= len {
        return None;
    }

    // The first character after the opening delimiter must not be whitespace
    // (CommonMark rule: avoids treating `* item` as italic).
    if chars[start].is_whitespace() {
        return None;
    }

    let mut j = start;
    while j + dlen <= len {
        // Check for closing delimiter.
        if chars[j..j + dlen] == *delim {
            // Don't allow closing delimiter preceded by whitespace for
            // single-char delimiters (avoids `foo *bar * baz` matching).
            if dlen == 1 && j > start && chars[j - 1].is_whitespace() {
                j += 1;
                continue;
            }
            let text: String = chars[start..j].iter().collect();
            return Some((text, j + dlen));
        }
        j += 1;
    }

    None
}

// ---------------------------------------------------------------------------
// Line classification helpers
// ---------------------------------------------------------------------------

/// Return the list item content if `line` is a bullet list item (`-`, `*`,
/// or `+` followed by a space).  Returns `None` otherwise.
fn parse_bullet(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for prefix in &["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

/// Return `(number_str, rest)` if `line` is a numbered list item.
fn parse_numbered(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    // Find the '.' that ends the number.
    let dot = trimmed.find('.')?;
    let num_part = &trimmed[..dot];
    // Must be all digits.
    if num_part.is_empty() || !num_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let rest = trimmed[dot + 1..].trim_start();
    Some((format!("{num_part}."), rest))
}

/// Returns `true` if the line is a horizontal rule (`---`, `***`, `___`
/// with optional surrounding whitespace, at least 3 chars).
fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let first = trimmed.chars().next().unwrap_or(' ');
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    trimmed.chars().all(|c| c == first || c == ' ')
}

/// Returns `true` if the line is a table separator row (`|---|---|`).
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') {
        return false;
    }
    trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// Count leading space characters in `line`.
fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

// ---------------------------------------------------------------------------
// Terminal width
// ---------------------------------------------------------------------------

/// Return the current terminal width, defaulting to 80 if unavailable.
fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Strip all ANSI escape sequences from `s`.
    fn strip(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
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

    fn contains_ansi(s: &str) -> bool {
        s.contains('\x1b')
    }

    // -----------------------------------------------------------------------
    // no_highlight pass-through
    // -----------------------------------------------------------------------

    #[test]
    fn no_highlight_returns_original() {
        let md = "# Heading\n**bold** text";
        let result = render_markdown(md, true);
        assert_eq!(result, md);
    }

    // -----------------------------------------------------------------------
    // Headers
    // -----------------------------------------------------------------------

    #[test]
    fn h1_renders_bold_yellow() {
        let result = render_markdown("# Title", false);
        assert!(contains_ansi(&result));
        assert!(strip(&result).contains("Title"));
        // Bold + yellow
        assert!(result.contains(BOLD));
        assert!(result.contains(YELLOW));
    }

    #[test]
    fn h2_renders_bold_yellow() {
        let result = render_markdown("## Subtitle", false);
        assert!(result.contains(BOLD_YELLOW));
        assert!(strip(&result).contains("Subtitle"));
    }

    #[test]
    fn h3_renders_bold_cyan() {
        let result = render_markdown("### Section", false);
        assert!(result.contains(BOLD_CYAN));
        assert!(strip(&result).contains("Section"));
    }

    // -----------------------------------------------------------------------
    // Horizontal rule
    // -----------------------------------------------------------------------

    #[test]
    fn horizontal_rule_renders_line() {
        let result = render_markdown("---", false);
        // Should contain multiple '─' characters (box drawing).
        assert!(result.contains('─'));
    }

    #[test]
    fn horizontal_rule_three_stars() {
        assert!(is_horizontal_rule("***"));
    }

    #[test]
    fn horizontal_rule_underscores() {
        assert!(is_horizontal_rule("___"));
    }

    #[test]
    fn not_a_horizontal_rule() {
        assert!(!is_horizontal_rule("--"));
        assert!(!is_horizontal_rule("hello"));
    }

    // -----------------------------------------------------------------------
    // Bold & italic
    // -----------------------------------------------------------------------

    #[test]
    fn bold_renders() {
        let result = render_markdown("**bold text**", false);
        assert!(result.contains(BOLD));
        assert!(strip(&result).contains("bold text"));
    }

    #[test]
    fn italic_renders() {
        let result = render_markdown("*italic text*", false);
        assert!(result.contains(DIM));
        assert!(strip(&result).contains("italic text"));
    }

    #[test]
    fn bold_with_double_underscore() {
        let result = render_markdown("__bold__", false);
        assert!(result.contains(BOLD));
        assert!(strip(&result).contains("bold"));
    }

    // -----------------------------------------------------------------------
    // Code spans
    // -----------------------------------------------------------------------

    #[test]
    fn code_span_renders_green() {
        let result = render_markdown("`SELECT 1`", false);
        assert!(result.contains(GREEN));
        assert!(strip(&result).contains("SELECT 1"));
    }

    // -----------------------------------------------------------------------
    // Code fences
    // -----------------------------------------------------------------------

    #[test]
    fn sql_fence_renders() {
        let md = "```sql\nSELECT 1;\n```";
        let result = render_markdown(md, false);
        // Should contain the SQL content.
        assert!(strip(&result).contains("SELECT 1;"));
        // The fence box should be present.
        assert!(result.contains('┌'));
    }

    #[test]
    fn generic_fence_renders_green() {
        let md = "```bash\necho hello\n```";
        let result = render_markdown(md, false);
        assert!(result.contains(GREEN));
        assert!(strip(&result).contains("echo hello"));
    }

    #[test]
    fn fence_without_lang_renders() {
        let md = "```\nplain code\n```";
        let result = render_markdown(md, false);
        assert!(strip(&result).contains("plain code"));
    }

    // -----------------------------------------------------------------------
    // Bullet lists
    // -----------------------------------------------------------------------

    #[test]
    fn bullet_dash_renders() {
        let result = render_markdown("- item one", false);
        assert!(strip(&result).contains('•'));
        assert!(strip(&result).contains("item one"));
    }

    #[test]
    fn bullet_star_renders() {
        let result = render_markdown("* item two", false);
        assert!(strip(&result).contains('•'));
        assert!(strip(&result).contains("item two"));
    }

    // -----------------------------------------------------------------------
    // Numbered lists
    // -----------------------------------------------------------------------

    #[test]
    fn numbered_list_renders() {
        let result = render_markdown("1. First item", false);
        assert!(strip(&result).contains("1."));
        assert!(strip(&result).contains("First item"));
    }

    // -----------------------------------------------------------------------
    // Tables
    // -----------------------------------------------------------------------

    #[test]
    fn table_renders_header_bold() {
        let md = "| Col A | Col B |\n|-------|-------|\n| val1  | val2  |";
        let result = render_markdown(md, false);
        let stripped = strip(&result);
        assert!(stripped.contains("Col A"));
        assert!(stripped.contains("Col B"));
        assert!(stripped.contains("val1"));
        // Header row should have BOLD in the raw result.
        assert!(result.contains(BOLD));
    }

    // -----------------------------------------------------------------------
    // Links
    // -----------------------------------------------------------------------

    #[test]
    fn link_renders_text_and_url() {
        let result = render_markdown("[Postgres docs](https://www.postgresql.org)", false);
        let stripped = strip(&result);
        assert!(stripped.contains("Postgres docs"));
        assert!(stripped.contains("https://www.postgresql.org"));
    }

    // -----------------------------------------------------------------------
    // Plain text preservation
    // -----------------------------------------------------------------------

    #[test]
    fn plain_text_preserved() {
        let text = "Just a normal sentence.";
        let result = render_markdown(text, false);
        assert_eq!(strip(&result).trim(), text);
    }

    // -----------------------------------------------------------------------
    // Mixed content
    // -----------------------------------------------------------------------

    #[test]
    fn mixed_content_renders() {
        let md = "# Title\n\nSome **bold** and *italic* text.\n\n- item\n\n`code here`";
        let result = render_markdown(md, false);
        let stripped = strip(&result);
        assert!(stripped.contains("Title"));
        assert!(stripped.contains("bold"));
        assert!(stripped.contains("italic"));
        assert!(stripped.contains("item"));
        assert!(stripped.contains("code here"));
    }

    // -----------------------------------------------------------------------
    // parse_bullet / parse_numbered
    // -----------------------------------------------------------------------

    #[test]
    fn parse_bullet_detects_dash() {
        assert_eq!(parse_bullet("- hello"), Some("hello"));
    }

    #[test]
    fn parse_bullet_detects_star() {
        assert_eq!(parse_bullet("* hello"), Some("hello"));
    }

    #[test]
    fn parse_bullet_returns_none_for_non_bullet() {
        assert!(parse_bullet("hello").is_none());
        assert!(parse_bullet("1. item").is_none());
    }

    #[test]
    fn parse_numbered_detects() {
        let r = parse_numbered("1. first");
        assert!(r.is_some());
        let (num, rest) = r.unwrap();
        assert_eq!(num, "1.");
        assert_eq!(rest, "first");
    }

    #[test]
    fn parse_numbered_returns_none_for_non_numeric() {
        assert!(parse_numbered("a. not a list").is_none());
        assert!(parse_numbered("- bullet").is_none());
    }

    // -----------------------------------------------------------------------
    // is_table_separator
    // -----------------------------------------------------------------------

    #[test]
    fn table_separator_detected() {
        assert!(is_table_separator("|---|---|"));
        assert!(is_table_separator("| :--- | ---: |"));
    }

    #[test]
    fn table_separator_rejects_non_separator() {
        assert!(!is_table_separator("| col | col |"));
        assert!(!is_table_separator("hello"));
    }
}
