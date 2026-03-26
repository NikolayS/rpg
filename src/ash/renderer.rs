//! Ratatui renderer for `/ash`.
//!
//! Renders stacked bar charts and drill-down tables.
//! Takes aggregated data from sampler, uses state from state.rs.

use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::ash::sampler::AshSnapshot;
use crate::ash::state::{AshState, DrillLevel};

// ---------------------------------------------------------------------------
// Color scheme — exact 24-bit RGB values matching pg_ash COLOR_SCHEME.md
// ---------------------------------------------------------------------------

/// Return the color for a given `wait_event_type` string.
///
/// If `no_color` is true (or the `NO_COLOR` environment variable is set),
/// `Color::Reset` is returned for every variant, leaving styling to the
/// terminal defaults.
pub fn wait_type_color(wait_event_type: &str, no_color: bool) -> Color {
    if no_color || std::env::var_os("NO_COLOR").is_some() {
        return Color::Reset;
    }
    match wait_event_type {
        "CPU*" => Color::Rgb(80, 250, 123),    // green — on CPU or uninstrumented
        "IdleTx" => Color::Rgb(241, 250, 140), // light yellow
        "IO" => Color::Rgb(30, 100, 255),      // vivid blue
        "Lock" => Color::Rgb(255, 85, 85),     // red
        "LWLock" => Color::Rgb(255, 121, 198), // pink
        "IPC" => Color::Rgb(0, 200, 255),      // cyan
        "Client" => Color::Rgb(255, 220, 100), // yellow
        "Timeout" => Color::Rgb(255, 165, 0),  // orange
        "BufferPin" => Color::Rgb(0, 210, 180), // teal
        "Activity" => Color::Rgb(150, 100, 255), // purple
        "Extension" => Color::Rgb(190, 150, 255), // light purple
        _ => Color::Rgb(180, 180, 180),        // gray (Unknown/Other)
    }
}

// ---------------------------------------------------------------------------
// Timeline helpers
// ---------------------------------------------------------------------------

/// Find the dominant `wait_event_type` in a snapshot (the one with the
/// highest count).  Returns `"CPU*"` when `by_type` is empty, treating
/// zero-wait time as on-CPU.
fn dominant_wait_type(snap: &AshSnapshot) -> &str {
    snap.by_type
        .iter()
        .max_by_key(|(_, &v)| v)
        .map(|(k, _)| k.as_str())
        .unwrap_or("CPU*")
}

/// Build a `Line` of `█` spans representing the timeline bar chart.
///
/// Each position corresponds to one snapshot.  The timeline is always
/// `width` columns wide; if there are fewer snapshots than `width`, the
/// left side is padded with spaces.
fn timeline_line<'a>(snapshots: &'a [AshSnapshot], width: usize, no_color: bool) -> Line<'a> {
    let bar = "\u{2588}"; // █
    let cols = width.max(1);

    // We only render the most recent `cols` snapshots.
    let snap_slice = if snapshots.len() >= cols {
        &snapshots[snapshots.len() - cols..]
    } else {
        snapshots
    };

    let pad = cols.saturating_sub(snap_slice.len());
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(pad + snap_slice.len());

    // Left-pad with spaces when there are fewer snapshots than columns.
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }

    for snap in snap_slice {
        let wt = dominant_wait_type(snap);
        let color = wait_type_color(wt, no_color);
        spans.push(Span::styled(bar, Style::default().fg(color)));
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Drill-down table helpers
// ---------------------------------------------------------------------------

/// A single row for the drill-down table (pre-computed, allocation per-frame
/// is intentional here since row count is small in practice).
struct DrillRow {
    label: String,
    wait_type: String,
    count: u32,
}

/// Collect rows for the current drill level from the most recent snapshot.
fn collect_drill_rows(
    snapshots: &[AshSnapshot],
    level: &DrillLevel,
) -> Vec<DrillRow> {
    let Some(snap) = snapshots.last() else {
        return Vec::new();
    };

    match level {
        DrillLevel::WaitType => {
            let mut rows: Vec<DrillRow> = snap
                .by_type
                .iter()
                .map(|(k, &v)| DrillRow {
                    label: k.clone(),
                    wait_type: k.clone(),
                    count: v,
                })
                .collect();
            rows.sort_by(|a, b| b.count.cmp(&a.count));
            rows
        }
        DrillLevel::WaitEvent { selected_type } => {
            // Filter by_event entries that belong to this wait_event_type.
            // Naming convention: keys in by_event are stored as
            // "<wait_event_type>/<wait_event>" so we can filter by prefix.
            let prefix = format!("{selected_type}/");
            let mut rows: Vec<DrillRow> = snap
                .by_event
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, &v)| {
                    let label = k.strip_prefix(&prefix).unwrap_or(k.as_str()).to_owned();
                    DrillRow {
                        label,
                        wait_type: selected_type.clone(),
                        count: v,
                    }
                })
                .collect();
            rows.sort_by(|a, b| b.count.cmp(&a.count));
            rows
        }
        DrillLevel::QueryId { selected_event, .. } => {
            let prefix = format!("{selected_event}/");
            let mut rows: Vec<DrillRow> = snap
                .by_query
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
                .map(|(k, &v)| {
                    let label = k.strip_prefix(&prefix).unwrap_or(k.as_str()).to_owned();
                    DrillRow {
                        label,
                        wait_type: String::new(),
                        count: v,
                    }
                })
                .collect();
            rows.sort_by(|a, b| b.count.cmp(&a.count));
            rows
        }
        DrillLevel::Pid { .. } => Vec::new(),
    }
}

