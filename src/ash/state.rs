//! Drill-down state machine for `/ash`.
//!
//! Manages the four drill-down levels:
//!   1. `wait_event_type`
//!   2. `wait_event`
//!   3. `query_id`
//!   4. pid
//!
//! Also manages zoom/time-range for history mode.

use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Drill-down depth within the ASH view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DrillLevel {
    /// Top level: grouped by `wait_event_type`.
    #[default]
    WaitType,
    /// Second level: grouped by `wait_event`, filtered to one type.
    WaitEvent {
        /// The `wait_event_type` that was selected at the previous level.
        selected_type: String,
    },
    /// Third level: grouped by `query_id`, filtered to one event.
    QueryId {
        /// The `wait_event_type` selected two levels up.
        selected_type: String,
        /// The `wait_event` that was selected at the previous level.
        selected_event: String,
    },
    /// Fourth level: individual PIDs.
    Pid {
        /// The `wait_event_type` selected three levels up.
        selected_type: String,
        /// The `wait_event` selected two levels up.
        selected_event: String,
        /// The `query_id` selected at the previous level (may be None).
        selected_query_id: Option<i64>,
    },
}

/// Time range mode for the ASH view.
#[derive(Debug, Clone)]
pub enum ViewMode {
    Live,
    History { from: SystemTime, to: SystemTime },
}

/// Top-level state for the ASH TUI.
#[derive(Debug)]
pub struct AshState {
    /// Current drill-down level.
    pub level: DrillLevel,
    /// Time range mode (live vs. history with explicit bounds).
    pub mode: ViewMode,
    /// Index of the highlighted row in the drill-down table.
    pub selected_row: usize,
    /// Refresh interval in seconds. Valid values: 1, 5, 10.
    pub refresh_interval_secs: u64,
    /// Whether `pg_ash` is installed on the server.
    ///
    /// When true, historical data from `ash.samples` is used to pre-populate
    /// the ring buffer on startup and history mode can query wider windows.
    /// Stored for future use (e.g. status-bar indicator); branching currently
    /// uses `PgAshInfo.installed` directly in the event loop.
    #[allow(dead_code)]
    pub pg_ash_installed: bool,

    // --- renderer-compatible aliases kept in sync with `mode` and
    //     `refresh_interval_secs` to avoid breaking renderer.rs ---
    /// True when `mode` is `ViewMode::History`. Mirrors `mode`.
    pub is_history: bool,
    /// Refresh interval cast to u32 for renderer display. Mirrors
    /// `refresh_interval_secs`.
    pub refresh_secs: u32,

    /// Zoom level for bucket aggregation (1–6). Active in both Live and
    /// History mode. Cycles forward on `]` and backward on `[`.
    ///
    /// | Level | bucket_secs |
    /// |-------|-------------|
    /// | 1     | 1           |
    /// | 2     | 15          |
    /// | 3     | 30          |
    /// | 4     | 60          |
    /// | 5     | 300         |
    /// | 6     | 600         |
    pub zoom_level: u8,

    /// When true, render the color legend overlay (`l` key toggles).
    pub show_legend: bool,

    /// Number of live samples dropped due to `statement_timeout` this session.
    /// Displayed in the status bar when non-zero.
    pub missed_samples: u32,

    /// Cursor column index from the right of the timeline.
    ///
    /// `Some(n)` means the cursor line is drawn `n` columns from the right
    /// edge of the bar area.  `None` means no cursor is shown (Live mode).
    pub cursor_col: Option<usize>,

    /// How many buckets the timeline has been panned back from "now".
    ///
    /// `0` = live end (cursor hidden, Live mode).  Incremented by `pan_left`,
    /// decremented by `pan_right`.  Resets to `0` when returning to Live mode.
    pub pan_offset: i64,
}

impl AshState {
    pub fn new(pg_ash_installed: bool) -> Self {
        Self {
            level: DrillLevel::WaitType,
            mode: ViewMode::Live,
            selected_row: 0,
            refresh_interval_secs: 1,
            pg_ash_installed,
            is_history: false,
            refresh_secs: 1,
            zoom_level: 1,
            show_legend: false,
            missed_samples: 0,
            cursor_col: None,
            pan_offset: 0,
        }
    }

