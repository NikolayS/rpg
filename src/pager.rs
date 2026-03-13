//! Built-in TUI pager for query results.
//!
//! Enters alternate screen mode, displays pre-formatted text with vertical
//! and horizontal scrolling, and returns to the REPL when the user presses
//! `q`, `Esc`, or `Ctrl-C`.
//!
//! ## Search
//!
//! Press `/` to enter forward-search mode or `?` for backward search.
//! Type a pattern and press `Enter` to highlight all case-insensitive matches.
//! Use `n` / `N` to jump forward / backward through matches.
//! `Esc` during search input cancels without searching.
//! The status bar shows "Match M of N" while a search is active.
//!
//! ## Column Freezing
//!
//! Press `f` to cycle through frozen column counts (0 → 1 → 2 → … → max → 0).
//! Frozen columns are pinned at the left edge and remain visible during
//! horizontal scrolling. A `│` separator is drawn between frozen and
//! scrollable columns. The status bar shows "Frozen: N" when N > 0.

use std::io;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Display `content` in a full-screen scrollable pager.
///
/// The content is pre-formatted text (the same output that would normally
/// be printed to stdout). The pager enters alternate screen mode and
/// returns when the user presses `q`, `Esc`, or `Ctrl-C`.
///
/// Returns `Ok(())` on clean exit, or an error if terminal control fails.
pub fn run_pager(content: &str) -> io::Result<()> {
    // Split content into owned lines so they outlive this function's scope.
    let lines: Vec<String> = content.lines().map(ToOwned::to_owned).collect();

    // Enter raw mode and alternate screen.
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = PagerState::new(&lines);

    // Run the event loop; always restore terminal state afterward.
    let result = run_pager_loop(&mut terminal, &lines, &mut state);

    // Restore terminal state — must happen even on error.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);

    result
}

// ---------------------------------------------------------------------------
// Column boundary detection
// ---------------------------------------------------------------------------

/// Parse the byte offsets of `|` separator characters from a psql-formatted
/// header line.
///
/// For example, given `" id | name | value "`, returns the byte positions of
/// each `|` character: `[4, 11]`. These offsets are used to split lines into
/// frozen and scrollable portions.
///
/// The function scans the first non-divider line (a line that is not composed
/// entirely of `-`, `+`, and whitespace) among the first few lines of content.
pub fn detect_col_boundaries(lines: &[String]) -> Vec<usize> {
    // Find the first line that looks like a data/header row (has `|`).
    let header = lines
        .iter()
        .find(|l| l.contains('|') && !is_divider_line(l));
    let Some(line) = header else {
        return Vec::new();
    };
    line.char_indices()
        .filter(|(_, c)| *c == '|')
        .map(|(i, _)| i)
        .collect()
}

/// Returns `true` if `line` is a psql horizontal-rule line like
/// `+------+-------+` or `--------+--------`.
fn is_divider_line(line: &str) -> bool {
    !line.is_empty()
        && line
            .chars()
            .all(|c| c == '-' || c == '+' || c == ' ' || c == '|')
        && line.contains('-')
}

// ---------------------------------------------------------------------------
// Search helpers
// ---------------------------------------------------------------------------

/// Find all case-insensitive occurrences of `pattern` across `lines`.
///
/// Returns a list of `(line_index, byte_offset)` pairs, one per match.
/// The matches are ordered top-to-bottom, left-to-right.
pub fn find_matches(lines: &[String], pattern: &str) -> Vec<(usize, usize)> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let needle = pattern.to_lowercase();
    let mut results = Vec::new();
    for (line_idx, line) in lines.iter().enumerate() {
        let haystack = line.to_lowercase();
        let mut start = 0;
        while let Some(pos) = haystack[start..].find(&needle) {
            results.push((line_idx, start + pos));
            start += pos + needle.len();
        }
    }
    results
}