/// Render one drill-down row as a `Line`.
fn drill_row_line(
    row: &DrillRow,
    is_selected: bool,
    max_count: u32,
    no_color: bool,
) -> Line<'_> {
    let marker = if is_selected { "\u{25b6} " } else { "  " }; // ▶ or two spaces
    let bar_len = if max_count > 0 {
        ((row.count as u64 * 20 / max_count as u64).max(1)) as usize
    } else {
        1
    };
    let bar: String = "\u{2588}".repeat(bar_len); // █ × bar_len
    let color = wait_type_color(&row.wait_type, no_color);

    let base_style = if is_selected {
        Style::default().reversed()
    } else {
        Style::default()
    };

    Line::from(vec![
        Span::styled(marker.to_owned(), base_style),
        Span::styled(format!("{:<20}", row.label), base_style),
        Span::styled(format!("{:>6}   ", row.count), base_style),
        Span::styled(bar, base_style.fg(color)),
    ])
}

// ---------------------------------------------------------------------------
// Public draw entry point
// ---------------------------------------------------------------------------

/// Draw a single frame of the `/ash` TUI.
///
/// * `frame`     — ratatui frame to render into.
/// * `snapshots` — ring buffer of aggregated snapshots, most recent last.
/// * `state`     — current drill-down / zoom state.
/// * `no_color`  — when true, use terminal default colors (respects NO_COLOR).
pub fn draw_frame(
    frame: &mut Frame,
    snapshots: &[AshSnapshot],
    state: &AshState,
    no_color: bool,
) {
    let area = frame.area();

    // Outer border.
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .title(" /ash \u{2014} Active Session History ");
    let inner = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    // Split the inner area into four horizontal bands:
    //   [0] status bar   Length(2)
    //   [1] timeline     Min(6)
    //   [2] drill-down   Min(8)
    //   [3] footer       Length(3)
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(inner);

    // -----------------------------------------------------------------------
    // [0] Status bar
    // -----------------------------------------------------------------------
    let active = snapshots.last().map(|s| s.active_count).unwrap_or(0);
    let mode_label = if state.is_history { "History" } else { "Live" };
    let status_text = format!(
        "[{mode_label}]  refresh: {}s   Active sessions: {}",
        state.refresh_secs, active
    );
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default()),
        chunks[0],
    );

    // -----------------------------------------------------------------------
    // [1] Timeline
    // -----------------------------------------------------------------------
    let timeline_block = Block::default()
        .borders(Borders::ALL)
        .title(" Timeline (last 60s) ");
    let timeline_inner = timeline_block.inner(chunks[1]);
    frame.render_widget(timeline_block, chunks[1]);

    // Reserve one line for the header label, rest for the bar.
    let tl_chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
        .split(timeline_inner);

    frame.render_widget(
        Paragraph::new("each column = 1s, colored by dominant wait type")
            .style(Style::default().fg(Color::DarkGray)),
        tl_chunks[0],
    );

    let bar_width = tl_chunks[1].width as usize;
    let bar_line = timeline_line(snapshots, bar_width, no_color);
    frame.render_widget(Paragraph::new(bar_line), tl_chunks[1]);

    // -----------------------------------------------------------------------
    // [2] Drill-down table
    // -----------------------------------------------------------------------
    let table_block = Block::default()
        .borders(Borders::ALL)
        .title(" Drill-down ");
    let table_inner = table_block.inner(chunks[2]);
    frame.render_widget(table_block, chunks[2]);

    if matches!(state.level, DrillLevel::Pid { .. }) {
        frame.render_widget(
            Paragraph::new("pid-level drill-down: coming soon"),
            table_inner,
        );
    } else {
        // Header row.
        let header_chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
            .split(table_inner);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<20}", "WAIT TYPE"),
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>6}   ", "COUNT"),
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(
                    "BAR",
                    Style::default().add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ])),
            header_chunks[0],
        );

        let rows = collect_drill_rows(snapshots, &state.level);
        let max_count = rows.iter().map(|r| r.count).max().unwrap_or(1);
        let list_height = header_chunks[1].height as usize;

        // Scrolling window: keep selected_row visible.
        let visible_start = if state.selected_row >= list_height {
            state.selected_row - list_height + 1
        } else {
            0
        };

        let lines: Vec<Line<'_>> = rows
            .iter()
            .enumerate()
            .skip(visible_start)
            .take(list_height)
            .map(|(i, row)| {
                let is_selected = i == state.selected_row;
                drill_row_line(row, is_selected, max_count, no_color)
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), header_chunks[1]);
    }

    // -----------------------------------------------------------------------
    // [3] Footer / key hints
    // -----------------------------------------------------------------------
    let mut hint = String::from("q:quit  \u{2191}\u{2193}:select  Enter:drill  b:back  r:refresh");
    if state.is_history {
        hint.push_str("  \u{2190}\u{2192}:zoom");
    }
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}
