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

//! EXPLAIN plan issue detection heuristics.
//!
//! This module identifies common performance problems in `PostgreSQL`
//! EXPLAIN (ANALYZE) output.  Each heuristic operates on an [`ExplainNode`]
//! and returns an [`Option<PlanIssue>`].  The main entry point
//! [`detect_issues`] walks the plan tree, applies all heuristics, and
//! returns a deduplicated, sorted list of findings.
//!
//! # Note on parallel development
//!
//! The EXPLAIN parser is being built concurrently by a separate agent.
//! Until the two modules are merged, this file contains its own minimal
//! [`ExplainNode`] / [`ExplainPlan`] definitions.  When the parser lands,
//! replace these with imports from `super::parser` and delete the local
//! copies.

// Public API not yet wired to the rest of the application; suppress
// dead_code lints until the caller module is built.
#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Plan node types (temporary — will be replaced by parser module types)
// ---------------------------------------------------------------------------

/// A single node in a `PostgreSQL` EXPLAIN plan tree.
///
/// Fields use `Option` to reflect that EXPLAIN output is inconsistent:
/// not all fields appear for every node type or `PostgreSQL` version.
#[derive(Debug, Clone, Default)]
pub struct ExplainNode {
    /// Zero-based index assigned during tree traversal (breadth-first).
    pub index: usize,
    /// Node type string as reported by `PostgreSQL`, e.g. `"Seq Scan"`,
    /// `"Hash Join"`, `"Nested Loop"`.
    pub node_type: String,
    /// Relation name for scan nodes (`Seq Scan`, `Index Scan`, etc.).
    pub relation_name: Option<String>,
    /// Planner's estimate of rows returned.
    pub estimated_rows: f64,
    /// Actual rows returned (requires ANALYZE).
    pub actual_rows: f64,
    /// Actual total time in milliseconds (requires ANALYZE).
    pub actual_total_ms: f64,
    /// This node's share of total plan execution time (0.0–100.0).
    ///
    /// Set by [`ExplainPlan::compute_time_percents`] after tree construction.
    pub time_percent: f64,
    /// Number of times this node was executed (loop count).
    pub loops: f64,
    /// Rows removed by a filter predicate (requires ANALYZE).
    pub rows_removed_by_filter: f64,
    /// Sort space type: `"Memory"` or `"Disk"` (Sort nodes only).
    pub sort_space_type: Option<String>,
    /// Number of hash batches (Hash nodes only; > 1 means disk spill).
    pub hash_batches: Option<u64>,
    /// Number of parallel workers planned (Gather / Gather Merge nodes).
    pub workers_planned: Option<u64>,
    /// Number of parallel workers actually launched (requires ANALYZE).
    pub workers_launched: Option<u64>,
    /// Direct child nodes.
    pub children: Vec<ExplainNode>,
}

/// The full EXPLAIN plan: a root node plus aggregate timing data.
#[derive(Debug, Clone, Default)]
pub struct ExplainPlan {
    /// Root of the plan tree.
    pub root: ExplainNode,
    /// Total execution time in milliseconds (from `Execution Time:` line).
    ///
    /// Used to compute each node's [`ExplainNode::time_percent`].
    pub total_execution_ms: f64,
}

impl ExplainPlan {
    /// Walk the tree and set `time_percent` on every node.
    ///
    /// Must be called after building the tree so that heuristics can rely on
    /// this field being populated.  Nodes with `actual_total_ms == 0.0` or
    /// when `total_execution_ms == 0.0` receive `time_percent = 0.0`.
    pub fn compute_time_percents(&mut self) {
        let total = self.total_execution_ms;
        Self::assign_time_percent(&mut self.root, total);
    }

    fn assign_time_percent(node: &mut ExplainNode, total_ms: f64) {
        if total_ms > 0.0 {
            node.time_percent = (node.actual_total_ms / total_ms) * 100.0;
        }
        for child in &mut node.children {
            Self::assign_time_percent(child, total_ms);
        }
    }

    /// Assign sequential indexes (depth-first pre-order) to every node.
    pub fn assign_indexes(&mut self) {
        let mut counter = 0usize;
        Self::assign_index_recursive(&mut self.root, &mut counter);
    }

    fn assign_index_recursive(node: &mut ExplainNode, counter: &mut usize) {
        node.index = *counter;
        *counter += 1;
        for child in &mut node.children {
            Self::assign_index_recursive(child, counter);
        }
    }
}

// ---------------------------------------------------------------------------
// Issue types
// ---------------------------------------------------------------------------

/// Severity of a detected plan issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IssueSeverity {
    /// Dominates execution time; the most urgent finding.
    Slow,
    /// Something is off and likely hurts performance.
    Warn,
    /// Notable, but may be intentional or acceptable.
    Info,
}