/// Return the index of the first match in `matches` whose line is >= `from_line`,
/// searching forward. Wraps around if no match is found after `from_line`.
///
/// Returns `None` when `matches` is empty.
fn first_match_from(matches: &[(usize, usize)], from_line: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    // Try to find a match at or after `from_line`.
    if let Some(idx) = matches.iter().position(|&(line, _)| line >= from_line) {
        return Some(idx);
    }
    // Wrap: return the very first match.
    Some(0)
}

/// Return the index of the last match in `matches` whose line is <= `before_line`,
/// searching backward. Wraps around if no match is found before `before_line`.
///
/// Returns `None` when `matches` is empty.
fn last_match_before(matches: &[(usize, usize)], before_line: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    // Try to find the last match at or before `before_line`.
    if let Some(idx) = matches.iter().rposition(|&(line, _)| line <= before_line) {
        return Some(idx);
    }
    // Wrap: return the very last match.
    Some(matches.len() - 1)
}

// ---------------------------------------------------------------------------
// Pager state
// ---------------------------------------------------------------------------

/// All mutable state for the pager, gathered in one struct.
struct PagerState {
    scroll_y: usize,
    scroll_x: usize,
    /// Active search pattern (after the user confirmed with Enter).
    search_pattern: Option<String>,
    /// All match positions: (`line_index`, `byte_column`).
    match_positions: Vec<(usize, usize)>,
    /// Which match is currently "current" (0-based index into `match_positions`).
    current_match: usize,
    /// Some(buf) while the user is typing a search query; `None` otherwise.
    search_input: Option<String>,
    /// Direction of the pending search (`/` = forward, `?` = backward).
    search_forward: bool,
    /// Number of columns currently frozen at the left edge (0 = none).
    frozen_cols: usize,
    /// Byte offsets of `|` separators detected in the header line.
    col_boundaries: Vec<usize>,
}

impl PagerState {
    fn new(lines: &[String]) -> Self {
        Self {
            scroll_y: 0,
            scroll_x: 0,
            search_pattern: None,
            match_positions: Vec::new(),
            current_match: 0,
            search_input: None,
            search_forward: true,
            frozen_cols: 0,
            col_boundaries: detect_col_boundaries(lines),
        }
    }

    /// Apply a confirmed search pattern to `lines` and jump to the first
    /// relevant match.
    fn apply_search(&mut self, pattern: String, lines: &[String], forward: bool) {
        self.match_positions = find_matches(lines, &pattern);
        self.search_pattern = Some(pattern);

        if self.match_positions.is_empty() {
            self.current_match = 0;
            return;
        }

        let idx = if forward {
            first_match_from(&self.match_positions, self.scroll_y)
        } else {
            last_match_before(&self.match_positions, self.scroll_y)
        };

        if let Some(i) = idx {
            self.current_match = i;
            self.scroll_y = self.match_positions[i].0;
        }
    }

    /// Jump to the next match (forward).
    fn next_match(&mut self) {
        if self.match_positions.is_empty() {
            return;
        }
        self.current_match = (self.current_match + 1) % self.match_positions.len();
        self.scroll_y = self.match_positions[self.current_match].0;
    }

    /// Jump to the previous match (backward).
    fn prev_match(&mut self) {
        if self.match_positions.is_empty() {
            return;
        }
        self.current_match = self
            .current_match
            .checked_sub(1)
            .unwrap_or(self.match_positions.len() - 1);
        self.scroll_y = self.match_positions[self.current_match].0;
    }

    /// `true` while the user is in the search-input prompt.
    fn in_search_input(&self) -> bool {
        self.search_input.is_some()
    }

    /// Cycle frozen column count: 0 → 1 → … → `max_cols` → 0.
    fn toggle_freeze(&mut self) {
        let max_cols = self.col_boundaries.len();
        if max_cols == 0 {
            return;
        }
        self.frozen_cols = if self.frozen_cols >= max_cols {
            0
        } else {
            self.frozen_cols + 1
        };
        // Reset horizontal scroll when changing freeze level.
        self.scroll_x = 0;
    }

