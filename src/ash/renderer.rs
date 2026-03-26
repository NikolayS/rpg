//! Ratatui renderer for `/ash`.
//!
//! Renders a scrolling stacked-bar timeline, a summary metrics row, and a
//! drill-down table with Time / %DB Time / AAS / Bar columns.

use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
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
/// When `no_color` is true (or the `NO_COLOR` environment variable is set),
/// `Color::Reset` is returned, leaving styling to terminal defaults.
pub fn wait_type_color(wait_event_type: &str, no_color: bool) -> Color {
    if no_color || std::env::var_os("NO_COLOR").is_some() {
        return Color::Reset;
    }
    match wait_event_type {
        "CPU*" => Color::Rgb(80, 250, 123),       // green
        "IdleTx" => Color::Rgb(241, 250, 140),    // light yellow
        "IO" => Color::Rgb(30, 100, 255),         // vivid blue
        "Lock" => Color::Rgb(255, 85, 85),        // red
        "LWLock" => Color::Rgb(255, 121, 198),    // pink
        "IPC" => Color::Rgb(0, 200, 255),         // cyan
        "Client" => Color::Rgb(255, 220, 100),    // yellow
        "Timeout" => Color::Rgb(255, 165, 0),     // orange
        "BufferPin" => Color::Rgb(0, 210, 180),   // teal
        "Activity" => Color::Rgb(150, 100, 255),  // purple
        "Extension" => Color::Rgb(190, 150, 255), // light purple
        _ => Color::Rgb(180, 180, 180),           // gray
    }
}

// ---------------------------------------------------------------------------
// Timeline helpers
// ---------------------------------------------------------------------------

/// One aggregated display bucket, derived from one or more raw snapshots.
struct Bucket {
    /// Average active sessions (total active counts / number of raw samples).
    aas: f64,
    /// Dominant wait type by sample count across all raw samples in the bucket.
    wait_type: String,
}

/// Aggregate raw snapshots into display buckets according to `bucket_secs`.
///
/// Buckets are formed by grouping contiguous samples of `bucket_secs` size.
/// The rightmost bucket is the most recent; older buckets are to the left.
/// Returns at most `max_cols` buckets (trim from the left).
fn aggregate_buckets(snapshots: &[AshSnapshot], bucket_secs: u64, max_cols: usize) -> Vec<Bucket> {
    if snapshots.is_empty() || max_cols == 0 {
        return Vec::new();
    }

    let step = usize::try_from(bucket_secs.max(1)).unwrap_or(usize::MAX);
    // How many raw samples per bucket (1 at zoom level 1, more at higher levels).
    // We chunk from the end so the most-recent data always fills the rightmost column.
    let total = snapshots.len();
    let num_full = total / step;
    let remainder = total % step;

    // Build chunks: first chunk may be partial (oldest data), then full chunks.
    let mut chunks: Vec<&[AshSnapshot]> = Vec::new();
    let mut offset = 0;
    if remainder > 0 {
        chunks.push(&snapshots[..remainder]);
        offset = remainder;
    }
    while offset < total {
        chunks.push(&snapshots[offset..offset + step]);
        offset += step;
    }
    let _ = num_full; // already computed via chunks

    // Trim to max_cols (keep rightmost).
    if chunks.len() > max_cols {
        let drop = chunks.len() - max_cols;
        chunks.drain(..drop);
    }

    chunks
        .into_iter()
        .map(|chunk| {
            let total_active: u64 = chunk.iter().map(|s| u64::from(s.active_count)).sum();
            #[allow(clippy::cast_precision_loss)]
            let aas = total_active as f64 / chunk.len() as f64;

            // Dominant wait type: sum counts per type across all samples in bucket.
            let mut type_totals: std::collections::HashMap<&str, u64> =
                std::collections::HashMap::new();
            for snap in chunk {
                for (k, &v) in &snap.by_type {
                    *type_totals.entry(k.as_str()).or_insert(0) += u64::from(v);
                }
            }
            let wt = type_totals
                .into_iter()
                .max_by_key(|(_, v)| *v)
                .map_or("CPU*", |(k, _)| k);

            Bucket {
                aas,
                wait_type: wt.to_owned(),
            }
        })
        .collect()
}

