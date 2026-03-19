// Copyright 2026 Rpg contributors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `EXPLAIN ANALYZE` text-format parser and plan data structures.
//!
//! Parses the standard `PostgreSQL` text-format output of `EXPLAIN ANALYZE`
//! into a structured tree of [`ExplainNode`]s with timing, buffer, and
//! other statistics populated.
//!
//! # Usage
//!
//! ```
//! use rpg::explain::parse;
//!
//! let output = "Seq Scan on users  (cost=0.00..1.05 rows=5 width=4) \
//!               (actual time=0.010..0.012 rows=5 loops=1)\n\
//!     Planning Time: 0.1 ms\n\
//!     Execution Time: 0.2 ms\n";
//! let plan = parse(output).unwrap();
//! assert_eq!(plan.nodes.len(), 1);
//! ```

// This module is a new feature not yet wired into the main binary.
// Suppress dead-code warnings until Sprint 2 adds the UI layer.
#![allow(dead_code)]

pub mod issues;
pub mod render;

use std::fmt;

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

/// A fully-parsed `EXPLAIN ANALYZE` plan.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlan {
    /// Root nodes of the plan tree (usually exactly one).
    pub nodes: Vec<ExplainNode>,
    /// Planning time in milliseconds, if present.
    pub planning_time_ms: Option<f64>,
    /// Execution time in milliseconds, if present.
    pub execution_time_ms: Option<f64>,
    /// Trigger statistics, if any triggers fired.
    pub triggers: Vec<TriggerInfo>,
}

/// Statistics for a single trigger that fired during execution.
#[derive(Debug, Clone)]
pub struct TriggerInfo {
    /// Trigger name.
    pub name: String,
    /// Total time spent in the trigger in milliseconds.
    pub time_ms: f64,
    /// Number of times the trigger was called.
    pub calls: u64,
}

/// A single node in the EXPLAIN plan tree.
#[derive(Debug, Clone)]
pub struct ExplainNode {
    /// Node type label, e.g. `"Seq Scan"`, `"Hash Join"`, `"Index Scan"`.
    pub node_type: String,
    /// Relation (table or index) name, if applicable.
    pub relation: Option<String>,
    /// Table alias, if present.
    pub alias: Option<String>,
    /// Planner-estimated row count.
    pub estimated_rows: Option<f64>,
    /// Actual row count from execution.
    pub actual_rows: Option<f64>,
    /// Planner estimated cost as `(startup, total)`.
    pub estimated_cost: Option<(f64, f64)>,
    /// Actual time in ms as `(startup, total)`.
    pub actual_time_ms: Option<(f64, f64)>,
    /// Number of loops for this node.
    pub loops: u64,
    /// Self time = `actual_total * loops` − `sum(child.actual_total * child.loops)`.
    pub exclusive_time_ms: f64,
    /// Percentage of total execution time spent in this node (exclusive).
    pub time_percent: f64,
    /// Shared buffer hits.
    pub shared_hit: u64,
    /// Shared buffer reads.
    pub shared_read: u64,
    /// Shared buffers dirtied.
    pub shared_dirtied: u64,
    /// Shared buffers written.
    pub shared_written: u64,
    /// Sort method, e.g. `"quicksort"`, `"external merge"`.
    pub sort_method: Option<String>,
    /// Sort space used, e.g. `"Memory: 25kB"` or `"Disk: 38412kB"`.
    pub sort_space: Option<String>,
    /// Number of hash batches used.
    pub hash_batches: Option<u64>,
    /// Number of hash buckets used.
    pub hash_buckets: Option<u64>,
    /// Filter expression string.
    pub filter: Option<String>,
    /// Number of rows removed by filter.
    pub rows_removed_by_filter: Option<u64>,
    /// Index condition string.
    pub index_cond: Option<String>,
    /// Join type, e.g. `"Inner"`, `"Left"`, `"Anti"`.
    pub join_type: Option<String>,
    /// Planned number of parallel workers.
    pub workers_planned: Option<u64>,
    /// Actual number of parallel workers launched.
    pub workers_launched: Option<u64>,
    /// Child nodes (sub-plans).
    pub children: Vec<ExplainNode>,
    /// Original text lines for this node (for display/debug purposes).
    pub raw_lines: Vec<String>,
    /// Indentation depth (0 = root).
    pub depth: usize,
}

