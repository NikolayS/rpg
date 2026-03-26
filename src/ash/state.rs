//! Drill-down state machine for `/ash`.
//!
//! Manages the four drill-down levels:
//!   1. wait_event_type
//!   2. wait_event
//!   3. query_id
//!   4. pid
//!
//! Also manages zoom/time-range for history mode.

// TODO: full key-handling and zoom logic by ash-state agent.
// The structs/enums below are the minimal public API that renderer.rs depends on.

/// The currently active drill-down level.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DrillLevel {
    /// Top level: grouped by wait_event_type.
    #[default]
    WaitType,
    /// Second level: grouped by wait_event, filtered to one type.
    WaitEvent {
        /// The wait_event_type that was selected at the previous level.
        selected_type: String,
    },
    /// Third level: grouped by query_id, filtered to one event.
    QueryId {
        /// The wait_event that was selected at the previous level.
        selected_event: String,
    },
    /// Fourth level: individual PIDs.
    Pid,
}

/// Top-level state for the ASH TUI.
#[derive(Debug, Default)]
pub struct AshState {
    /// Current drill-down level.
    pub level: DrillLevel,
    /// Index of the highlighted row in the drill-down table.
    pub selected_row: usize,
    /// When true, the view is in history (non-live) mode.
    pub is_history: bool,
    /// Refresh interval in seconds (live mode).
    pub refresh_secs: u32,
}