/// Render the scrolling right-to-left stacked-bar timeline into a vec of
/// `Line`s (one per row, bottom-aligned).
///
/// * `chart_height` — available rows (not counting any header/border lines).
/// * `chart_width`  — available columns.
/// * `cpu_count`    — where to draw the `─` reference line.
fn build_timeline_lines(
    snapshots: &[AshSnapshot],
    state: &AshState,
    chart_height: usize,
    chart_width: usize,
    cpu_count: u32,
    no_color: bool,
) -> Vec<Line<'static>> {
    let h = chart_height.max(1);
    let w = chart_width.max(1);

    let buckets = aggregate_buckets(snapshots, state.bucket_secs(), w);

    // Find max AAS across all buckets to scale bar height.
    let max_aas = buckets
        .iter()
        .map(|b| b.aas)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    // For each bucket, compute bar height in rows (bottom-up, 1-indexed from bottom).
    // bar_height[i] = number of filled rows from the bottom for bucket i.
    #[allow(clippy::cast_precision_loss)]
    let h_f64 = h as f64;
    let bar_heights: Vec<usize> = buckets
        .iter()
        .map(|b| {
            let frac = b.aas / max_aas;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let height = (frac * h_f64).round() as usize;
            height.clamp(0, h)
        })
        .collect();

    // CPU count reference row: which row index from the top corresponds to cpu_count?
    // Row 0 = top, row h-1 = bottom.
    // A row is "cpu line" if it is the row where cpu_count falls when scaled.
    let cpu_row_from_bottom: Option<usize> = if cpu_count > 0 {
        let frac = f64::from(cpu_count) / max_aas;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let row_from_bottom = (frac * h_f64).round() as usize;
        Some(row_from_bottom.clamp(1, h))
    } else {
        None
    };

    // Number of left-pad columns when fewer buckets than width.
    let pad_cols = w.saturating_sub(buckets.len());

    // Build lines from top (row_from_top = 0) to bottom (row_from_top = h-1).
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(h);
    for row_from_top in 0..h {
        let row_from_bottom = h - row_from_top; // 1-indexed from bottom

        let is_cpu_line = cpu_row_from_bottom.is_some_and(|r| r == row_from_bottom);

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(w + 1);

        // Left padding (empty space for columns with no data yet).
        if pad_cols > 0 {
            if is_cpu_line {
                spans.push(Span::styled(
                    "\u{2500}".repeat(pad_cols), // ─
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::raw(" ".repeat(pad_cols)));
            }
        }

        for (col_idx, bucket) in buckets.iter().enumerate() {
            let filled = bar_heights.get(col_idx).copied().unwrap_or(0);
            let color = wait_type_color(&bucket.wait_type, no_color);

            let ch: String = if row_from_bottom <= filled {
                // Inside the bar — draw filled block.
                "\u{2588}".to_owned() // █
            } else if is_cpu_line {
                // CPU reference line passes through empty space.
                "\u{2500}".to_owned() // ─
            } else {
                " ".to_owned()
            };

            let style = if row_from_bottom <= filled {
                Style::default().fg(color)
            } else if is_cpu_line {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };

            spans.push(Span::styled(ch, style));
        }

        lines.push(Line::from(spans));
    }

    lines
}

// ---------------------------------------------------------------------------
// Summary row helpers
// ---------------------------------------------------------------------------

/// Compute summary metrics over the current snapshot window.
///
/// Returns `(db_time_secs, wall_secs, aas, cpu_count)`.
fn compute_summary(snapshots: &[AshSnapshot]) -> (f64, f64, f64, u32) {
    if snapshots.is_empty() {
        return (0.0, 0.0, 0.0, 0);
    }

    // db_time = sum of active_count × 1s per snapshot (each snap = 1 raw second).
    let db_time: f64 = snapshots.iter().map(|s| f64::from(s.active_count)).sum();

    // wall = span from first to last sample timestamp, plus one second for the
    // last bucket itself.  Fall back to snapshot count when timestamps are zero.
    let first_ts = snapshots.first().map_or(0, |s| s.ts);
    let last_ts = snapshots.last().map_or(0, |s| s.ts);
    #[allow(clippy::cast_precision_loss)]
    let wall = if last_ts > first_ts {
        (last_ts - first_ts + 1) as f64
    } else {
        snapshots.len() as f64
    };

    let aas = if wall > 0.0 { db_time / wall } else { 0.0 };
    let cpu_count = snapshots.last().map_or(0, |s| s.cpu_count);

    (db_time, wall, aas, cpu_count)
}

// ---------------------------------------------------------------------------
// Drill-down table helpers
// ---------------------------------------------------------------------------

/// A single row for the drill-down table (pre-computed per-frame).
struct DrillRow {
    label: String,
    wait_type: String,
    /// Total session-seconds for this entry over the window.
    time_secs: f64,
    /// Percentage of total DB time.
    pct_db: f64,
    /// Average active sessions for this entry.
    aas: f64,
    /// Whether this is a sub-event row (indented under an expanded type).
    is_sub: bool,
}

