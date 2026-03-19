//! EXPLAIN plan rendering: summary header and colored tree output.
//!
//! Provides static text rendering of `PostgreSQL` EXPLAIN (ANALYZE) plans
//! with ANSI color coding, Unicode box-drawing tree lines, hot-path
//! detection, and a summary header showing key metrics and detected issues.
//!
//! This module is standalone — it will be wired into the REPL in a later PR.
//!
//! Copyright 2026

#![allow(dead_code)]

use std::fmt::Write as FmtWrite;

// ---------------------------------------------------------------------------
// ANSI escape code helpers
// ---------------------------------------------------------------------------

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BRIGHT_RED_BOLD: &str = "\x1b[1;91m";
const CYAN: &str = "\x1b[36m";
const BOLD_WHITE: &str = "\x1b[1;37m";

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// Severity level for a detected plan issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueSeverity {
    /// Performance-critical problem (e.g. sequential scan on large table).
    Slow,
    /// Potential performance problem (e.g. row estimate mismatch, sort spill).
    Warn,
    /// Informational note.
    Info,
}

impl IssueSeverity {
    fn label(self) -> &'static str {
        match self {
            Self::Slow => "SLOW",
            Self::Warn => "WARN",
            Self::Info => "INFO",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Self::Slow => BRIGHT_RED_BOLD,
            Self::Warn => YELLOW,
            Self::Info => CYAN,
        }
    }
}

/// A detected issue in the EXPLAIN plan.
#[derive(Debug, Clone)]
pub struct PlanIssue {
    /// Severity of the issue.
    pub severity: IssueSeverity,
    /// Human-readable description (e.g. "Seq Scan on orders (2.1M rows, 1,204 ms)").
    pub message: String,
}

/// Top-level plan metadata for the summary header.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlan {
    /// Root node of the plan tree.
    pub root: ExplainNode,
    /// Total execution time in milliseconds (EXPLAIN ANALYZE only).
    pub execution_time_ms: Option<f64>,
    /// Planning time in milliseconds (EXPLAIN ANALYZE only).
    pub planning_time_ms: Option<f64>,
    /// Whether this is an EXPLAIN ANALYZE (vs plain EXPLAIN).
    pub is_analyze: bool,
}

impl ExplainPlan {
    /// Total shared buffer hits across all nodes in the plan.
    pub fn total_shared_hit(&self) -> u64 {
        self.root.total_shared_hit()
    }

    /// Total shared buffer reads across all nodes in the plan.
    pub fn total_shared_read(&self) -> u64 {
        self.root.total_shared_read()
    }

    /// Total actual rows returned by the root node (after all loops).
    pub fn total_rows(&self) -> Option<f64> {
        #[allow(clippy::cast_precision_loss)]
        self.root.actual_rows.map(|r| r * self.root.loops as f64)
    }

    /// Peak memory usage in bytes (from Sort nodes reporting sort space).
    pub fn peak_memory_bytes(&self) -> Option<u64> {
        self.root.peak_memory_bytes()
    }
}

/// A single node in the EXPLAIN plan tree.
///
/// This is a minimal rendering-focused struct. When the parser module lands,
/// this will be replaced by the parser's canonical type.
#[derive(Debug, Clone, Default)]
pub struct ExplainNode {
    /// Node type string (e.g. "Seq Scan", "Hash Join", "Sort").
    pub node_type: String,
    /// Relation name for scan nodes (e.g. "orders").
    pub relation: Option<String>,
    /// `(startup_ms, total_ms)` from EXPLAIN ANALYZE.
    pub actual_time_ms: Option<(f64, f64)>,
    /// Actual rows returned per loop.
    pub actual_rows: Option<f64>,
    /// Exclusive (self) time in milliseconds, excluding children.
    pub exclusive_time_ms: f64,
    /// Fraction of total plan time spent in this node (0.0–100.0).
    pub time_percent: f64,
    /// Number of times this node was executed (loop count).
    pub loops: u64,
    /// Shared buffer hits.
    pub shared_hit: u64,
    /// Shared buffer reads (from disk or OS cache).
    pub shared_read: u64,
    /// Filter expression string.
    pub filter: Option<String>,
    /// Rows removed by filter per loop.
    pub rows_removed_by_filter: Option<u64>,
    /// Sort method (e.g. "external merge", "quicksort").
    pub sort_method: Option<String>,
    /// Sort space used (e.g. "38472kB").
    pub sort_space: Option<String>,
    /// Child nodes.
    pub children: Vec<ExplainNode>,
}

