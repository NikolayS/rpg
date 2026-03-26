//! Drill-down state machine for `/ash`.
//!
//! Manages the four drill-down levels:
//!   1. wait_event_type
//!   2. wait_event
//!   3. query_id
//!   4. pid
//!
//! Also manages zoom/time-range for history mode.

// TODO: implemented by ash-state agent

/// Top-level state for the ASH TUI.
pub struct AshState;
