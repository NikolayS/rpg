//! Interactive TUI history picker for `\s`.
//!
//! Opens a full-screen picker that lets the user filter and select a history
//! entry by typing a substring pattern.  Returns `Some(entry)` when the user
//! presses Enter, or `None` when they press Esc or Ctrl-C.

use std::io::{self, IsTerminal, Write};

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Terminal,
};

// ---------------------------------------------------------------------------
// TerminalGuard — RAII wrapper for raw mode + alternate screen
// ---------------------------------------------------------------------------

/// RAII guard that enables raw mode and enters the alternate screen on
/// construction, then restores the terminal unconditionally on drop —
/// even if the caller panics.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let mut stdout = io::stdout();
        let _ = stdout.write_all(b"\x1b[H\x1b[2J\x1b[H");
        let _ = stdout.flush();
        let _ = io::stderr().write_all(b"\x1b[r");
        let _ = io::stderr().flush();
    }
}

// ---------------------------------------------------------------------------
// PickerState
// ---------------------------------------------------------------------------

/// Mutable state for the history picker event loop.
struct PickerState {
    /// All history entries in reverse chronological order (most recent first).
    entries: Vec<String>,
    /// Current filter string as typed by the user.
    filter: String,
    /// Indices into `entries` of the entries that match `filter`.
    matches: Vec<usize>,
    /// Index within `matches` of the currently highlighted row.
    selected: usize,
}

impl PickerState {
    fn new(entries: Vec<String>) -> Self {
        let count = entries.len();
        Self {
            entries,
            filter: String::new(),
            matches: (0..count).collect(),
            selected: 0,
        }
    }

    /// Recompute `matches` after `filter` changes.
    fn update_matches(&mut self) {
        let filter_lc = self.filter.to_lowercase();
        self.matches = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| filter_lc.is_empty() || e.to_lowercase().contains(&filter_lc))
            .map(|(i, _)| i)
            .collect();
        if !self.matches.is_empty() {
            self.selected = self.selected.min(self.matches.len() - 1);
        } else {
            self.selected = 0;
        }
    }

    /// Return the currently highlighted history entry, if any.
    fn selected_entry(&self) -> Option<&str> {
        self.matches
            .get(self.selected)
            .and_then(|&i| self.entries.get(i))
            .map(String::as_str)
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.matches.len() {
            self.selected += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate `s` to at most `max_chars` Unicode scalar values, appending `…`
/// when the string is shortened.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the interactive history picker.
///
/// `entries` must be in **chronological order** (oldest first), as returned by
/// `read_history_entries()`.  The picker reverses them internally so that the
/// most recent entry appears at the top of the list.
///
/// Returns `Some(query)` when the user confirms a selection with Enter, or
/// `None` when they cancel with Esc or Ctrl-C.
///
/// Returns `Err` when terminal setup fails (e.g. stdin is not a TTY).
pub fn run(entries: Vec<String>) -> io::Result<Option<String>> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "history picker requires a TTY",
        ));
    }

    // Reverse so the most-recent entry appears first.
    let mut reversed = entries;
    reversed.reverse();

    let _guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut state = PickerState::new(reversed);

    loop {
        let total = state.entries.len();
        let match_count = state.matches.len();

        terminal.draw(|f| {
            let area = f.area();

            let title = if state.filter.is_empty() {
                format!(" History ─── {total} entries ")
            } else {
                format!(" History ─── {match_count} matches ")
            };

            let outer = Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(" ↑↓ navigate · Enter select · Esc cancel ");
            let inner = outer.inner(area);
            f.render_widget(outer, area);

            // Split inner area: filter bar (1 line) on top, list below.
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(inner);

            // Filter / search bar.
            let filter_display = format!("> {}", state.filter);
            f.render_widget(
                Paragraph::new(filter_display).style(Style::default().fg(Color::Yellow)),
                chunks[0],
            );

            // Compute which rows are visible (manual scrolling window).
            let list_height = chunks[1].height as usize;
            let visible_start = if state.selected >= list_height {
                state.selected - list_height + 1
            } else {
                0
            };
            // Leave 2 chars for the "▶ " / "  " prefix.
            let entry_width = chunks[1].width.saturating_sub(2) as usize;

            let items: Vec<ListItem> = state
                .matches
                .iter()
                .skip(visible_start)
                .take(list_height)
                .enumerate()
                .map(|(offset, &entry_idx)| {
                    let row = visible_start + offset;
                    let text = truncate(&state.entries[entry_idx], entry_width);
                    let is_selected = row == state.selected;
                    let (prefix, style) = if is_selected {
                        (
                            "▶ ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        ("  ", Style::default())
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(prefix, style),
                        Span::styled(text, style),
                    ]))
                })
                .collect();

            f.render_widget(List::new(items), chunks[1]);
        })?;

        if !event::poll(std::time::Duration::from_millis(50))? {
            continue;
        }

        let Event::Key(key) = event::read()? else {
            continue;
        };

        match (key.code, key.modifiers) {
            // Cancel.
            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                return Ok(None);
            }
            // Confirm selection.
            (KeyCode::Enter, _) => {
                return Ok(state.selected_entry().map(ToOwned::to_owned));
            }
            // Navigate up.
            (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                state.move_up();
            }
            // Navigate down.
            (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                state.move_down();
            }
            // Erase last filter character.
            (KeyCode::Backspace, _) => {
                state.filter.pop();
                state.update_matches();
            }
            // Append a printable character to the filter.
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                state.filter.push(c);
                state.update_matches();
            }
            _ => {}
        }
    }
}