impl ExplainNode {
    fn total_shared_hit(&self) -> u64 {
        self.shared_hit
            + self
                .children
                .iter()
                .map(ExplainNode::total_shared_hit)
                .sum::<u64>()
    }

    fn total_shared_read(&self) -> u64 {
        self.shared_read
            + self
                .children
                .iter()
                .map(ExplainNode::total_shared_read)
                .sum::<u64>()
    }

    fn peak_memory_bytes(&self) -> Option<u64> {
        let self_mem = self.sort_space.as_deref().and_then(parse_sort_space_bytes);
        let child_max = self
            .children
            .iter()
            .filter_map(ExplainNode::peak_memory_bytes)
            .max();
        match (self_mem, child_max) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    /// Return the index path to the most expensive leaf node (by
    /// `exclusive_time_ms`). The returned `Vec<usize>` contains indices into
    /// the `children` slices at each level. An empty vec means this node is
    /// the hot leaf.
    fn hot_path(&self) -> Vec<usize> {
        if self.children.is_empty() {
            return vec![];
        }
        let (idx, child) = self
            .children
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.exclusive_time_ms
                    .partial_cmp(&b.exclusive_time_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("children is non-empty");
        let mut path = vec![idx];
        path.extend(child.hot_path());
        path
    }
}

// ---------------------------------------------------------------------------
// Number formatting helpers
// ---------------------------------------------------------------------------

/// Format a `u64` with thousand separators: `1_234_567` → `"1,234,567"`.
fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// Format a `f64` rows value with thousand separators.
///
/// Values ≥ 1,000,000 are shown as `"2.1M"`, values ≥ 1,000 as `"48,301"`.
fn fmt_rows(rows: f64) -> String {
    if rows >= 1_000_000.0 {
        format!("{:.1}M", rows / 1_000_000.0)
    } else {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        fmt_int(rows as u64)
    }
}

/// Format a millisecond duration as `"1,842 ms"`.
fn fmt_ms(ms: f64) -> String {
    if ms < 1.0 {
        format!("{ms:.3} ms")
    } else if ms < 10.0 {
        format!("{ms:.2} ms")
    } else {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let n = ms as u64;
        format!("{} ms", fmt_int(n))
    }
}

/// Parse a `PostgreSQL` sort space string such as `"38472kB"` into bytes.
fn parse_sort_space_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(kb) = s.strip_suffix("kB") {
        kb.trim().parse::<u64>().ok().map(|v| v * 1024)
    } else if let Some(mb) = s.strip_suffix("MB") {
        mb.trim().parse::<u64>().ok().map(|v| v * 1024 * 1024)
    } else if let Some(b) = s.strip_suffix('B') {
        b.trim().parse::<u64>().ok()
    } else {
        None
    }
}

/// Format bytes as a binary unit string: `"42 MiB"`, `"3 KiB"`, `"512 B"`.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
fn fmt_bytes_binary(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{} GiB", bytes / (1024 * 1024 * 1024))
    } else if bytes >= 1024 * 1024 {
        let mib = bytes as f64 / (1024.0 * 1024.0);
        if mib >= 100.0 {
            format!("{} MiB", mib as u64)
        } else {
            format!("{mib:.0} MiB")
        }
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

// ---------------------------------------------------------------------------
// Summary header
// ---------------------------------------------------------------------------

/// Render the summary header for an EXPLAIN plan.
///
/// Produces a box like:
/// ```text
/// ── EXPLAIN ANALYZE ──────────────────────────────────
///   Execution: 1,842 ms │ Planning: 12 ms │ Rows: 48,301
///   Buffers: 124,800 hit, 3,201 read │ Peak mem: 42 MiB
///
///   Issues (3):
///     SLOW  Seq Scan on orders (2.1M rows, 1,204 ms)
///     WARN  Sort spilled to disk (38 MiB)
///     WARN  Row estimate 1,483x off on Nested Loop
/// ─────────────────────────────────────────────────────
/// ```
pub fn render_summary(plan: &ExplainPlan, issues: &[PlanIssue]) -> String {
    let mut out = String::new();

    let title = if plan.is_analyze {
        "EXPLAIN ANALYZE"
    } else {
        "EXPLAIN"
    };

    // Top rule — fixed width 52 chars total.
    let rule_len = 52usize;
    let title_part = format!("── {title} ");
    let dashes = rule_len.saturating_sub(title_part.len());
    let top_rule = format!("{BOLD_WHITE}{title_part}{}{RESET}", "─".repeat(dashes));
    out.push_str(&top_rule);
    out.push('\n');

    // Metrics line.
    let mut metrics = String::new();
    if let Some(exec) = plan.execution_time_ms {
        write!(metrics, "Execution: {}", fmt_ms(exec)).ok();
    }
    if let Some(plan_t) = plan.planning_time_ms {
        if !metrics.is_empty() {
            write!(metrics, " {DIM}│{RESET} ").ok();
        }
        write!(metrics, "Planning: {}", fmt_ms(plan_t)).ok();
    }
    if let Some(rows) = plan.total_rows() {
        if !metrics.is_empty() {
            write!(metrics, " {DIM}│{RESET} ").ok();
        }
        write!(metrics, "Rows: {}", fmt_rows(rows)).ok();
    }
    if !metrics.is_empty() {
        writeln!(out, "  {metrics}").ok();
    }

    // Buffer / memory line.
    let hit = plan.total_shared_hit();
    let read = plan.total_shared_read();
    if hit > 0 || read > 0 {
        let mut buf_line = format!("  Buffers: {} hit", fmt_int(hit));
        if read > 0 {
            write!(buf_line, ", {} read", fmt_int(read)).ok();
        }
        if let Some(mem) = plan.peak_memory_bytes() {
            write!(
                buf_line,
                " {DIM}│{RESET} Peak mem: {}",
                fmt_bytes_binary(mem)
            )
            .ok();
        }
        out.push_str(&buf_line);
        out.push('\n');
    } else if let Some(mem) = plan.peak_memory_bytes() {
        writeln!(out, "  Peak mem: {}", fmt_bytes_binary(mem)).ok();
    }

    // Issues section.
    if !issues.is_empty() {
        out.push('\n');
        writeln!(out, "  Issues ({}):", issues.len()).ok();
        for issue in issues {
            let color = issue.severity.ansi();
            let label = issue.severity.label();
            writeln!(out, "    {color}{BOLD}{label}{RESET}  {}", issue.message).ok();
        }
    }

    // Bottom rule.
    let bottom_rule = format!("{DIM}{}{RESET}", "─".repeat(rule_len));
    out.push_str(&bottom_rule);
    out.push('\n');

    out
}

// ---------------------------------------------------------------------------
// Colored tree rendering
// ---------------------------------------------------------------------------

/// State threaded through the recursive tree rendering.
struct TreeState<'a> {
    issues: &'a [PlanIssue],
    terminal_width: usize,
    /// Stack of "is last child" booleans for each nesting level.
    prefix_stack: Vec<bool>,
    /// Set of node indices (DFS order) on the hot path.
    hot_path_set: std::collections::HashSet<usize>,
    /// DFS counter to assign node indices.
    node_counter: usize,
}

impl<'a> TreeState<'a> {
    fn new(plan: &'a ExplainPlan, issues: &'a [PlanIssue], terminal_width: usize) -> Self {
        // Build hot path set by DFS-walking to identify hot nodes.
        let mut hot_set = std::collections::HashSet::new();
        let mut idx_counter = 0usize;
        collect_hot_path(
            &plan.root,
            &plan.root.hot_path(),
            &mut hot_set,
            &mut idx_counter,
        );
        Self {
            issues,
            terminal_width,
            prefix_stack: vec![],
            hot_path_set: hot_set,
            node_counter: 0,
        }
    }
}

/// Walk the tree and record DFS indices of nodes on the hot path.
fn collect_hot_path(
    node: &ExplainNode,
    hot_path: &[usize],
    hot_set: &mut std::collections::HashSet<usize>,
    counter: &mut usize,
) {
    let my_idx = *counter;
    *counter += 1;
    // The root is always on the hot path by definition.
    // A node is hot if it is the starting node or if its index matches
    // the first element of the remaining hot_path slice.
    hot_set.insert(my_idx);
    if let Some((&next, rest)) = hot_path.split_first() {
        for (i, child) in node.children.iter().enumerate() {
            if i == next {
                collect_hot_path(child, rest, hot_set, counter);
            } else {
                // Not on hot path — advance counter without inserting.
                let mut dummy_set = std::collections::HashSet::new();
                collect_hot_path(child, &[], &mut dummy_set, counter);
            }
        }
    } else {
        // No more hot-path directions — advance counter for remaining children.
        for child in &node.children {
            let mut dummy_set = std::collections::HashSet::new();
            collect_hot_path(child, &[], &mut dummy_set, counter);
        }
    }
}

/// Choose ANSI color based on `time_percent`.
fn time_percent_color(pct: f64) -> &'static str {
    if pct >= 60.0 {
        BRIGHT_RED_BOLD
    } else if pct >= 30.0 {
        RED
    } else if pct >= 10.0 {
        YELLOW
    } else {
        DIM
    }
}

