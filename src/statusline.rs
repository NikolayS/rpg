//! Persistent one-line status bar for the interactive REPL (FR-25).
#![cfg(not(feature = "wasi"))]
//!
//! Renders a status line at the bottom of the terminal using direct ANSI
//! escape sequences.  Uses the terminal scroll region (CSR) to reserve the
//! last row so that normal output does not overwrite it.
//!
//! Format:
//! ```text
//!  db-host:5432/mydb │ SQL │ tx:idle │ last: 12ms │ ai: 847/4096 tok
//! ```
//!
//! The status bar writes to **stderr** so that it does not mix with query
//! output on stdout.

use std::io::{self, IsTerminal, Write};

use crate::repl::{AutoExplain, ExecMode, InputMode, TxState};

// ---------------------------------------------------------------------------
// Status bar state
// ---------------------------------------------------------------------------

/// Persistent status bar displayed at the bottom of the terminal.
pub struct StatusLine {
    /// Whether the status bar is enabled.
    pub enabled: bool,
    /// Cached terminal width (columns).
    term_cols: u16,
    /// Cached terminal height (rows).  The status bar occupies this row.
    term_rows: u16,
    /// Connection label: `host:port/dbname`.
    conn_label: String,
    /// Current input mode.
    input_mode: InputMode,
    /// Current execution mode.
    exec_mode: ExecMode,
    /// Current transaction state.
    tx_state: TxState,
    /// Duration of the last query (milliseconds), or `None` if no query yet.
    last_duration_ms: Option<u64>,
    /// Cumulative AI tokens used this session.
    ai_tokens_used: u64,
    /// Configured AI token budget (0 = unlimited / no AI configured).
    ai_token_budget: u32,
    /// Current auto-EXPLAIN level (shown when not Off).
    auto_explain: AutoExplain,
}