/// A single performance issue detected in an EXPLAIN plan.
#[derive(Debug, Clone)]
pub struct PlanIssue {
    /// How serious the issue is.
    pub severity: IssueSeverity,
    /// Index of the [`ExplainNode`] where the issue was found.
    pub node_index: usize,
    /// Short, human-readable title.
    pub title: String,
    /// Longer description with relevant metrics.
    pub detail: String,
    /// Optional actionable suggestion.
    pub suggestion: Option<String>,
}

// ---------------------------------------------------------------------------
// Individual heuristics
// ---------------------------------------------------------------------------

/// Heuristic 1 — Sequential scan on a large table.
///
/// Fires when:
/// - `node_type` contains `"Seq Scan"`
/// - `actual_rows > 10_000`
/// - `time_percent > 10.0`
pub fn check_seq_scan_large(node: &ExplainNode) -> Option<PlanIssue> {
    if !node.node_type.contains("Seq Scan") {
        return None;
    }
    if node.actual_rows <= 10_000.0 || node.time_percent <= 10.0 {
        return None;
    }

    let table = node.relation_name.as_deref().unwrap_or("<unknown>");
    Some(PlanIssue {
        severity: IssueSeverity::Slow,
        node_index: node.index,
        title: format!("Seq Scan on large table \"{table}\""),
        detail: format!(
            "Scanning {:.0} rows without an index filter, consuming {:.1}% of total time",
            node.actual_rows, node.time_percent,
        ),
        suggestion: Some(format!(
            "Consider CREATE INDEX ON {table} (...) for the most selective filter column"
        )),
    })
}

/// Heuristic 2 — Poor row estimate (planner estimate vs. actual rows).
///
/// Fires when the ratio `actual_rows / estimated_rows` is outside
/// `[0.1, 10.0]` (i.e. the planner is off by more than 10x in either
/// direction).  Nodes with `estimated_rows == 0` or `actual_rows == 0`
/// are skipped to avoid division by zero (the latter also covers plain
/// EXPLAIN without ANALYZE, where no actual rows are available).
pub fn check_row_estimate_error(node: &ExplainNode) -> Option<PlanIssue> {
    if node.estimated_rows <= 0.0 || node.actual_rows <= 0.0 {
        return None;
    }

    let ratio = node.actual_rows / node.estimated_rows;
    if (0.1..=10.0).contains(&ratio) {
        return None;
    }

    let (direction, factor) = if ratio > 10.0 {
        ("underestimated", ratio)
    } else {
        ("overestimated", 1.0 / ratio)
    };

    Some(PlanIssue {
        severity: IssueSeverity::Warn,
        node_index: node.index,
        title: format!("Row estimate {direction} by {factor:.0}x"),
        detail: {
            let est = node.estimated_rows;
            let actual = node.actual_rows;
            format!(
                "Planner estimated {est:.0} rows but {actual:.0} were returned \
                 ({direction} by {factor:.1}x); \
                 bad estimates lead to suboptimal join strategies and memory allocation"
            )
        },
        suggestion: Some(
            "Run ANALYZE on the relevant table(s) or increase statistics targets with \
             ALTER TABLE ... ALTER COLUMN ... SET STATISTICS"
                .to_owned(),
        ),
    })
}

/// Heuristic 3 — Sort spill to disk.
///
/// Fires when `sort_space_type == Some("Disk")`.
pub fn check_sort_spill(node: &ExplainNode) -> Option<PlanIssue> {
    match node.sort_space_type.as_deref() {
        Some("Disk") => {}
        _ => return None,
    }

    Some(PlanIssue {
        severity: IssueSeverity::Warn,
        node_index: node.index,
        title: "Sort spilled to disk".to_owned(),
        detail: "The sort operation exceeded `work_mem` and wrote temporary files to disk, \
                 significantly slowing down the query"
            .to_owned(),
        suggestion: Some(
            "Increase `work_mem` for this session: SET work_mem = '256MB'; \
             or reduce the result set before sorting"
                .to_owned(),
        ),
    })
}

/// Heuristic 4 — Hash join / Hash Aggregate spill to disk.
///
/// Fires when `hash_batches > 1` (more than one batch means disk spill).
pub fn check_hash_spill(node: &ExplainNode) -> Option<PlanIssue> {
    let batches = node.hash_batches?;
    if batches <= 1 {
        return None;
    }

    Some(PlanIssue {
        severity: IssueSeverity::Warn,
        node_index: node.index,
        title: format!("Hash spilled to disk ({batches} batches)"),
        detail: format!(
            "The hash operation used {batches} disk-based batches, \
             indicating the hash table exceeded `work_mem`"
        ),
        suggestion: Some(
            "Increase `work_mem` for this session: SET work_mem = '256MB'; \
             or consider adding an index to avoid the hash operation"
                .to_owned(),
        ),
    })
}

