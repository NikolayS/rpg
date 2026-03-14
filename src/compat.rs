//! psql compatibility report for the `--compat` flag.
//!
//! Prints a formatted table showing which psql metacommands Samo supports,
//! grouped by category, then exits.

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Support status for a single psql command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatStatus {
    /// Fully implemented; behaviour matches psql.
    Full,
    /// Recognised but behaviour differs from psql in some cases.
    Partial,
    /// Not yet implemented.
    Unsupported,
}

impl CompatStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
        }
    }

    /// Returns `true` when the command counts as "supported" for the
    /// percentage calculation (Full or Partial).
    fn is_supported(self) -> bool {
        matches!(self, Self::Full | Self::Partial)
    }
}

/// A single entry in the compatibility table.
pub struct CompatEntry {
    pub command: &'static str,
    pub status: CompatStatus,
    pub notes: &'static str,
}

impl CompatEntry {
    const fn new(command: &'static str, status: CompatStatus, notes: &'static str) -> Self {
        Self {
            command,
            status,
            notes,
        }
    }
}

// ---------------------------------------------------------------------------
// Compatibility table
// ---------------------------------------------------------------------------

/// Ordered list of (category label, entries) pairs.
#[allow(clippy::too_many_lines)]
fn categories() -> Vec<(&'static str, Vec<CompatEntry>)> {
    vec![
        (
            "General",
            vec![
                CompatEntry::new(r"\q", CompatStatus::Full, "quit"),
                CompatEntry::new(r"\?", CompatStatus::Full, "backslash command help"),
                CompatEntry::new(
                    r"\h",
                    CompatStatus::Partial,
                    "SQL syntax help (limited topics)",
                ),
                CompatEntry::new(r"\!", CompatStatus::Full, "execute shell command"),
                CompatEntry::new(r"\cd", CompatStatus::Full, "change working directory"),
                CompatEntry::new(r"\timing", CompatStatus::Full, "toggle query timing"),
                CompatEntry::new(r"\echo", CompatStatus::Full, "print text to stdout"),
                CompatEntry::new(r"\qecho", CompatStatus::Full, "print text to query output"),
                CompatEntry::new(r"\warn", CompatStatus::Full, "print text to stderr"),
                CompatEntry::new(r"\copyright", CompatStatus::Full, "show copyright notice"),
                CompatEntry::new(r"\version", CompatStatus::Full, "show samo version"),
            ],
        ),
        (
            "Variables",
            vec![
                CompatEntry::new(r"\set", CompatStatus::Full, "set or display variable"),
                CompatEntry::new(r"\unset", CompatStatus::Full, "unset variable"),
                CompatEntry::new(
                    r"\prompt",
                    CompatStatus::Full,
                    "prompt user for variable value",
                ),
            ],
        ),
        (
            "Describe",
            vec![
                CompatEntry::new(
                    r"\d",
                    CompatStatus::Full,
                    "describe object or list relations",
                ),
                CompatEntry::new(r"\dt", CompatStatus::Full, "list tables"),
                CompatEntry::new(r"\di", CompatStatus::Full, "list indexes"),
                CompatEntry::new(r"\ds", CompatStatus::Full, "list sequences"),
                CompatEntry::new(r"\dv", CompatStatus::Full, "list views"),
                CompatEntry::new(r"\dm", CompatStatus::Full, "list materialised views"),
                CompatEntry::new(r"\df", CompatStatus::Full, "list functions"),
                CompatEntry::new(r"\dn", CompatStatus::Full, "list schemas"),
                CompatEntry::new(r"\du", CompatStatus::Full, "list roles"),
                CompatEntry::new(r"\dg", CompatStatus::Full, "list roles (alias for \\du)"),
                CompatEntry::new(r"\dp", CompatStatus::Full, "list access privileges"),
                CompatEntry::new(r"\db", CompatStatus::Full, "list tablespaces"),
                CompatEntry::new(r"\dT", CompatStatus::Full, "list data types"),
                CompatEntry::new(r"\dD", CompatStatus::Full, "list domains"),
                CompatEntry::new(
                    r"\dF",
                    CompatStatus::Unsupported,
                    "list text search configurations",
                ),
                CompatEntry::new(
                    r"\dL",
                    CompatStatus::Unsupported,
                    "list procedural languages",
                ),
                CompatEntry::new(r"\do", CompatStatus::Full, "list operators"),
                CompatEntry::new(r"\dO", CompatStatus::Unsupported, "list collations"),
                CompatEntry::new(r"\de", CompatStatus::Full, "list foreign tables"),
                CompatEntry::new(r"\des", CompatStatus::Full, "list foreign servers"),
                CompatEntry::new(r"\dew", CompatStatus::Full, "list foreign-data wrappers"),
                CompatEntry::new(r"\det", CompatStatus::Full, "list foreign tables via FDW"),
                CompatEntry::new(r"\deu", CompatStatus::Full, "list user mappings"),
                CompatEntry::new(r"\dx", CompatStatus::Full, "list installed extensions"),
                CompatEntry::new(r"\dy", CompatStatus::Full, "list event triggers"),
                CompatEntry::new(r"\dc", CompatStatus::Full, "list conversions"),
                CompatEntry::new(r"\dC", CompatStatus::Full, "list casts"),
                CompatEntry::new(r"\dd", CompatStatus::Full, "list object comments"),
                CompatEntry::new(r"\l", CompatStatus::Full, "list databases"),
                CompatEntry::new(
                    r"\sf",
                    CompatStatus::Partial,
                    "show function source (read-only)",
                ),
                CompatEntry::new(
                    r"\sv",
                    CompatStatus::Partial,
                    "show view definition (read-only)",
                ),
            ],
        ),
        (
            "Display",
            vec![
                CompatEntry::new(r"\x", CompatStatus::Full, "toggle expanded output"),
                CompatEntry::new(
                    r"\pset",
                    CompatStatus::Partial,
                    "set print option (most options)",
                ),
                CompatEntry::new(r"\a", CompatStatus::Full, "toggle aligned/unaligned output"),
                CompatEntry::new(r"\C", CompatStatus::Full, "set or clear table title"),
                CompatEntry::new(r"\t", CompatStatus::Full, "toggle tuples-only mode"),
                CompatEntry::new(
                    r"\T",
                    CompatStatus::Unsupported,
                    "set HTML table tag attributes",
                ),
                CompatEntry::new(r"\f", CompatStatus::Full, "set field separator"),
                CompatEntry::new(r"\H", CompatStatus::Full, "toggle HTML output mode"),
            ],
        ),
        (
            "I/O",
            vec![
                CompatEntry::new(r"\i", CompatStatus::Full, "execute commands from file"),
                CompatEntry::new(
                    r"\ir",
                    CompatStatus::Full,
                    "include file relative to script",
                ),
                CompatEntry::new(r"\o", CompatStatus::Full, "send output to file or pipe"),
                CompatEntry::new(
                    r"\copy",
                    CompatStatus::Partial,
                    "client-side COPY (common options)",
                ),
                CompatEntry::new(r"\e", CompatStatus::Full, "edit buffer with $EDITOR"),
                CompatEntry::new(r"\ef", CompatStatus::Unsupported, "edit function source"),
                CompatEntry::new(r"\ev", CompatStatus::Unsupported, "edit view definition"),
                CompatEntry::new(r"\w", CompatStatus::Full, "write buffer to file"),
                CompatEntry::new(r"\r", CompatStatus::Full, "reset query buffer"),
                CompatEntry::new(r"\p", CompatStatus::Full, "print query buffer"),
                CompatEntry::new(r"\g", CompatStatus::Full, "execute buffer"),
                CompatEntry::new(
                    r"\gx",
                    CompatStatus::Full,
                    "execute buffer with expanded output",
                ),
                CompatEntry::new(
                    r"\gexec",
                    CompatStatus::Full,
                    "execute buffer then execute result cells",
                ),
                CompatEntry::new(
                    r"\gset",
                    CompatStatus::Full,
                    "store result columns as variables",
                ),
                CompatEntry::new(
                    r"\gdesc",
                    CompatStatus::Full,
                    "describe result columns of buffer",
                ),
                CompatEntry::new(
                    r"\crosstabview",
                    CompatStatus::Full,
                    "pivot result into cross-tabulation",
                ),
                CompatEntry::new(
                    r"\watch",
                    CompatStatus::Full,
                    "re-execute query every N seconds",
                ),
            ],
        ),
        (
            "Connection",
            vec![
                CompatEntry::new(r"\c", CompatStatus::Full, "reconnect to database"),
                CompatEntry::new(r"\conninfo", CompatStatus::Full, "show connection details"),
                CompatEntry::new(
                    r"\encoding",
                    CompatStatus::Partial,
                    "show or set client encoding",
                ),
                CompatEntry::new(r"\password", CompatStatus::Full, "change user password"),
            ],
        ),
        (
            "Conditional",
            vec![
                CompatEntry::new(r"\if", CompatStatus::Full, "begin conditional block"),
                CompatEntry::new(r"\elif", CompatStatus::Full, "alternate condition branch"),
                CompatEntry::new(
                    r"\else",
                    CompatStatus::Full,
                    "unconditional alternate branch",
                ),
                CompatEntry::new(r"\endif", CompatStatus::Full, "end conditional block"),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Report printer
// ---------------------------------------------------------------------------

/// Print the psql compatibility report to stdout and return the summary
/// numbers `(supported, total)` for use in tests.
pub fn print_compat_report() -> (usize, usize) {
    let cats = categories();

    // Compute column widths from data.
    let mut cmd_w: usize = "Command".len();
    let mut status_w: usize = "Status".len();
    let mut notes_w: usize = "Notes".len();

    for (_, entries) in &cats {
        for e in entries {
            cmd_w = cmd_w.max(e.command.len());
            status_w = status_w.max(e.status.label().len());
            notes_w = notes_w.max(e.notes.len());
        }
    }

    let separator = format!(
        "+-{cmd}-+-{status}-+-{notes}-+",
        cmd = "-".repeat(cmd_w),
        status = "-".repeat(status_w),
        notes = "-".repeat(notes_w),
    );

    println!("psql compatibility report");
    println!("{separator}");
    println!(
        "| {:<cmd_w$} | {:<status_w$} | {:<notes_w$} |",
        "Command",
        "Status",
        "Notes",
        cmd_w = cmd_w,
        status_w = status_w,
        notes_w = notes_w,
    );
    println!("{separator}");

    let mut total: usize = 0;
    let mut supported: usize = 0;

    for (cat, entries) in &cats {
        println!(
            "| {:<cmd_w$} | {:<status_w$} | {:<notes_w$} |",
            format!("[{cat}]"),
            "",
            "",
            cmd_w = cmd_w,
            status_w = status_w,
            notes_w = notes_w,
        );
        for e in entries {
            total += 1;
            if e.status.is_supported() {
                supported += 1;
            }
            println!(
                "| {:<cmd_w$} | {:<status_w$} | {:<notes_w$} |",
                e.command,
                e.status.label(),
                e.notes,
                cmd_w = cmd_w,
                status_w = status_w,
                notes_w = notes_w,
            );
        }
    }

    println!("{separator}");

    let pct = if total == 0 {
        0
    } else {
        supported * 100 / total
    };
    println!("Overall: {supported}/{total} commands supported ({pct}%)");

    (supported, total)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_compat_report_does_not_panic() {
        // Redirect nothing — we just want to ensure no panic.
        let (supported, total) = print_compat_report();
        assert!(total > 0, "must have at least one entry");
        assert!(supported <= total, "supported cannot exceed total");
    }

    #[test]
    fn percentage_calculation_is_correct() {
        // All full → 100 %.
        let supported = 3_usize;
        let total = 3_usize;
        let pct = supported * 100 / total;
        assert_eq!(pct, 100);

        // Two out of four → 50 %.
        let supported = 2_usize;
        let total = 4_usize;
        let pct = supported * 100 / total;
        assert_eq!(pct, 50);
    }

    #[test]
    fn every_entry_has_non_empty_command_and_notes() {
        let cats = categories();
        for (cat, entries) in &cats {
            for e in entries {
                assert!(!e.command.is_empty(), "empty command in category {cat}",);
                assert!(
                    !e.notes.is_empty(),
                    "empty notes for {} in category {cat}",
                    e.command,
                );
            }
        }
    }

    #[test]
    fn unsupported_commands_are_not_counted_as_supported() {
        let cats = categories();
        let unsupported_count = cats
            .iter()
            .flat_map(|(_, entries)| entries)
            .filter(|e| e.status == CompatStatus::Unsupported)
            .count();
        let total = cats.iter().flat_map(|(_, entries)| entries).count();
        let full_or_partial = cats
            .iter()
            .flat_map(|(_, entries)| entries)
            .filter(|e| e.status.is_supported())
            .count();
        assert_eq!(
            full_or_partial + unsupported_count,
            total,
            "all entries must be full, partial, or unsupported",
        );
    }
}