impl Default for ExplainNode {
    fn default() -> Self {
        Self {
            node_type: String::new(),
            relation: None,
            alias: None,
            estimated_rows: None,
            actual_rows: None,
            estimated_cost: None,
            actual_time_ms: None,
            loops: 1,
            exclusive_time_ms: 0.0,
            time_percent: 0.0,
            shared_hit: 0,
            shared_read: 0,
            shared_dirtied: 0,
            shared_written: 0,
            sort_method: None,
            sort_space: None,
            hash_batches: None,
            hash_buckets: None,
            filter: None,
            rows_removed_by_filter: None,
            index_cond: None,
            join_type: None,
            workers_planned: None,
            workers_launched: None,
            children: Vec::new(),
            raw_lines: Vec::new(),
            depth: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Error returned when parsing an EXPLAIN plan fails.
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Human-readable description of what went wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "explain parse error: {}", self.message)
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser internals
// ---------------------------------------------------------------------------

/// Return the number of leading space characters in `s`.
///
/// Used to derive the indentation level for tree-building comparisons.
fn leading_spaces(s: &str) -> usize {
    s.len() - s.trim_start_matches(' ').len()
}

/// Parse `cost=A..B` from a node header, returning `(A, B)`.
fn parse_cost(s: &str) -> Option<(f64, f64)> {
    // Matches "cost=0.00..1.05" anywhere in the string.
    let after = s.split("cost=").nth(1)?;
    let range = after.split_whitespace().next()?;
    let (a, b) = range.split_once("..")?;
    let startup = a.parse().ok()?;
    let total = b.parse().ok()?;
    Some((startup, total))
}

/// Parse `rows=N` returning N as `f64`.
fn parse_rows_label(s: &str, label: &str) -> Option<f64> {
    // label is e.g. "rows=" — find the value after it.
    let after = s.split(label).nth(1)?;
    let token = after.split([' ', ')', ',']).next()?;
    token.parse().ok()
}

/// Parse `time=A..B` from the actual stats parenthesis, returning `(A, B)`.
fn parse_actual_time(s: &str) -> Option<(f64, f64)> {
    let after = s.split("time=").nth(1)?;
    let range = after.split([' ', ')', ',']).next()?;
    let (a, b) = range.split_once("..")?;
    let startup = a.parse().ok()?;
    let total = b.parse().ok()?;
    Some((startup, total))
}

/// Parse `loops=N` from the actual stats parenthesis.
fn parse_loops(s: &str) -> Option<u64> {
    let after = s.split("loops=").nth(1)?;
    let token = after.split([' ', ')', ',']).next()?;
    token.parse().ok()
}

/// Parse a buffer counter `hit=N` / `read=N` etc. from a Buffers line.
fn parse_buf_counter(s: &str, label: &str) -> u64 {
    s.split(label)
        .nth(1)
        .and_then(|after| after.split_whitespace().next())
        .and_then(|tok| tok.parse().ok())
        .unwrap_or(0)
}

/// Parse `Planning Time: N ms` or `Execution Time: N ms`.
fn parse_time_line(s: &str) -> Option<f64> {
    // Both lines end with " ms".
    let colon_pos = s.find(':')?;
    let after = s[colon_pos + 1..].trim();
    let num = after.trim_end_matches(" ms").trim();
    num.parse().ok()
}

/// Parse a node header line.
///
/// A node header is either the root line (no leading `->`) or a child line
/// starting with `->`.  Both contain `(cost=...)` and optionally
/// `(actual time=... rows=... loops=...)`.
///
/// Returns the partially-populated [`ExplainNode`] and the raw leading-space
/// count (used for tree-building depth comparisons).
fn parse_node_header(line: &str) -> Option<(ExplainNode, usize)> {
    // Determine depth from leading spaces.
    let spaces = leading_spaces(line);
    let trimmed = line.trim_start();

    // Determine the remainder after stripping the `->` marker.
    let node_text = trimmed.strip_prefix("-> ").unwrap_or(trimmed);

    // Must contain `(cost=` to be a node header.
    if !node_text.contains("(cost=") {
        return None;
    }

    // Split on the first `(cost=` to get the node type + relation part.
    let cost_pos = node_text.find("(cost=")?;
    let type_part = node_text[..cost_pos].trim();
    let rest_after_type = &node_text[cost_pos..];

    // Parse the `(cost=... rows=... width=...)` parenthesised group.
    let est_paren_end = rest_after_type.find(')')?;
    let est_paren = &rest_after_type[..=est_paren_end];

    let estimated_cost = parse_cost(est_paren);
    let estimated_rows = parse_rows_label(est_paren, "rows=");

    // Parse optional `(actual time=... rows=... loops=...)` group.
    let after_est = &rest_after_type[est_paren_end + 1..];
    let (actual_time_ms, actual_rows, loops) = if let Some(act_start) = after_est.find("(actual ") {
        let act_rest = &after_est[act_start..];
        let act_end = act_rest
            .find(')')
            .unwrap_or(act_rest.len().saturating_sub(1));
        let act_paren = &act_rest[..=act_end];
        let time = parse_actual_time(act_paren);
        let rows = parse_rows_label(act_paren, "rows=");
        let lp = parse_loops(act_paren).unwrap_or(1);
        (time, rows, lp)
    } else if after_est.contains("(never executed)") {
        // Node was never executed (e.g. unreachable branch in a plan).
        (None, None, 0)
    } else {
        (None, None, 1)
    };

    // Decode node type and optional relation / alias from `type_part`.
    // Examples:
    //   "Seq Scan on users"
    //   "Index Scan using users_pkey on users"
    //   "Hash Join"
    //   "Hash"
    //   "Gather"  (parallel)
    //   "CTE Scan on cte_name"
    //   "Sort"
    //   "Seq Scan on users u"  — with alias
    let (node_type, relation, alias) = decode_node_type_part(type_part);

    // Return raw space count; tree-level depth is assigned by build_tree.
    let node = ExplainNode {
        node_type,
        relation,
        alias,
        estimated_rows,
        actual_rows,
        estimated_cost,
        actual_time_ms,
        loops,
        raw_lines: vec![line.to_owned()],
        depth: 0, // filled in by build_tree
        ..ExplainNode::default()
    };

    Some((node, spaces))
}

/// Decode the free-text part before `(cost=...)` into
/// `(node_type, relation, alias)`.
///
/// Handles patterns such as:
/// - `"Seq Scan on users"` → `("Seq Scan", Some("users"), None)`
/// - `"Seq Scan on users u"` → `("Seq Scan", Some("users"), Some("u"))`
/// - `"Index Scan using idx_users_email on users"` → `("Index Scan", Some("users"), None)`
/// - `"Hash Join"` → `("Hash Join", None, None)`
/// - `"CTE Scan on my_cte"` → `("CTE Scan", Some("my_cte"), None)`
fn decode_node_type_part(type_part: &str) -> (String, Option<String>, Option<String>) {
    // Priority 1: "Index Scan using idx_name on table [alias]"
    // We must check " using " before " on " because the idx name also
    // precedes " on ", which would otherwise be split incorrectly.
    if let Some(using_pos) = type_part.find(" using ") {
        let node_type = type_part[..using_pos].trim().to_owned();
        let after_using = &type_part[using_pos + 7..];
        // after_using = "idx_name on table [alias]"
        if let Some(on_pos) = find_on_keyword(after_using) {
            let after_on = after_using[on_pos + 4..].trim();
            // after_on = "table" or "table alias"
            let mut parts = after_on.splitn(3, ' ');
            let relation = parts.next().map(str::to_owned);
            let alias = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);
            return (node_type, relation, alias);
        }
        // "Index Scan using idx_name" with no table — unusual but handle it.
        return (node_type, None, None);
    }

    // Priority 2: "Seq Scan on table [alias]", "CTE Scan on cte_name", etc.
    if let Some(on_pos) = find_on_keyword(type_part) {
        let node_type = type_part[..on_pos].trim().to_owned();
        let after_on = type_part[on_pos + 4..].trim(); // skip " on "

        // after_on may be "users" or "users u" (with alias)
        let mut parts = after_on.splitn(3, ' ');
        let relation = parts.next().map(str::to_owned);
        let alias = parts.next().filter(|s| !s.is_empty()).map(str::to_owned);

        return (node_type, relation, alias);
    }

    // No "on" or "using" → pure node type, no relation.
    (type_part.trim().to_owned(), None, None)
}

/// Find the position of ` on ` as a word boundary (not in `Condition`, etc.).
fn find_on_keyword(s: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(pos) = s[start..].find(" on ") {
        let abs = start + pos;
        // Make sure the character before " on " is alphanumeric/closing paren.
        let prev_ok = abs == 0
            || s[..abs]
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == ')');
        if prev_ok {
            return Some(abs);
        }
        start = abs + 4;
    }
    None
}