/// Heuristic 5 — High filter removal ratio.
///
/// Fires when more than 90% of scanned rows are discarded by a filter
/// predicate, i.e.:
/// `rows_removed_by_filter > 0.9 * (actual_rows + rows_removed_by_filter)`
pub fn check_high_filter_removal(node: &ExplainNode) -> Option<PlanIssue> {
    let removed = node.rows_removed_by_filter;
    if removed <= 0.0 {
        return None;
    }

    let total_scanned = node.actual_rows + removed;
    if total_scanned <= 0.0 {
        return None;
    }

    let removal_ratio = removed / total_scanned;
    if removal_ratio <= 0.9 {
        return None;
    }

    let pct = removal_ratio * 100.0;
    Some(PlanIssue {
        severity: IssueSeverity::Warn,
        node_index: node.index,
        title: format!("High filter removal ({pct:.0}% of rows discarded)"),
        detail: format!(
            "{removed:.0} of {total_scanned:.0} scanned rows ({pct:.1}%) were removed \
             by a filter predicate; most of the scan work is wasted"
        ),
        suggestion: Some(
            "Add an index that covers the filter column(s) to avoid scanning \
             and discarding large numbers of rows"
                .to_owned(),
        ),
    })
}

/// Heuristic 6 — Parallel workers not launched.
///
/// Fires when `workers_planned > workers_launched` (requires both fields
/// to be present and `workers_planned > 0`).
pub fn check_workers_not_launched(node: &ExplainNode) -> Option<PlanIssue> {
    let planned = node.workers_planned?;
    let launched = node.workers_launched?;

    if planned == 0 || launched >= planned {
        return None;
    }

    let missing = planned - launched;
    Some(PlanIssue {
        severity: IssueSeverity::Info,
        node_index: node.index,
        title: format!("Parallel workers not launched ({launched}/{planned} started)"),
        detail: format!(
            "PostgreSQL planned {planned} parallel workers but only {launched} were launched \
             ({missing} missing); the query ran with reduced parallelism"
        ),
        suggestion: Some(
            "Check `max_worker_processes`, `max_parallel_workers`, and \
             `max_parallel_workers_per_gather`; also verify that the table \
             is large enough to justify parallelism"
                .to_owned(),
        ),
    })
}

/// Heuristic 7 — Nested Loop with high loop count on a child Seq Scan.
///
/// Fires when:
/// - `node_type` contains `"Nested Loop"`
/// - At least one direct child has `node_type` containing `"Seq Scan"`
/// - That child's `loops` count exceeds 100
pub fn check_nested_loop_seq_scan(node: &ExplainNode) -> Option<PlanIssue> {
    if !node.node_type.contains("Nested Loop") {
        return None;
    }

    let bad_child = node
        .children
        .iter()
        .find(|child| child.node_type.contains("Seq Scan") && child.loops > 100.0)?;

    let table = bad_child.relation_name.as_deref().unwrap_or("<unknown>");
    Some(PlanIssue {
        severity: IssueSeverity::Slow,
        node_index: node.index,
        title: format!(
            "Nested Loop drives repeated Seq Scan on \"{table}\" ({:.0} loops)",
            bad_child.loops,
        ),
        detail: format!(
            "The Nested Loop executes a Seq Scan on \"{table}\" {:.0} times; \
             this multiplies the scan cost by the outer row count",
            bad_child.loops,
        ),
        suggestion: Some(format!(
            "Add an index on the join column(s) of \"{table}\" so the inner \
             side can use an Index Scan instead of a Seq Scan"
        )),
    })
}

// ---------------------------------------------------------------------------
// Tree walker helpers
// ---------------------------------------------------------------------------

/// Recursively collect all nodes from the tree in depth-first order.
fn collect_nodes<'a>(node: &'a ExplainNode, out: &mut Vec<&'a ExplainNode>) {
    out.push(node);
    for child in &node.children {
        collect_nodes(child, out);
    }
}

