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
use std::time::Duration;

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

/// Entry point. Blocks until the user exits with `q`, `Esc`, or `Ctrl-C`.
/// Entry point for the `/ash` TUI.
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

    // Pre-populate ring buffer from pg_ash history when available.
    // Fills the left side of the timeline; live data scrolls in on the right.
    if pg_ash.installed {
        // Pre-populate using the current zoom window (bucket_secs × 600 samples).
        let history_window = state.bucket_secs() * 600;
        let history = sampler::query_ash_history(client, history_window).await;
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

    loop {
        // 1. Collect snapshot data for this frame.
        //
        //    Live mode: take a fresh snapshot and append to the ring buffer.
        //    History mode: fetch from pg_ash; on error or if not installed,
        //    fall back to the live ring buffer so the TUI never goes blank.
        let snap_slice: Vec<sampler::AshSnapshot> = match &state.mode {
            ViewMode::History { from, to } => {
                let window = to
                    .duration_since(*from)
                    .unwrap_or_default()
                    .as_secs()
                    .max(1);
                let v = sampler::query_ash_history(client, window).await;
                if v.is_empty() {
                    // Fall back to live ring buffer when history is unavailable.
                    snapshots.iter().cloned().collect()
                } else {
                    v
                }
            }
            ViewMode::Live => {
                if let Ok(mut snap) = sampler::live_snapshot(client).await {
                    // User-supplied --cpu N overrides auto-detected value.
                    if cpu_override.is_some() {
                        snap.cpu_count = cpu_override;
                    }
                    if snapshots.len() == 600 {
                        snapshots.pop_front();
                    }
                    snapshots.push_back(snap);
                }
                // On transient errors: ring buffer retains prior data; keep looping.
                snapshots.iter().cloned().collect()
            }
        };

        // 2. Draw frame.
        terminal.draw(|f| {
            renderer::draw_frame(f, &snap_slice, &state, no_color);
        })?;

        // 3. Poll crossterm events with timeout = refresh_interval_secs.
        let timeout = Duration::from_secs(state.refresh_interval_secs);
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                // 5. Enter: drill into selected row.
                if key.code == KeyCode::Enter {
                    // Compute row data from the current snapshot for drill_into.
                    if let Some(last) = snap_slice.last() {
                        if let Some((wtype, wevent, qid)) = collect_selected_row_data(last, &state)
                        {
                            state.drill_into(&wtype, &wevent, qid);
                        }
                    }
                    continue;
                }

                // 4. All other keys go through handle_key; true = exit.
                let list_len = compute_list_len(&snap_slice, &state);
                if state.handle_key(key, list_len) {
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
