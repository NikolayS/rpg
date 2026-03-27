//! Ratatui renderer for `/ash`.
//!
//! Renders a scrolling stacked-bar timeline, a summary metrics row, and a
//! drill-down table with Time / %DB Time / AAS / Bar columns.

use ratatui::{
    layout::{Constraint, Direction, Layout},
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

/// Detect whether the terminal supports 24-bit truecolor.
///
/// Checks `COLORTERM=truecolor|24bit` (the standard advertisement).  Falls
/// back to false so 256-color terminals get the indexed palette.
fn terminal_has_truecolor() -> bool {
    matches!(
        std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_lowercase()
            .as_str(),
        "truecolor" | "24bit"
    )
}

/// Return the color for a given `wait_event_type` string.
///
/// Uses 24-bit RGB when the terminal advertises truecolor (`COLORTERM=truecolor`),
/// otherwise falls back to the closest xterm-256 index so the chart looks
/// correct in standard 256-color terminals (e.g. remote SSH without truecolor).
///
/// When `no_color` is true (or `NO_COLOR` is set), returns `Color::Reset`.
pub fn wait_type_color(wait_event_type: &str, no_color: bool) -> Color {
    if no_color || std::env::var_os("NO_COLOR").is_some() {
        return Color::Reset;
    }
    if terminal_has_truecolor() {
        // Exact 24-bit RGB matching pg_ash COLOR_SCHEME.md.
        match wait_event_type {
            "CPU*" => Color::Rgb(80, 250, 123),
            "IdleTx" => Color::Rgb(241, 250, 140),
            "IO" => Color::Rgb(30, 100, 255),
            "Lock" => Color::Rgb(255, 85, 85),
            "LWLock" => Color::Rgb(255, 121, 198),
            "IPC" => Color::Rgb(0, 200, 255),
            "Client" => Color::Rgb(255, 220, 100),
            "Timeout" => Color::Rgb(255, 165, 0),
            "BufferPin" => Color::Rgb(0, 210, 180),
            "Activity" => Color::Rgb(150, 100, 255),
            "Extension" => Color::Rgb(190, 150, 255),
            _ => Color::Rgb(180, 180, 180),
        }
    } else {
        // Nearest xterm-256 indices — visually close, work everywhere.
        match wait_event_type {
            "CPU*" => Color::Indexed(84),       // bright green
            "IdleTx" => Color::Indexed(228),    // light yellow
            "IO" => Color::Indexed(27),         // vivid blue
            "Lock" => Color::Indexed(203),      // coral red
            "LWLock" => Color::Indexed(212),    // pink
            "IPC" => Color::Indexed(45),        // cyan
            "Client" => Color::Indexed(221),    // yellow
            "Timeout" => Color::Indexed(214),   // orange
            "BufferPin" => Color::Indexed(43),  // teal
            "Activity" => Color::Indexed(135),  // purple
            "Extension" => Color::Indexed(183), // light purple
            _ => Color::Indexed(246),           // gray
        }
    }
}

// ---------------------------------------------------------------------------
// Timeline helpers
// ---------------------------------------------------------------------------

/// One aggregated display bucket, derived from one or more raw snapshots.
///
/// Each bucket carries the average AAS per wait type so the timeline can
/// render a proper stacked bar (one color segment per wait type).
struct Bucket {
    /// Total average active sessions across all wait types.
    aas: f64,
    /// AAS broken down by wait type, sorted descending by count so the
    /// bottom of the bar starts with the busiest type.
    ///
    /// Each entry is `(wait_type_name, aas_for_that_type)`.
    by_type: Vec<(String, f64)>,
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
    let total = snapshots.len();
    let remainder = total % step;

    // Build chunks from oldest to newest; first chunk may be partial.
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

    // Trim to max_cols (keep rightmost = most recent).
    if chunks.len() > max_cols {
        let drop = chunks.len() - max_cols;
        chunks.drain(..drop);
    }

    // Compute global type totals across ALL buckets to establish a stable
    // ordering.  The same order is applied to every bucket so dominant types
    // always occupy the same vertical position — bars don't jump as load shifts.
    let mut global_totals: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    for snap in snapshots {
        for (k, &v) in &snap.by_type {
            *global_totals.entry(k.as_str()).or_insert(0.0) += f64::from(v);
        }
    }
    // Stable order: highest global total first (= bottom of bar), tie-break by name.
    let mut global_order: Vec<&str> = global_totals.keys().copied().collect();
    global_order.sort_by(|a, b| {
        global_totals[b]
            .partial_cmp(&global_totals[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });

    chunks
        .into_iter()
        .map(|chunk| {
            #[allow(clippy::cast_precision_loss)]
            let n = chunk.len() as f64;

            // Sum counts per type across all samples in the bucket.
            let mut type_sums: std::collections::HashMap<&str, f64> =
                std::collections::HashMap::new();
            for snap in chunk {
                for (k, &v) in &snap.by_type {
                    *type_sums.entry(k.as_str()).or_insert(0.0) += f64::from(v);
                }
            }

            // Apply the global stable order — include ALL types, even absent ones
            // (aas = 0.0).  Zero-height entries reserve their vertical slot so
            // the position of every type is frame-stable; the renderer skips
            // zero-aas entries when building segments.
            let by_type: Vec<(String, f64)> = global_order
                .iter()
                .map(|&k| {
                    let aas = type_sums.get(k).copied().unwrap_or(0.0) / n;
                    (k.to_owned(), aas)
                })
                .collect();

            #[allow(clippy::cast_precision_loss)]
            let aas: f64 = by_type.iter().map(|(_, v)| v).sum();

            Bucket { aas, by_type }
        })
        .collect()
}

/// One colored segment within a stacked timeline bar.
///
/// Coordinates are in row-from-bottom space (1 = bottommost row).
struct Segment {
    color: Color,
    /// Inclusive lower bound in row-from-bottom space.
    bottom: usize,
    /// Inclusive upper bound in row-from-bottom space.
    top: usize,
}

/// Width reserved for the Y-axis label column (e.g. " 9.9 ").
const YAXIS_WIDTH: u16 = 5;

/// Build Y-axis label lines for the timeline.
///
/// Returns `chart_height` lines, each containing a right-aligned AAS value
/// at the top, midpoint, and bottom; other rows are blank.  The labels are
/// formatted as right-aligned 4-char strings (e.g. " 9.9", "19.1").
fn build_yaxis_lines(max_aas: f64, chart_height: usize) -> Vec<Line<'static>> {
    let h = chart_height.max(1);
    let label_style = Style::default().fg(Color::DarkGray);

    // Label positions depend on chart height for appropriate density.
    // h <= 6:  top + bottom only
    // h <= 14: top + mid + bottom
    // h >  14: top + 1/4 + mid + 3/4 + bottom
    let labeled_rows: Vec<(usize, f64)> = if h <= 6 {
        vec![(0, max_aas), (h.saturating_sub(1), 0.0)]
    } else if h <= 14 {
        vec![
            (0, max_aas),
            (h / 2, max_aas / 2.0),
            (h.saturating_sub(1), 0.0),
        ]
    } else {
        vec![
            (0, max_aas),
            (h / 4, max_aas * 3.0 / 4.0),
            (h / 2, max_aas / 2.0),
            (h * 3 / 4, max_aas / 4.0),
            (h.saturating_sub(1), 0.0),
        ]
    };

    (0..h)
        .map(|row| {
            if let Some(&(_, val)) = labeled_rows.iter().find(|(r, _)| *r == row) {
                // Format: at most 4 chars + trailing space = 5 total.
                let s = if val == 0.0 {
                    "   0 ".to_owned()
                } else if val < 10.0 {
                    format!("{val:4.1} ")
                } else {
                    format!("{val:4.0} ")
                };
                Line::from(Span::styled(s, label_style))
            } else {
                Line::from(Span::styled("     ", label_style))
            }
        })
        .collect()
}

/// Render the scrolling stacked-bar timeline into a vec of `Line`s.
///
/// Build the stacked `Segment` list for one bucket column.
fn bucket_segments(bucket: &Bucket, max_aas: f64, h: usize, no_color: bool) -> Vec<Segment> {
    #[allow(clippy::cast_precision_loss)]
    let h_f64 = h as f64;
    let mut segs: Vec<Segment> = Vec::new();
    let mut filled_so_far = 0usize;
    for (wtype, type_aas) in &bucket.by_type {
        if *type_aas <= 0.0 {
            continue;
        }
        let frac = type_aas / max_aas;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let height = ((frac * h_f64).round() as usize).clamp(1, h);
        let bottom = filled_so_far + 1;
        let top = (filled_so_far + height).min(h);
        if bottom > h {
            break;
        }
        segs.push(Segment {
            color: wait_type_color(wtype, no_color),
            bottom,
            top,
        });
        filled_so_far = top;
        if filled_so_far >= h {
            break;
        }
    }
    segs
}

fn build_timeline_lines(
    snapshots: &[AshSnapshot],
    state: &AshState,
    chart_height: usize,
    chart_width: usize,
    cpu_count: Option<u32>,
    no_color: bool,
) -> (Vec<Line<'static>>, f64) {
    let h = chart_height.max(1);
    let w = chart_width.max(1);

    let buckets = aggregate_buckets(snapshots, state.bucket_secs(), w);

    // Scale: find the maximum AAS across all buckets so bars fill the chart.
    let max_aas = buckets
        .iter()
        .map(|b| b.aas)
        .fold(0.0_f64, f64::max)
        .max(1.0);

    #[allow(clippy::cast_precision_loss)]
    let h_f64 = h as f64;

    // Pre-compute per-column stacked segment boundaries.
    let col_segments: Vec<Vec<Segment>> = buckets
        .iter()
        .map(|bucket| bucket_segments(bucket, max_aas, h, no_color))
        .collect();

    // CPU reference line — only drawn when cpu_count is known.
    // Color: red when current AAS > cpu_count (overloaded), gray otherwise.
    let current_aas = buckets.last().map_or(0.0, |b| b.aas);
    let cpu_line_color = cpu_count.map_or(Color::DarkGray, |n| {
        if current_aas > f64::from(n) {
            Color::Red
        } else {
            Color::DarkGray
        }
    });
    let cpu_row_from_bottom: Option<usize> = cpu_count.and_then(|n| {
        if n == 0 || max_aas <= 0.0 {
            return None;
        }
        let frac = f64::from(n) / max_aas;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let r = (frac * h_f64).round() as usize;
        Some(r.clamp(1, h))
    });

    // Empty state: no buckets at all — return a single centered message line.
    if buckets.is_empty() {
        let msg = "No active sessions";
        let pad = w.saturating_sub(msg.len()) / 2;
        let empty_line = Line::from(vec![
            Span::raw(" ".repeat(pad)),
            Span::styled(msg, Style::default().fg(Color::DarkGray)),
        ]);
        let mut empty_lines = vec![Line::raw(""); h / 2];
        if empty_lines.len() < h {
            empty_lines.push(empty_line);
        }
        return (empty_lines, 0.0);
    }

    let pad_cols = w.saturating_sub(buckets.len());

    // Render rows top-to-bottom.
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(h);
    for row_from_top in 0..h {
        let row_from_bottom = h - row_from_top; // 1-indexed
        let is_cpu_line = cpu_row_from_bottom.is_some_and(|r| r == row_from_bottom);

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(w + 1);

        // Left padding.
        if pad_cols > 0 {
            let pad_ch = if is_cpu_line { "\u{2500}" } else { " " };
            let pad_style = if is_cpu_line {
                Style::default().fg(cpu_line_color)
            } else {
                Style::default()
            };
            spans.push(Span::styled(pad_ch.repeat(pad_cols), pad_style));
        }

        // One span per column.
        for segs in &col_segments {
            // Find which segment covers this row, if any.
            let seg = segs
                .iter()
                .find(|s| row_from_bottom >= s.bottom && row_from_bottom <= s.top);

            let (ch, style) = if let Some(s) = seg {
                ("\u{2588}".to_owned(), Style::default().fg(s.color)) // █ filled
            } else if is_cpu_line {
                ("\u{2500}".to_owned(), Style::default().fg(cpu_line_color)) // ─
            } else {
                (" ".to_owned(), Style::default())
            };

            spans.push(Span::styled(ch, style));
        }

        lines.push(Line::from(spans));
    }

    (lines, max_aas)
}

// ---------------------------------------------------------------------------
// Summary row helpers
// ---------------------------------------------------------------------------

/// Compute summary metrics over the current snapshot window.
///
/// Returns `(db_time_secs, wall_secs, aas, cpu_count)`.
fn compute_summary(snapshots: &[AshSnapshot]) -> (f64, f64, f64, Option<u32>) {
    if snapshots.is_empty() {
        return (0.0, 0.0, 0.0, None);
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
    let cpu_count = snapshots.last().and_then(|s| s.cpu_count);

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

// ---------------------------------------------------------------------------
// Legend overlay
// ---------------------------------------------------------------------------

/// Ordered list of all wait event types for the color legend.
const LEGEND_TYPES: &[&str] = &[
    "CPU*",
    "IO",
    "Lock",
    "LWLock",
    "IPC",
    "IdleTx",
    "Client",
    "Timeout",
    "BufferPin",
    "Activity",
    "Extension",
    "Other",
];

/// Width of the legend overlay in columns.
const LEGEND_WIDTH: u16 = 14;

/// Render a color legend as a floating overlay in the top-right corner of `area`.
///
/// Each line shows a colored block character followed by the wait type name.
/// Only rendered when the area is tall enough to fit all entries.
fn render_legend(frame: &mut Frame, area: ratatui::layout::Rect, no_color: bool) {
    #[allow(clippy::cast_possible_truncation)]
    let legend_height = LEGEND_TYPES.len() as u16;
    if area.height < legend_height || area.width < LEGEND_WIDTH + 4 {
        return;
    }
    let legend_x = area.x + area.width.saturating_sub(LEGEND_WIDTH);
    let legend_rect = ratatui::layout::Rect {
        x: legend_x,
        y: area.y,
        width: LEGEND_WIDTH,
        height: legend_height,
    };
    let lines: Vec<Line<'static>> = LEGEND_TYPES
        .iter()
        .map(|&wtype| {
            let color = wait_type_color(wtype, no_color);
            Line::from(vec![
                Span::styled("\u{2588} ", Style::default().fg(color)),
                Span::raw(wtype.to_owned()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), legend_rect);
}

/// Render the timeline band (chunk[1]) inside `draw_frame`.
/// Summary metrics passed into the timeline renderer for the bottom title.
struct SummaryMetrics {
    db_time: f64,
    wall: f64,
    aas: f64,
}

fn render_timeline(
    frame: &mut Frame,
    snapshots: &[AshSnapshot],
    state: &AshState,
    area: ratatui::layout::Rect,
    no_color: bool,
    summary: &SummaryMetrics,
) {
    let cpu_count = snapshots.last().and_then(|s| s.cpu_count);
    let cpu_ref_label = cpu_count.map_or(String::new(), |n| format!("  CPU ref: {n}"));
    let timeline_title = format!(
        " Timeline  bucket: {}  AAS: {:.2}{cpu_ref_label} ",
        state.zoom_label(),
        summary.aas,
    );
    let bottom_title = format!(
        " DB TIME: {:.1}s   WALL: {:.1}s ",
        summary.db_time, summary.wall,
    );
    let timeline_block = Block::default()
        .borders(Borders::ALL)
        .title(timeline_title)
        .title_bottom(bottom_title);
    let timeline_inner = timeline_block.inner(area);
    frame.render_widget(timeline_block, area);

    // Split inner area: narrow Y-axis label column on the left, bars on the right.
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(YAXIS_WIDTH), Constraint::Min(1)])
        .split(timeline_inner);

    let bar_area = h_chunks[1];
    let yaxis_area = h_chunks[0];

    let (tl_lines, max_aas) = build_timeline_lines(
        snapshots,
        state,
        bar_area.height as usize,
        bar_area.width as usize,
        cpu_count,
        no_color,
    );
    frame.render_widget(Paragraph::new(tl_lines), bar_area);

    let yaxis_lines = build_yaxis_lines(max_aas, yaxis_area.height as usize);
    frame.render_widget(Paragraph::new(yaxis_lines), yaxis_area);

    // Legend overlay — rendered top-right inside bar_area when `l` is toggled.
    if state.show_legend {
        render_legend(frame, bar_area, no_color);
    }
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

// Minimum terminal height required to render the `/ash` TUI without garbling.
const MIN_HEIGHT: u16 = 18;

/// Draw a single frame of the `/ash` TUI.
///
/// * `frame`     — ratatui frame to render into.
/// * `snapshots` — ring buffer of raw snapshots, most recent last.
/// * `state`     — current drill-down / zoom state.
/// * `no_color`  — when true, use terminal default colors.
pub fn draw_frame(frame: &mut Frame, snapshots: &[AshSnapshot], state: &AshState, no_color: bool) {
    let area = frame.area();

    if area.height < MIN_HEIGHT {
        frame.render_widget(
            Paragraph::new("terminal too small (need \u{2265}18 rows)")
                .style(Style::default().fg(Color::Red)),
            area,
        );
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(area);

    // [0] Status bar — title + live metrics on one line
    let active = snapshots.last().map_or(0, |s| s.active_count);
    let mode_label = if state.is_history { "History" } else { "Live" };
    let status_text = format!(
        "/ash  [{mode_label}]  interval: {}s   window: {}   active: {active}",
        state.refresh_interval_secs,
        state.window_label(),
    );
    frame.render_widget(
        Paragraph::new(status_text).style(Style::default().fg(Color::Cyan)),
        chunks[0],
    );

    // [1] Timeline — summary metrics embedded in bottom border title
    let (db_time, wall, aas, _cpu) = compute_summary(snapshots);
    let summary = SummaryMetrics { db_time, wall, aas };
    render_timeline(frame, snapshots, state, chunks[1], no_color, &summary);

    // [2] Drill-down table
    render_drill_table(frame, snapshots, state, chunks[2], wall, db_time, no_color);

    // [3] Footer — context-sensitive key hints
    frame.render_widget(
        Paragraph::new(state.hint_line()).style(Style::default().fg(Color::DarkGray)),
        chunks[3],
    );
}