impl StatusLine {
    /// Create a new `StatusLine`.
    ///
    /// The bar is enabled by default only when stderr is a terminal.
    /// Pass `enabled = false` for non-interactive / piped sessions.
    pub fn new(enabled: bool) -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            enabled,
            term_cols: cols,
            term_rows: rows,
            conn_label: String::new(),
            input_mode: InputMode::default(),
            exec_mode: ExecMode::default(),
            tx_state: TxState::default(),
            last_duration_ms: None,
            ai_tokens_used: 0,
            ai_token_budget: 0,
            auto_explain: AutoExplain::Off,
        }
    }

    /// Return `true` when stderr is a terminal (interactive session).
    pub fn is_interactive() -> bool {
        io::stderr().is_terminal()
    }

    /// Set the connection label (`host:port/dbname`).
    pub fn set_connection(&mut self, host: &str, port: u16, dbname: &str) {
        self.conn_label = format!("{host}:{port}/{dbname}");
    }

    /// Update state after a query completes and re-render.
    pub fn update(
        &mut self,
        tx_state: TxState,
        duration_ms: u64,
        tokens_used: u64,
        token_budget: u32,
        input_mode: InputMode,
        exec_mode: ExecMode,
    ) {
        self.tx_state = tx_state;
        self.last_duration_ms = Some(duration_ms);
        self.ai_tokens_used = tokens_used;
        self.ai_token_budget = token_budget;
        self.input_mode = input_mode;
        self.exec_mode = exec_mode;
        self.render();
    }

    /// Correct the displayed transaction state and re-render.
    ///
    /// Used when the REPL knows the true server-side state differs from what
    /// the last `update()` recorded (e.g. after an internal read-only
    /// transaction whose trailing `commit;` is invisible to `update_from_sql`).
    pub fn set_tx_state(&mut self, state: TxState) {
        self.tx_state = state;
        self.render();
    }

    /// Set the current auto-EXPLAIN level and re-render.
    pub fn set_auto_explain(&mut self, mode: AutoExplain) {
        self.auto_explain = mode;
        self.render();
    }

    /// Refresh the cached terminal size (call on SIGWINCH / resize events).
    ///
    /// Re-installs the scroll region **only when the terminal size changed**.
    /// `setup_scroll_region()` emits DECSTBM, which unconditionally moves the
    /// cursor to the home position (row 1, col 1) as a side effect.  The REPL
    /// loop calls this method before every prompt, so emitting DECSTBM every
    /// iteration would reset the cursor to row 1 before each prompt.
    pub fn on_resize(&mut self) {
        let old_rows = self.term_rows;
        let old_cols = self.term_cols;
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            self.term_cols = cols;
            self.term_rows = rows;
        }
        if self.term_rows != old_rows || self.term_cols != old_cols {
            self.setup_scroll_region();
            // DECSTBM moves cursor to row 1.  Restore it to the bottom of the
            // scroll region so the next prompt appears at the correct position.
            if self.enabled {
                let bottom = self.term_rows.saturating_sub(1);
                let _ = write!(io::stderr(), "\x1b[{bottom};1H");
                let _ = io::stderr().flush();
            }
        }
        self.render();
    }

    /// Install the terminal scroll region, reserving the last row for the
    /// status bar.  Call once at REPL startup.
    pub fn setup_scroll_region(&self) {
        if !self.enabled {
            return;
        }
        let last = self.term_rows;
        // Set scroll region to rows 1 .. (last-1), leaving the final row free.
        // ANSI: ESC [ top ; bottom r   (1-based)
        let _ = write!(io::stderr(), "\x1b[1;{}r", last.saturating_sub(1));
        let _ = io::stderr().flush();
    }

    /// Re-install the scroll region and reposition the cursor to the bottom
    /// of the scroll region.
    ///
    /// Call this after returning from the built-in pager.  `LeaveAlternateScreen`
    /// restores the main screen buffer but the DECSTBM scroll region constraint
    /// may still be active; `setup_scroll_region()` re-establishes it, which
    /// moves the cursor to row 1 as a side-effect.  This method then moves the
    /// cursor back to the bottom of the scroll region so the next prompt
    /// appears at the correct position.
    pub fn setup_scroll_region_and_restore_cursor(&self) {
        self.setup_scroll_region();
        if !self.enabled {
            return;
        }
        let bottom = self.term_rows.saturating_sub(1);
        let _ = write!(io::stderr(), "\x1b[{bottom};1H");
        let _ = io::stderr().flush();
    }

    /// Restore the full scroll region.  Call at REPL exit.
    pub fn teardown_scroll_region(&self) {
        if !self.enabled {
            return;
        }
        // Reset scroll region to the full terminal.
        let _ = write!(io::stderr(), "\x1b[r");
        // Clear the status bar row.
        self.clear_row();
        let _ = io::stderr().flush();
    }

    /// Clear the status bar row (used before pager handoff and at exit).
    pub fn clear(&self) {
        if !self.enabled {
            return;
        }
        self.clear_row();
        let _ = io::stderr().flush();
    }

    /// Render the status bar to stderr.
    pub fn render(&self) {
        if !self.enabled {
            return;
        }
        let content = self.format_status();
        self.write_status_row(&content);
    }

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Format the status string (without ANSI codes, padded to terminal width).
    fn format_status(&self) -> String {
        // Mode label.
        let mode = match self.exec_mode {
            ExecMode::Interactive => match self.input_mode {
                InputMode::Sql => "SQL",
                InputMode::Text2Sql => "text2sql",
            },
            ExecMode::Plan => "plan",
            ExecMode::Yolo => "yolo",
        };

        // Transaction state label.
        let tx = match self.tx_state {
            TxState::Idle => "idle",
            TxState::InTransaction => "in-tx",
            TxState::Failed => "failed",
        };

        // Last query duration.
        let duration = match self.last_duration_ms {
            None => String::new(),
            Some(ms) if ms < 1000 => format!(" │ last: {ms}ms"),
            #[allow(clippy::cast_precision_loss)]
            Some(ms) => format!(" │ last: {:.1}s", ms as f64 / 1000.0),
        };

        // AI token usage (only when a budget is configured or tokens were used).
        let ai = if self.ai_token_budget > 0 || self.ai_tokens_used > 0 {
            format!(" │ ai: {}/{}tok", self.ai_tokens_used, self.ai_token_budget)
        } else {
            String::new()
        };

        // Auto-EXPLAIN indicator (only when active).
        let explain = if self.auto_explain == AutoExplain::Off {
            String::new()
        } else {
            format!(" │ explain:{}", self.auto_explain.label())
        };

        // Assemble the status string.
        let conn = if self.conn_label.is_empty() {
            String::new()
        } else {
            format!(" {} │", self.conn_label)
        };
        let inner = format!("{conn} {mode} │ tx:{tx}{duration}{explain}{ai} ");

        // Pad or truncate to terminal width.
        let width = self.term_cols as usize;
        let char_count = inner.chars().count();
        if char_count < width {
            let pad = " ".repeat(width - char_count);
            format!("{inner}{pad}")
        } else {
            inner.chars().take(width).collect()
        }
    }

    /// Write `content` to the last terminal row using save/restore cursor.
    ///
    /// The bar uses reverse-video (`\x1b[7m`) as its base style.  Within that
    /// context a foreground-color code *becomes* the background, so we inject
    /// color codes around the `tx:` segment to signal transaction state:
    /// yellow for an open transaction, red for a failed one.
    fn write_status_row(&self, content: &str) {
        // Colorise the tx segment within reverse-video context.
        // \x1b[33m = yellow fg (→ yellow bg in reverse), \x1b[39m = reset fg.
        // \x1b[31m = red fg   (→ red bg in reverse),    \x1b[39m = reset fg.
        let colored: std::borrow::Cow<str> = match self.tx_state {
            TxState::Idle => content.into(),
            TxState::InTransaction => content
                .replacen("tx:in-tx", "\x1b[33mtx:in-tx\x1b[39m", 1)
                .into(),
            TxState::Failed => content
                .replacen("tx:failed", "\x1b[31mtx:failed\x1b[39m", 1)
                .into(),
        };

        let row = self.term_rows;
        let mut stderr = io::stderr();
        // \x1b[s         — save cursor position
        // \x1b[{row};1H  — move to last row, column 1 (1-based)
        // \x1b[7m        — reverse video (background fills to end of line)
        // {colored}      — status string with per-segment color codes
        // \x1b[K         — erase to end of line with current attributes
        // \x1b[0m        — reset attributes
        // \x1b[u         — restore cursor position
        let _ = write!(
            stderr,
            "\x1b[s\x1b[{row};1H\x1b[7m{colored}\x1b[K\x1b[0m\x1b[u"
        );
        let _ = stderr.flush();
    }

    /// Erase the status bar row without disturbing the cursor.
    fn clear_row(&self) {
        let row = self.term_rows;
        let mut stderr = io::stderr();
        // \x1b[2K erases the entire line; no need to manually write blanks.
        let _ = write!(stderr, "\x1b[s\x1b[{row};1H\x1b[2K\x1b[u");
        let _ = stderr.flush();
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::{AutoExplain, ExecMode, InputMode, TxState};

    /// Build a non-rendering `StatusLine` with fixed terminal dimensions.
    fn make_sl() -> StatusLine {
        let mut sl = StatusLine::new(false);
        sl.term_cols = 80;
        sl.term_rows = 24;
        sl
    }

    #[test]
    fn format_status_default_contains_sql_and_idle() {
        let sl = make_sl();
        let s = sl.format_status();
        assert!(s.contains("SQL"), "default mode should be SQL: {s:?}");
        assert!(s.contains("tx:idle"), "default tx should be idle: {s:?}");
    }

    #[test]
    fn format_status_with_connection_label() {
        let mut sl = make_sl();
        sl.set_connection("localhost", 5432, "mydb");
        let s = sl.format_status();
        assert!(
            s.contains("localhost:5432/mydb"),
            "connection label missing: {s:?}",
        );
    }

    #[test]
    fn format_status_no_duration_before_first_query() {
        let sl = make_sl();
        let s = sl.format_status();
        assert!(
            !s.contains("last:"),
            "no duration before first query: {s:?}"
        );
    }

    #[test]
    fn format_status_duration_milliseconds() {
        let mut sl = make_sl();
        sl.last_duration_ms = Some(250);
        let s = sl.format_status();
        assert!(s.contains("250ms"), "ms duration should be shown: {s:?}");
    }

    #[test]
    fn format_status_duration_999ms_uses_ms_suffix() {
        let mut sl = make_sl();
        sl.last_duration_ms = Some(999);
        let s = sl.format_status();
        assert!(s.contains("999ms"), "sub-second duration in ms: {s:?}");
    }

    #[test]
    fn format_status_duration_1000ms_uses_seconds() {
        let mut sl = make_sl();
        sl.last_duration_ms = Some(1000);
        let s = sl.format_status();
        assert!(s.contains("1.0s"), "1000ms should display as 1.0s: {s:?}");
    }

    #[test]
    fn format_status_duration_seconds() {
        let mut sl = make_sl();
        sl.last_duration_ms = Some(2500);
        let s = sl.format_status();
        assert!(
            s.contains("2.5s"),
            "seconds duration should be shown: {s:?}"
        );
    }

    #[test]
    fn format_status_in_transaction_state() {
        let mut sl = make_sl();
        sl.tx_state = TxState::InTransaction;
        let s = sl.format_status();
        assert!(s.contains("tx:in-tx"), "in-tx state label: {s:?}");
    }

    #[test]
    fn format_status_failed_transaction_state() {
        let mut sl = make_sl();
        sl.tx_state = TxState::Failed;
        let s = sl.format_status();
        assert!(s.contains("tx:failed"), "failed tx label: {s:?}");
    }

    #[test]
    fn format_status_ai_tokens_shown_when_budget_configured() {
        let mut sl = make_sl();
        sl.ai_token_budget = 4096;
        sl.ai_tokens_used = 100;
        let s = sl.format_status();
        assert!(s.contains("ai:"), "AI token section must appear: {s:?}");
        assert!(s.contains("100"), "used token count must appear: {s:?}");
        assert!(s.contains("4096"), "budget must appear: {s:?}");
    }

    #[test]
    fn format_status_ai_tokens_shown_when_tokens_used_and_no_budget() {
        let mut sl = make_sl();
        sl.ai_tokens_used = 42;
        // budget is 0 (default) but tokens were used
        let s = sl.format_status();
        assert!(
            s.contains("ai:"),
            "AI section must appear when tokens used: {s:?}"
        );
    }

    #[test]
    fn format_status_ai_tokens_hidden_by_default() {
        let sl = make_sl();
        let s = sl.format_status();
        assert!(
            !s.contains("ai:"),
            "AI section must be hidden by default: {s:?}"
        );
    }

    #[test]
    fn format_status_auto_explain_on_shown() {
        let mut sl = make_sl();
        sl.auto_explain = AutoExplain::On;
        let s = sl.format_status();
        assert!(
            s.contains("explain:on"),
            "auto-explain:on should appear: {s:?}"
        );
    }

    #[test]
    fn format_status_auto_explain_analyze_shown() {
        let mut sl = make_sl();
        sl.auto_explain = AutoExplain::Analyze;
        let s = sl.format_status();
        assert!(
            s.contains("explain:analyze"),
            "explain:analyze should appear: {s:?}"
        );
    }

    #[test]
    fn format_status_auto_explain_off_hidden() {
        let sl = make_sl();
        let s = sl.format_status();
        assert!(
            !s.contains("explain:"),
            "auto-explain:off must be hidden: {s:?}"
        );
    }

    #[test]
    fn format_status_exec_mode_plan() {
        let mut sl = make_sl();
        sl.exec_mode = ExecMode::Plan;
        let s = sl.format_status();
        assert!(s.contains("plan"), "plan mode label must appear: {s:?}");
    }

    #[test]
    fn format_status_exec_mode_yolo() {
        let mut sl = make_sl();
        sl.exec_mode = ExecMode::Yolo;
        let s = sl.format_status();
        assert!(s.contains("yolo"), "yolo mode label must appear: {s:?}");
    }

    #[test]
    fn format_status_input_mode_text2sql() {
        let mut sl = make_sl();
        sl.input_mode = InputMode::Text2Sql;
        let s = sl.format_status();
        assert!(
            s.contains("text2sql"),
            "text2sql mode label must appear: {s:?}"
        );
    }

    #[test]
    fn format_status_padded_to_terminal_width() {
        let mut sl = make_sl();
        sl.term_cols = 100;
        let s = sl.format_status();
        assert_eq!(
            s.chars().count(),
            100,
            "status string must be padded to terminal width",
        );
    }

    #[test]
    fn format_status_truncated_when_content_exceeds_terminal_width() {
        let mut sl = make_sl();
        sl.term_cols = 10;
        // A long connection label forces truncation.
        sl.conn_label = "very-long-host-name:5432/very-long-database-name".to_owned();
        let s = sl.format_status();
        assert_eq!(
            s.chars().count(),
            10,
            "status string must be truncated to terminal width",
        );
    }

    #[test]
    fn format_status_no_connection_label_when_empty() {
        let sl = make_sl();
        let s = sl.format_status();
        // Without a connection the label separator " │" should not appear
        // before the mode token.
        assert!(
            !s.starts_with(" │"),
            "no leading separator without connection: {s:?}"
        );
    }

    #[test]
    fn set_connection_updates_label() {
        let mut sl = make_sl();
        sl.set_connection("db.example.com", 5433, "prod");
        assert_eq!(sl.conn_label, "db.example.com:5433/prod");
    }
}
