//! Built-in TUI pager for query results.
//!
//! Enters alternate screen mode, displays pre-formatted text with vertical
//! and horizontal scrolling, and returns to the REPL when the user presses
//! `q`, `Esc`, or `Ctrl-C`.

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

    let mut scroll_y: usize = 0;
    let mut scroll_x: usize = 0;

    // Run the event loop; always restore terminal state afterward.
    let result = run_pager_loop(&mut terminal, &lines, &mut scroll_y, &mut scroll_x);

    // Restore terminal state — must happen even on error.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);

    result
}

/// Draw one frame of the pager into `frame`.
fn draw_frame(
    frame: &mut ratatui::Frame,
    lines: &[String],
    scroll_y: usize,
    scroll_x: usize,
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

    // Render content lines with horizontal scroll applied.
    let visible_lines: Vec<Line> = lines
        .iter()
        .skip(scroll_y)
        .take(content_height)
        .map(|line| {
            let display: &str = if scroll_x < line.len() {
                &line[scroll_x..]
            } else {
                ""
            };
            Line::from(display.to_owned())
        })
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, content_area);

    // Status bar.
    let pct = if max_scroll_y == 0 {
        100usize
    } else {
        (scroll_y * 100) / max_scroll_y
    };
    let last_visible = (scroll_y + content_height).min(lines.len());
    let status_text = format!(
        " Lines {}-{} of {} ({pct}%) \
         \u{2014} q:quit \u{2191}\u{2193}:scroll PgUp/PgDn:page",
        scroll_y + 1,
        last_visible,
        lines.len(),
    );
    let status = Paragraph::new(Line::from(Span::styled(
        status_text,
        Style::default().fg(Color::Black).bg(Color::White),
    )));
    frame.render_widget(status, status_area);
}

fn run_pager_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    lines: &[String],
    scroll_y: &mut usize,
    scroll_x: &mut usize,
) -> io::Result<()> {
    loop {
        let area = terminal.size()?;
        // Reserve 1 line for status bar at bottom.
        let content_height = area.height.saturating_sub(1) as usize;
        let content_width = area.width as usize;
        let max_scroll_y = lines.len().saturating_sub(content_height);
        let max_line_width = lines.iter().map(String::len).max().unwrap_or(0);
        let max_scroll_x = max_line_width.saturating_sub(content_width);

        terminal.draw(|frame| {
            draw_frame(
                frame,
                lines,
                *scroll_y,
                *scroll_x,
                content_height,
                max_scroll_y,
            );
        })?;

        // Poll for input with a short timeout to remain responsive to resize.
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if *scroll_y < max_scroll_y {
                            *scroll_y += 1;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *scroll_y = scroll_y.saturating_sub(1);
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        *scroll_y = (*scroll_y + content_height).min(max_scroll_y);
                    }
                    KeyCode::PageUp | KeyCode::Char('b') => {
                        *scroll_y = scroll_y.saturating_sub(content_height);
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        *scroll_y = 0;
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        *scroll_y = max_scroll_y;
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        if *scroll_x < max_scroll_x {
                            // Scroll 4 columns at a time.
                            *scroll_x = (*scroll_x + 4).min(max_scroll_x);
                        }
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        *scroll_x = scroll_x.saturating_sub(4);
                    }
                    _ => {}
                }
            }
            // Non-key events (resize, mouse, etc.) are silently ignored;
            // the next draw call picks up any terminal-size change.
        }
    }
    Ok(())
}

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
    use super::needs_paging;

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
}