    /// Human-readable label for the current zoom level.
    pub fn zoom_label(&self) -> &'static str {
        match self.zoom_level {
            2 => "15s",
            3 => "30s",
            4 => "1min",
            5 => "5min",
            6 => "10min",
            _ => "1s",
        }
    }

    /// Context-sensitive key hint line for the footer.
    ///
    /// Three states:
    /// - Panning (`pan_offset > 0`): Esc returns to live; show `b:back` if drilled.
    /// - Top level, not panning: q/Esc both quit.
    /// - Drilled below top level, not panning: Esc/b go back one level.
    pub fn hint_line(&self) -> &'static str {
        if self.pan_offset > 0 && !self.is_at_top_level() {
            "q:quit  b:back  Esc:live  \u{2191}\u{2193}:select  Enter:drill  [/]:zoom  \u{2190}\u{2192}:pan  r:refresh  l:legend"
        } else if self.pan_offset > 0 {
            "q:quit  Esc:live  \u{2191}\u{2193}:select  Enter:drill  [/]:zoom  \u{2190}\u{2192}:pan  r:refresh  l:legend"
        } else if self.is_at_top_level() {
            "q/Esc:quit  \u{2191}\u{2193}:select  Enter:drill  [/]:zoom  \u{2190}\u{2192}:pan  r:refresh  l:legend"
        } else {
            "q:quit  Esc/b:back  \u{2191}\u{2193}:select  Enter:drill  [/]:zoom  \u{2190}\u{2192}:pan  r:refresh  l:legend"
        }
    }

    /// Number of raw 1-second samples that make up one display bucket at the
    /// current zoom level.
    pub fn bucket_secs(&self) -> u64 {
        match self.zoom_level {
            2 => 15,
            3 => 30,
            4 => 60,
            5 => 300,
            6 => 600,
            _ => 1,
        }
    }

    /// Advance zoom level: 1→2→3→4→5→6→1.
    ///
    /// Also updates `refresh_interval_secs` to match the new bucket size so
    /// live sampling rate stays consistent with the display granularity.
    pub fn zoom_cycle_forward(&mut self) {
        self.zoom_level = if self.zoom_level >= 6 {
            1
        } else {
            self.zoom_level + 1
        };
        self.sync_refresh_to_zoom();
    }

    /// Retreat zoom level: 1→6→5→4→3→2→1.
    ///
    /// Also updates `refresh_interval_secs` to match the new bucket size so
    /// live sampling rate stays consistent with the display granularity.
    pub fn zoom_cycle_back(&mut self) {
        self.zoom_level = if self.zoom_level <= 1 {
            6
        } else {
            self.zoom_level - 1
        };
        self.sync_refresh_to_zoom();
    }

    /// Pan the timeline one bucket to the left (into the past).
    ///
    /// Switches from Live to History mode on the first press, freezing the ring
    /// buffer so new samples are not appended while the user scrubs history.
    pub fn pan_left(&mut self) {
        if matches!(self.mode, ViewMode::Live) {
            // Initialise a non-zero window so the history query returns data.
            // Use refresh_interval_secs * 600 (the live ring buffer depth) as
            // a reasonable starting window; minimum 60 s.
            let now = SystemTime::now();
            let window_secs = (self.refresh_interval_secs * 600).max(60);
            let from = now - Duration::from_secs(window_secs);
            self.mode = ViewMode::History { from, to: now };
        }
        self.pan_offset += 1;
        self.cursor_col = usize::try_from(self.pan_offset).ok();
        self.sync_aliases();
    }

    /// Pan the timeline one bucket to the right (towards the present).
    ///
    /// Returns to Live mode and clears the cursor when the pan reaches "now".
    pub fn pan_right(&mut self) {
        if self.pan_offset > 0 {
            self.pan_offset -= 1;
        }
        if self.pan_offset == 0 {
            self.mode = ViewMode::Live;
            self.cursor_col = None;
        } else {
            self.cursor_col = usize::try_from(self.pan_offset).ok();
        }
        self.sync_aliases();
    }

    /// Set `refresh_interval_secs` to match `bucket_secs` so the live
    /// sampling interval equals the display bucket granularity.
    ///
    /// Capped at 60s — polling less than once per minute would make the TUI
    /// feel unresponsive even at coarse zoom levels.
    fn sync_refresh_to_zoom(&mut self) {
        self.refresh_interval_secs = self.bucket_secs().min(60);
        self.sync_aliases();
    }

    /// Sync the renderer-alias fields from the canonical fields.
    fn sync_aliases(&mut self) {
        self.is_history = matches!(self.mode, ViewMode::History { .. });
        self.refresh_secs = u32::try_from(self.refresh_interval_secs).unwrap_or(u32::MAX);
    }

    /// Returns `true` when the current drill level is at the top (no parent to go back to).
    pub fn is_at_top_level(&self) -> bool {
        matches!(self.level, DrillLevel::WaitType)
    }

    /// Handle a key event. Returns `true` if the application should exit.
    pub fn handle_key(&mut self, key: KeyEvent, list_len: usize) -> bool {
        // q and Ctrl-C always quit.
        if key.code == KeyCode::Char('q')
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        // Esc: if in History/cursor mode, snap back to Live first.
        // If already Live at top drill level, quit.
        if key.code == KeyCode::Esc {
            if self.pan_offset > 0 {
                // Exit pan/cursor mode → return to live view.
                self.pan_offset = 0;
                self.cursor_col = None;
                self.mode = ViewMode::Live;
                return false;
            }
            if self.is_at_top_level() {
                return true;
            }
            self.go_back();
            return false;
        }

        match key.code {
            KeyCode::Up if self.selected_row > 0 => {
                self.selected_row -= 1;
            }
            KeyCode::Down if list_len > 0 && self.selected_row < list_len - 1 => {
                self.selected_row += 1;
            }
            // Enter: caller is responsible for calling drill_into with row data.
            KeyCode::Char('b') => {
                self.go_back();
            }
            KeyCode::Char('[') => {
                self.zoom_cycle_back();
            }
            KeyCode::Char(']') => {
                self.zoom_cycle_forward();
            }
            KeyCode::Left => {
                self.pan_left();
            }
            KeyCode::Right => {
                self.pan_right();
            }
            KeyCode::Char('r') => {
                self.cycle_refresh();
            }
            KeyCode::Char('l') => {
                self.show_legend = !self.show_legend;
            }
            _ => {}
        }

        false
    }

    /// Advance one drill-down level using the provided row data.
    ///
    /// The caller supplies the type, event, and optional `query_id` that apply
    /// to the currently selected row.
    pub fn drill_into(
        &mut self,
        selected_type: &str,
        selected_event: &str,
        selected_query_id: Option<i64>,
    ) {
        self.level = match &self.level {
            DrillLevel::WaitType => DrillLevel::WaitEvent {
                selected_type: selected_type.to_owned(),
            },
            DrillLevel::WaitEvent { .. } => DrillLevel::QueryId {
                selected_type: selected_type.to_owned(),
                selected_event: selected_event.to_owned(),
            },
            DrillLevel::QueryId { .. } => DrillLevel::Pid {
                selected_type: selected_type.to_owned(),
                selected_event: selected_event.to_owned(),
                selected_query_id,
            },
            // Already at the deepest level; no-op.
            DrillLevel::Pid { .. } => return,
        };
        self.selected_row = 0;
    }

    /// Retreat one drill-down level; clamps at `WaitType`.
    pub fn go_back(&mut self) {
        self.level = match &self.level {
            DrillLevel::WaitType | DrillLevel::WaitEvent { .. } => DrillLevel::WaitType,
            DrillLevel::QueryId { selected_type, .. } => DrillLevel::WaitEvent {
                selected_type: selected_type.clone(),
            },
            DrillLevel::Pid {
                selected_type,
                selected_event,
                ..
            } => DrillLevel::QueryId {
                selected_type: selected_type.clone(),
                selected_event: selected_event.clone(),
            },
        };
        self.selected_row = 0;
    }

    /// Cycle refresh interval: 1 -> 5 -> 10 -> 1.
    pub fn cycle_refresh(&mut self) {
        self.refresh_interval_secs = match self.refresh_interval_secs {
            1 => 5,
            5 => 10,
            _ => 1,
        };
        self.sync_aliases();
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{AshState, DrillLevel, ViewMode};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // --- handle_key: exit keys ---

    #[test]
    fn exit_on_q() {
        let mut s = AshState::new(false);
        assert!(s.handle_key(key(KeyCode::Char('q')), 5));
    }

    #[test]
    fn exit_on_esc() {
        let mut s = AshState::new(false);
        assert!(s.handle_key(key(KeyCode::Esc), 5));
    }

    #[test]
    fn exit_on_ctrl_c() {
        let mut s = AshState::new(false);
        assert!(s.handle_key(ctrl('c'), 5));
    }

    #[test]
    fn no_exit_on_enter() {
        let mut s = AshState::new(false);
        assert!(!s.handle_key(key(KeyCode::Enter), 5));
    }

    // --- handle_key: navigation ---

    #[test]
    fn down_increments_row() {
        let mut s = AshState::new(false);
        assert!(!s.handle_key(key(KeyCode::Down), 5));
        assert_eq!(s.selected_row, 1);
    }

    #[test]
    fn down_clamps_at_last_row() {
        let mut s = AshState::new(false);
        s.selected_row = 4;
        s.handle_key(key(KeyCode::Down), 5);
        assert_eq!(s.selected_row, 4);
    }

    #[test]
    fn up_decrements_row() {
        let mut s = AshState::new(false);
        s.selected_row = 3;
        s.handle_key(key(KeyCode::Up), 5);
        assert_eq!(s.selected_row, 2);
    }

    #[test]
    fn up_clamps_at_zero() {
        let mut s = AshState::new(false);
        s.handle_key(key(KeyCode::Up), 5);
        assert_eq!(s.selected_row, 0);
    }

    #[test]
    fn down_no_op_on_empty_list() {
        let mut s = AshState::new(false);
        s.handle_key(key(KeyCode::Down), 0);
        assert_eq!(s.selected_row, 0);
    }

    // --- go_back ---

    #[test]
    fn go_back_from_wait_type_stays() {
        let mut s = AshState::new(false);
        s.go_back();
        assert_eq!(s.level, DrillLevel::WaitType);
    }

    #[test]
    fn go_back_from_wait_event() {
        let mut s = AshState::new(false);
        s.level = DrillLevel::WaitEvent {
            selected_type: "Lock".into(),
        };
        s.go_back();
        assert_eq!(s.level, DrillLevel::WaitType);
    }

    #[test]
    fn go_back_from_query_id() {
        let mut s = AshState::new(false);
        s.level = DrillLevel::QueryId {
            selected_type: "Lock".into(),
            selected_event: "relation".into(),
        };
        s.go_back();
        assert_eq!(
            s.level,
            DrillLevel::WaitEvent {
                selected_type: "Lock".into()
            }
        );
    }

    #[test]
    fn go_back_from_pid() {
        let mut s = AshState::new(false);
        s.level = DrillLevel::Pid {
            selected_type: "Lock".into(),
            selected_event: "relation".into(),
            selected_query_id: Some(42),
        };
        s.go_back();
        assert_eq!(
            s.level,
            DrillLevel::QueryId {
                selected_type: "Lock".into(),
                selected_event: "relation".into(),
            }
        );
    }

    // --- go_back resets selected_row ---

    #[test]
    fn go_back_resets_row() {
        let mut s = AshState::new(false);
        s.selected_row = 7;
        s.level = DrillLevel::WaitEvent {
            selected_type: "IO".into(),
        };
        s.go_back();
        assert_eq!(s.selected_row, 0);
    }

    // --- cycle_refresh ---

    #[test]
    fn cycle_refresh_sequence() {
        let mut s = AshState::new(false);
        assert_eq!(s.refresh_interval_secs, 1);
        s.cycle_refresh();
        assert_eq!(s.refresh_interval_secs, 5);
        s.cycle_refresh();
        assert_eq!(s.refresh_interval_secs, 10);
        s.cycle_refresh();
        assert_eq!(s.refresh_interval_secs, 1);
    }

    #[test]
    fn cycle_refresh_syncs_alias() {
        let mut s = AshState::new(false);
        s.cycle_refresh();
        assert_eq!(s.refresh_secs, 5);
    }

    // --- drill_into sequence ---

    #[test]
    fn drill_into_full_sequence() {
        let mut s = AshState::new(false);
        assert_eq!(s.level, DrillLevel::WaitType);

        s.drill_into("Lock", "relation", None);
        assert_eq!(
            s.level,
            DrillLevel::WaitEvent {
                selected_type: "Lock".into()
            }
        );

        s.drill_into("Lock", "relation", None);
        assert_eq!(
            s.level,
            DrillLevel::QueryId {
                selected_type: "Lock".into(),
                selected_event: "relation".into(),
            }
        );

        s.drill_into("Lock", "relation", Some(99));
        assert_eq!(
            s.level,
            DrillLevel::Pid {
                selected_type: "Lock".into(),
                selected_event: "relation".into(),
                selected_query_id: Some(99),
            }
        );

        // Already at deepest level — no-op.
        s.drill_into("Lock", "relation", Some(99));
        assert!(matches!(s.level, DrillLevel::Pid { .. }));
    }

    #[test]
    fn drill_into_resets_row() {
        let mut s = AshState::new(false);
        s.selected_row = 5;
        s.drill_into("IO", "DataFileRead", None);
        assert_eq!(s.selected_row, 0);
    }

    /// Zooming via `[`/`]` must keep `refresh_interval_secs` in sync with
    /// `bucket_secs`, capped at 60s.
    #[test]
    fn zoom_cycle_syncs_refresh_to_bucket() {
        let mut s = AshState::new(false);
        // Start: zoom_level=1, bucket_secs=1, refresh=1
        assert_eq!(s.refresh_interval_secs, 1);

        s.zoom_cycle_forward(); // level 2 → bucket_secs=15
        assert_eq!(s.bucket_secs(), 15);
        assert_eq!(s.refresh_interval_secs, 15);

        s.zoom_cycle_forward(); // level 3 → bucket_secs=30
        assert_eq!(s.bucket_secs(), 30);
        assert_eq!(s.refresh_interval_secs, 30);

        s.zoom_cycle_forward(); // level 4 → bucket_secs=60
        assert_eq!(s.bucket_secs(), 60);
        assert_eq!(s.refresh_interval_secs, 60);

        s.zoom_cycle_forward(); // level 5 → bucket_secs=300, capped at 60
        assert_eq!(s.bucket_secs(), 300);
        assert_eq!(s.refresh_interval_secs, 60, "refresh capped at 60s");

        s.zoom_cycle_back(); // back to level 4 → bucket_secs=60
        assert_eq!(s.refresh_interval_secs, 60);

        s.zoom_cycle_back(); // back to level 3 → bucket_secs=30
        assert_eq!(s.refresh_interval_secs, 30);

        s.zoom_cycle_back(); // back to level 2 → bucket_secs=15
        assert_eq!(s.refresh_interval_secs, 15);

        s.zoom_cycle_back(); // back to level 1 → bucket_secs=1
        assert_eq!(s.refresh_interval_secs, 1);
    }

    // --- pan_left / pan_right ---

    #[test]
    fn pan_left_transitions_to_history() {
        let mut s = AshState::new(false);
        assert!(matches!(s.mode, ViewMode::Live));
        assert_eq!(s.pan_offset, 0);

        s.pan_left();

        assert_eq!(s.pan_offset, 1);
        assert_eq!(s.cursor_col, Some(1));
        assert!(matches!(s.mode, ViewMode::History { .. }));
        assert!(s.is_history);
    }

    #[test]
    fn pan_left_increments_offset() {
        let mut s = AshState::new(false);
        s.pan_left();
        s.pan_left();
        s.pan_left();
        assert_eq!(s.pan_offset, 3);
        assert_eq!(s.cursor_col, Some(3));
    }

    #[test]
    fn pan_right_decrements_offset() {
        let mut s = AshState::new(false);
        s.pan_left();
        s.pan_left();
        s.pan_left();
        s.pan_right();
        assert_eq!(s.pan_offset, 2);
        assert_eq!(s.cursor_col, Some(2));
        assert!(matches!(s.mode, ViewMode::History { .. }));
    }

    #[test]
    fn pan_right_returns_to_live() {
        let mut s = AshState::new(false);
        s.pan_left();
        assert!(matches!(s.mode, ViewMode::History { .. }));

        s.pan_right();
        assert_eq!(s.pan_offset, 0);
        assert_eq!(s.cursor_col, None);
        assert!(matches!(s.mode, ViewMode::Live));
        assert!(!s.is_history);
    }

    #[test]
    fn pan_right_noop_at_zero() {
        let mut s = AshState::new(false);
        s.pan_right();
        assert_eq!(s.pan_offset, 0);
        assert!(matches!(s.mode, ViewMode::Live));
    }

    // --- Esc returns to live ---

    #[test]
    fn esc_returns_to_live_when_panning() {
        let mut s = AshState::new(false);
        s.pan_left();
        s.pan_left();
        s.pan_left();
        assert_eq!(s.pan_offset, 3);

        let exit = s.handle_key(key(KeyCode::Esc), 5);
        assert!(!exit, "Esc should not exit when panning");
        assert_eq!(s.pan_offset, 0);
        assert_eq!(s.cursor_col, None);
        assert!(matches!(s.mode, ViewMode::Live));
    }

    #[test]
    fn esc_returns_to_live_before_drill_back() {
        let mut s = AshState::new(false);
        s.drill_into("IO", "DataFileRead", None);
        s.pan_left();
        s.pan_left();

        // Esc should clear pan first, not go back one drill level.
        let exit = s.handle_key(key(KeyCode::Esc), 5);
        assert!(!exit);
        assert_eq!(s.pan_offset, 0);
        assert!(matches!(s.level, DrillLevel::WaitEvent { .. }));
    }

    // --- hint_line ---

    #[test]
    fn hint_line_top_level() {
        let s = AshState::new(false);
        assert!(s.hint_line().contains("q/Esc:quit"));
    }

    #[test]
    fn hint_line_panning() {
        let mut s = AshState::new(false);
        s.pan_left();
        assert!(s.hint_line().contains("Esc:live"));
        assert!(!s.hint_line().contains("b:back"));
    }

    #[test]
    fn hint_line_drilled() {
        let mut s = AshState::new(false);
        s.drill_into("IO", "DataFileRead", None);
        assert!(s.hint_line().contains("Esc/b:back"));
    }

    #[test]
    fn hint_line_panning_and_drilled() {
        let mut s = AshState::new(false);
        s.drill_into("IO", "DataFileRead", None);
        s.pan_left();
        let hint = s.hint_line();
        assert!(hint.contains("Esc:live"));
        assert!(hint.contains("b:back"));
    }

    // --- key dispatch: Left/Right ---

    #[test]
    fn left_key_pans_left() {
        let mut s = AshState::new(false);
        s.handle_key(key(KeyCode::Left), 5);
        assert_eq!(s.pan_offset, 1);
        assert!(s.is_history);
    }

    #[test]
    fn right_key_pans_right() {
        let mut s = AshState::new(false);
        s.handle_key(key(KeyCode::Left), 5);
        s.handle_key(key(KeyCode::Right), 5);
        assert_eq!(s.pan_offset, 0);
        assert!(!s.is_history);
    }
}