    /// Return the byte offset in a line where the frozen portion ends.
    ///
    /// When `frozen_cols` is N, we freeze everything up to (and including)
    /// the Nth `|` character, so the split point is just after that `|`.
    /// Returns `None` when no columns are frozen.
    fn frozen_split_byte(&self) -> Option<usize> {
        if self.frozen_cols == 0 {
            return None;
        }
        // col_boundaries holds positions of each `|`.
        // frozen_cols == 1 means we keep everything up through boundary[0].
        let boundary_idx = self.frozen_cols.saturating_sub(1);
        self.col_boundaries.get(boundary_idx).map(|&pos| pos + 1) // +1 to include the `|` itself
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Build a single `Line` for `raw_line` at `line_idx` with horizontal scroll
/// and search-match highlighting applied to the `display_str` slice.
///
/// `col_offset` is the byte offset into `raw_line` where `display_str` starts.
fn build_line_from_slice<'a>(
    raw_line: &'a str,
    display_str: &'a str,
    col_offset: usize,
    line_idx: usize,
    pat_len: usize,
    match_positions: &[(usize, usize)],
    current_match: usize,
) -> Line<'a> {
    if pat_len == 0 {
        return Line::from(display_str.to_owned());
    }

    // Gather (byte_col_in_raw_line, global_match_index) for matches on this line.
    let line_matches: Vec<(usize, usize)> = match_positions
        .iter()
        .enumerate()
        .filter(|(_, &(l, _))| l == line_idx)
        .map(|(match_idx, &(_, col))| (col, match_idx))
        .collect();

    if line_matches.is_empty() {
        return Line::from(display_str.to_owned());
    }

    let highlight_style = Style::default().bg(Color::Yellow).fg(Color::Black);
    let current_highlight_style = Style::default().bg(Color::LightYellow).fg(Color::Black);

    // Build spans by splitting around each match.
    // `cursor` tracks our position in `display_str` (byte offset within it).
    let mut spans: Vec<Span> = Vec::new();
    let mut cursor = 0usize;

    // Ensure `raw_line` and `display_str` are consistent (display_str is a
    // sub-slice of raw_line starting at col_offset).
    let _ = raw_line; // acknowledged; col_offset is the bridge

    for (col, match_idx) in &line_matches {
        // `col` is a byte offset in `raw_line`.
        // Skip matches entirely to the left of the visible slice.
        if *col + pat_len <= col_offset {
            continue;
        }

        // Start of match in `display_str` coordinates.
        let match_start = col.saturating_sub(col_offset);

        // Already rendered past this match start — skip.
        if match_start < cursor {
            continue;
        }

        // Plain text before the match.
        if match_start > cursor {
            let end = match_start.min(display_str.len());
            if display_str.is_char_boundary(cursor) && display_str.is_char_boundary(end) {
                spans.push(Span::raw(display_str[cursor..end].to_owned()));
                cursor = end;
            }
        }

        // The highlighted match span.
        let match_end = (match_start + pat_len).min(display_str.len());
        if display_str.is_char_boundary(cursor)
            && display_str.is_char_boundary(match_end)
            && match_end > cursor
        {
            let style = if *match_idx == current_match {
                current_highlight_style
            } else {
                highlight_style
            };
            spans.push(Span::styled(
                display_str[cursor..match_end].to_owned(),
                style,
            ));
            cursor = match_end;
        }
    }

    // Remaining text after the last match.
    if cursor < display_str.len() && display_str.is_char_boundary(cursor) {
        spans.push(Span::raw(display_str[cursor..].to_owned()));
    }

    Line::from(spans)
}

/// Build a single `Line` for `line` at `line_idx`, applying horizontal scroll
/// and search-match highlighting.
fn build_line<'a>(
    line: &'a str,
    line_idx: usize,
    col_offset: usize,
    pat_len: usize,
    match_positions: &[(usize, usize)],
    current_match: usize,
) -> Line<'a> {
    let display_str: &str = if col_offset < line.len() {
        &line[col_offset..]
    } else {
        ""
    };
    build_line_from_slice(
        line,
        display_str,
        col_offset,
        line_idx,
        pat_len,
        match_positions,
        current_match,
    )
}

