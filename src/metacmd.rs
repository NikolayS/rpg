//! Backslash (meta) command parser for Samo.
//!
//! Provides a richer parser than the original [`crate::repl`] implementation.
//! Key features:
//!
//! - Full `\d` family with greedy longest-match prefix parsing.
//! - `+` (extra detail) and `S` (include system objects) modifiers.
//! - Optional pattern argument extracted after the modifiers.
//! - `echo_hidden` flag threads through from [`crate::repl::ReplSettings`].

use crate::repl::ExpandedMode;

// ---------------------------------------------------------------------------
// MetaCmd enum
// ---------------------------------------------------------------------------

/// Recognised backslash meta-command types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetaCmd {
    // -- Existing commands --------------------------------------------------
    /// `\q` — quit the REPL.
    Quit,
    /// `\?` — display backslash command help.
    Help,
    /// `\conninfo` — show current connection details.
    ConnInfo,
    /// `\timing [on|off]` — toggle/set query timing output.
    Timing(Option<bool>),
    /// `\x [on|off|auto]` — toggle/set expanded display mode.
    Expanded(ExpandedMode),

    // -- Describe family (stubs; handlers will be added in #27) ------------
    /// `\d [pattern]` — describe object or list all relations.
    DescribeObject,
    /// `\dt [pattern]` — list tables.
    ListTables,
    /// `\di [pattern]` — list indexes.
    ListIndexes,
    /// `\ds [pattern]` — list sequences.
    ListSequences,
    /// `\dv [pattern]` — list views.
    ListViews,
    /// `\dm [pattern]` — list materialised views.
    ListMatViews,
    /// `\df [pattern]` — list functions.
    ListFunctions,
    /// `\dn [pattern]` — list schemas.
    ListSchemas,
    /// `\du [pattern]` / `\dg [pattern]` — list roles.
    ListRoles,
    /// `\dp [pattern]` — list access privileges.
    ListPrivileges,
    /// `\db [pattern]` — list tablespaces.
    ListTablespaces,
    /// `\dT [pattern]` — list data types.
    ListTypes,
    /// `\dx [pattern]` — list installed extensions.
    ListExtensions,
    /// `\l [pattern]` — list databases.
    ListDatabases,
    /// `\dE [pattern]` — list foreign tables.
    ListForeignTables,
    /// `\dD [pattern]` — list domains.
    ListDomains,
    /// `\dc [pattern]` — list conversions.
    ListConversions,
    /// `\dC [pattern]` — list casts.
    ListCasts,
    /// `\dd [pattern]` — list object comments.
    ListComments,
    /// `\des [pattern]` — list foreign servers.
    ListForeignServers,
    /// `\dew [pattern]` — list foreign-data wrappers.
    ListFdws,
    /// `\det [pattern]` — list foreign tables via FDW.
    ListForeignTablesViaFdw,
    /// `\deu [pattern]` — list user mappings.
    ListUserMappings,

    // -- Session commands (stubs; handlers will be added in #28) -----------
    /// `\sf [funcname]` — show function source.
    ShowFunctionSource,
    /// `\sv [viewname]` — show view definition.
    ShowViewDef,
    /// `\c [db [user [host [port]]]]` — reconnect.
    Reconnect,
    /// `\h [command]` — SQL syntax help.
    SqlHelp,

    // -- Fallback ----------------------------------------------------------
    /// Unrecognised command; carries the original command token.
    Unknown(String),
}

impl MetaCmd {
    /// Return a short human-readable label for stub commands.
    ///
    /// Used when printing "not yet implemented" messages.
    pub fn label(&self) -> &'static str {
        match self {
            Self::DescribeObject => "\\d",
            Self::ListTables => "\\dt",
            Self::ListIndexes => "\\di",
            Self::ListSequences => "\\ds",
            Self::ListViews => "\\dv",
            Self::ListMatViews => "\\dm",
            Self::ListFunctions => "\\df",
            Self::ListSchemas => "\\dn",
            Self::ListRoles => "\\du / \\dg",
            Self::ListPrivileges => "\\dp",
            Self::ListTablespaces => "\\db",
            Self::ListTypes => "\\dT",
            Self::ListExtensions => "\\dx",
            Self::ListDatabases => "\\l",
            Self::ListForeignTables => "\\dE",
            Self::ListDomains => "\\dD",
            Self::ListConversions => "\\dc",
            Self::ListCasts => "\\dC",
            Self::ListComments => "\\dd",
            Self::ListForeignServers => "\\des",
            Self::ListFdws => "\\dew",
            Self::ListForeignTablesViaFdw => "\\det",
            Self::ListUserMappings => "\\deu",
            Self::ShowFunctionSource => "\\sf",
            Self::ShowViewDef => "\\sv",
            Self::Reconnect => "\\c",
            Self::SqlHelp => "\\h",
            // Non-stub commands should never reach this.
            _ => "\\?",
        }
    }
}