/// Build the tree-prefix string for a given depth using `prefix_stack`.
///
/// `prefix_stack[i]` is `true` when the ancestor at depth `i` was the
/// last child (so we draw a space instead of `│ `).
fn build_prefix(stack: &[bool], is_last: bool) -> String {
    let mut s = String::new();
    for &was_last in stack {
        if was_last {
            s.push_str("   ");
        } else {
            s.push_str("│  ");
        }
    }
    if is_last {
        s.push_str("╰─ ");
    } else {
        s.push_str("├─ ");
    }
    s
}

/// Render a single node line and recurse into children.
fn render_node(
    node: &ExplainNode,
    state: &mut TreeState<'_>,
    is_last: bool,
    is_root: bool,
    out: &mut String,
) {
    let my_idx = state.node_counter;
    state.node_counter += 1;

    let is_hot = state.hot_path_set.contains(&my_idx);
    let color = time_percent_color(node.time_percent);

    // Build node label: "Seq Scan on orders" or just "Hash Join".
    let mut label = node.node_type.clone();
    if let Some(rel) = &node.relation {
        write!(label, " on {rel}").ok();
    }

    // Add loop indicator.
    if node.loops > 1 {
        write!(label, " (×{})", fmt_int(node.loops)).ok();
    }

    // Issue marker: ⚠ if any issue message contains a node identifier.
    let has_issue = {
        let node_key = node.relation.as_deref().unwrap_or(&node.node_type);
        state
            .issues
            .iter()
            .any(|iss| iss.message.contains(node_key))
    };

    // Right-side annotation: time + rows.
    let mut right = String::new();
    if let Some((_, total)) = node.actual_time_ms {
        write!(right, "{}", fmt_ms(total)).ok();
    }
    if let Some(rows) = node.actual_rows {
        if !right.is_empty() {
            right.push_str(", ");
        }
        write!(right, "{} rows", fmt_rows(rows)).ok();
    }

    // Tree prefix.
    let prefix = if is_root {
        String::new()
    } else {
        build_prefix(&state.prefix_stack, is_last)
    };

    // Compose the full line.
    let marker = if has_issue { " ⚠" } else { "" };

    // Wrap label+right into terminal width.
    // The visible prefix length approximates character count (no ANSI in prefix).
    let prefix_vis_len = prefix.chars().count();
    let right_len = right.chars().count();
    let left_part = format!("{label}{marker}");
    let left_vis = left_part.chars().count();

    // Pad so the right column aligns towards the terminal width.
    let avail = state
        .terminal_width
        .saturating_sub(prefix_vis_len + left_vis + 2);
    let padding = if right_len > 0 {
        " ".repeat(avail.saturating_sub(right_len).max(1))
    } else {
        String::new()
    };

    let bold_start = if is_hot { BOLD } else { "" };
    let bold_end = if is_hot { RESET } else { "" };

    // Assemble line with color.
    let line = if right.is_empty() {
        format!("{prefix}{color}{bold_start}{left_part}{bold_end}{RESET}")
    } else {
        format!("{prefix}{color}{bold_start}{left_part}{padding}{right}{bold_end}{RESET}")
    };

    out.push_str(&line);
    out.push('\n');

    // Render children recursively.
    let n = node.children.len();
    for (i, child) in node.children.iter().enumerate() {
        let child_is_last = i + 1 == n;
        state.prefix_stack.push(is_last || is_root);
        render_node(child, state, child_is_last, false, out);
        state.prefix_stack.pop();
    }
}