/// Build a `Line` with a frozen prefix, a `│` separator, and a scrollable
/// suffix (with `scroll_x` applied to the suffix).
fn build_line_frozen<'a>(
    line: &'a str,
    line_idx: usize,
    frozen_end: usize,
    scroll_x: usize,
    pat_len: usize,
    match_positions: &[(usize, usize)],
    current_match: usize,
) -> Line<'a> {
    // --- Frozen portion ---
    let frozen_str: &str = if frozen_end <= line.len() {
        // Clamp to a valid char boundary.
        let end = if line.is_char_boundary(frozen_end) {
            frozen_end
        } else {
            // Walk back to the nearest char boundary.
            (0..frozen_end)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0)
        };
        &line[..end]
    } else {
        line
    };

    // --- Scrollable portion (starts right after the frozen section) ---
    let scrollable_start = frozen_end.min(line.len());
    let scrollable_raw: &str = if scrollable_start < line.len() {
        &line[scrollable_start..]
    } else {
        ""
    };
    let scroll_col = scroll_x.min(scrollable_raw.len());
    // Clamp to char boundary.
    let scroll_col = (0..=scroll_col)
        .rev()
        .find(|&i| scrollable_raw.is_char_boundary(i))
        .unwrap_or(0);
    let scrollable_str: &str = &scrollable_raw[scroll_col..];

    // Absolute col_offset for the scrollable part in `line` coordinates.
    let scrollable_col_offset = scrollable_start + scroll_col;

    // Build each portion with highlight support.
    let frozen_line = build_line_from_slice(
        line,
        frozen_str,
        0,
        line_idx,
        pat_len,
        match_positions,
        current_match,
    );
    let scrollable_line = build_line_from_slice(
        line,
        scrollable_str,
        scrollable_col_offset,
        line_idx,
        pat_len,
        match_positions,
        current_match,
    );

    // Combine: frozen spans + separator + scrollable spans.
    let sep_style = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span> = frozen_line.spans;
    spans.push(Span::styled("\u{2502}", sep_style)); // │
    spans.extend(scrollable_line.spans);

    Line::from(spans)
}

/// Draw one frame of the pager into `frame`.
fn draw_frame(
    frame: &mut ratatui::Frame,
    lines: &[String],
    state: &PagerState,
    content_height: usize,
    max_scroll_y: usize,
) {
    let area = frame.area();
    let content_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    let status_area = Rect {
        x: area.x,
        y: area.height.saturating_sub(1),
        width: area.width,
        height: 1,
    };

    let pat_len = state.search_pattern.as_deref().map_or(0, str::len);
    let frozen_split = state.frozen_split_byte();

    let visible_lines: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(state.scroll_y)
        .take(content_height)
        .map(|(line_idx, line)| {
            if let Some(frozen_end) = frozen_split {
                build_line_frozen(
                    line,
                    line_idx,
                    frozen_end,
                    state.scroll_x,
                    pat_len,
                    &state.match_positions,
                    state.current_match,
                )
            } else {
                build_line(
                    line,
                    line_idx,
                    state.scroll_x,
                    pat_len,
                    &state.match_positions,
                    state.current_match,
                )
            }
        })
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, content_area);

    // Status bar — search input prompt, match info, or normal hints.
    let status_text = if let Some(ref buf) = state.search_input {
        let prefix = if state.search_forward { "/" } else { "?" };
        format!("{prefix}{buf}")
    } else {
        let pct = if max_scroll_y == 0 {
            100usize
        } else {
            (state.scroll_y * 100) / max_scroll_y
        };
        let last_visible = (state.scroll_y + content_height).min(lines.len());
        let mut base = format!(
            " Lines {}-{} of {} ({pct}%) \
             \u{2014} q:quit \u{2191}\u{2193}:scroll PgUp/PgDn:page /:search f:freeze",
            state.scroll_y + 1,
            last_visible,
            lines.len(),
        );
        if state.frozen_cols > 0 {
            base = format!("{base} \u{2014} Frozen: {}", state.frozen_cols);
        }
        if !state.match_positions.is_empty() {
            format!(
                "{base} \u{2014} Match {} of {}",
                state.current_match + 1,
                state.match_positions.len(),
            )
        } else if state.search_pattern.is_some() {
            format!("{base} \u{2014} No matches")
        } else {
            base
        }
    };

    let status = Paragraph::new(Line::from(Span::styled(
        status_text,
        Style::default().fg(Color::Black).bg(Color::White),
    )));
    frame.render_widget(status, status_area);
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