/// Collect node indices that are descendants of a Limit node where the
/// descendant's `actual_rows` equals the Limit's `actual_rows`.
///
/// A Limit node artificially truncates its children's output: the children
/// may have high estimated row counts (the unfiltered estimate) while only
/// returning exactly as many rows as the Limit allows.  Row estimate warnings
/// on such children are false positives.
fn collect_limit_constrained_indices(
    node: &ExplainNode,
    limit_rows: Option<f64>,
    out: &mut std::collections::HashSet<usize>,
) {
    let effective_limit = if node.node_type == "Limit" {
        // When entering a Limit node, its actual_rows is the cap for children.
        Some(node.actual_rows)
    } else {
        limit_rows
    };

    if let Some(limit) = effective_limit {
        if node.node_type != "Limit" && (node.actual_rows - limit).abs() < 0.5 {
            // This node returned exactly the Limit's row count — its estimate
            // is for the full un-limited result, so skip the row estimate check.
            out.insert(node.index);
        }
    }

    for child in &node.children {
        collect_limit_constrained_indices(child, effective_limit, out);
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Run all issue-detection heuristics on every node of `plan`.
///
/// Returns issues sorted by:
/// 1. Severity ascending (`Slow` first, then `Warn`, then `Info`).
/// 2. `time_percent` descending within the same severity tier (highest
///    time cost first).
pub fn detect_issues(plan: &ExplainPlan) -> Vec<PlanIssue> {
    // Collect all nodes.
    let mut nodes: Vec<&ExplainNode> = Vec::new();
    collect_nodes(&plan.root, &mut nodes);

    // Build the set of nodes that are limit-constrained (descendants of a
    // Limit node whose actual rows match the Limit's row count).  Row estimate
    // warnings for these nodes are suppressed — the child's estimated rows
    // reflect the full un-limited result set, not a planning error.
    let mut limit_constrained = std::collections::HashSet::new();
    collect_limit_constrained_indices(&plan.root, None, &mut limit_constrained);

    // Apply every heuristic to every node.
    let mut issues: Vec<PlanIssue> = nodes
        .iter()
        .flat_map(|node| {
            // Skip row estimate check for limit-constrained nodes.
            let row_est_issue = if limit_constrained.contains(&node.index) {
                None
            } else {
                check_row_estimate_error(node)
            };
            [
                check_seq_scan_large(node),
                row_est_issue,
                check_sort_spill(node),
                check_hash_spill(node),
                check_high_filter_removal(node),
                check_workers_not_launched(node),
                check_nested_loop_seq_scan(node),
            ]
        })
        .flatten()
        .collect();

    // Sort: severity (Slow < Warn < Info via Ord), then time_percent desc.
    // We look up the node's time_percent by node_index.
    let time_map: std::collections::HashMap<usize, f64> =
        nodes.iter().map(|n| (n.index, n.time_percent)).collect();

    issues.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| {
            let ta = time_map.get(&a.node_index).copied().unwrap_or(0.0);
            let tb = time_map.get(&b.node_index).copied().unwrap_or(0.0);
            // Descending by time_percent.
            tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    issues
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

    fn seq_scan(relation: &str, actual_rows: f64, time_percent: f64) -> ExplainNode {
        ExplainNode {
            index: 0,
            node_type: "Seq Scan".to_owned(),
            relation_name: Some(relation.to_owned()),
            actual_rows,
            time_percent,
            ..Default::default()
        }
    }

    fn make_plan(root: ExplainNode, total_ms: f64) -> ExplainPlan {
        ExplainPlan {
            root,
            total_execution_ms: total_ms,
        }
    }

    // -----------------------------------------------------------------------
    // Heuristic 1: seq scan on large table
    // -----------------------------------------------------------------------

    #[test]
    fn seq_scan_large_fires() {
        let node = seq_scan("orders", 50_000.0, 40.0);
        let issue = check_seq_scan_large(&node).expect("should detect large seq scan");
        assert_eq!(issue.severity, IssueSeverity::Slow);
        assert!(issue.title.contains("orders"));
        assert!(issue.suggestion.is_some());
    }

    #[test]
    fn seq_scan_small_table_no_issue() {
        let node = seq_scan("tiny", 500.0, 40.0);
        assert!(check_seq_scan_large(&node).is_none());
    }

    #[test]
    fn seq_scan_low_time_percent_no_issue() {
        let node = seq_scan("orders", 50_000.0, 5.0);
        assert!(check_seq_scan_large(&node).is_none());
    }

    #[test]
    fn seq_scan_exactly_at_threshold_no_issue() {
        // Boundary: exactly 10_000 rows and exactly 10% — should NOT fire
        // (the condition is strict greater-than).
        let node = seq_scan("orders", 10_000.0, 10.0);
        assert!(check_seq_scan_large(&node).is_none());
    }

    #[test]
    fn seq_scan_just_above_threshold_fires() {
        let node = seq_scan("orders", 10_001.0, 10.1);
        assert!(check_seq_scan_large(&node).is_some());
    }

    #[test]
    fn non_seq_scan_node_ignored() {
        let node = ExplainNode {
            node_type: "Index Scan".to_owned(),
            actual_rows: 100_000.0,
            time_percent: 80.0,
            ..Default::default()
        };
        assert!(check_seq_scan_large(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 2: row estimate error
    // -----------------------------------------------------------------------

    #[test]
    fn row_estimate_underestimate_fires() {
        let node = ExplainNode {
            estimated_rows: 100.0,
            actual_rows: 2_000.0,
            ..Default::default()
        };
        let issue = check_row_estimate_error(&node).expect("should fire for 20x underestimate");
        assert_eq!(issue.severity, IssueSeverity::Warn);
        assert!(issue.title.contains("underestimated"));
    }

    #[test]
    fn row_estimate_overestimate_fires() {
        let node = ExplainNode {
            estimated_rows: 10_000.0,
            actual_rows: 50.0,
            ..Default::default()
        };
        let issue = check_row_estimate_error(&node).expect("should fire for 200x overestimate");
        assert!(issue.title.contains("overestimated"));
    }

    #[test]
    fn row_estimate_accurate_no_issue() {
        let node = ExplainNode {
            estimated_rows: 1_000.0,
            actual_rows: 1_100.0,
            ..Default::default()
        };
        assert!(check_row_estimate_error(&node).is_none());
    }

    #[test]
    fn row_estimate_exact_10x_no_issue() {
        // ratio == 10.0 exactly should NOT fire (boundary is inclusive).
        let node = ExplainNode {
            estimated_rows: 100.0,
            actual_rows: 1_000.0,
            ..Default::default()
        };
        assert!(check_row_estimate_error(&node).is_none());
    }

    #[test]
    fn row_estimate_just_over_10x_fires() {
        let node = ExplainNode {
            estimated_rows: 100.0,
            actual_rows: 1_001.0,
            ..Default::default()
        };
        assert!(check_row_estimate_error(&node).is_some());
    }

    #[test]
    fn row_estimate_zero_estimated_no_crash() {
        let node = ExplainNode {
            estimated_rows: 0.0,
            actual_rows: 500.0,
            ..Default::default()
        };
        // Must not panic; should return None.
        assert!(check_row_estimate_error(&node).is_none());
    }

    #[test]
    fn row_estimate_zero_actual_no_crash() {
        // Covers plain EXPLAIN without ANALYZE (actual_rows stays 0.0).
        // Must not produce "inf" or panic.
        let node = ExplainNode {
            estimated_rows: 1_000.0,
            actual_rows: 0.0,
            ..Default::default()
        };
        assert!(check_row_estimate_error(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 3: sort spill to disk
    // -----------------------------------------------------------------------

    #[test]
    fn sort_spill_disk_fires() {
        let node = ExplainNode {
            node_type: "Sort".to_owned(),
            sort_space_type: Some("Disk".to_owned()),
            ..Default::default()
        };
        let issue = check_sort_spill(&node).expect("should fire for disk sort");
        assert_eq!(issue.severity, IssueSeverity::Warn);
        assert!(issue.title.contains("disk"));
    }

    #[test]
    fn sort_in_memory_no_issue() {
        let node = ExplainNode {
            node_type: "Sort".to_owned(),
            sort_space_type: Some("Memory".to_owned()),
            ..Default::default()
        };
        assert!(check_sort_spill(&node).is_none());
    }

    #[test]
    fn sort_space_absent_no_issue() {
        let node = ExplainNode {
            node_type: "Sort".to_owned(),
            sort_space_type: None,
            ..Default::default()
        };
        assert!(check_sort_spill(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 4: hash spill
    // -----------------------------------------------------------------------

    #[test]
    fn hash_spill_fires_for_multiple_batches() {
        let node = ExplainNode {
            node_type: "Hash".to_owned(),
            hash_batches: Some(4),
            ..Default::default()
        };
        let issue = check_hash_spill(&node).expect("should fire for 4 batches");
        assert_eq!(issue.severity, IssueSeverity::Warn);
        assert!(issue.title.contains("4 batches"));
    }

    #[test]
    fn hash_single_batch_no_issue() {
        let node = ExplainNode {
            node_type: "Hash".to_owned(),
            hash_batches: Some(1),
            ..Default::default()
        };
        assert!(check_hash_spill(&node).is_none());
    }

    #[test]
    fn hash_batches_absent_no_issue() {
        let node = ExplainNode {
            node_type: "Hash".to_owned(),
            hash_batches: None,
            ..Default::default()
        };
        assert!(check_hash_spill(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 5: high filter removal
    // -----------------------------------------------------------------------

    #[test]
    fn high_filter_removal_fires() {
        let node = ExplainNode {
            node_type: "Seq Scan".to_owned(),
            actual_rows: 100.0,
            rows_removed_by_filter: 9_000.0,
            ..Default::default()
        };
        // total = 9100; removed ratio = 9000/9100 ≈ 0.989 > 0.9
        let issue = check_high_filter_removal(&node).expect("should fire");
        assert_eq!(issue.severity, IssueSeverity::Warn);
        assert!(issue.detail.contains("9000"));
    }

    #[test]
    fn moderate_filter_no_issue() {
        let node = ExplainNode {
            actual_rows: 500.0,
            rows_removed_by_filter: 400.0,
            ..Default::default()
        };
        // removal ratio = 400/900 ≈ 0.44 — fine
        assert!(check_high_filter_removal(&node).is_none());
    }

    #[test]
    fn exactly_90_percent_removal_no_issue() {
        // Exactly 90% should NOT fire (threshold is strict greater-than).
        let node = ExplainNode {
            actual_rows: 100.0,
            rows_removed_by_filter: 900.0,
            ..Default::default()
        };
        assert!(check_high_filter_removal(&node).is_none());
    }

    #[test]
    fn no_rows_removed_no_issue() {
        let node = ExplainNode {
            actual_rows: 1_000.0,
            rows_removed_by_filter: 0.0,
            ..Default::default()
        };
        assert!(check_high_filter_removal(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 6: parallel workers not launched
    // -----------------------------------------------------------------------

    #[test]
    fn workers_not_launched_fires() {
        let node = ExplainNode {
            node_type: "Gather".to_owned(),
            workers_planned: Some(4),
            workers_launched: Some(2),
            ..Default::default()
        };
        let issue = check_workers_not_launched(&node).expect("should fire");
        assert_eq!(issue.severity, IssueSeverity::Info);
        assert!(issue.title.contains("2/4"));
    }

    #[test]
    fn all_workers_launched_no_issue() {
        let node = ExplainNode {
            node_type: "Gather".to_owned(),
            workers_planned: Some(4),
            workers_launched: Some(4),
            ..Default::default()
        };
        assert!(check_workers_not_launched(&node).is_none());
    }

    #[test]
    fn workers_launched_exceeds_planned_no_issue() {
        // Defensive: launched >= planned should not fire.
        let node = ExplainNode {
            workers_planned: Some(2),
            workers_launched: Some(3),
            ..Default::default()
        };
        assert!(check_workers_not_launched(&node).is_none());
    }

    #[test]
    fn workers_planned_zero_no_issue() {
        let node = ExplainNode {
            workers_planned: Some(0),
            workers_launched: Some(0),
            ..Default::default()
        };
        assert!(check_workers_not_launched(&node).is_none());
    }

    #[test]
    fn workers_fields_absent_no_issue() {
        let node = ExplainNode {
            workers_planned: None,
            workers_launched: None,
            ..Default::default()
        };
        assert!(check_workers_not_launched(&node).is_none());
    }

    // -----------------------------------------------------------------------
    // Heuristic 7: nested loop with high loop child seq scan
    // -----------------------------------------------------------------------

    #[test]
    fn nested_loop_seq_scan_fires() {
        let inner = ExplainNode {
            index: 1,
            node_type: "Seq Scan".to_owned(),
            relation_name: Some("products".to_owned()),
            loops: 500.0,
            ..Default::default()
        };
        let outer_node = ExplainNode {
            index: 0,
            node_type: "Nested Loop".to_owned(),
            children: vec![inner],
            ..Default::default()
        };
        let issue = check_nested_loop_seq_scan(&outer_node).expect("should fire for 500 loops");
        assert_eq!(issue.severity, IssueSeverity::Slow);
        assert!(issue.title.contains("products"));
        assert!(issue.title.contains("500"));
    }

    #[test]
    fn nested_loop_few_loops_no_issue() {
        let inner = ExplainNode {
            node_type: "Seq Scan".to_owned(),
            loops: 10.0,
            ..Default::default()
        };
        let outer_node = ExplainNode {
            node_type: "Nested Loop".to_owned(),
            children: vec![inner],
            ..Default::default()
        };
        assert!(check_nested_loop_seq_scan(&outer_node).is_none());
    }

    #[test]
    fn nested_loop_index_scan_child_no_issue() {
        // High loops but child is an Index Scan — should not fire.
        let inner = ExplainNode {
            node_type: "Index Scan".to_owned(),
            loops: 5_000.0,
            ..Default::default()
        };
        let outer_node = ExplainNode {
            node_type: "Nested Loop".to_owned(),
            children: vec![inner],
            ..Default::default()
        };
        assert!(check_nested_loop_seq_scan(&outer_node).is_none());
    }

    #[test]
    fn non_nested_loop_ignored() {
        let inner = ExplainNode {
            node_type: "Seq Scan".to_owned(),
            loops: 5_000.0,
            ..Default::default()
        };
        let outer_node = ExplainNode {
            node_type: "Hash Join".to_owned(),
            children: vec![inner],
            ..Default::default()
        };
        assert!(check_nested_loop_seq_scan(&outer_node).is_none());
    }

    // -----------------------------------------------------------------------
    // detect_issues: integration + sorting
    // -----------------------------------------------------------------------

    #[test]
    fn detect_issues_empty_plan() {
        let plan = ExplainPlan {
            root: ExplainNode {
                index: 0,
                node_type: "Result".to_owned(),
                estimated_rows: 1.0,
                actual_rows: 1.0,
                ..Default::default()
            },
            total_execution_ms: 0.1,
        };
        let issues = detect_issues(&plan);
        assert!(issues.is_empty());
    }

    #[test]
    fn detect_issues_collects_multiple_issues() {
        let mut plan = ExplainPlan {
            root: ExplainNode {
                index: 0,
                node_type: "Seq Scan".to_owned(),
                relation_name: Some("big_table".to_owned()),
                estimated_rows: 100.0,
                actual_rows: 500_000.0,
                actual_total_ms: 800.0,
                sort_space_type: None,
                ..Default::default()
            },
            total_execution_ms: 1_000.0,
        };
        plan.compute_time_percents();

        let issues = detect_issues(&plan);

        // Should detect both seq scan (Slow) and row estimate error (Warn).
        assert!(issues.len() >= 2);

        // Slow should come before Warn.
        assert_eq!(issues[0].severity, IssueSeverity::Slow);
        assert_eq!(issues[1].severity, IssueSeverity::Warn);
    }

    #[test]
    fn detect_issues_sorted_by_severity_then_time() {
        // Create two Warn-severity issues: one with higher time_percent.
        let mut low_time_node = ExplainNode {
            index: 0,
            node_type: "Sort".to_owned(),
            sort_space_type: Some("Disk".to_owned()),
            actual_total_ms: 10.0,
            ..Default::default()
        };
        let mut high_time_node = ExplainNode {
            index: 1,
            node_type: "Sort".to_owned(),
            sort_space_type: Some("Disk".to_owned()),
            actual_total_ms: 500.0,
            ..Default::default()
        };

        // Manually assign time percents (no full plan needed for this test).
        low_time_node.time_percent = 1.0;
        high_time_node.time_percent = 50.0;

        let plan = ExplainPlan {
            root: ExplainNode {
                index: 0,
                node_type: "Sort".to_owned(),
                sort_space_type: Some("Disk".to_owned()),
                actual_total_ms: 10.0,
                time_percent: 1.0,
                children: vec![ExplainNode {
                    index: 1,
                    node_type: "Sort".to_owned(),
                    sort_space_type: Some("Disk".to_owned()),
                    actual_total_ms: 500.0,
                    time_percent: 50.0,
                    ..Default::default()
                }],
                ..Default::default()
            },
            total_execution_ms: 1_000.0,
        };

        let issues = detect_issues(&plan);
        // Both are Warn; the one with higher time_percent (node 1, 50%) should be first.
        assert!(issues.len() >= 2);
        let warn_issues: Vec<&PlanIssue> = issues
            .iter()
            .filter(|i| i.severity == IssueSeverity::Warn)
            .collect();
        assert!(warn_issues.len() >= 2);
        // The first Warn issue should reference node 1 (higher time_percent).
        assert_eq!(warn_issues[0].node_index, 1);
        assert_eq!(warn_issues[1].node_index, 0);
    }

    #[test]
    fn detect_issues_severity_ordering_slow_before_info() {
        // Slow issue on node 0, Info issue on node 1.
        let plan = ExplainPlan {
            root: ExplainNode {
                index: 0,
                node_type: "Gather".to_owned(),
                workers_planned: Some(4),
                workers_launched: Some(1),
                children: vec![ExplainNode {
                    index: 1,
                    node_type: "Seq Scan".to_owned(),
                    relation_name: Some("events".to_owned()),
                    actual_rows: 2_000_000.0,
                    time_percent: 95.0,
                    ..Default::default()
                }],
                ..Default::default()
            },
            total_execution_ms: 10_000.0,
        };

        let issues = detect_issues(&plan);
        assert!(!issues.is_empty());

        // Verify severities are in ascending order (Slow < Warn < Info).
        let severities: Vec<IssueSeverity> = issues.iter().map(|i| i.severity).collect();
        let mut sorted = severities.clone();
        sorted.sort();
        assert_eq!(severities, sorted);
    }

    #[test]
    fn compute_time_percents_populates_fields() {
        let mut plan = ExplainPlan {
            root: ExplainNode {
                index: 0,
                actual_total_ms: 750.0,
                ..Default::default()
            },
            total_execution_ms: 1_000.0,
        };
        plan.compute_time_percents();
        assert!((plan.root.time_percent - 75.0).abs() < 0.01);
    }

    #[test]
    fn compute_time_percents_zero_total_no_panic() {
        let mut plan = ExplainPlan {
            root: ExplainNode {
                actual_total_ms: 500.0,
                ..Default::default()
            },
            total_execution_ms: 0.0,
        };
        // Should not panic; time_percent stays 0.0.
        plan.compute_time_percents();
        assert!(plan.root.time_percent.abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Limit node false-positive suppression
    // -----------------------------------------------------------------------

    /// A Limit node stops execution after N rows.  Children under a Limit may
    /// have large estimated row counts (the full un-limited estimate) while
    /// returning only as many rows as the Limit allows.  The row estimate check
    /// must not fire for these nodes — they are not a planning error.
    #[test]
    fn row_estimate_under_limit_no_false_positive() {
        // Limit(10) → Index Only Scan  estimated=1_000_000, actual=10
        // Without the fix, this would fire with "overestimated 100000x".
        let index_scan = ExplainNode {
            index: 1,
            node_type: "Index Only Scan".to_owned(),
            estimated_rows: 1_000_000.0,
            actual_rows: 10.0,
            actual_total_ms: 0.1,
            time_percent: 1.0,
            ..Default::default()
        };
        let limit_node = ExplainNode {
            index: 0,
            node_type: "Limit".to_owned(),
            estimated_rows: 10.0,
            actual_rows: 10.0,
            actual_total_ms: 0.1,
            time_percent: 1.0,
            children: vec![index_scan],
            ..Default::default()
        };
        let plan = ExplainPlan {
            root: limit_node,
            total_execution_ms: 0.1,
        };

        let issues = detect_issues(&plan);
        // The index scan under Limit should NOT generate a row estimate issue.
        let row_est_issues: Vec<&PlanIssue> = issues
            .iter()
            .filter(|i| i.title.contains("estimate"))
            .collect();
        assert!(
            row_est_issues.is_empty(),
            "expected no row estimate false positive under Limit, got: {row_est_issues:?}"
        );
    }

    /// A node with a genuine estimate error NOT under a Limit should still fire.
    #[test]
    fn row_estimate_not_under_limit_still_fires() {
        // No Limit ancestor: genuine row estimate error should be detected.
        let bad_scan = ExplainNode {
            index: 0,
            node_type: "Seq Scan".to_owned(),
            estimated_rows: 1_000_000.0,
            actual_rows: 10.0,
            actual_total_ms: 100.0,
            time_percent: 50.0,
            ..Default::default()
        };
        let plan = ExplainPlan {
            root: bad_scan,
            total_execution_ms: 200.0,
        };

        let issues = detect_issues(&plan);
        let row_est_issues: Vec<&PlanIssue> = issues
            .iter()
            .filter(|i| i.title.contains("estimate"))
            .collect();
        assert!(
            !row_est_issues.is_empty(),
            "expected row estimate issue for genuine mismatch without Limit"
        );
    }

    /// End-to-end: parse a real EXPLAIN ANALYZE plan with Limit, verify no
    /// false-positive row estimate warning from the Limit-constrained child.
    #[test]
    fn limit_false_positive_suppressed_via_parse() {
        // This is the text PostgreSQL emits for:
        //   EXPLAIN ANALYZE SELECT * FROM orders WHERE status = 'pending' LIMIT 10;
        // The Seq Scan estimates 203200 rows but only returns 10 (limited by
        // the Limit node).  Without the fix, a "20320x overestimated" warning
        // fires on the Seq Scan — which is a false positive.
        let plan_text = "Limit  (cost=0.00..2.32 rows=10 width=61) (actual time=0.040..0.044 rows=10 loops=1)\n  ->  Seq Scan on orders  (cost=0.00..47139.00 rows=203200 width=61) (actual time=0.040..0.043 rows=10 loops=1)\n        Filter: (status = 'pending'::text)\n        Rows Removed by Filter: 66\n        Buffers: shared hit=2\nPlanning Time: 0.050 ms\nExecution Time: 0.034 ms\n";

        let parsed = crate::explain::parse(plan_text).expect("parse should succeed");

        // Verify parse tree structure.
        assert_eq!(parsed.nodes.len(), 1, "should have one root node (Limit)");
        let root = &parsed.nodes[0];
        assert_eq!(root.node_type, "Limit");
        assert_eq!(root.estimated_rows, Some(10.0));
        assert_eq!(root.children.len(), 1, "Limit should have one child");
        let seq_scan = &root.children[0];
        assert_eq!(seq_scan.node_type, "Seq Scan");
        assert_eq!(seq_scan.estimated_rows, Some(203_200.0));
        assert_eq!(seq_scan.actual_rows, Some(10.0));

        let issues_plan = crate::explain::to_issues_plan(&parsed);

        // Verify issues plan structure.
        assert_eq!(issues_plan.root.node_type, "Limit");
        assert_eq!(issues_plan.root.index, 0);
        assert_eq!(issues_plan.root.children.len(), 1);
        assert_eq!(issues_plan.root.children[0].node_type, "Seq Scan");
        assert_eq!(issues_plan.root.children[0].index, 1);

        let issues = detect_issues(&issues_plan);

        let row_est_issues: Vec<&PlanIssue> = issues
            .iter()
            .filter(|i| i.title.contains("estimate"))
            .collect();
        assert!(
            row_est_issues.is_empty(),
            "expected no row estimate false positive under Limit, got: {row_est_issues:?}"
        );
    }
}