/// Sort drill rows descending by `time_secs`.
fn sort_drill_rows(rows: &mut [DrillRow]) {
    rows.sort_by(|a, b| {
        b.time_secs
            .partial_cmp(&a.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Aggregate snapshots by `wait_event_type` into drill rows.
fn drill_rows_wait_type(snapshots: &[AshSnapshot], wall: f64, total: f64) -> Vec<DrillRow> {
    let mut type_totals: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for snap in snapshots {
        for (k, &v) in &snap.by_type {
            *type_totals.entry(k.clone()).or_insert(0.0) += f64::from(v);
        }
    }
    let mut rows: Vec<DrillRow> = type_totals
        .into_iter()
        .map(|(wt, t)| DrillRow {
            label: wt.clone(),
            wait_type: wt,
            time_secs: t,
            pct_db: t / total * 100.0,
            aas: t / wall,
            is_sub: false,
        })
        .collect();
    sort_drill_rows(&mut rows);
    rows
}

/// Aggregate snapshots by `wait_event` (filtered to `selected_type`) into drill rows.
fn drill_rows_wait_event(
    snapshots: &[AshSnapshot],
    selected_type: &str,
    wall: f64,
    total: f64,
) -> Vec<DrillRow> {
    let prefix = format!("{selected_type}/");
    let mut type_total = 0.0_f64;
    let mut event_totals: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for snap in snapshots {
        if let Some(&v) = snap.by_type.get(selected_type) {
            type_total += f64::from(v);
        }
        for (k, &v) in &snap.by_event {
            if k.starts_with(&prefix) {
                let event_name = k.strip_prefix(&prefix).unwrap_or("").to_owned();
                *event_totals.entry(event_name).or_insert(0.0) += f64::from(v);
            }
        }
    }
    let mut rows: Vec<DrillRow> = Vec::with_capacity(event_totals.len() + 1);
    rows.push(DrillRow {
        label: selected_type.to_owned(),
        wait_type: selected_type.to_owned(),
        time_secs: type_total,
        pct_db: type_total / total * 100.0,
        aas: type_total / wall,
        is_sub: false,
    });
    let mut sub_rows: Vec<DrillRow> = event_totals
        .into_iter()
        .map(|(ev, t)| DrillRow {
            label: format!("{selected_type}:{ev}"),
            wait_type: selected_type.to_owned(),
            time_secs: t,
            pct_db: t / total * 100.0,
            aas: t / wall,
            is_sub: true,
        })
        .collect();
    sort_drill_rows(&mut sub_rows);
    rows.extend(sub_rows);
    rows
}

/// Aggregate snapshots by `query_id` (filtered to type+event) into drill rows.
fn drill_rows_query_id(
    snapshots: &[AshSnapshot],
    selected_type: &str,
    selected_event: &str,
    wall: f64,
    total: f64,
) -> Vec<DrillRow> {
    // Keys are "<wtype>/<wevent>/<query_label>".
    let prefix = format!("{selected_type}/{selected_event}/");
    let mut query_totals: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for snap in snapshots {
        for (k, &v) in &snap.by_query {
            if k.starts_with(&prefix) {
                let label = k.strip_prefix(&prefix).unwrap_or("").to_owned();
                *query_totals.entry(label).or_insert(0.0) += f64::from(v);
            }
        }
    }
    let mut rows: Vec<DrillRow> = query_totals
        .into_iter()
        .map(|(label, t)| DrillRow {
            label,
            wait_type: selected_type.to_owned(),
            time_secs: t,
            pct_db: t / total * 100.0,
            aas: t / wall,
            is_sub: false,
        })
        .collect();
    sort_drill_rows(&mut rows);
    rows
}

/// Collect drill rows for the current level, computing time/pct/aas from the
/// full snapshot window (not just the last snapshot).
fn collect_drill_rows(
    snapshots: &[AshSnapshot],
    level: &DrillLevel,
    wall_secs: f64,
    total_db_time: f64,
) -> Vec<DrillRow> {
    if snapshots.is_empty() {
        return Vec::new();
    }
    let wall = wall_secs.max(1.0);
    let total = total_db_time.max(f64::EPSILON);
    match level {
        DrillLevel::WaitType => drill_rows_wait_type(snapshots, wall, total),
        DrillLevel::WaitEvent { selected_type } => {
            drill_rows_wait_event(snapshots, selected_type, wall, total)
        }
        DrillLevel::QueryId {
            selected_type,
            selected_event,
        } => drill_rows_query_id(snapshots, selected_type, selected_event, wall, total),
        DrillLevel::Pid { .. } => Vec::new(),
    }
}

/// Render one drill-down row as a `Line<'static>`.
fn drill_row_line(row: &DrillRow, is_selected: bool, no_color: bool) -> Line<'static> {
    let color = wait_type_color(&row.wait_type, no_color);
    let base_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    // Prefix: ▶ for selected top-level, ● for sub-event, two spaces for others.
    let prefix = if row.is_sub {
        "  \u{25cf} ".to_owned() // "  ● "
    } else if is_selected {
        "\u{25b6} ".to_owned() // "▶ "
    } else {
        "  ".to_owned()
    };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bar_len = ((row.pct_db / 5.0).round() as usize).clamp(0, 20);
    let bar: String = "\u{2588}".repeat(bar_len);

    Line::from(vec![
        Span::styled(prefix, base_style),
        Span::styled(format!("{:<22}", row.label), base_style),
        Span::styled(format!("{:>8.1}s", row.time_secs), base_style),
        Span::styled(format!("  {:>5.1}%", row.pct_db), base_style),
        Span::styled(format!("  {:>5.2}", row.aas), base_style),
        Span::styled(format!("  {bar}"), base_style.fg(color)),
    ])
}

// ---------------------------------------------------------------------------
// Public draw entry point
// ---------------------------------------------------------------------------

/// Render the timeline band (chunk[1]) inside `draw_frame`.
fn render_timeline(
    frame: &mut Frame,
    snapshots: &[AshSnapshot],
    state: &AshState,
    area: ratatui::layout::Rect,
    no_color: bool,
) {
    let cpu_count = snapshots.last().map_or(0, |s| s.cpu_count);
    let timeline_title = format!(
        " Timeline  bucket: {}  CPU ref: {cpu_count} ",
        state.zoom_label(),
    );
    let timeline_block = Block::default().borders(Borders::ALL).title(timeline_title);
    let timeline_inner = timeline_block.inner(area);
    frame.render_widget(timeline_block, area);
    let tl_lines = build_timeline_lines(
        snapshots,
        state,
        timeline_inner.height as usize,
        timeline_inner.width as usize,
        cpu_count,
        no_color,
    );
    frame.render_widget(Paragraph::new(tl_lines), timeline_inner);
}

/// Render the drill-down table band (chunk[3]) inside `draw_frame`.
fn render_drill_table(
    frame: &mut Frame,
    snapshots: &[AshSnapshot],
    state: &AshState,
    area: ratatui::layout::Rect,
    wall: f64,
    db_time: f64,
    no_color: bool,
) {
    let table_block = Block::default().borders(Borders::ALL).title(" Drill-down ");
    let table_inner = table_block.inner(area);
    frame.render_widget(table_block, area);

    if matches!(state.level, DrillLevel::Pid { .. }) {
        frame.render_widget(
            Paragraph::new("pid-level drill-down: coming soon"),
            table_inner,
        );
        return;
    }

    let header_chunks =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(table_inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<22}", "STAT NAME"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>9}", "TIME"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>8}", "%DB TIME"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>8}", "AAS"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("  BAR", Style::default().add_modifier(Modifier::BOLD)),
        ])),
        header_chunks[0],
    );

    let rows = collect_drill_rows(snapshots, &state.level, wall, db_time);
    let list_height = header_chunks[1].height as usize;
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
        .map(|(i, row)| drill_row_line(row, i == state.selected_row, no_color))
        .collect();
    frame.render_widget(Paragraph::new(lines), header_chunks[1]);
}