/// Compute the maximum horizontal scroll offset given the current state and
/// terminal dimensions.
fn max_scroll_x(lines: &[String], state: &PagerState, content_width: usize) -> usize {
    let max_line_width = lines.iter().map(String::len).max().unwrap_or(0);
    let scrollable_width = if let Some(frozen_end) = state.frozen_split_byte() {
        // Frozen portion + `│` separator (1 display cell) consumes space.
        content_width.saturating_sub(frozen_end + 1)
    } else {
        content_width
    };
    let scrollable_content_width = if state.frozen_cols > 0 {
        let frozen_end = state.frozen_split_byte().unwrap_or(0);
        lines
            .iter()
            .map(|l| l.len().saturating_sub(frozen_end))
            .max()
            .unwrap_or(0)
    } else {
        max_line_width
    };
    scrollable_content_width.saturating_sub(scrollable_width)
}

/// Handle a key event while in search-input mode.
/// Returns `true` if the pager should quit.
fn handle_search_key(key: event::KeyEvent, state: &mut PagerState, lines: &[String]) -> bool {
    match key.code {
        KeyCode::Esc => {
            state.search_input = None;
        }
        KeyCode::Enter => {
            if let Some(pattern) = state.search_input.take() {
                let forward = state.search_forward;
                state.apply_search(pattern, lines, forward);
            }
        }
        KeyCode::Backspace => {
            if let Some(ref mut buf) = state.search_input {
                buf.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(ref mut buf) = state.search_input {
                buf.push(c);
            }
        }
        _ => {}
    }
    false
}

/// Handle a key event while in normal navigation mode.
/// Returns `true` if the pager should quit.
fn handle_nav_key(
    key: event::KeyEvent,
    state: &mut PagerState,
    max_scroll_y: usize,
    max_x: usize,
    content_height: usize,
) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
        KeyCode::Down | KeyCode::Char('j') => {
            if state.scroll_y < max_scroll_y {
                state.scroll_y += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.scroll_y = state.scroll_y.saturating_sub(1);
        }
        KeyCode::PageDown | KeyCode::Char(' ') => {
            state.scroll_y = (state.scroll_y + content_height).min(max_scroll_y);
        }
        KeyCode::PageUp | KeyCode::Char('b') => {
            state.scroll_y = state.scroll_y.saturating_sub(content_height);
        }
        KeyCode::Home | KeyCode::Char('g') => {
            state.scroll_y = 0;
        }
        KeyCode::End | KeyCode::Char('G') => {
            state.scroll_y = max_scroll_y;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            // Scroll 4 columns at a time.
            state.scroll_x = (state.scroll_x + 4).min(max_x);
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.scroll_x = state.scroll_x.saturating_sub(4);
        }
        KeyCode::Char('f') => {
            state.toggle_freeze();
        }
        KeyCode::Char('/') => {
            state.search_input = Some(String::new());
            state.search_forward = true;
        }
        KeyCode::Char('?') => {
            state.search_input = Some(String::new());
            state.search_forward = false;
        }
        KeyCode::Char('n') => {
            state.next_match();
        }
        KeyCode::Char('N') => {
            state.prev_match();
        }
        _ => {}
    }
    false
}