/// Parse a `Buffers: ...` detail line and update the node's buffer counters.
fn apply_buffers_line(node: &mut ExplainNode, line: &str) {
    let s = line;
    if s.contains("hit=") {
        node.shared_hit = parse_buf_counter(s, "hit=");
    }
    if s.contains("read=") {
        node.shared_read = parse_buf_counter(s, "read=");
    }
    if s.contains("dirtied=") {
        node.shared_dirtied = parse_buf_counter(s, "dirtied=");
    }
    if s.contains("written=") {
        node.shared_written = parse_buf_counter(s, "written=");
    }
}

/// Parse a `Sort Method: quicksort  Memory: 25kB` line.
fn apply_sort_line(node: &mut ExplainNode, line: &str) {
    // "Sort Method: quicksort  Memory: 25kB"
    // "Sort Method: external merge  Disk: 38412kB"
    let lower = line.to_lowercase();
    if let Some(method_pos) = lower.find("sort method:") {
        let after = line[method_pos + 12..].trim();
        // Split on "  " to separate method from space annotation.
        if let Some(double_space) = after.find("  ") {
            node.sort_method = Some(after[..double_space].trim().to_owned());
            node.sort_space = Some(after[double_space..].trim().to_owned());
        } else {
            node.sort_method = Some(after.to_owned());
        }
    }
}

/// Parse a `Hash Batches: N  Original Buckets: M` style line.
fn apply_hash_line(node: &mut ExplainNode, line: &str) {
    // "Batches: N  Memory Usage: MkB"
    // "Hash Batches: N  Original Batches: M"
    let lower = line.to_lowercase();
    if lower.contains("batches:") {
        if let Some(v) = extract_label_u64(line, "Batches:") {
            node.hash_batches = Some(v);
        }
    }
    if lower.contains("buckets:") {
        if let Some(v) = extract_label_u64(line, "Buckets:") {
            node.hash_buckets = Some(v);
        }
    }
}

/// Extract an integer value after `label ` (case-sensitive label).
fn extract_label_u64(s: &str, label: &str) -> Option<u64> {
    let pos = s.find(label)?;
    let after = s[pos + label.len()..].trim_start();
    let tok = after.split(|c: char| !c.is_ascii_digit()).next()?;
    tok.parse().ok()
}

