//! Persistent one-line status bar for the interactive REPL (FR-25).
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

    /// Set the current auto-EXPLAIN level and re-render.
    pub fn set_auto_explain(&mut self, mode: AutoExplain) {
        self.auto_explain = mode;
        self.render();
    }

    /// Refresh the cached terminal size (call on SIGWINCH / resize events).
    pub fn on_resize(&mut self) {
        if let Ok((cols, rows)) = crossterm::terminal::size() {
            self.term_cols = cols;
            self.term_rows = rows;
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
    fn write_status_row(&self, content: &str) {
        let row = self.term_rows;
        let mut stderr = io::stderr();
        // \x1b[s       — save cursor position
        // \x1b[{row};0H — move to last row, column 1
        // \x1b[7m      — reverse video
        // {content}    — status string padded to width
        // \x1b[0m      — reset attributes
        // \x1b[u       — restore cursor position
        let _ = write!(stderr, "\x1b[s\x1b[{row};0H\x1b[7m{content}\x1b[0m\x1b[u");
        let _ = stderr.flush();
    }

    /// Erase the status bar row without disturbing the cursor.
    fn clear_row(&self) {
        let row = self.term_rows;
        let width = self.term_cols as usize;
        let blank = " ".repeat(width);
        let mut stderr = io::stderr();
        let _ = write!(stderr, "\x1b[s\x1b[{row};0H{blank}\x1b[u");
        let _ = stderr.flush();
    }
}
