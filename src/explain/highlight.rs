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

//! Terminal color highlighting for EXPLAIN ANALYZE output.
//!
//! Applies ANSI colors to key elements of `PostgreSQL` EXPLAIN output
//! to make plans scannable at a glance.

/// Apply ANSI color highlighting to an EXPLAIN ANALYZE plan string.
///
/// Preserves all text and indentation — only wraps tokens with ANSI codes.
/// Returns input unchanged when `no_color` is true.
pub fn highlight_explain(plan: &str, no_color: bool) -> String {
    if no_color {
        return plan.to_owned();
    }
    plan.lines()
        .map(highlight_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn highlight_line(line: &str) -> String {
    // Node type patterns: Seq Scan, Index Scan, Hash Join, etc.
    // Color: bold cyan (\x1b[1;36m)
    let node_types = [
        "Seq Scan",
        "Index Scan",
        "Index Only Scan",
        "Bitmap Heap Scan",
        "Bitmap Index Scan",
        "Hash Join",
        "Merge Join",
        "Nested Loop",
        "Hash",
        "Sort",
        "Aggregate",
        "Group",
        "Limit",
        "Append",
        "Subquery Scan",
        "CTE Scan",
        "Materialize",
        "Unique",
        "Gather",
        "Gather Merge",
        "Parallel Seq Scan",
    ];

    let mut result = line.to_owned();

    // Highlight node types
    for node in &node_types {
        if result.contains(node) {
            result = result.replace(node, &format!("\x1b[1;36m{node}\x1b[0m"));
            break; // only first match per line
        }
    }

    // Highlight actual time — color by magnitude
    if let Some(time_ms) = extract_actual_time(&result) {
        let color = if time_ms >= 100.0 {
            "\x1b[31m" // red — slow
        } else if time_ms >= 10.0 {
            "\x1b[33m" // yellow — moderate
        } else {
            "\x1b[32m" // green — fast
        };
        // Find and colorize the actual time= segment
        if let Some(pos) = result.find("actual time=") {
            let end = result[pos..]
                .find(')')
                .map_or(result.len(), |i| pos + i + 1);
            let segment = result[pos..end].to_owned();
            result = format!(
                "{}{}{}{}\x1b[0m{}",
                &result[..pos],
                color,
                segment,
                "\x1b[0m",
                &result[end..]
            );
        }
    }

    // Highlight Filter and Index Cond lines — yellow
    if result.trim_start().starts_with("Filter:")
        || result.trim_start().starts_with("Index Cond:")
        || result.trim_start().starts_with("Recheck Cond:")
    {
        result = format!("\x1b[33m{result}\x1b[0m");
    }

    // Highlight Planning Time and Execution Time — bold
    if result.trim_start().starts_with("Planning Time:")
        || result.trim_start().starts_with("Execution Time:")
    {
        result = format!("\x1b[1m{result}\x1b[0m");
    }

    result
}

/// Extract the higher actual time value (ms) from a plan line.
/// Returns None if not found.
fn extract_actual_time(line: &str) -> Option<f64> {
    // actual time=X..Y — return Y
    let pos = line.find("actual time=")?;
    let after = &line[pos + "actual time=".len()..];
    let dotdot = after.find("..")?;
    let after_dots = &after[dotdot + 2..];
    let end = after_dots
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(after_dots.len());
    after_dots[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_scan_highlighted() {
        let line = "  ->  Seq Scan on orders  (cost=0.00..100.00 rows=1000 width=8)";
        let result = highlight_line(line);
        assert!(
            result.contains("\x1b[1;36m"),
            "Seq Scan should be cyan bold"
        );
        assert!(result.contains("Seq Scan"), "node type preserved");
    }

    #[test]
    fn slow_actual_time_is_red() {
        let line = "  ->  Seq Scan on orders  (cost=0.00..100.00 rows=1000 width=8) (actual time=150.000..200.000 rows=1000 loops=1)";
        let result = highlight_line(line);
        assert!(result.contains("\x1b[31m"), "slow node should be red");
    }

    #[test]
    fn fast_actual_time_is_green() {
        let line = "  ->  Index Scan using idx on t  (cost=0.00..8.00 rows=1 width=8) (actual time=0.100..0.200 rows=1 loops=1)";
        let result = highlight_line(line);
        assert!(result.contains("\x1b[32m"), "fast node should be green");
    }

    #[test]
    fn filter_line_is_yellow() {
        let line = "        Filter: (status = 'active')";
        let result = highlight_line(line);
        assert!(result.contains("\x1b[33m"), "Filter line should be yellow");
    }

    #[test]
    fn no_color_flag_returns_unchanged() {
        let plan = "Seq Scan on orders\n  Filter: (x = 1)";
        let result = highlight_explain(plan, true);
        assert_eq!(result, plan, "no_color=true should return unchanged");
    }

    #[test]
    fn execution_time_is_bold() {
        let line = "Execution Time: 847.234 ms";
        let result = highlight_line(line);
        assert!(result.contains("\x1b[1m"), "Execution Time should be bold");
    }
}