/// Render the plan tree with ANSI colors, hot-path bolding, issue markers,
/// and Unicode box-drawing characters.
///
/// - Nodes using <10% of total time are dim.
/// - 10–30%: yellow.
/// - 30–60%: red.
/// - >60%: bright red + bold.
/// - The hot path (chain to the most expensive leaf) is additionally bolded.
/// - Nodes with a matching issue entry show a ⚠ marker.
/// - Loop counts shown as `(×N)`.
/// - Time and row counts right-aligned within `terminal_width`.
pub fn render_colored_tree(
    plan: &ExplainPlan,
    issues: &[PlanIssue],
    terminal_width: usize,
) -> String {
    let mut out = String::new();
    let mut state = TreeState::new(plan, issues, terminal_width);
    render_node(&plan.root, &mut state, true, true, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Combined output
// ---------------------------------------------------------------------------

/// Render the full enhanced EXPLAIN output: summary header + colored tree.
pub fn render_enhanced(plan: &ExplainPlan, issues: &[PlanIssue], terminal_width: usize) -> String {
    let mut out = render_summary(plan, issues);
    out.push('\n');
    out.push_str(&render_colored_tree(plan, issues, terminal_width));
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helper builders
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn leaf(
        node_type: &str,
        relation: Option<&str>,
        actual_time_ms: Option<(f64, f64)>,
        actual_rows: f64,
        exclusive_time_ms: f64,
        time_percent: f64,
        loops: u64,
        shared_hit: u64,
        shared_read: u64,
    ) -> ExplainNode {
        ExplainNode {
            node_type: node_type.to_owned(),
            relation: relation.map(str::to_owned),
            actual_time_ms,
            actual_rows: Some(actual_rows),
            exclusive_time_ms,
            time_percent,
            loops,
            shared_hit,
            shared_read,
            filter: None,
            rows_removed_by_filter: None,
            sort_method: None,
            sort_space: None,
            children: vec![],
        }
    }

    fn simple_plan() -> ExplainPlan {
        // Hash Join
        //   ├─ Seq Scan on orders   (hot, 65% time)
        //   ╰─ Hash
        //        ╰─ Seq Scan on users
        let seq_orders = leaf(
            "Seq Scan",
            Some("orders"),
            Some((0.0, 1204.0)),
            2_100_000.0,
            1204.0,
            65.4,
            1,
            120_000,
            3000,
        );
        let seq_users = leaf(
            "Seq Scan",
            Some("users"),
            Some((0.0, 100.0)),
            50_000.0,
            100.0,
            5.4,
            1,
            4800,
            201,
        );
        let hash_node = ExplainNode {
            node_type: "Hash".to_owned(),
            relation: None,
            actual_time_ms: Some((0.0, 105.0)),
            actual_rows: Some(50_000.0),
            exclusive_time_ms: 5.0,
            time_percent: 0.3,
            loops: 1,
            shared_hit: 0,
            shared_read: 0,
            filter: None,
            rows_removed_by_filter: None,
            sort_method: None,
            sort_space: None,
            children: vec![seq_users],
        };
        let root = ExplainNode {
            node_type: "Hash Join".to_owned(),
            relation: None,
            actual_time_ms: Some((0.0, 1842.0)),
            actual_rows: Some(48_301.0),
            exclusive_time_ms: 533.0,
            time_percent: 28.9,
            loops: 1,
            shared_hit: 0,
            shared_read: 0,
            filter: None,
            rows_removed_by_filter: None,
            sort_method: None,
            sort_space: None,
            children: vec![seq_orders, hash_node],
        };
        ExplainPlan {
            root,
            execution_time_ms: Some(1842.0),
            planning_time_ms: Some(12.0),
            is_analyze: true,
        }
    }

    fn issues_for_plan() -> Vec<PlanIssue> {
        vec![
            PlanIssue {
                severity: IssueSeverity::Slow,
                message: "Seq Scan on orders (2.1M rows, 1,204 ms)".to_owned(),
            },
            PlanIssue {
                severity: IssueSeverity::Warn,
                message: "Sort spilled to disk (38 MiB)".to_owned(),
            },
            PlanIssue {
                severity: IssueSeverity::Warn,
                message: "Row estimate 1,483x off on Nested Loop".to_owned(),
            },
        ]
    }

    // -----------------------------------------------------------------------
    // Number formatting
    // -----------------------------------------------------------------------

    #[test]
    fn test_fmt_int_no_separator() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1000), "1,000");
    }

    #[test]
    fn test_fmt_int_thousands() {
        assert_eq!(fmt_int(1_842), "1,842");
        assert_eq!(fmt_int(48_301), "48,301");
        assert_eq!(fmt_int(1_234_567), "1,234,567");
        assert_eq!(fmt_int(124_800), "124,800");
    }

    #[test]
    fn test_fmt_rows_millions() {
        assert_eq!(fmt_rows(2_100_000.0), "2.1M");
        assert_eq!(fmt_rows(48_301.0), "48,301");
        assert_eq!(fmt_rows(999.0), "999");
    }

    #[test]
    fn test_fmt_ms() {
        assert_eq!(fmt_ms(1842.0), "1,842 ms");
        assert_eq!(fmt_ms(12.0), "12 ms");
        assert_eq!(fmt_ms(0.5), "0.500 ms");
        assert_eq!(fmt_ms(5.25), "5.25 ms");
    }

    #[test]
    fn test_fmt_bytes_binary() {
        assert_eq!(fmt_bytes_binary(42 * 1024 * 1024), "42 MiB");
        assert_eq!(fmt_bytes_binary(38 * 1024 * 1024), "38 MiB");
        assert_eq!(fmt_bytes_binary(3 * 1024), "3 KiB");
        assert_eq!(fmt_bytes_binary(512), "512 B");
        assert_eq!(fmt_bytes_binary(2 * 1024 * 1024 * 1024), "2 GiB");
    }

    #[test]
    fn test_parse_sort_space_bytes() {
        assert_eq!(parse_sort_space_bytes("38472kB"), Some(38472 * 1024));
        assert_eq!(parse_sort_space_bytes("10MB"), Some(10 * 1024 * 1024));
        assert_eq!(parse_sort_space_bytes("1024B"), Some(1024));
        assert_eq!(parse_sort_space_bytes("bad"), None);
    }

    // -----------------------------------------------------------------------
    // Summary header
    // -----------------------------------------------------------------------

    #[test]
    fn test_summary_header_contains_key_metrics() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let summary = render_summary(&plan, &issues);

        // Title
        assert!(summary.contains("EXPLAIN ANALYZE"));
        // Execution time with thousand separator
        assert!(summary.contains("1,842 ms"));
        // Planning time
        assert!(summary.contains("12 ms"));
        // Rows
        assert!(summary.contains("48,301"));
        // Buffer hit
        assert!(summary.contains("124,800 hit"));
        // Buffer read
        assert!(summary.contains("3,201 read"));
        // Issues count
        assert!(summary.contains("Issues (3)"));
    }

    #[test]
    fn test_summary_issue_labels() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let summary = render_summary(&plan, &issues);

        assert!(summary.contains("SLOW"));
        assert!(summary.contains("WARN"));
        assert!(summary.contains("Seq Scan on orders"));
        assert!(summary.contains("Sort spilled to disk"));
    }

    #[test]
    fn test_summary_no_issues() {
        let plan = simple_plan();
        let summary = render_summary(&plan, &[]);
        assert!(!summary.contains("Issues"));
    }

    #[test]
    fn test_summary_plain_explain_title() {
        let mut plan = simple_plan();
        plan.is_analyze = false;
        plan.execution_time_ms = None;
        plan.planning_time_ms = None;
        let summary = render_summary(&plan, &[]);
        // Should show EXPLAIN not EXPLAIN ANALYZE.
        // The top_rule contains "── EXPLAIN ─…"; "ANALYZE" must not appear.
        assert!(summary.contains("EXPLAIN"));
        assert!(!summary.contains("ANALYZE"));
    }

    #[test]
    fn test_summary_peak_memory() {
        let mut plan = simple_plan();
        plan.root.sort_space = Some("38472kB".to_owned());
        plan.root.sort_method = Some("external merge".to_owned());
        let summary = render_summary(&plan, &[]);
        assert!(summary.contains("Peak mem:"));
        assert!(summary.contains("MiB"));
    }

    // -----------------------------------------------------------------------
    // Color codes
    // -----------------------------------------------------------------------

    #[test]
    fn test_color_for_time_percent() {
        // <10%: dim
        assert_eq!(time_percent_color(5.0), DIM);
        assert_eq!(time_percent_color(9.9), DIM);
        // 10–30%: yellow
        assert_eq!(time_percent_color(10.0), YELLOW);
        assert_eq!(time_percent_color(29.9), YELLOW);
        // 30–60%: red
        assert_eq!(time_percent_color(30.0), RED);
        assert_eq!(time_percent_color(59.9), RED);
        // ≥60%: bright red bold
        assert_eq!(time_percent_color(60.0), BRIGHT_RED_BOLD);
        assert_eq!(time_percent_color(100.0), BRIGHT_RED_BOLD);
    }

    // -----------------------------------------------------------------------
    // Colored tree
    // -----------------------------------------------------------------------

    #[test]
    fn test_tree_contains_node_types() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let tree = render_colored_tree(&plan, &issues, 80);

        assert!(tree.contains("Hash Join"));
        assert!(tree.contains("Seq Scan"));
        assert!(tree.contains("orders"));
        assert!(tree.contains("Hash"));
        assert!(tree.contains("users"));
    }

    #[test]
    fn test_tree_contains_box_drawing() {
        let plan = simple_plan();
        let tree = render_colored_tree(&plan, &[], 80);
        // Should have at least one box-drawing connector.
        assert!(tree.contains("├─") || tree.contains("╰─"));
    }

    #[test]
    fn test_tree_issue_marker() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let tree = render_colored_tree(&plan, &issues, 80);
        // The Seq Scan on orders node should have the ⚠ marker.
        assert!(tree.contains('⚠'));
    }

    #[test]
    fn test_tree_no_issue_marker_when_no_issues() {
        let plan = simple_plan();
        let tree = render_colored_tree(&plan, &[], 80);
        assert!(!tree.contains('⚠'));
    }

    #[test]
    fn test_tree_contains_time() {
        let plan = simple_plan();
        let tree = render_colored_tree(&plan, &[], 80);
        // Root node actual_time_ms total is 1842 ms.
        assert!(tree.contains("1,842 ms"));
        // Seq Scan on orders: 1204 ms.
        assert!(tree.contains("1,204 ms"));
    }

    #[test]
    fn test_tree_contains_rows() {
        let plan = simple_plan();
        let tree = render_colored_tree(&plan, &[], 80);
        // orders: 2.1M rows
        assert!(tree.contains("2.1M rows"));
        // users: 50,000 rows
        assert!(tree.contains("50,000 rows"));
    }

    #[test]
    fn test_tree_loops_shown() {
        let mut plan = simple_plan();
        plan.root.children[0].loops = 5;
        let tree = render_colored_tree(&plan, &[], 80);
        assert!(tree.contains("×5"));
    }

    #[test]
    fn test_tree_ansi_codes_present() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let tree = render_colored_tree(&plan, &issues, 80);
        // ANSI escape codes should be present.
        assert!(tree.contains('\x1b'));
    }

    // -----------------------------------------------------------------------
    // Hot path detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_hot_path_most_expensive_leaf() {
        // The hot path should lead to Seq Scan on orders (exclusive_time_ms=1204).
        let plan = simple_plan();
        let hot = plan.root.hot_path();
        // Root → child 0 (Seq Scan on orders, exclusive=1204) is hottest.
        assert_eq!(hot, vec![0]);
    }

    #[test]
    fn test_hot_path_leaf_node_is_empty() {
        let node = leaf("Seq Scan", Some("t"), None, 100.0, 100.0, 50.0, 1, 0, 0);
        assert!(node.hot_path().is_empty());
    }

    #[test]
    fn test_hot_path_nested() {
        // root → [seq(excl=5), nested(excl=1) → [idx(excl=200)]]
        // At root level: seq.exclusive=5 > nested.exclusive=1, so hot child is seq(idx=0).
        let idx = leaf("Index Scan", Some("t"), None, 10.0, 200.0, 70.0, 1, 0, 0);
        let nested = ExplainNode {
            node_type: "Nested Loop".to_owned(),
            relation: None,
            actual_time_ms: None,
            actual_rows: Some(10.0),
            exclusive_time_ms: 1.0,
            time_percent: 0.5,
            loops: 1,
            shared_hit: 0,
            shared_read: 0,
            filter: None,
            rows_removed_by_filter: None,
            sort_method: None,
            sort_space: None,
            children: vec![idx],
        };
        let seq = leaf("Seq Scan", Some("s"), None, 1000.0, 5.0, 2.0, 1, 0, 0);
        let root = ExplainNode {
            node_type: "Hash Join".to_owned(),
            relation: None,
            actual_time_ms: None,
            actual_rows: Some(100.0),
            exclusive_time_ms: 10.0,
            time_percent: 3.0,
            loops: 1,
            shared_hit: 0,
            shared_read: 0,
            filter: None,
            rows_removed_by_filter: None,
            sort_method: None,
            sort_space: None,
            children: vec![seq, nested],
        };
        // seq(idx=0) has exclusive=5, nested(idx=1) has exclusive=1 → hot is [0].
        let hot = root.hot_path();
        assert_eq!(hot, vec![0]);
    }

    // -----------------------------------------------------------------------
    // Combined output
    // -----------------------------------------------------------------------

    #[test]
    fn test_render_enhanced_has_both_sections() {
        let plan = simple_plan();
        let issues = issues_for_plan();
        let out = render_enhanced(&plan, &issues, 80);
        // Summary section.
        assert!(out.contains("EXPLAIN ANALYZE"));
        assert!(out.contains("Issues (3)"));
        // Tree section.
        assert!(out.contains("Hash Join"));
        assert!(out.contains("╰─") || out.contains("├─"));
    }

    // -----------------------------------------------------------------------
    // Buffer totals
    // -----------------------------------------------------------------------

    #[test]
    fn test_plan_total_shared_hit() {
        let plan = simple_plan();
        // Root: 0, seq_orders: 120_000, hash: 0, seq_users: 4_800
        assert_eq!(plan.total_shared_hit(), 124_800);
    }

    #[test]
    fn test_plan_total_shared_read() {
        let plan = simple_plan();
        // seq_orders: 3_000, seq_users: 201
        assert_eq!(plan.total_shared_read(), 3_201);
    }
}