// ---------------------------------------------------------------------------
// ParsedMeta
// ---------------------------------------------------------------------------

/// A fully parsed backslash meta-command.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedMeta {
    /// The recognised command type.
    pub cmd: MetaCmd,
    /// `+` modifier — show extra detail.
    pub plus: bool,
    /// `S` modifier — include system objects.
    pub system: bool,
    /// Optional pattern / argument following the command and modifiers.
    pub pattern: Option<String>,
    /// Whether internally-generated SQL should be echoed to stdout.
    ///
    /// Set by the caller from [`crate::repl::ReplSettings::echo_hidden`] at
    /// dispatch time; the parser always initialises this to `false`.
    pub echo_hidden: bool,
}

impl ParsedMeta {
    /// Construct a simple (no-modifier, no-pattern) result.
    fn simple(cmd: MetaCmd) -> Self {
        Self {
            cmd,
            plus: false,
            system: false,
            pattern: None,
            echo_hidden: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a backslash command string into a [`ParsedMeta`].
///
/// `input` may or may not include the leading `\`.  Surrounding whitespace is
/// trimmed before parsing.
pub fn parse(input: &str) -> ParsedMeta {
    let input = input.trim().trim_start_matches('\\');

    if input.is_empty() {
        return ParsedMeta::simple(MetaCmd::Unknown(String::new()));
    }

    // Dispatch on the first character.
    match input.chars().next() {
        Some('q') => {
            // Accept both `\q` and `\quit` (psql supports both).
            if let Some(rest) = input.strip_prefix("quit") {
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    return ParsedMeta::simple(MetaCmd::Quit);
                }
            }
            parse_simple_or_unknown(input, "q", MetaCmd::Quit)
        }
        Some('?') => parse_simple_or_unknown(input, "?", MetaCmd::Help),
        Some('c') => parse_c_family(input),
        Some('h') => parse_h(input),
        Some('x') => parse_x(input),
        Some('l') => parse_l(input),
        Some('d') => parse_d_family(input),
        Some('s') => parse_sf_sv(input),
        Some('t') => parse_timing(input),
        _ => ParsedMeta::simple(MetaCmd::Unknown(input.to_owned())),
    }
}

// ---------------------------------------------------------------------------
// Command-specific parsers
// ---------------------------------------------------------------------------

/// Parse commands that must match a fixed token exactly (e.g. `\q`, `\?`).
fn parse_simple_or_unknown(input: &str, token: &str, cmd: MetaCmd) -> ParsedMeta {
    // `input` has had the leading `\` stripped already.
    // Accept `token` optionally followed by whitespace (any trailing arg is
    // ignored for these commands, matching psql behaviour).
    let rest = input.strip_prefix(token).unwrap_or("");
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        ParsedMeta::simple(cmd)
    } else {
        ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
    }
}

/// Parse `\timing [on|off]`.
fn parse_timing(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix("timing") else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let arg = rest.trim();
    let mode = match arg.to_lowercase().as_str() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    ParsedMeta::simple(MetaCmd::Timing(mode))
}

/// Parse `\x [on|off|auto]`.
fn parse_x(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('x') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let arg = rest.trim();
    let mode = match arg.to_lowercase().as_str() {
        "on" => ExpandedMode::On,
        "off" => ExpandedMode::Off,
        "auto" => ExpandedMode::Auto,
        _ => ExpandedMode::Toggle,
    };
    ParsedMeta::simple(MetaCmd::Expanded(mode))
}

/// Parse `\conninfo`, `\c`, or unknown `\c…`.
fn parse_c_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("conninfo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ConnInfo);
        }
    }
    // `\c [db [user [host [port]]]]` — treat the rest as a raw argument.
    // For now just mark as Reconnect stub.
    if let Some(rest) = input.strip_prefix('c') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let pattern = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Reconnect,
                plus: false,
                system: false,
                pattern: if pattern.is_empty() {
                    None
                } else {
                    Some(pattern.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\h [topic]` — SQL syntax help.
///
/// The entire remainder of the line (after `h` and leading whitespace) is
/// treated as the topic argument, so `\h SELECT` passes `"SELECT"` and plain
/// `\h` passes `None`.
fn parse_h(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('h') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let pattern_str = rest.trim();
    ParsedMeta {
        cmd: MetaCmd::SqlHelp,
        plus: false,
        system: false,
        pattern: if pattern_str.is_empty() {
            None
        } else {
            Some(pattern_str.to_owned())
        },
        echo_hidden: false,
    }
}

/// Parse `\sf` and `\sv`.
fn parse_sf_sv(input: &str) -> ParsedMeta {
    // `\sv` must be checked before `\sf` to avoid a prefix match on `sv`.
    if let Some(rest) = input.strip_prefix("sv") {
        // Accept `+` modifier followed by optional pattern.
        let (plus, _system, pattern) = parse_modifiers_and_pattern(rest);
        return ParsedMeta {
            cmd: MetaCmd::ShowViewDef,
            plus,
            system: false,
            pattern,
            echo_hidden: false,
        };
    }
    if let Some(rest) = input.strip_prefix("sf") {
        let (plus, _system, pattern) = parse_modifiers_and_pattern(rest);
        return ParsedMeta {
            cmd: MetaCmd::ShowFunctionSource,
            plus,
            system: false,
            pattern,
            echo_hidden: false,
        };
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\l [pattern]` — list databases.
fn parse_l(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('l') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    // Use the shared modifier parser so `\lS`, `\l+S`, `\lS+` all work.
    let (plus, system, pattern) = parse_modifiers_and_pattern(rest);
    ParsedMeta {
        cmd: MetaCmd::ListDatabases,
        plus,
        system,
        pattern,
        echo_hidden: false,
    }
}

// ---------------------------------------------------------------------------
// \d family parser
// ---------------------------------------------------------------------------

/// Ordered table of multi-character `\d` sub-commands.
///
/// Entries are tried in order — put longer prefixes first so that `\des` is
/// matched before `\d` alone.
static D_SUBCMDS: &[(&str, MetaCmd)] = &[
    // 3-character sub-commands (must come before 2-char variants)
    ("des", MetaCmd::ListForeignServers),
    ("dew", MetaCmd::ListFdws),
    ("det", MetaCmd::ListForeignTablesViaFdw),
    ("deu", MetaCmd::ListUserMappings),
    // 2-character sub-commands — case-sensitive where needed
    ("dT", MetaCmd::ListTypes),
    ("dE", MetaCmd::ListForeignTables),
    ("dD", MetaCmd::ListDomains),
    ("dC", MetaCmd::ListCasts),
    ("dt", MetaCmd::ListTables),
    ("di", MetaCmd::ListIndexes),
    ("ds", MetaCmd::ListSequences),
    ("dv", MetaCmd::ListViews),
    ("dm", MetaCmd::ListMatViews),
    ("df", MetaCmd::ListFunctions),
    ("dn", MetaCmd::ListSchemas),
    ("du", MetaCmd::ListRoles),
    ("dg", MetaCmd::ListRoles),
    ("dp", MetaCmd::ListPrivileges),
    ("db", MetaCmd::ListTablespaces),
    ("dx", MetaCmd::ListExtensions),
    ("dd", MetaCmd::ListComments),
    ("dc", MetaCmd::ListConversions),
];

/// Parse the `\d` family of commands.
///
/// Algorithm:
/// 1. Try all multi-character prefixes (longest first).
/// 2. If none match, fall back to bare `\d`.
/// 3. Parse modifier characters (`+`, `S`) from the remainder.
/// 4. Remainder after whitespace is the pattern.
fn parse_d_family(input: &str) -> ParsedMeta {
    // `input` has already had the leading `\` stripped.

    // Try each sub-command prefix (they all include the leading `d`).
    // `D_SUBCMDS` is ordered longest-first so greedy matching is correct.
    for (prefix, cmd) in D_SUBCMDS {
        if let Some(rest) = input.strip_prefix(prefix) {
            // `rest` is whatever follows the sub-command token, e.g. `+S users`.
            let (plus, system, pattern) = parse_modifiers_and_pattern(rest);
            return ParsedMeta {
                cmd: cmd.clone(),
                plus,
                system,
                pattern,
                echo_hidden: false,
            };
        }
    }

    // Bare `\d [pattern]`.
    let rest = &input[1..]; // skip the 'd'
    let (plus, system, pattern) = parse_modifiers_and_pattern(rest);
    ParsedMeta {
        cmd: MetaCmd::DescribeObject,
        plus,
        system,
        pattern,
        echo_hidden: false,
    }
}

/// Parse optional `+` and `S` modifier characters from the beginning of
/// `rest`, then extract any trailing pattern argument.
///
/// `rest` is the string after the sub-command prefix (e.g. after `dt`).
/// Modifiers must appear before any whitespace.
///
/// Supports all orderings: `+S`, `S+`, `+`, `S`, or none.
///
/// Returns `(plus, system, pattern)`.
fn parse_modifiers_and_pattern(rest: &str) -> (bool, bool, Option<String>) {
    let mut plus = false;
    let mut system = false;

    // Walk chars until we hit whitespace or a non-modifier character.
    let mut end = 0;
    for ch in rest.chars() {
        if ch == '+' {
            plus = true;
            end += ch.len_utf8();
        } else if ch == 'S' {
            system = true;
            end += ch.len_utf8();
        } else {
            break;
        }
    }

    let after_modifiers = &rest[end..];
    let pattern_str = after_modifiers.trim();
    let pattern = if pattern_str.is_empty() {
        None
    } else {
        Some(pattern_str.to_owned())
    };

    (plus, system, pattern)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::ExpandedMode;

    // Helper: parse and return (cmd, plus, system, pattern).
    fn p(input: &str) -> (MetaCmd, bool, bool, Option<String>) {
        let m = parse(input);
        (m.cmd, m.plus, m.system, m.pattern)
    }

    // -- Existing commands ---------------------------------------------------

    #[test]
    fn parse_quit() {
        assert_eq!(parse("\\q").cmd, MetaCmd::Quit);
        assert!(!parse("\\q").plus);
        assert!(!parse("\\q").system);
        assert_eq!(parse("\\q").pattern, None);
    }

    #[test]
    fn parse_quit_long_form() {
        // `\quit` must be accepted as an alias for `\q`.
        assert_eq!(parse("\\quit").cmd, MetaCmd::Quit);
    }

    #[test]
    fn parse_help() {
        assert_eq!(parse("\\?").cmd, MetaCmd::Help);
    }

    #[test]
    fn parse_conninfo() {
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
    }

    #[test]
    fn parse_timing_on() {
        assert_eq!(parse("\\timing on").cmd, MetaCmd::Timing(Some(true)));
    }

    #[test]
    fn parse_timing_off() {
        assert_eq!(parse("\\timing off").cmd, MetaCmd::Timing(Some(false)));
    }

    #[test]
    fn parse_timing_toggle() {
        assert_eq!(parse("\\timing").cmd, MetaCmd::Timing(None));
    }

    #[test]
    fn parse_expanded_on() {
        assert_eq!(parse("\\x on").cmd, MetaCmd::Expanded(ExpandedMode::On));
    }

    #[test]
    fn parse_expanded_auto() {
        assert_eq!(parse("\\x auto").cmd, MetaCmd::Expanded(ExpandedMode::Auto));
    }

    #[test]
    fn parse_expanded_toggle() {
        assert_eq!(parse("\\x").cmd, MetaCmd::Expanded(ExpandedMode::Toggle));
    }

    // -- Unknown command -----------------------------------------------------

    #[test]
    fn parse_unknown() {
        // Unknown commands store the name WITHOUT a leading backslash.
        // The display layer (dispatch_meta) adds `\` when printing.
        assert_eq!(parse("\\foo").cmd, MetaCmd::Unknown("foo".to_owned()));
    }

    // -- \l ------------------------------------------------------------------

    #[test]
    fn parse_list_databases() {
        let m = parse("\\l");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert!(!m.plus);
        assert!(m.pattern.is_none());
    }

    #[test]
    fn parse_list_databases_plus() {
        let m = parse("\\l+");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert!(m.plus);
    }

    #[test]
    fn parse_list_databases_pattern() {
        let m = parse("\\l mydb");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert_eq!(m.pattern, Some("mydb".to_owned()));
    }

    #[test]
    fn parse_list_databases_system() {
        let m = parse("\\lS");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert!(m.system);
        assert!(!m.plus);
    }

    #[test]
    fn parse_list_databases_plus_system() {
        let m = parse("\\l+S");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert!(m.plus);
        assert!(m.system);
    }

    #[test]
    fn parse_list_databases_system_plus() {
        let m = parse("\\lS+");
        assert_eq!(m.cmd, MetaCmd::ListDatabases);
        assert!(m.plus);
        assert!(m.system);
    }

    // -- \dt -----------------------------------------------------------------

    #[test]
    fn parse_list_tables_bare() {
        let (cmd, plus, system, pat) = p("\\dt");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(!plus);
        assert!(!system);
        assert!(pat.is_none());
    }

    #[test]
    fn parse_list_tables_plus() {
        let (cmd, plus, _, _) = p("\\dt+");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(plus);
    }

    #[test]
    fn parse_list_tables_system() {
        let (cmd, _, system, _) = p("\\dtS");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(system);
    }

    #[test]
    fn parse_list_tables_plus_system() {
        let (cmd, plus, system, _) = p("\\dt+S");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(plus);
        assert!(system);
    }

    #[test]
    fn parse_list_tables_system_plus() {
        let (cmd, plus, system, _) = p("\\dtS+");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(plus);
        assert!(system);
    }

    #[test]
    fn parse_list_tables_with_pattern() {
        let (cmd, _, _, pat) = p("\\dt users");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert_eq!(pat, Some("users".to_owned()));
    }

    #[test]
    fn parse_list_tables_plus_with_pattern() {
        let (cmd, plus, _, pat) = p("\\dt+ public.*");
        assert_eq!(cmd, MetaCmd::ListTables);
        assert!(plus);
        assert_eq!(pat, Some("public.*".to_owned()));
    }

    // -- \d ------------------------------------------------------------------

    #[test]
    fn parse_describe_bare() {
        let (cmd, _, _, pat) = p("\\d");
        assert_eq!(cmd, MetaCmd::DescribeObject);
        assert!(pat.is_none());
    }

    #[test]
    fn parse_describe_with_pattern() {
        let (cmd, _, _, pat) = p("\\d users");
        assert_eq!(cmd, MetaCmd::DescribeObject);
        assert_eq!(pat, Some("users".to_owned()));
    }

    // -- Greedy multi-char sub-commands --------------------------------------

    #[test]
    fn parse_des_not_confused_with_d() {
        assert_eq!(parse("\\des").cmd, MetaCmd::ListForeignServers);
    }

    #[test]
    fn parse_dew_foreign_data_wrappers() {
        assert_eq!(parse("\\dew").cmd, MetaCmd::ListFdws);
    }

    #[test]
    fn parse_det_foreign_tables_via_fdw() {
        assert_eq!(parse("\\det").cmd, MetaCmd::ListForeignTablesViaFdw);
    }

    #[test]
    fn parse_deu_user_mappings() {
        assert_eq!(parse("\\deu").cmd, MetaCmd::ListUserMappings);
    }

    #[test]
    fn parse_dt_uppercase_types() {
        assert_eq!(parse("\\dT").cmd, MetaCmd::ListTypes);
    }

    #[test]
    fn parse_de_uppercase_foreign_tables() {
        assert_eq!(parse("\\dE").cmd, MetaCmd::ListForeignTables);
    }

    #[test]
    fn parse_dd_uppercase_domains() {
        assert_eq!(parse("\\dD").cmd, MetaCmd::ListDomains);
    }

    #[test]
    fn parse_dc_uppercase_casts() {
        assert_eq!(parse("\\dC").cmd, MetaCmd::ListCasts);
    }

    #[test]
    fn parse_dd_lowercase_comments() {
        assert_eq!(parse("\\dd").cmd, MetaCmd::ListComments);
    }

    #[test]
    fn parse_dc_lowercase_conversions() {
        assert_eq!(parse("\\dc").cmd, MetaCmd::ListConversions);
    }

    #[test]
    fn parse_di_indexes() {
        assert_eq!(parse("\\di").cmd, MetaCmd::ListIndexes);
    }

    #[test]
    fn parse_ds_sequences() {
        assert_eq!(parse("\\ds").cmd, MetaCmd::ListSequences);
    }

    #[test]
    fn parse_dv_views() {
        assert_eq!(parse("\\dv").cmd, MetaCmd::ListViews);
    }

    #[test]
    fn parse_dm_mat_views() {
        assert_eq!(parse("\\dm").cmd, MetaCmd::ListMatViews);
    }

    #[test]
    fn parse_df_functions() {
        assert_eq!(parse("\\df").cmd, MetaCmd::ListFunctions);
    }

    #[test]
    fn parse_dn_schemas() {
        assert_eq!(parse("\\dn").cmd, MetaCmd::ListSchemas);
    }

    #[test]
    fn parse_du_roles() {
        assert_eq!(parse("\\du").cmd, MetaCmd::ListRoles);
    }

    #[test]
    fn parse_dg_roles() {
        assert_eq!(parse("\\dg").cmd, MetaCmd::ListRoles);
    }

    #[test]
    fn parse_dp_privileges() {
        assert_eq!(parse("\\dp").cmd, MetaCmd::ListPrivileges);
    }

    #[test]
    fn parse_db_tablespaces() {
        assert_eq!(parse("\\db").cmd, MetaCmd::ListTablespaces);
    }

    #[test]
    fn parse_dx_extensions() {
        assert_eq!(parse("\\dx").cmd, MetaCmd::ListExtensions);
    }

    // -- \sf / \sv -----------------------------------------------------------

    #[test]
    fn parse_show_function_source() {
        let m = parse("\\sf my_func");
        assert_eq!(m.cmd, MetaCmd::ShowFunctionSource);
        assert_eq!(m.pattern, Some("my_func".to_owned()));
    }

    #[test]
    fn parse_show_function_source_plus() {
        // `\sf+ my_func` — plus modifier must be recognised.
        let m = parse("\\sf+ my_func");
        assert_eq!(m.cmd, MetaCmd::ShowFunctionSource);
        assert!(m.plus, "expected plus=true for \\sf+");
        assert_eq!(m.pattern, Some("my_func".to_owned()));
    }

    #[test]
    fn parse_show_function_source_plus_no_pattern() {
        // `\sf+` with no pattern is valid (returns None pattern).
        let m = parse("\\sf+");
        assert_eq!(m.cmd, MetaCmd::ShowFunctionSource);
        assert!(m.plus);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_show_view_def() {
        let m = parse("\\sv my_view");
        assert_eq!(m.cmd, MetaCmd::ShowViewDef);
        assert_eq!(m.pattern, Some("my_view".to_owned()));
    }

    #[test]
    fn parse_show_view_def_plus() {
        // `\sv+ my_view` — plus modifier must be recognised.
        let m = parse("\\sv+ my_view");
        assert_eq!(m.cmd, MetaCmd::ShowViewDef);
        assert!(m.plus, "expected plus=true for \\sv+");
        assert_eq!(m.pattern, Some("my_view".to_owned()));
    }

    #[test]
    fn parse_show_view_def_plus_no_pattern() {
        let m = parse("\\sv+");
        assert_eq!(m.cmd, MetaCmd::ShowViewDef);
        assert!(m.plus);
        assert_eq!(m.pattern, None);
    }

    // -- \c ------------------------------------------------------------------

    #[test]
    fn parse_reconnect_bare() {
        assert_eq!(parse("\\c").cmd, MetaCmd::Reconnect);
    }

    #[test]
    fn parse_reconnect_with_db() {
        let m = parse("\\c mydb");
        assert_eq!(m.cmd, MetaCmd::Reconnect);
        assert_eq!(m.pattern, Some("mydb".to_owned()));
    }

    // -- \h ------------------------------------------------------------------

    #[test]
    fn parse_sql_help() {
        let m = parse("\\h");
        assert_eq!(m.cmd, MetaCmd::SqlHelp);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_sql_help_with_topic() {
        // `\h SELECT` must capture "SELECT" as the pattern so the right
        // synopsis is shown instead of the full topic list.
        let m = parse("\\h SELECT");
        assert_eq!(m.cmd, MetaCmd::SqlHelp);
        assert_eq!(m.pattern, Some("SELECT".to_owned()));
    }

    #[test]
    fn parse_sql_help_multi_word_topic() {
        let m = parse("\\h CREATE TABLE");
        assert_eq!(m.cmd, MetaCmd::SqlHelp);
        assert_eq!(m.pattern, Some("CREATE TABLE".to_owned()));
    }

    // -- echo_hidden default -------------------------------------------------

    #[test]
    fn echo_hidden_defaults_to_false() {
        assert!(!parse("\\dt").echo_hidden);
    }

    // -- No leading backslash -----------------------------------------------

    #[test]
    fn parse_without_leading_backslash() {
        assert_eq!(parse("q").cmd, MetaCmd::Quit);
    }
}