/// Parse a `Filter: (...)` line.
fn apply_filter_line(node: &mut ExplainNode, line: &str) {
    if let Some(pos) = line.find("Filter:") {
        node.filter = Some(line[pos + 7..].trim().to_owned());
    }
}

/// Parse a `Rows Removed by Filter: N` line.
fn apply_rows_removed_line(node: &mut ExplainNode, line: &str) {
    if let Some(pos) = line.to_lowercase().find("rows removed by filter:") {
        let after = line[pos + 23..].trim();
        if let Ok(n) = after.parse::<u64>() {
            node.rows_removed_by_filter = Some(n);
        }
    }
}

/// Parse an `Index Cond: (...)` line.
fn apply_index_cond_line(node: &mut ExplainNode, line: &str) {
    if let Some(pos) = line.find("Index Cond:") {
        node.index_cond = Some(line[pos + 11..].trim().to_owned());
    }
}

/// Parse `Workers Planned: N` and `Workers Launched: N`.
fn apply_workers_line(node: &mut ExplainNode, line: &str) {
    let lower = line.to_lowercase();
    if lower.contains("workers planned:") {
        if let Some(v) = extract_label_u64(line, "Planned:") {
            node.workers_planned = Some(v);
        }
    }
    if lower.contains("workers launched:") {
        if let Some(v) = extract_label_u64(line, "Launched:") {
            node.workers_launched = Some(v);
        }
    }
}

// ---------------------------------------------------------------------------
// Tree builder
// ---------------------------------------------------------------------------

/// A raw line categorised during the first pass.
#[derive(Debug)]
enum LineKind {
    /// A node header line (contains `(cost=...)`), with its raw indentation
    /// (number of leading spaces before the `->` marker or node type).
    NodeHeader(usize),
    /// A detail / annotation line belonging to the current node.
    Detail,
    /// `Planning Time: N ms`
    PlanningTime(f64),
    /// `Execution Time: N ms`
    ExecutionTime(f64),
    /// `Trigger ...` statistics line.
    Trigger,
    /// Blank line or separator — ignored.
    Blank,
}

/// Classify a single line.
fn classify_line(line: &str) -> LineKind {
    let trimmed = line.trim();

    if trimmed.is_empty() || trimmed.starts_with("---") || trimmed.starts_with("===") {
        return LineKind::Blank;
    }

    let lower = trimmed.to_lowercase();

    if lower.starts_with("planning time:") {
        if let Some(v) = parse_time_line(trimmed) {
            return LineKind::PlanningTime(v);
        }
    }
    if lower.starts_with("execution time:") {
        if let Some(v) = parse_time_line(trimmed) {
            return LineKind::ExecutionTime(v);
        }
    }
    if lower.starts_with("trigger ") {
        return LineKind::Trigger;
    }

    // Node header detection: trimmed must contain "(cost=" and the raw line
    // must either start with spaces+"->" or be a top-level node line
    // (no leading "->", but contains "(cost=").
    if trimmed.contains("(cost=") {
        // Store raw indent (leading spaces) — used only as a depth key.
        let raw_indent = leading_spaces(line);
        return LineKind::NodeHeader(raw_indent);
    }

    LineKind::Detail
}

/// Flat list entry before the tree is constructed.
struct FlatNode {
    /// Raw leading-space count from the original text line.
    ///
    /// Used to determine parent-child relationships during tree building
    /// (nodes with a larger `raw_indent` are deeper in the tree).
    raw_indent: usize,
    node: ExplainNode,
}

/// Assign tree-level depths recursively (`0` = root, `1` = child, …).
fn assign_depths(nodes: &mut [ExplainNode], depth: usize) {
    for node in nodes.iter_mut() {
        node.depth = depth;
        assign_depths(&mut node.children, depth + 1);
    }
}

/// Build a tree from a flat list of nodes ordered as they appear in the
/// EXPLAIN output.
///
/// Each [`FlatNode`] carries a `raw_indent` (number of leading spaces before
/// the `->` marker or node type text) that increases monotonically with
/// nesting depth.  The algorithm works as follows:
///
/// 1. Maintain a stack of "open" nodes.  A node is open while we may still
///    receive children for it.
/// 2. For each incoming node N with `raw_indent` I:
///    - Pop every stack entry whose `raw_indent` >= I.  Those popped nodes
///      are "complete" — they become children of the new stack-top (or roots).
///    - Push N onto the stack.
/// 3. After processing all nodes, drain the remaining stack, popping each
///    completed node onto its parent (or roots).
fn build_tree(mut flat: Vec<FlatNode>) -> Vec<ExplainNode> {
    if flat.is_empty() {
        return Vec::new();
    }

    // Stack of open FlatNodes waiting to collect children.
    let mut stack: Vec<FlatNode> = Vec::new();
    // Completed root nodes in document order.
    let mut roots: Vec<ExplainNode> = Vec::new();

    for flat_node in flat.drain(..) {
        let indent = flat_node.raw_indent;

        // Close open nodes at the same or deeper indentation level.
        while stack.last().is_some_and(|top| top.raw_indent >= indent) {
            let closed = stack.pop().unwrap().node;
            if let Some(parent) = stack.last_mut() {
                parent.node.children.push(closed);
            } else {
                roots.push(closed);
            }
        }

        stack.push(flat_node);
    }

    // Flush remaining open nodes from deepest to shallowest.
    while let Some(item) = stack.pop() {
        let closed = item.node;
        if let Some(parent) = stack.last_mut() {
            parent.node.children.push(closed);
        } else {
            roots.push(closed);
        }
    }

    // The final flush adds root nodes in reverse document order because we
    // pop shallowest-last and push to `roots`.  Reverse to restore order.
    // (Children were appended in correct order during processing.)
    roots.reverse();

    assign_depths(&mut roots, 0);

    roots
}

