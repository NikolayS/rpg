//! Data layer for `/ash`.
//!
//! Polls `pg_stat_activity` for live data and optionally queries
//! `ash.samples` when pg_ash is installed.

// TODO: full implementation by ash-sampler agent.
// The structs below are the minimal public API that renderer.rs depends on.

use std::collections::HashMap;

/// A single point-in-time sample of active session counts, aggregated from
/// `pg_stat_activity` (or `ash.samples` when pg_ash is available).
#[derive(Debug, Default, Clone)]
pub struct AshSnapshot {
    /// Unix timestamp (seconds) when the sample was taken.
    pub ts: i64,
    /// Total active (non-idle) sessions at sample time.
    pub active_count: u32,
    /// Counts grouped by `wait_event_type` (e.g. "Lock", "IO", "CPU*").
    pub by_type: HashMap<String, u32>,
    /// Counts grouped by `wait_event` (the specific event name).
    pub by_event: HashMap<String, u32>,
    /// Counts grouped by `query_id` (normalized query fingerprint).
    pub by_query: HashMap<String, u32>,
}
