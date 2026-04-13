//! `/ash` — Active Session History TUI for rpg.
//!
//! # Architecture
//!
//! ```text
//! sampler.rs   — polls pg_stat_activity / ash.samples, aggregates data
//! state.rs     — drill-down state machine, zoom/time-range, key handling
//! renderer.rs  — ratatui widgets: stacked bars, color scheme, layout
//! mod.rs       — public entry point: run_ash()
//! ```
//!
//! # Color scheme (matches `pg_ash`)
//!
//! | wait_event_type          | Color        |
//! |--------------------------|--------------|
//! | CPU* (uninstrumented)    | Bright green |
//! | Lock                     | Red          |
//! | LWLock                   | Bright yellow|
//! | IO                       | Blue         |
//! | IdleTx (idle in tx)      | Yellow       |
//! | Other                    | Default      |

pub mod renderer;
pub mod sampler;
pub mod state;

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::time::{Duration, Instant};

use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio_postgres::Client;

use crate::repl::ReplSettings;

pub use state::AshState;

use state::ViewMode;

// ---------------------------------------------------------------------------
// TerminalGuard — RAII wrapper (same pattern as history_picker.rs)
// ---------------------------------------------------------------------------

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
// Public entry point
// ---------------------------------------------------------------------------

/// Entry point for the `/ash` TUI. Blocks until the user exits with `q`, `Esc`, or `Ctrl-C`.
///
/// `cpu_override` — explicit vCPU count supplied via `/ash --cpu N`.
/// When `None`, the sampler tries `pg_proctab`; if unavailable the CPU
/// reference line is hidden rather than showing a misleading value.
pub async fn run_ash(
    client: &Client,
    settings: &ReplSettings,
    cpu_override: Option<u32>,
) -> anyhow::Result<()> {
    if !io::stdout().is_terminal() {
        anyhow::bail!("/ash requires an interactive terminal");
    }

    let pg_ash = sampler::detect_pg_ash(client).await;
    let mut state = AshState::new(pg_ash.installed);
    let mut snapshots: VecDeque<sampler::AshSnapshot> = VecDeque::with_capacity(600);
    let timeout_ms = settings.config.ash.sample_timeout_ms();

    // Pre-populate ring buffer from pg_ash history when available.
    // Fills the left side of the timeline; live data scrolls in on the right.
    if pg_ash.installed {
        // Pre-populate using the current zoom window (bucket_secs × 600 samples).
        let history_window = state.bucket_secs() * 600;
        let history = sampler::query_ash_history(client, history_window, timeout_ms).await;
        for snap in history {
            if snapshots.len() == 600 {
                snapshots.pop_front();
            }
            snapshots.push_back(snap);
        }
    }

    let no_color = settings.no_highlight;

    let _guard = TerminalGuard::new()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    'outer: loop {
        // 1. Collect snapshot data for this frame.
        let snap_slice =
            collect_frame_snapshots(client, &mut snapshots, &mut state, cpu_override, timeout_ms)
                .await;
        let in_history = state.pan_offset > 0 || matches!(state.mode, ViewMode::History { .. });

        // 2. Draw frame.
        terminal.draw(|f| {
            renderer::draw_frame(f, &snap_slice, &state, no_color);
        })?;

        // 3. Drain key events.
        //
        //    In History/cursor mode: block indefinitely waiting for a keypress —
        //    the display is frozen so there's no need for a periodic redraw.
        //    In Live mode: loop until the refresh interval elapses, then re-sample.
        //
        //    Previously a single event::poll(timeout) meant any key press caused
        //    an immediate re-sample at the top of the outer loop, producing extra
        //    data points and skewing the Y-axis. Now we loop until the interval
        //    elapses, handling as many key events as arrive, then break out to
        //    take the next scheduled sample.
        let deadline = if in_history {
            // Frozen: no timeout — wait indefinitely for a keypress.
            None
        } else {
            Some(Instant::now() + Duration::from_secs(state.refresh_interval_secs))
        };
        loop {
            let remaining = match deadline {
                Some(d) => {
                    let r = d.saturating_duration_since(Instant::now());
                    if r.is_zero() {
                        break;
                    }
                    r
                }
                // History mode: long poll — effectively infinite wait for keypress.
                None => Duration::from_secs(60),
            };
            if !event::poll(remaining)? {
                // Timeout elapsed (Live mode) — time for the next sample.
                break;
            }
            if let Event::Key(key) = event::read()? {
                // Enter: drill into selected row.
                if key.code == KeyCode::Enter {
                    if let Some(last) = snap_slice.last() {
                        if let Some((wtype, wevent, qid)) = collect_selected_row_data(last, &state)
                        {
                            state.drill_into(&wtype, &wevent, qid);
                        }
                    }
                    // Redraw immediately after drill-in so the view updates
                    // without waiting for the next sample tick.
                    terminal.draw(|f| {
                        renderer::draw_frame(f, &snap_slice, &state, no_color);
                    })?;
                    continue;
                }

                // All other keys go through handle_key; true = exit.
                let list_len = compute_list_len(&snap_slice, &state);
                if state.handle_key(key, list_len) {
                    break 'outer;
                }
                // Redraw after state change (selection move, zoom, legend toggle)
                // so the UI feels responsive within the same sample tick.
                terminal.draw(|f| {
                    renderer::draw_frame(f, &snap_slice, &state, no_color);
                })?;
                // If we just transitioned from History/cursor → Live (e.g. Esc),
                // break the inner loop so the outer loop recomputes `in_history`
                // and sets a proper deadline instead of the 60s poll.
                let still_in_history =
                    state.pan_offset > 0 || matches!(state.mode, ViewMode::History { .. });
                if in_history && !still_in_history {
                    break;
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect the snapshot slice to display for this frame.
///
/// In Live mode: take a fresh 1-second sample, append to the ring buffer,
/// and return the ring as a slice.
/// In History mode: query `pg_ash` for the configured window (falls back to
/// the live ring when `pg_ash` is unavailable).
async fn collect_frame_snapshots(
    client: &Client,
    snapshots: &mut std::collections::VecDeque<sampler::AshSnapshot>,
    state: &mut AshState,
    cpu_override: Option<u32>,
    timeout_ms: u64,
) -> Vec<sampler::AshSnapshot> {
    match &state.mode {
        ViewMode::History { from, to } => {
            let window = to
                .duration_since(*from)
                .unwrap_or_default()
                .as_secs()
                .max(1);
            let v = sampler::query_ash_history(client, window, timeout_ms).await;
            if v.is_empty() {
                snapshots.iter().cloned().collect()
            } else {
                v
            }
        }
        ViewMode::Live => {
            match sampler::live_snapshot(client, timeout_ms).await {
                Ok(sampler::LiveSnapshotResult::Ok(mut snap)) => {
                    if cpu_override.is_some() {
                        snap.cpu_count = cpu_override;
                    }
                    if snapshots.len() == 600 {
                        snapshots.pop_front();
                    }
                    snapshots.push_back(snap);
                }
                Ok(sampler::LiveSnapshotResult::Missed) => {
                    state.missed_samples = state.missed_samples.saturating_add(1);
                }
                Err(_) => {}
            }
            snapshots.iter().cloned().collect()
        }
    }
}

/// Return the number of rows in the current drill-down level.
fn compute_list_len(snapshots: &[sampler::AshSnapshot], state: &AshState) -> usize {
    use state::DrillLevel;
    let Some(snap) = snapshots.last() else {
        return 0;
    };
    match &state.level {
        DrillLevel::WaitType => snap.by_type.len(),
        DrillLevel::WaitEvent { selected_type } => {
            let prefix = format!("{selected_type}/");
            snap.by_event
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .count()
        }
        DrillLevel::QueryId {
            selected_type,
            selected_event,
        } => {
            let prefix = format!("{selected_type}/{selected_event}/");
            snap.by_query
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .count()
        }
        DrillLevel::Pid { .. } => 0,
    }
}

/// Extract (`wait_type`, `wait_event`, `query_id`) for the currently selected row.
///
/// Returns None when there are no snapshots or the level is Pid (no-op).
fn collect_selected_row_data(
    snap: &sampler::AshSnapshot,
    state: &AshState,
) -> Option<(String, String, Option<i64>)> {
    use state::DrillLevel;

    match &state.level {
        DrillLevel::WaitType => {
            // Sort by count descending, then pick state.selected_row.
            let mut entries: Vec<(&String, &u32)> = snap.by_type.iter().collect();
            entries.sort_by(|a, b| b.1.cmp(a.1));
            let (wtype, _) = entries.get(state.selected_row)?;
            Some(((*wtype).clone(), String::new(), None))
        }
        DrillLevel::WaitEvent { selected_type } => {
            let prefix = format!("{selected_type}/");
            let mut entries: Vec<(&String, &u32)> = snap
                .by_event
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .collect();
            entries.sort_by(|a, b| b.1.cmp(a.1));
            let (key, _) = entries.get(state.selected_row)?;
            let wevent = (*key).strip_prefix(&prefix).unwrap_or("").to_owned();
            Some((selected_type.clone(), wevent, None))
        }
        DrillLevel::QueryId {
            selected_type,
            selected_event,
        } => {
            let prefix = format!("{selected_type}/{selected_event}/");
            let mut entries: Vec<(&String, &u32)> = snap
                .by_query
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .collect();
            entries.sort_by(|a, b| b.1.cmp(a.1));
            let (key, _) = entries.get(state.selected_row)?;
            // Try to parse query label as a numeric query_id.
            let label = (*key).strip_prefix(&prefix).unwrap_or("");
            let qid: Option<i64> = label.parse().ok();
            Some((selected_type.clone(), selected_event.clone(), qid))
        }
        DrillLevel::Pid { .. } => None,
    }
}
