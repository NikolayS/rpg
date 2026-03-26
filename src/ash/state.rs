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

/// Minimum allowed history window (seconds).
const ZOOM_MIN_SECS: u64 = 10;
/// Maximum allowed history window (seconds).
const ZOOM_MAX_SECS: u64 = 3600;

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
    /// True when `pg_ash` extension is installed and available.
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
    /// History mode. Cycles forward on `→` and backward on `←`.
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
        }
    }

    /// Human-readable label for the current zoom level.
    pub fn zoom_label(&self) -> &'static str {
        match self.zoom_level {
            1 => "1s",
            2 => "15s",
            3 => "30s",
            4 => "1min",
            5 => "5min",
            6 => "10min",
            _ => "1s",
        }
    }

    /// Number of raw 1-second samples that make up one display bucket at the
    /// current zoom level.
    pub fn bucket_secs(&self) -> u64 {
        match self.zoom_level {
            1 => 1,
            2 => 15,
            3 => 30,
            4 => 60,
            5 => 300,
            6 => 600,
            _ => 1,
        }
    }

    /// Advance zoom level: 1→2→3→4→5→6→1.
    pub fn zoom_cycle_forward(&mut self) {
        self.zoom_level = if self.zoom_level >= 6 {
            1
        } else {
            self.zoom_level + 1
        };
    }

    /// Retreat zoom level: 1→6→5→4→3→2→1.
    pub fn zoom_cycle_back(&mut self) {
        self.zoom_level = if self.zoom_level <= 1 {
            6
        } else {
            self.zoom_level - 1
        };
    }

    /// Sync the renderer-alias fields from the canonical fields.
    fn sync_aliases(&mut self) {
        self.is_history = matches!(self.mode, ViewMode::History { .. });
        self.refresh_secs = u32::try_from(self.refresh_interval_secs).unwrap_or(u32::MAX);
    }

    /// Handle a key event. Returns `true` if the application should exit.
    pub fn handle_key(&mut self, key: KeyEvent, list_len: usize) -> bool {
        // Exit keys.
        if key.code == KeyCode::Char('q')
            || key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return true;
        }

        match key.code {
            KeyCode::Up => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                }
            }
            KeyCode::Down => {
                if list_len > 0 && self.selected_row < list_len - 1 {
                    self.selected_row += 1;
                }
            }
            // Enter: caller is responsible for calling drill_into with row data.
            KeyCode::Char('b') => {
                self.go_back();
            }
            KeyCode::Left => {
                self.zoom_cycle_back();
                // Also shrink the History window when in History mode.
                if matches!(self.mode, ViewMode::History { .. }) {
                    self.zoom_in();
                }
            }
            KeyCode::Right => {
                self.zoom_cycle_forward();
                // Also expand the History window when in History mode.
                if matches!(self.mode, ViewMode::History { .. }) {
                    self.zoom_out();
                }
            }
            KeyCode::Char('r') => {
                self.cycle_refresh();
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

    /// Halve the history time window; no-op in live mode or if already at min.
    pub fn zoom_in(&mut self) {
        if let ViewMode::History { from, to } = &self.mode {
            let window = to
                .duration_since(*from)
                .unwrap_or(Duration::from_secs(ZOOM_MIN_SECS));
            let new_window = window / 2;
            if new_window.as_secs() >= ZOOM_MIN_SECS {
                let new_to = *to;
                let new_from = new_to - new_window;
                self.mode = ViewMode::History {
                    from: new_from,
                    to: new_to,
                };
                self.sync_aliases();
            }
        }
    }

    /// Double the history time window; no-op in live mode or if already at max.
    pub fn zoom_out(&mut self) {
        if let ViewMode::History { from, to } = &self.mode {
            let window = to
                .duration_since(*from)
                .unwrap_or(Duration::from_secs(ZOOM_MIN_SECS));
            let new_window = window * 2;
            if new_window.as_secs() <= ZOOM_MAX_SECS {
                let new_to = *to;
                let new_from = new_to - new_window;
                self.mode = ViewMode::History {
                    from: new_from,
                    to: new_to,
                };
                self.sync_aliases();
            }
        }
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
    use std::time::{Duration, SystemTime};

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

    // --- zoom_in / zoom_out ---

    fn history_state(window_secs: u64) -> AshState {
        let to = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let from = to - Duration::from_secs(window_secs);
        let mut s = AshState::new(false);
        s.mode = ViewMode::History { from, to };
        s.is_history = true;
        s
    }

    fn window_secs(s: &AshState) -> u64 {
        if let ViewMode::History { from, to } = s.mode {
            to.duration_since(from).unwrap_or_default().as_secs()
        } else {
            panic!("not in history mode");
        }
    }

    #[test]
    fn zoom_in_halves_window() {
        let mut s = history_state(60);
        s.zoom_in();
        assert_eq!(window_secs(&s), 30);
    }

    #[test]
    fn zoom_in_clamps_at_min() {
        let mut s = history_state(10);
        s.zoom_in(); // 10/2 = 5 < ZOOM_MIN_SECS → no-op
        assert_eq!(window_secs(&s), 10);
    }

    #[test]
    fn zoom_out_doubles_window() {
        let mut s = history_state(60);
        s.zoom_out();
        assert_eq!(window_secs(&s), 120);
    }

    #[test]
    fn zoom_out_clamps_at_max() {
        let mut s = history_state(3600);
        s.zoom_out(); // 3600*2 = 7200 > ZOOM_MAX_SECS → no-op
        assert_eq!(window_secs(&s), 3600);
    }

    #[test]
    fn zoom_in_no_op_in_live_mode() {
        let mut s = AshState::new(false);
        s.zoom_in(); // should not panic
        assert!(matches!(s.mode, ViewMode::Live));
    }

    #[test]
    fn zoom_out_no_op_in_live_mode() {
        let mut s = AshState::new(false);
        s.zoom_out(); // should not panic
        assert!(matches!(s.mode, ViewMode::Live));
    }
}