fn run_pager_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    lines: &[String],
    state: &mut PagerState,
) -> io::Result<()> {
    loop {
        let area = terminal.size()?;
        let content_height = area.height.saturating_sub(1) as usize;
        let content_width = area.width as usize;
        let max_y = lines.len().saturating_sub(content_height);
        let max_x = max_scroll_x(lines, state, content_width);

        terminal.draw(|frame| {
            draw_frame(frame, lines, state, content_height, max_y);
        })?;

        // Poll for input with a short timeout to remain responsive to resize.
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let quit = if state.in_search_input() {
                    handle_search_key(key, state, lines)
                } else {
                    handle_nav_key(key, state, max_y, max_x, content_height)
                };
                if quit {
                    break;
                }
            }
            // Non-key events (resize, mouse, etc.) are silently ignored;
            // the next draw call picks up any terminal-size change.
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public utility
// ---------------------------------------------------------------------------

/// Check whether `content` needs paging based on the terminal height.
///
/// Returns `true` if the number of lines in `content` exceeds `rows`.
pub fn needs_paging(content: &str, rows: usize) -> bool {
    content.lines().count() > rows
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        detect_col_boundaries, find_matches, first_match_from, is_divider_line, last_match_before,
        needs_paging,
    };

    // --- needs_paging ---

    #[test]
    fn test_needs_paging_empty() {
        assert!(!needs_paging("", 24));
    }

    #[test]
    fn test_needs_paging_fits() {
        let content = "line1\nline2\nline3";
        assert!(!needs_paging(content, 24));
    }

    #[test]
    fn test_needs_paging_exact() {
        // Exactly 3 lines in a 3-row terminal: does not need paging.
        let content = "line1\nline2\nline3";
        assert!(!needs_paging(content, 3));
    }

    #[test]
    fn test_needs_paging_exceeds() {
        // 4 lines in a 3-row terminal: needs paging.
        let content = "line1\nline2\nline3\nline4";
        assert!(needs_paging(content, 3));
    }

    #[test]
    fn test_needs_paging_single_line() {
        assert!(!needs_paging("only one line", 1));
        assert!(!needs_paging("only one line", 24));
    }

    #[test]
    fn test_needs_paging_zero_rows() {
        // Zero-row terminal: always needs paging when there's any content.
        assert!(needs_paging("one line", 0));
    }

    // --- find_matches ---

    #[test]
    fn test_find_matches_empty_pattern() {
        let lines = vec!["hello world".to_owned()];
        assert!(find_matches(&lines, "").is_empty());
    }

    #[test]
    fn test_find_matches_no_match() {
        let lines = vec!["hello world".to_owned(), "foo bar".to_owned()];
        assert!(find_matches(&lines, "zzz").is_empty());
    }

    #[test]
    fn test_find_matches_single_line_single_match() {
        let lines = vec!["hello world".to_owned()];
        let matches = find_matches(&lines, "world");
        assert_eq!(matches, vec![(0, 6)]);
    }

    #[test]
    fn test_find_matches_case_insensitive() {
        let lines = vec!["Hello WORLD hello".to_owned()];
        let matches = find_matches(&lines, "hello");
        // "Hello" at 0, "hello" at 12
        assert_eq!(matches, vec![(0, 0), (0, 12)]);
    }

    #[test]
    fn test_find_matches_multiple_lines() {
        let lines = vec![
            "foo bar".to_owned(),
            "no match here".to_owned(),
            "another foo".to_owned(),
        ];
        let matches = find_matches(&lines, "foo");
        assert_eq!(matches, vec![(0, 0), (2, 8)]);
    }

    #[test]
    fn test_find_matches_overlapping_not_supported() {
        // Non-overlapping: "aa" in "aaa" → two non-overlapping matches at 0
        // (the second 'a' at 1 forms another "aa" at 1, but our impl is non-overlapping).
        let lines = vec!["aaaa".to_owned()];
        let matches = find_matches(&lines, "aa");
        // Non-overlapping: [0, 2]
        assert_eq!(matches, vec![(0, 0), (0, 2)]);
    }

    #[test]
    fn test_find_matches_empty_lines() {
        let lines: Vec<String> = vec![];
        assert!(find_matches(&lines, "foo").is_empty());
    }

    // --- first_match_from ---

    #[test]
    fn test_first_match_from_empty() {
        assert_eq!(first_match_from(&[], 0), None);
    }

    #[test]
    fn test_first_match_from_at_start() {
        let matches = vec![(0, 0), (2, 3), (5, 1)];
        assert_eq!(first_match_from(&matches, 0), Some(0));
    }

    #[test]
    fn test_first_match_from_middle() {
        let matches = vec![(0, 0), (2, 3), (5, 1)];
        assert_eq!(first_match_from(&matches, 3), Some(2));
    }

    #[test]
    fn test_first_match_from_wraps() {
        // from_line beyond all matches → wraps to first
        let matches = vec![(0, 0), (2, 3)];
        assert_eq!(first_match_from(&matches, 10), Some(0));
    }

    // --- last_match_before ---

    #[test]
    fn test_last_match_before_empty() {
        assert_eq!(last_match_before(&[], 5), None);
    }

    #[test]
    fn test_last_match_before_at_end() {
        let matches = vec![(0, 0), (2, 3), (5, 1)];
        assert_eq!(last_match_before(&matches, 5), Some(2));
    }

    #[test]
    fn test_last_match_before_middle() {
        let matches = vec![(0, 0), (2, 3), (5, 1)];
        assert_eq!(last_match_before(&matches, 3), Some(1));
    }

    #[test]
    fn test_last_match_before_wraps() {
        // before_line before all matches → wraps to last
        let matches = vec![(3, 0), (5, 1)];
        assert_eq!(last_match_before(&matches, 1), Some(1));
    }

    // --- is_divider_line ---

    #[test]
    fn test_is_divider_line_true() {
        assert!(is_divider_line("+----+------+"));
        assert!(is_divider_line("------+------"));
        assert!(is_divider_line("+----------+"));
    }

    #[test]
    fn test_is_divider_line_false() {
        assert!(!is_divider_line(" id | name | value "));
        assert!(!is_divider_line("  1 | alice | 42   "));
        assert!(!is_divider_line(""));
    }

    // --- detect_col_boundaries ---

    #[test]
    fn test_detect_col_boundaries_empty() {
        let lines: Vec<String> = vec![];
        assert!(detect_col_boundaries(&lines).is_empty());
    }

    #[test]
    fn test_detect_col_boundaries_no_pipes() {
        let lines = vec!["hello world".to_owned(), "foo bar".to_owned()];
        assert!(detect_col_boundaries(&lines).is_empty());
    }

    #[test]
    fn test_detect_col_boundaries_single_separator() {
        // " id | name "  — pipe at position 4
        let lines = vec![" id | name ".to_owned()];
        let boundaries = detect_col_boundaries(&lines);
        assert_eq!(boundaries, vec![4]);
    }

    #[test]
    fn test_detect_col_boundaries_two_separators() {
        // " id | name | value "
        //  0123 4      5678901234
        let line = " id | name | value ".to_owned();
        // Count the bytes to find the `|` positions.
        let expected: Vec<usize> = line
            .char_indices()
            .filter(|(_, c)| *c == '|')
            .map(|(i, _)| i)
            .collect();
        let lines = vec![line];
        assert_eq!(detect_col_boundaries(&lines), expected);
        assert_eq!(expected.len(), 2);
    }

    #[test]
    fn test_detect_col_boundaries_skips_divider_line() {
        // The first line is a divider; the second is the real header.
        let lines = vec![
            "+----+------+".to_owned(),
            " id | name ".to_owned(),
            "  1 | alice".to_owned(),
        ];
        let boundaries = detect_col_boundaries(&lines);
        // Should pick " id | name " which has `|` at position 4.
        assert_eq!(boundaries, vec![4]);
    }

    #[test]
    fn test_detect_col_boundaries_psql_style() {
        // Typical psql aligned output:
        //  id | username | email
        // ----+----------+-------
        //   1 | alice    | a@b.com
        let lines = vec![
            " id | username | email  ".to_owned(),
            "----+----------+--------".to_owned(),
            "  1 | alice    | a@b.com".to_owned(),
        ];
        let boundaries = detect_col_boundaries(&lines);
        // Pipes are in the header line " id | username | email  "
        let expected: Vec<usize> = lines[0]
            .char_indices()
            .filter(|(_, c)| *c == '|')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(boundaries, expected);
        assert_eq!(boundaries.len(), 2);
    }
}