/// Draw a single frame of the `/ash` TUI.
///
/// * `frame`     — ratatui frame to render into.
/// * `snapshots` — ring buffer of raw snapshots, most recent last.
/// * `state`     — current drill-down / zoom state.
/// * `no_color`  — when true, use terminal default colors.
pub fn draw_frame(frame: &mut Frame, snapshots: &[AshSnapshot], state: &AshState, no_color: bool) {
    let area = frame.area();
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .title(" /ash \u{2014} Active Session History ");
    let inner = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(inner);

    // [0] Status bar
    let active = snapshots.last().map_or(0, |s| s.active_count);
    let mode_label = if state.is_history { "History" } else { "Live" };
    let status_text = format!(
        "[{mode_label}]  refresh: {}s   zoom: {}   active: {active}",
        state.refresh_secs,
        state.zoom_label(),
    );
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default()),
        chunks[0],
    );

    // [1] Timeline
    render_timeline(frame, snapshots, state, chunks[1], no_color);

    // [2] Summary row
    let (db_time, wall, aas, cpu) = compute_summary(snapshots);
    let summary_text =
        format!("DB TIME: {db_time:.1}s    WALL: {wall:.1}s    AAS: {aas:.2}    CPUs: {cpu}",);
    frame.render_widget(
        Paragraph::new(summary_text).style(Style::default().fg(Color::Cyan)),
        chunks[2],
    );

    // [3] Drill-down table
    render_drill_table(frame, snapshots, state, chunks[3], wall, db_time, no_color);

    // [4] Footer / key hints
    let hint =
        "q:quit  \u{2191}\u{2193}:select  Enter:drill  b:back  r:refresh  \u{2190}\u{2192}:zoom";
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[4],
    );
}