// ---------------------------------------------------------------------------
// Exclusive-time calculation
// ---------------------------------------------------------------------------

/// Walk the plan tree and compute `exclusive_time_ms` and `time_percent`
/// for every node.
///
/// `total_ms` is the top-level execution time (from `Execution Time: N ms`).
/// If not available, we use the root node's `actual_time_ms.1 * loops`.
pub fn compute_exclusive_times(nodes: &mut [ExplainNode], total_ms: f64) {
    for node in nodes.iter_mut() {
        compute_exclusive_times_rec(node, total_ms);
    }
}

fn compute_exclusive_times_rec(node: &mut ExplainNode, total_ms: f64) {
    // Recurse into children first.
    for child in &mut node.children {
        compute_exclusive_times_rec(child, total_ms);
    }

    // Compute inclusive time for this node.
    // Allow precision loss: loops is typically a small integer.
    #[allow(clippy::cast_precision_loss)]
    let inclusive = node
        .actual_time_ms
        .map_or(0.0, |(_, total)| total * node.loops as f64);

    // Sum up children's inclusive times.
    #[allow(clippy::cast_precision_loss)]
    let children_total: f64 = node
        .children
        .iter()
        .map(|c| c.actual_time_ms.map_or(0.0, |(_, t)| t * c.loops as f64))
        .sum();

    node.exclusive_time_ms = (inclusive - children_total).max(0.0);

    if total_ms > 0.0 {
        node.time_percent = node.exclusive_time_ms / total_ms * 100.0;
    }
}

// ---------------------------------------------------------------------------
// Public parse entry point
// ---------------------------------------------------------------------------

/// Parse a `PostgreSQL` text-format `EXPLAIN [ANALYZE]` output string into an
/// [`ExplainPlan`].
///
/// Returns a [`ParseError`] if no recognisable node headers are found.
///
/// # Examples
///
/// ```
/// let out = "Seq Scan on orders  (cost=0.00..5.06 rows=6 width=4)\n\
///            Planning Time: 0.05 ms\n\
///            Execution Time: 0.10 ms\n";
/// let plan = rpg::explain::parse(out).unwrap();
/// assert_eq!(plan.nodes[0].node_type, "Seq Scan");
/// ```
pub fn parse(input: &str) -> Result<ExplainPlan, ParseError> {
    let mut flat: Vec<FlatNode> = Vec::new();
    let mut planning_time_ms: Option<f64> = None;
    let mut execution_time_ms: Option<f64> = None;
    let mut triggers: Vec<TriggerInfo> = Vec::new();

    // We keep track of which FlatNode index the current detail lines belong to.
    let mut current_idx: Option<usize> = None;

    for raw_line in input.lines() {
        match classify_line(raw_line) {
            LineKind::Blank => {}

            LineKind::PlanningTime(v) => {
                planning_time_ms = Some(v);
            }

            LineKind::ExecutionTime(v) => {
                execution_time_ms = Some(v);
            }

            LineKind::Trigger => {
                // "Trigger trigger_name: time=N.NNN calls=N"
                let parsed = parse_trigger_line(raw_line);
                triggers.extend(parsed);
            }

            LineKind::NodeHeader(_indent) => {
                if let Some((node, raw_indent)) = parse_node_header(raw_line) {
                    flat.push(FlatNode { raw_indent, node });
                    current_idx = Some(flat.len() - 1);
                }
            }

            LineKind::Detail => {
                // Associate detail line with the most recent node.
                if let Some(idx) = current_idx {
                    let node = &mut flat[idx].node;
                    node.raw_lines.push(raw_line.to_owned());
                    let trimmed = raw_line.trim();

                    if trimmed.starts_with("Buffers:") {
                        apply_buffers_line(node, trimmed);
                    } else if trimmed.to_lowercase().starts_with("sort method:") {
                        apply_sort_line(node, trimmed);
                    } else if trimmed.to_lowercase().starts_with("batches:")
                        || trimmed.to_lowercase().starts_with("hash batches:")
                        || trimmed.to_lowercase().starts_with("buckets:")
                        || trimmed.to_lowercase().starts_with("hash buckets:")
                    {
                        apply_hash_line(node, trimmed);
                    } else if trimmed.starts_with("Filter:") {
                        apply_filter_line(node, trimmed);
                    } else if trimmed
                        .to_lowercase()
                        .starts_with("rows removed by filter:")
                    {
                        apply_rows_removed_line(node, trimmed);
                    } else if trimmed.starts_with("Index Cond:") {
                        apply_index_cond_line(node, trimmed);
                    } else if trimmed.to_lowercase().starts_with("workers") {
                        apply_workers_line(node, trimmed);
                    }
                }
            }
        }
    }

    if flat.is_empty() {
        return Err(ParseError::new(
            "no plan nodes found — is the input valid EXPLAIN output?",
        ));
    }

    let mut roots = build_tree(flat);

    // Determine total execution time for time_percent calculation.
    #[allow(clippy::cast_precision_loss)]
    let total_ms = execution_time_ms.unwrap_or_else(|| {
        roots
            .first()
            .and_then(|n| n.actual_time_ms.map(|(_, t)| t * n.loops as f64))
            .unwrap_or(0.0)
    });

    compute_exclusive_times(&mut roots, total_ms);

    Ok(ExplainPlan {
        nodes: roots,
        planning_time_ms,
        execution_time_ms,
        triggers,
    })
}

