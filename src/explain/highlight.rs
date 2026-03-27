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
//! Applies ANSI colors to key elements of Postgres EXPLAIN output
//! to make plans scannable at a glance.
//!
//! All byte offsets are computed against the **original** line text, then
//! applied in a single left-to-right reconstruction pass. This avoids the
//! class of panics where ANSI sequences injected in an earlier pass shift
//! byte offsets used by a later pass (see #745).

// IMPORTANT: longer/more-specific names must come before shorter prefixes.
// e.g. "Hash Join" before "Hash", "Bitmap Heap Scan" before "Bitmap Index Scan",
// "Index Only Scan" before "Index Scan", "Gather Merge" before "Gather",
// "Parallel Seq Scan" before "Seq Scan".
// The `break` after the first match means ordering determines which pattern
// wins when one is a substring of another.
static NODE_TYPES: [&str; 21] = [
    "Index Only Scan",
    "Bitmap Heap Scan",
    "Bitmap Index Scan",
    "Parallel Seq Scan",
    "Subquery Scan",
    "Gather Merge",
    "Index Scan",
    "Seq Scan",
    "Hash Join",
    "Merge Join",
    "Nested Loop",
    "Aggregate",
    "Materialize",
    "Append",
    "CTE Scan",
    "Unique",
    "Gather",
    "Group",
    "Limit",
    "Sort",
    "Hash",
];

/// A colored span: byte range `[start, end)` in the original line plus an
/// ANSI color prefix. The reset `\x1b[0m` is appended automatically.
struct Span {
    start: usize,
    end: usize,
    color: &'static str,
}

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
    // Whole-line wraps: these are exclusive — if one matches, wrap the entire
    // line and return early. Check on the raw line (no ANSI yet).
    let trimmed = line.trim_start();
    if trimmed.starts_with("Filter:")
        || trimmed.starts_with("Index Cond:")
        || trimmed.starts_with("Recheck Cond:")
    {
        return format!("\x1b[33m{line}\x1b[0m");
    }
    if trimmed.starts_with("Planning Time:") || trimmed.starts_with("Execution Time:") {
        return format!("\x1b[1m{line}\x1b[0m");
    }

    // Collect non-overlapping spans from the ORIGINAL line. All byte offsets
    // reference `line` — we never compute offsets against a mutated string.
    let mut spans: Vec<Span> = Vec::new();

    // Span 1: node type — bold cyan. First longest match wins.
    for node in &NODE_TYPES {
        if let Some(start) = line.find(node) {
            spans.push(Span {
                start,
                end: start + node.len(),
                color: "\x1b[1;36m",
            });
            break;
        }
    }

    // Span 2: actual time segment — colored by magnitude.
    if let Some(time_ms) = extract_actual_time(line) {
        let color = if time_ms >= 100.0 {
            "\x1b[31m" // red — slow
        } else if time_ms >= 10.0 {
            "\x1b[33m" // yellow — moderate
        } else {
            "\x1b[32m" // green — fast
        };
        if let Some(pos) = line.find("actual time=") {
            let end = line[pos..].find(')').map_or(line.len(), |i| pos + i + 1);
            spans.push(Span {
                start: pos,
                end,
                color,
            });
        }
    }

    if spans.is_empty() {
        return line.to_owned();
    }

    // Sort spans by start offset so we can apply them left-to-right.
    spans.sort_by_key(|s| s.start);

    // Single-pass reconstruction: copy uncolored text between spans verbatim,
    // wrap each span with its ANSI color + reset.
    let mut out = String::with_capacity(line.len() + spans.len() * 12);
    let mut cursor = 0;
    for span in &spans {
        // Copy text before this span.
        out.push_str(&line[cursor..span.start]);
        // Emit colored span.
        out.push_str(span.color);
        out.push_str(&line[span.start..span.end]);
        out.push_str("\x1b[0m");
        cursor = span.end;
    }
    // Copy any remaining text after the last span.
    out.push_str(&line[cursor..]);

    out
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

    #[test]
    fn hash_join_not_just_hash() {
        let line = "  ->  Hash Join  (cost=0.00..100.00 rows=1000 width=8)";
        let result = highlight_line(line);
        assert!(result.contains("Hash Join"), "Hash Join text preserved");
        assert!(
            result.contains("\x1b[1;36m"),
            "Hash Join should be highlighted"
        );
    }

    #[test]
    fn no_double_reset_on_node_with_time() {
        // A line with both a node type and actual time must not have
        // adjacent double reset codes.
        let line = "  ->  Hash Join  (cost=0.00..100.00 rows=1000 width=8) (actual time=150.000..200.000 rows=1000 loops=1)";
        let result = highlight_line(line);
        assert!(
            !result.contains("\x1b[0m\x1b[0m"),
            "should not have adjacent double reset codes"
        );
        assert!(result.contains("\x1b[31m"), "slow time should be red");
        assert!(
            result.contains("\x1b[1;36m"),
            "node type should be cyan bold"
        );
    }

    /// Regression test for #745: a line with both a node type AND `actual
    /// time=` would panic with "byte index N is not a char boundary" in the
    /// old multi-pass-mutate implementation because ANSI codes injected in
    /// pass 1 shifted the byte offsets used by pass 2.
    #[test]
    fn node_type_and_actual_time_no_panic() {
        let line = "->  Seq Scan on workload  (cost=0.00..1693.00 rows=100000 width=12) (actual time=0.012..8.234 rows=100000 loops=1)";
        let result = highlight_line(line);

        // Must not panic (the old code panicked here).
        // Verify both highlights are present and correctly placed.
        assert!(
            result.contains("\x1b[1;36mSeq Scan\x1b[0m"),
            "node type should be wrapped in cyan bold"
        );
        assert!(
            result.contains("\x1b[32mactual time=0.012..8.234 rows=100000 loops=1)\x1b[0m"),
            "actual time segment should be wrapped in green"
        );

        // Original text (minus ANSI codes) must be preserved.
        let stripped = strip_ansi(&result);
        assert_eq!(stripped, line, "stripped output must equal original line");
    }

    /// Verify that Parallel Seq Scan is matched as itself, not as Seq Scan.
    #[test]
    fn parallel_seq_scan_not_plain_seq_scan() {
        let line = "  ->  Parallel Seq Scan on big_table  (cost=0.00..4321.00 rows=50000 width=16) (actual time=0.023..45.678 rows=50000 loops=2)";
        let result = highlight_line(line);
        assert!(
            result.contains("\x1b[1;36mParallel Seq Scan\x1b[0m"),
            "should highlight full 'Parallel Seq Scan', not just 'Seq Scan'"
        );
    }

    /// Strip ANSI escape sequences from a string for comparison.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip until 'm' (end of CSI sequence).
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