/// Parse a trigger statistics line such as:
/// `  Trigger trigger_name: time=3.402 calls=1`
fn parse_trigger_line(line: &str) -> Option<TriggerInfo> {
    // Format: "Trigger <name>: time=N calls=N"
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("Trigger ")?;
    let colon_pos = rest.find(':')?;
    let name = rest[..colon_pos].trim().to_owned();
    let stats = &rest[colon_pos + 1..];

    let time_ms = stats
        .split("time=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let calls = stats
        .split("calls=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    Some(TriggerInfo {
        name,
        time_ms,
        calls,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // 1. Simple sequential scan
    // -----------------------------------------------------------------------

    #[test]
    fn test_seq_scan() {
        let input = "\
Seq Scan on users  (cost=0.00..1.05 rows=5 width=4) (actual time=0.010..0.012 rows=5 loops=1)
  Planning Time: 0.080 ms
  Execution Time: 0.030 ms
";
        let plan = parse(input).expect("parse should succeed");
        assert_eq!(plan.nodes.len(), 1, "should have one root node");
        let node = &plan.nodes[0];
        assert_eq!(node.node_type, "Seq Scan");
        assert_eq!(node.relation.as_deref(), Some("users"));
        assert_eq!(node.estimated_rows, Some(5.0));
        assert_eq!(node.actual_rows, Some(5.0));
        assert_eq!(node.loops, 1);
        assert_eq!(node.estimated_cost, Some((0.00, 1.05)));
        assert_eq!(node.actual_time_ms, Some((0.010, 0.012)));
        assert_eq!(plan.planning_time_ms, Some(0.080));
        assert_eq!(plan.execution_time_ms, Some(0.030));
        assert!(node.children.is_empty());
    }

    // -----------------------------------------------------------------------
    // 2. Index scan with filter
    // -----------------------------------------------------------------------

    #[test]
    fn test_index_scan_with_filter() {
        let input = "\
Index Scan using orders_pkey on orders  (cost=0.28..8.30 rows=1 width=100) (actual time=0.020..0.022 rows=1 loops=1)
  Index Cond: (id = 42)
  Filter: (status = 'active')
  Rows Removed by Filter: 0
  Buffers: shared hit=3
  Planning Time: 0.150 ms
  Execution Time: 0.040 ms
";
        let plan = parse(input).expect("parse should succeed");
        assert_eq!(plan.nodes.len(), 1);
        let node = &plan.nodes[0];
        assert_eq!(node.node_type, "Index Scan");
        assert_eq!(node.relation.as_deref(), Some("orders"));
        assert_eq!(node.index_cond.as_deref(), Some("(id = 42)"));
        assert_eq!(node.filter.as_deref(), Some("(status = 'active')"));
        assert_eq!(node.rows_removed_by_filter, Some(0));
        assert_eq!(node.shared_hit, 3);
        assert_eq!(node.shared_read, 0);
    }

    // -----------------------------------------------------------------------
    // 3. Hash join between two tables
    // -----------------------------------------------------------------------

    #[test]
    fn test_hash_join() {
        let input = "\
Hash Join  (cost=1.09..2.22 rows=5 width=8) (actual time=0.050..0.060 rows=5 loops=1)
  Hash Cond: (o.user_id = u.id)
  ->  Seq Scan on orders o  (cost=0.00..1.06 rows=6 width=8) (actual time=0.010..0.015 rows=6 loops=1)
  ->  Hash  (cost=1.05..1.05 rows=5 width=4) (actual time=0.020..0.020 rows=5 loops=1)
        ->  Seq Scan on users u  (cost=0.00..1.05 rows=5 width=4) (actual time=0.008..0.010 rows=5 loops=1)
  Planning Time: 0.200 ms
  Execution Time: 0.100 ms
";
        let plan = parse(input).expect("parse should succeed");
        assert_eq!(plan.nodes.len(), 1, "one root");
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "Hash Join");
        assert_eq!(root.children.len(), 2, "Hash Join has two children");

        // First child: Seq Scan on orders
        let child0 = &root.children[0];
        assert_eq!(child0.node_type, "Seq Scan");
        assert_eq!(child0.relation.as_deref(), Some("orders"));

        // Second child: Hash
        let child1 = &root.children[1];
        assert_eq!(child1.node_type, "Hash");
        assert_eq!(child1.children.len(), 1, "Hash has one child");
        assert_eq!(child1.children[0].node_type, "Seq Scan");
        assert_eq!(child1.children[0].relation.as_deref(), Some("users"));
    }

    // -----------------------------------------------------------------------
    // 4. Nested loop with index scan
    // -----------------------------------------------------------------------

    #[test]
    fn test_nested_loop_index_scan() {
        let input = "\
Nested Loop  (cost=0.28..16.32 rows=6 width=8) (actual time=0.030..0.080 rows=6 loops=1)
  ->  Seq Scan on orders o  (cost=0.00..1.06 rows=6 width=4) (actual time=0.008..0.012 rows=6 loops=1)
  ->  Index Scan using users_pkey on users u  (cost=0.28..2.49 rows=1 width=4) (actual time=0.010..0.010 rows=1 loops=6)
        Index Cond: (u.id = o.user_id)
  Planning Time: 0.250 ms
  Execution Time: 0.100 ms
";
        let plan = parse(input).expect("parse should succeed");
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "Nested Loop");
        assert_eq!(root.children.len(), 2);
        let idx_scan = &root.children[1];
        assert_eq!(idx_scan.node_type, "Index Scan");
        assert_eq!(idx_scan.relation.as_deref(), Some("users"));
        assert_eq!(idx_scan.alias.as_deref(), Some("u"));
        assert_eq!(idx_scan.loops, 6);
        assert_eq!(idx_scan.index_cond.as_deref(), Some("(u.id = o.user_id)"));
    }

    // -----------------------------------------------------------------------
    // 5. Sort with disk spill
    // -----------------------------------------------------------------------

    #[test]
    fn test_sort_disk_spill() {
        let input = "\
Sort  (cost=10000.42..10002.42 rows=800 width=8) (actual time=45.000..48.000 rows=800 loops=1)
  Sort Key: created_at
  Sort Method: external merge  Disk: 38412kB
  ->  Seq Scan on big_table  (cost=0.00..5000.00 rows=800 width=8) (actual time=0.010..20.000 rows=800 loops=1)
  Planning Time: 0.500 ms
  Execution Time: 50.000 ms
";
        let plan = parse(input).expect("parse should succeed");
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "Sort");
        assert_eq!(root.sort_method.as_deref(), Some("external merge"));
        assert!(
            root.sort_space.as_deref().unwrap_or("").contains("38412kB"),
            "sort_space should contain disk usage"
        );
        assert_eq!(root.children.len(), 1);
    }

    // -----------------------------------------------------------------------
    // 6. Parallel query (Gather + workers)
    // -----------------------------------------------------------------------

    #[test]
    fn test_parallel_query() {
        let input = "\
Gather  (cost=1000.00..8000.00 rows=10 width=4) (actual time=0.500..10.000 rows=10 loops=1)
  Workers Planned: 2
  Workers Launched: 2
  ->  Parallel Seq Scan on big_table  (cost=0.00..7000.00 rows=4 width=4) (actual time=0.020..8.000 rows=4 loops=3)
  Planning Time: 0.300 ms
  Execution Time: 10.050 ms
";
        let plan = parse(input).expect("parse should succeed");
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "Gather");
        assert_eq!(root.workers_planned, Some(2));
        assert_eq!(root.workers_launched, Some(2));
        assert_eq!(root.children.len(), 1);
        assert_eq!(root.children[0].node_type, "Parallel Seq Scan");
    }

    // -----------------------------------------------------------------------
    // 7. CTE scan
    // -----------------------------------------------------------------------

    #[test]
    fn test_cte_scan() {
        let input = "\
CTE Scan on cte_orders  (cost=18.75..18.85 rows=5 width=4) (actual time=0.100..0.110 rows=5 loops=1)
  CTE cte_orders
    ->  Seq Scan on orders  (cost=0.00..18.75 rows=875 width=100) (actual time=0.010..0.080 rows=875 loops=1)
  Planning Time: 0.180 ms
  Execution Time: 0.200 ms
";
        let plan = parse(input).expect("parse should succeed");
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "CTE Scan");
        assert_eq!(root.relation.as_deref(), Some("cte_orders"));
    }

    // -----------------------------------------------------------------------
    // 8. Plan with buffers
    // -----------------------------------------------------------------------

    #[test]
    fn test_buffers() {
        let input = "\
Seq Scan on large_table  (cost=0.00..55000.00 rows=3000000 width=100) (actual time=0.020..2000.000 rows=3000000 loops=1)
  Buffers: shared hit=100 read=49900 dirtied=5 written=2
  Planning Time: 1.000 ms
  Execution Time: 2500.000 ms
";
        let plan = parse(input).expect("parse should succeed");
        let node = &plan.nodes[0];
        assert_eq!(node.shared_hit, 100);
        assert_eq!(node.shared_read, 49900);
        assert_eq!(node.shared_dirtied, 5);
        assert_eq!(node.shared_written, 2);
    }

    // -----------------------------------------------------------------------
    // 9. EXPLAIN without ANALYZE (no actual times)
    // -----------------------------------------------------------------------

    #[test]
    fn test_explain_no_analyze() {
        let input = "\
Hash Join  (cost=1.09..2.22 rows=5 width=8)
  Hash Cond: (o.user_id = u.id)
  ->  Seq Scan on orders o  (cost=0.00..1.06 rows=6 width=8)
  ->  Hash  (cost=1.05..1.05 rows=5 width=4)
        ->  Seq Scan on users u  (cost=0.00..1.05 rows=5 width=4)
";
        let plan = parse(input).expect("parse should succeed");
        assert!(plan.planning_time_ms.is_none());
        assert!(plan.execution_time_ms.is_none());
        let root = &plan.nodes[0];
        assert_eq!(root.node_type, "Hash Join");
        assert!(root.actual_time_ms.is_none());
        assert_eq!(root.children.len(), 2);
        // No actual times → exclusive_time_ms should be 0.
        assert!(
            root.exclusive_time_ms.abs() < f64::EPSILON,
            "expected exclusive_time_ms=0 but got {}",
            root.exclusive_time_ms
        );
    }

    // -----------------------------------------------------------------------
    // Exclusive time calculation
    // -----------------------------------------------------------------------

    #[test]
    fn test_exclusive_time_calculation() {
        // Root: 100ms total, 1 loop
        // Child A: 30ms total, 1 loop
        // Child B: 50ms total, 1 loop
        // Root exclusive = 100 - 30 - 50 = 20ms
        let input = "\
Hash Join  (cost=1.00..2.00 rows=10 width=8) (actual time=0.010..100.000 rows=10 loops=1)
  ->  Seq Scan on t1  (cost=0.00..1.00 rows=5 width=4) (actual time=0.005..30.000 rows=5 loops=1)
  ->  Hash  (cost=1.00..1.00 rows=5 width=4) (actual time=0.002..50.000 rows=5 loops=1)
        ->  Seq Scan on t2  (cost=0.00..1.00 rows=5 width=4) (actual time=0.002..48.000 rows=5 loops=1)
  Execution Time: 100.000 ms
";
        let plan = parse(input).expect("parse should succeed");
        let root = &plan.nodes[0];
        // Root exclusive = 100 - 30 - 50 = 20
        assert!(
            (root.exclusive_time_ms - 20.0).abs() < 0.01,
            "root exclusive={:.3}",
            root.exclusive_time_ms
        );
        assert!(
            (root.time_percent - 20.0).abs() < 0.1,
            "root time_percent={:.3}",
            root.time_percent
        );

        // Child A (Seq Scan t1) has no children → exclusive = 30.
        let child_a = &root.children[0];
        assert!(
            (child_a.exclusive_time_ms - 30.0).abs() < 0.01,
            "child_a exclusive={:.3}",
            child_a.exclusive_time_ms
        );

        // Hash node: inclusive=50, child=48 → exclusive=2.
        let hash_node = &root.children[1];
        assert!(
            (hash_node.exclusive_time_ms - 2.0).abs() < 0.01,
            "hash exclusive={:.3}",
            hash_node.exclusive_time_ms
        );
    }

    // -----------------------------------------------------------------------
    // Node type / relation / alias decoding
    // -----------------------------------------------------------------------

    #[test]
    fn test_decode_node_type_part() {
        let (nt, rel, alias) = decode_node_type_part("Seq Scan on users");
        assert_eq!(nt, "Seq Scan");
        assert_eq!(rel.as_deref(), Some("users"));
        assert!(alias.is_none());

        let (nt, rel, alias) = decode_node_type_part("Seq Scan on users u");
        assert_eq!(nt, "Seq Scan");
        assert_eq!(rel.as_deref(), Some("users"));
        assert_eq!(alias.as_deref(), Some("u"));

        let (nt, rel, alias) = decode_node_type_part("Index Scan using idx_email on users");
        assert_eq!(nt, "Index Scan");
        assert_eq!(rel.as_deref(), Some("users"));
        assert!(alias.is_none());

        let (nt, rel, alias) = decode_node_type_part("Hash Join");
        assert_eq!(nt, "Hash Join");
        assert!(rel.is_none());
        assert!(alias.is_none());
    }

    // -----------------------------------------------------------------------
    // parse_cost
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_cost() {
        assert_eq!(
            parse_cost("(cost=0.00..1.05 rows=5 width=4)"),
            Some((0.00, 1.05))
        );
        assert_eq!(
            parse_cost("(cost=1000.00..8000.00 rows=10 width=4)"),
            Some((1000.00, 8000.00))
        );
        assert_eq!(parse_cost("no cost here"), None);
    }

    // -----------------------------------------------------------------------
    // parse_time_line
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_time_line() {
        assert_eq!(parse_time_line("Planning Time: 0.080 ms"), Some(0.080));
        assert_eq!(parse_time_line("Execution Time: 2500.000 ms"), Some(2500.0));
        assert_eq!(parse_time_line("Something else"), None);
    }

    // -----------------------------------------------------------------------
    // Empty / garbage input
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_input_returns_error() {
        assert!(parse("").is_err());
        assert!(parse("\n\n\n").is_err());
        assert!(parse("just some text\nno plan nodes here").is_err());
    }
}
