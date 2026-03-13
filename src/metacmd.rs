//! Backslash (meta) command parser for Samo.
//!
//! Provides a richer parser than the original [`crate::repl`] implementation.
//! Key features:
//!
//! - Full `\d` family with greedy longest-match prefix parsing.
//! - `+` (extra detail) and `S` (include system objects) modifiers.
//! - Optional pattern argument extracted after the modifiers.
//! - `echo_hidden` flag threads through from [`crate::repl::ReplSettings`].

use crate::output::ExpandedMode;

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

    // -- Variable commands (issue #32) ------------------------------------
    /// `\set [name [value]]` — set or display variables.
    ///
    /// Payload: `(name, value)` when both are given; `(name, "")` when only
    /// the name is given (displays the variable); `("", "")` when bare (lists
    /// all variables).
    Set(String, String),
    /// `\unset name` — unset a variable.
    Unset(String),
    /// `\pset [option [value]]` — set print option.
    Pset(String, Option<String>),
    /// `\a` — toggle aligned/unaligned output format.
    ToggleAlign,
    /// `\t [on|off]` — toggle or set tuples-only mode.
    TuplesOnly(Option<bool>),
    /// `\f [sep]` — set field separator for unaligned output.
    FieldSep(Option<String>),
    /// `\H` — toggle HTML output mode.
    ToggleHtml,
    /// `\C [title]` — set or clear table title.
    SetTitle(Option<String>),

    // -- I/O and utility commands (#33) ------------------------------------
    /// `\i file` — include (execute) commands from a file.
    Include,
    /// `\ir file` — include file relative to the current script's directory.
    IncludeRelative,
    /// `\o [file]` — send query output to a file (or restore stdout if no arg).
    Output,
    /// `\w file` — write the current query buffer to a file.
    WriteBuffer,
    /// `\r` — reset (clear) the query buffer.
    ResetBuffer,
    /// `\p` — print the current query buffer.
    PrintBuffer,
    /// `\e [file [line]]` — edit the buffer (or a file) with $EDITOR.
    Edit,
    /// `\! [command]` — execute a shell command.
    Shell,
    /// `\cd [dir]` — change the current working directory.
    Chdir,
    /// `\echo [text]` — print text to stdout (or the current output target).
    Echo,
    /// `\qecho [text]` — like \echo but writes to the query output target.
    QEcho,
    /// `\warn [text]` — print text to stderr.
    Warn,
    /// `\encoding [enc]` — show or set client encoding.
    Encoding,
    /// `\password [user]` — prompt for a new password for a user.
    Password,

    // -- Conditional execution (#37) --------------------------------------
    /// `\if expression` — begin a conditional block.
    ///
    /// The expression is stored in the `pattern` field after parsing.
    If,
    /// `\elif expression` — alternate branch of a conditional block.
    ///
    /// The expression is stored in the `pattern` field after parsing.
    Elif,
    /// `\else` — unconditional alternate branch of a conditional block.
    Else,
    /// `\endif` — end a conditional block.
    Endif,

    // -- Execution variants (#46) ------------------------------------------
    /// `\g [file||command]` — execute buffer, optionally redirect output.
    ///
    /// - `\g` → execute to stdout (same as `;`)
    /// - `\g filename` → execute, write output to file
    /// - `\g |command` → execute, pipe output through shell command
    ///
    /// `pattern` holds the file path or pipe target (with leading `|`), or
    /// `None` for plain stdout execution.
    GoExecute(Option<String>),
    /// `\gx [file]` — execute buffer with expanded output for this query only.
    ///
    /// - `\gx` → expanded output to stdout
    /// - `\gx filename` → expanded output to file
    ///
    /// `pattern` holds the optional file path.
    GoExecuteExpanded(Option<String>),
    /// `\gexec` — execute the buffer, then execute each result cell as SQL.
    ///
    /// Each non-NULL cell value in the result set is sent to the server as a
    /// separate SQL statement, row by row, column by column.  NULL cells are
    /// silently skipped.  Errors in individual cell statements are printed but
    /// do not stop processing of remaining cells.
    GExec,
    /// `\gset [prefix]` — execute buffer and store each column as a variable.
    ///
    /// The query must return exactly one row.  For each column, a variable
    /// named `{prefix}{column_name}` is set to the cell value (empty string
    /// for NULL).  `prefix` defaults to the empty string when omitted.
    GSet(Option<String>),

    // -- Watch (#47) -------------------------------------------------------
    /// `\watch [interval]` — re-execute the last query every N seconds.
    ///
    /// `pattern` carries the raw interval string (e.g. `"5"`, `"0.5"`,
    /// `"5s"`).  When `pattern` is `None` the default 2-second interval is
    /// used.
    Watch,

    // -- Buffer introspection (#52) ----------------------------------------
    /// `\gdesc` — describe the result columns of the current query buffer
    /// without executing it.
    ///
    /// Uses the extended-protocol `Describe` message so no rows are produced
    /// and no side-effects occur on the server.
    GDesc,

    // -- Copy (#copy) ------------------------------------------------------
    /// `\copy args` — client-side COPY between local file and table.
    ///
    /// The raw argument string (everything after `\copy `) is captured here
    /// and passed to [`crate::copy::parse_copy_args`] at dispatch time.
    Copy(String),
    // -- Extended query protocol (#57) ------------------------------------
    /// `\bind [param...]` — set positional parameters for the next query.
    ///
    /// The listed parameter values are stored and used when the next SQL
    /// statement is executed.  Execution uses the extended query protocol
    /// (`client.query`) so that server-side type inference applies.  The
    /// parameters are consumed after one use.
    ///
    /// Example: `select $1::int + $2::int \bind 3 4 \g` → `7`
    Bind(Vec<String>),
    /// `\parse stmt_name` — prepare the current query buffer as a named
    /// server-side prepared statement.
    ///
    /// Sends a `Parse` message via `client.prepare(sql)` and stores the
    /// resulting [`tokio_postgres::Statement`] under `stmt_name`.
    Parse(String),
    /// `\bind_named stmt_name [param...]` — execute a named prepared
    /// statement with the supplied positional parameters.
    ///
    /// Retrieves the previously stored [`tokio_postgres::Statement`] and
    /// calls `client.query(&stmt, &params)`.
    BindNamed(String, Vec<String>),
    /// `\close_prepared stmt_name` — deallocate a named prepared statement.
    ///
    /// Sends `DEALLOCATE stmt_name` and removes it from the local map.
    ClosePrepared(String),

    // -- Cross-tabulation (#54) --------------------------------------------
    /// `\crosstabview [colV [colH [colD [sortcolH]]]]` — execute the buffer
    /// and pivot the result into a cross-tabulation table.
    ///
    /// `args` carries the raw argument string (may be empty).
    CrosstabView(String),

    // -- Info commands ------------------------------------------------------
    /// `\copyright` — show `PostgreSQL` copyright and distribution terms.
    Copyright,

    // -- Diagnostic commands (#66) -----------------------------------------
    /// `\dba [subcommand]` — run a database diagnostic sub-command.
    ///
    /// The subcommand name (e.g. `"activity"`, `"locks"`) is stored in the
    /// `pattern` field.  The `plus` flag activates verbose output.
    Dba,

    // -- Named queries (#69) -----------------------------------------------
    /// `\ns name query` — save a named query.
    NamedSave(String, String),
    /// `\n name [args...]` — execute a named query.
    NamedExec(String, Vec<String>),
    /// `\n+` — list all named queries.
    NamedList,
    /// `\nd name` — delete a named query.
    NamedDelete(String),

    // -- Input mode --------------------------------------------------------
    /// `\sql` — switch to SQL input mode.
    SqlMode,
    /// `\text2sql` / `\t2s` — switch to text-to-SQL input mode.
    Text2SqlMode,
    /// `\mode` — show current mode summary.
    ShowMode,

    // -- Execution mode ----------------------------------------------------
    /// `\plan` — enter plan mode.
    PlanMode,
    /// `\yolo` — enter YOLO mode.
    YoloMode,
    /// `\observe` — enter observe mode (optional duration argument).
    ObserveMode,
    /// `\interactive` — return to interactive mode.
    InteractiveMode,

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
            Self::Include => "\\i",
            Self::IncludeRelative => "\\ir",
            Self::Output => "\\o",
            Self::WriteBuffer => "\\w",
            Self::ResetBuffer => "\\r",
            Self::PrintBuffer => "\\p",
            Self::Edit => "\\e",
            Self::Shell => "\\!",
            Self::Chdir => "\\cd",
            Self::Echo => "\\echo",
            Self::QEcho => "\\qecho",
            Self::Warn => "\\warn",
            Self::Encoding => "\\encoding",
            Self::Password => "\\password",
            Self::Copy(_) => "\\copy",
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
            // `\qecho [text]` — echo to output target
            if let Some(rest) = input.strip_prefix("qecho") {
                if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                    let text = rest.trim();
                    return ParsedMeta {
                        cmd: MetaCmd::QEcho,
                        plus: false,
                        system: false,
                        pattern: if text.is_empty() {
                            None
                        } else {
                            Some(text.to_owned())
                        },
                        echo_hidden: false,
                    };
                }
            }
            parse_simple_or_unknown(input, "q", MetaCmd::Quit)
        }
        Some('?') => parse_simple_or_unknown(input, "?", MetaCmd::Help),
        Some('a') => parse_simple_or_unknown(input, "a", MetaCmd::ToggleAlign),
        Some('c') => parse_c_family(input),
        Some('C') => parse_set_title(input),
        Some('e') => parse_e_family(input),
        Some('f') => parse_field_sep(input),
        Some('h') => parse_h(input),
        Some('H') => parse_simple_or_unknown(input, "H", MetaCmd::ToggleHtml),
        Some('i') => parse_i_family(input),
        Some('o') => parse_o(input),
        Some('p') => parse_p_family(input),
        Some('r') => parse_r_family(input),
        Some('s') => parse_s_family(input),
        Some('t') => parse_t_family(input),
        Some('u') => parse_unset(input),
        Some('w') => parse_w(input),
        Some('x') => parse_x(input),
        Some('b') => parse_b_family(input),
        Some('g') => parse_g_family(input),
        Some('l') => parse_l(input),
        Some('d') => parse_d_family(input),
        Some('n') => parse_n_family(input),
        Some('m') => parse_m_family(input),
        Some('y') => parse_y_family(input),
        Some('!') => parse_shell(input),
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

/// Dispatch `s`-family commands: `\set`, `\sf`, `\sv`.
fn parse_s_family(input: &str) -> ParsedMeta {
    // \sql — switch to SQL mode
    if let Some(rest) = input.strip_prefix("sql") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::SqlMode);
        }
    }
    if let Some(after) = input.strip_prefix("set") {
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            return parse_set(input);
        }
    }
    parse_sf_sv(input)
}

/// Parse `\set [name [value]]`.
fn parse_set(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix("set") else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    // Bare `\set` — list all variables.
    let rest = rest.trim();
    if rest.is_empty() {
        return ParsedMeta::simple(MetaCmd::Set(String::new(), String::new()));
    }
    // `\set name` or `\set name value`
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_owned();
    let value = parts.next().map_or("", str::trim).to_owned();
    ParsedMeta::simple(MetaCmd::Set(name, value))
}

/// Parse `\unset name`.
fn parse_unset(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix("unset") else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let name = rest.trim().to_owned();
    if name.is_empty() {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    }
    ParsedMeta::simple(MetaCmd::Unset(name))
}

/// Parse `\pset [option [value]]`.
fn parse_pset(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix("pset") else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return ParsedMeta::simple(MetaCmd::Pset(String::new(), None));
    }
    let mut parts = rest.splitn(2, char::is_whitespace);
    let option = parts.next().unwrap_or("").to_owned();
    let value = parts.next().map(|s| s.trim().to_owned());
    ParsedMeta::simple(MetaCmd::Pset(option, value))
}

/// Parse `\t [on|off]` — tuples-only toggle.
///
/// This function is called only when the `t` arm is reached; it must
/// distinguish `\t` from `\timing` by checking for the full word.
fn parse_t_family(input: &str) -> ParsedMeta {
    // `\text2sql` / `\t2s` — switch to text-to-SQL mode
    if let Some(rest) = input.strip_prefix("text2sql") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Text2SqlMode);
        }
    }
    if let Some(rest) = input.strip_prefix("t2s") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Text2SqlMode);
        }
    }
    // `\timing …` takes priority over `\t`
    if let Some(rest) = input.strip_prefix("timing") {
        let arg = rest.trim();
        let mode = match arg.to_lowercase().as_str() {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        return ParsedMeta::simple(MetaCmd::Timing(mode));
    }
    // `\t [on|off]`
    if let Some(rest) = input.strip_prefix('t') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            let mode = match arg.to_lowercase().as_str() {
                "on" => Some(true),
                "off" => Some(false),
                _ => None,
            };
            return ParsedMeta::simple(MetaCmd::TuplesOnly(mode));
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\mode` — show current input mode.
fn parse_m_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("mode") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ShowMode);
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\yolo` — enter YOLO execution mode.
fn parse_y_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("yolo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::YoloMode);
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\f [sep]` — field separator.
///
/// `\f` must be followed by nothing, whitespace, or the separator itself.
/// If the character immediately after `f` is a letter (e.g. `\foo`), it is
/// an unknown command, not a field-separator command.
fn parse_field_sep(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('f') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    // Reject if `f` is immediately followed by another letter/digit (e.g. `\foo`).
    if rest.starts_with(|c: char| c.is_alphanumeric()) {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    }
    let sep = rest.trim();
    let value = if sep.is_empty() {
        None
    } else {
        Some(sep.to_owned())
    };
    ParsedMeta::simple(MetaCmd::FieldSep(value))
}

/// Parse `\C [title]` — set table title.
fn parse_set_title(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('C') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let title = rest.trim();
    let value = if title.is_empty() {
        None
    } else {
        Some(title.to_owned())
    };
    ParsedMeta::simple(MetaCmd::SetTitle(value))
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

/// Parse `\conninfo`, `\crosstabview`, `\copy`, `\close_prepared`, `\copyright`, `\cd`, `\c`, or unknown `\c…`.
fn parse_c_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("conninfo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ConnInfo);
        }
    }
    // `\crosstabview [args]` — must be checked before `\copy` and `\c` (longest match).
    if let Some(rest) = input.strip_prefix("crosstabview") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let args = rest.trim().to_owned();
            return ParsedMeta::simple(MetaCmd::CrosstabView(args));
        }
    }
    // `\copyright` — must be checked before `\copy` and `\cd`.
    if let Some(rest) = input.strip_prefix("copyright") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Copyright);
        }
    }
    // `\copy args` — client-side COPY.  Must be checked before bare `\c`.
    if let Some(rest) = input.strip_prefix("copy") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Copy(rest.trim().to_owned()));
        }
    }
    // `\close_prepared stmt_name` — deallocate a named prepared statement.
    if let Some(rest) = input.strip_prefix("close_prepared") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let name = rest.trim().to_owned();
            if name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::ClosePrepared(name));
        }
    }
    // `\cd [dir]` — must be checked before bare `\c`.
    if let Some(rest) = input.strip_prefix("cd") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let dir = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Chdir,
                plus: false,
                system: false,
                pattern: if dir.is_empty() {
                    None
                } else {
                    Some(dir.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    // `\c [db [user [host [port]]]]` — treat the rest as a raw argument.
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
// I/O and utility command parsers (#33)
// ---------------------------------------------------------------------------

/// Parse `\e [file [line]]`, `\echo text`, `\encoding [enc]`, `\elif expr`,
/// `\else`, and `\endif`.
///
/// Longer prefixes are checked first to avoid false prefix matches.
fn parse_e_family(input: &str) -> ParsedMeta {
    // `\endif` — no argument.
    if let Some(rest) = input.strip_prefix("endif") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Endif);
        }
    }

    // `\encoding [enc]` — must come before bare `\e`.
    if let Some(rest) = input.strip_prefix("encoding") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let enc = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Encoding,
                plus: false,
                system: false,
                pattern: if enc.is_empty() {
                    None
                } else {
                    Some(enc.to_owned())
                },
                echo_hidden: false,
            };
        }
    }

    // `\echo [text]` — must come before bare `\e`.
    if let Some(rest) = input.strip_prefix("echo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let text = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Echo,
                plus: false,
                system: false,
                pattern: if text.is_empty() {
                    None
                } else {
                    Some(text.to_owned())
                },
                echo_hidden: false,
            };
        }
    }

    // `\elif <expression>` — expression captured in `pattern`.
    if let Some(rest) = input.strip_prefix("elif") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let expr = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Elif,
                plus: false,
                system: false,
                pattern: if expr.is_empty() {
                    None
                } else {
                    Some(expr.to_owned())
                },
                echo_hidden: false,
            };
        }
    }

    // `\else` — no argument.
    if let Some(rest) = input.strip_prefix("else") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Else);
        }
    }

    // `\e [file [line]]`
    if let Some(rest) = input.strip_prefix('e') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Edit,
                plus: false,
                system: false,
                pattern: if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\if expr`, `\i file`, and `\ir file`.
fn parse_i_family(input: &str) -> ParsedMeta {
    // `\interactive` — return to interactive execution mode.
    if let Some(rest) = input.strip_prefix("interactive") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::InteractiveMode);
        }
    }
    // `\if <expression>` — expression captured in `pattern`.
    if let Some(rest) = input.strip_prefix("if") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let expr = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::If,
                plus: false,
                system: false,
                pattern: if expr.is_empty() {
                    None
                } else {
                    Some(expr.to_owned())
                },
                echo_hidden: false,
            };
        }
    }

    // `\ir` must be checked before `\i` (longer prefix first).
    if let Some(rest) = input.strip_prefix("ir") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let path = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::IncludeRelative,
                plus: false,
                system: false,
                pattern: if path.is_empty() {
                    None
                } else {
                    Some(path.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    if let Some(rest) = input.strip_prefix('i') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let path = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Include,
                plus: false,
                system: false,
                pattern: if path.is_empty() {
                    None
                } else {
                    Some(path.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\o [file]` and `\observe [duration]`.
fn parse_o(input: &str) -> ParsedMeta {
    // `\observe [duration]` — enter observe execution mode.
    if let Some(rest) = input.strip_prefix("observe") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            let pattern = if arg.is_empty() {
                None
            } else {
                Some(arg.to_owned())
            };
            return ParsedMeta {
                cmd: MetaCmd::ObserveMode,
                pattern,
                plus: false,
                system: false,
                echo_hidden: false,
            };
        }
    }
    let Some(rest) = input.strip_prefix('o') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        let path = rest.trim();
        return ParsedMeta {
            cmd: MetaCmd::Output,
            plus: false,
            system: false,
            pattern: if path.is_empty() {
                None
            } else {
                Some(path.to_owned())
            },
            echo_hidden: false,
        };
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\r` (reset buffer), `\qecho text`.
///
/// Note: `\r` conflicts with nothing else starting with `r`, but we handle
/// `\qecho` here as a special prefix-matched command reached from the `q`
/// branch — wait, `\qecho` starts with `q`. This function is for `\r` only.
fn parse_r_family(input: &str) -> ParsedMeta {
    // `\r` — bare reset
    if let Some(rest) = input.strip_prefix('r') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ResetBuffer);
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\w file`, `\warn [text]`, and `\watch [interval]`.
fn parse_w(input: &str) -> ParsedMeta {
    // `\watch [interval]` — must come before `\warn` and `\w` (longest first).
    if let Some(rest) = input.strip_prefix("watch") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Watch,
                plus: false,
                system: false,
                pattern: if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    // `\warn [text]` — must come before bare `\w` (longer prefix wins).
    if let Some(rest) = input.strip_prefix("warn") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let text = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Warn,
                plus: false,
                system: false,
                pattern: if text.is_empty() {
                    None
                } else {
                    Some(text.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    if let Some(rest) = input.strip_prefix('w') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let path = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::WriteBuffer,
                plus: false,
                system: false,
                pattern: if path.is_empty() {
                    None
                } else {
                    Some(path.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\p` (print buffer), `\parse stmt_name`, `\password [user]`, and
/// `\pset [option [value]]`.
fn parse_p_family(input: &str) -> ParsedMeta {
    // `\password` must be checked before bare `\p` (longer prefix wins).
    if let Some(rest) = input.strip_prefix("password") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let user = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::Password,
                plus: false,
                system: false,
                pattern: if user.is_empty() {
                    None
                } else {
                    Some(user.to_owned())
                },
                echo_hidden: false,
            };
        }
    }
    // `\pset` must also be checked before bare `\p`.
    if let Some(rest) = input.strip_prefix("pset") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return parse_pset(input);
        }
    }
    // `\parse stmt_name` — prepare buffer as named prepared statement.
    if let Some(rest) = input.strip_prefix("parse") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let name = rest.trim().to_owned();
            if name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::Parse(name));
        }
    }
    // `\plan` — enter plan execution mode.
    if let Some(rest) = input.strip_prefix("plan") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::PlanMode);
        }
    }
    parse_simple_or_unknown(input, "p", MetaCmd::PrintBuffer)
}

/// Parse `\! [command]`.
fn parse_shell(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('!') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let cmd = rest.trim();
    ParsedMeta {
        cmd: MetaCmd::Shell,
        plus: false,
        system: false,
        pattern: if cmd.is_empty() {
            None
        } else {
            Some(cmd.to_owned())
        },
        echo_hidden: false,
    }
}

// ---------------------------------------------------------------------------
// \b family parser — extended query protocol commands (#57)
// ---------------------------------------------------------------------------

/// Split a whitespace-separated argument string into individual tokens.
///
/// A token may be single-quoted to include spaces: `'hello world'` → one
/// token.  Doubled single-quotes inside a quoted token are an escaped quote.
/// Unquoted tokens are delimited by ASCII whitespace.
fn split_params(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\'' {
            // Quoted token: consume until closing `'` ('' is an escape).
            chars.next(); // consume opening quote
            let mut token = String::new();
            loop {
                match chars.next() {
                    None => break,
                    Some('\'') => {
                        if chars.peek() == Some(&'\'') {
                            // Escaped quote inside quoted string.
                            chars.next();
                            token.push('\'');
                        } else {
                            break;
                        }
                    }
                    Some(ch) => token.push(ch),
                }
            }
            result.push(token);
        } else {
            // Unquoted token: consume until whitespace.
            let mut token = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                token.push(ch);
                chars.next();
            }
            result.push(token);
        }
    }

    result
}

/// Parse `\bind`, `\bind_named`, and `\close_prepared`.
///
/// Disambiguation order (longest match first):
///   `bind_named` → [`MetaCmd::BindNamed`]
///   `bind`       → [`MetaCmd::Bind`]
///   `close_prepared` (starts with `c`, not `b`) — handled in `parse_c_family`
///
/// Any unrecognised `b`-prefixed command falls through to [`MetaCmd::Unknown`].
fn parse_b_family(input: &str) -> ParsedMeta {
    // `\bind_named stmt_name [params...]` — checked before `\bind`.
    if let Some(rest) = input.strip_prefix("bind_named") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let rest = rest.trim();
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_owned();
            let params_str = parts.next().unwrap_or("").trim();
            let params = split_params(params_str);
            if name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::BindNamed(name, params));
        }
    }

    // `\bind [params...]`
    if let Some(rest) = input.strip_prefix("bind") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let params = split_params(rest.trim());
            return ParsedMeta::simple(MetaCmd::Bind(params));
        }
    }

    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

// ---------------------------------------------------------------------------
// \n family parser — named queries (#69)
// ---------------------------------------------------------------------------

/// Parse `\ns name query`, `\nd name`, `\n+`, and `\n name [args...]`.
///
/// Disambiguation order (longest match first):
///   `ns` → [`MetaCmd::NamedSave`]
///   `nd` → [`MetaCmd::NamedDelete`]
///   `n+` → [`MetaCmd::NamedList`]
///   `n`  → [`MetaCmd::NamedExec`]
fn parse_n_family(input: &str) -> ParsedMeta {
    // `\ns name query` — save a named query.  Must come before bare `\n`.
    if let Some(rest) = input.strip_prefix("ns") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let rest = rest.trim();
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_owned();
            let query = parts.next().map_or("", str::trim).to_owned();
            if name.is_empty() || query.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::NamedSave(name, query));
        }
    }

    // `\nd name` — delete a named query.  Must come before bare `\n`.
    if let Some(rest) = input.strip_prefix("nd") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let name = rest.trim().to_owned();
            if name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::NamedDelete(name));
        }
    }

    // `\n+` — list all named queries.
    if let Some(rest) = input.strip_prefix("n+") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::NamedList);
        }
    }

    // `\n name [args...]` — execute a named query.
    if let Some(rest) = input.strip_prefix('n') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let rest = rest.trim();
            if rest.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_owned();
            let args_str = parts.next().unwrap_or("").trim();
            let args = split_params(args_str);
            return ParsedMeta::simple(MetaCmd::NamedExec(name, args));
        }
    }

    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

// ---------------------------------------------------------------------------
// \g / \gx parser (#46)
// ---------------------------------------------------------------------------

/// Parse `\g [file||cmd]`, `\gx [file]`, `\gdesc`, `\gexec`, and `\gset [prefix]`.
///
/// Disambiguation order (longest match first):
///   `gexec` → [`MetaCmd::GExec`]
///   `gset`  → [`MetaCmd::GSet`]
///   `gdesc` → [`MetaCmd::GDesc`]
///   `gx`    → [`MetaCmd::GoExecuteExpanded`]
///   `g`     → [`MetaCmd::GoExecute`]
///
/// Any unrecognised `g`-prefixed command falls through to [`MetaCmd::Unknown`].
fn parse_g_family(input: &str) -> ParsedMeta {
    // `\gexec` — execute buffer, then execute each result cell as SQL.
    // Checked before the generic long-prefix guard below.
    if let Some(rest) = input.strip_prefix("gexec") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::GExec);
        }
    }

    // `\gset [prefix]` — store each column of the single result row as a variable.
    if let Some(rest) = input.strip_prefix("gset") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let prefix = rest.trim();
            return ParsedMeta::simple(MetaCmd::GSet(if prefix.is_empty() {
                None
            } else {
                Some(prefix.to_owned())
            }));
        }
    }

    // `\gdesc` — describe buffer columns without executing.
    if let Some(rest) = input.strip_prefix("gdesc") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::GDesc);
        }
    }

    // (gexec and gset are both handled above; no further long-prefix guards needed.)

    // `\gx [file]` — expanded execute; must be checked before bare `\g`.
    if let Some(rest) = input.strip_prefix("gx") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::GoExecuteExpanded(if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_owned())
                }),
                plus: false,
                system: false,
                pattern: None,
                echo_hidden: false,
            };
        }
    }

    // `\g [file||cmd]`
    if let Some(rest) = input.strip_prefix('g') {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let arg = rest.trim();
            return ParsedMeta {
                cmd: MetaCmd::GoExecute(if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_owned())
                }),
                plus: false,
                system: false,
                pattern: None,
                echo_hidden: false,
            };
        }
    }

    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
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
    ("dba", MetaCmd::Dba),
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
    use crate::output::ExpandedMode;

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

    // -- Variable commands (issue #32) ---------------------------------------

    #[test]
    fn parse_set_bare() {
        assert_eq!(
            parse("\\set").cmd,
            MetaCmd::Set(String::new(), String::new())
        );
    }

    #[test]
    fn parse_set_name_only() {
        assert_eq!(
            parse("\\set FOO").cmd,
            MetaCmd::Set("FOO".to_owned(), String::new())
        );
    }

    #[test]
    fn parse_set_name_value() {
        assert_eq!(
            parse("\\set FOO bar").cmd,
            MetaCmd::Set("FOO".to_owned(), "bar".to_owned())
        );
    }

    #[test]
    fn parse_set_value_with_spaces() {
        // Second token onwards is the value, trimmed.
        assert_eq!(
            parse("\\set X hello world").cmd,
            MetaCmd::Set("X".to_owned(), "hello world".to_owned())
        );
    }

    #[test]
    fn parse_unset_name() {
        assert_eq!(parse("\\unset FOO").cmd, MetaCmd::Unset("FOO".to_owned()));
    }

    #[test]
    fn parse_pset_bare() {
        assert_eq!(parse("\\pset").cmd, MetaCmd::Pset(String::new(), None));
    }

    #[test]
    fn parse_pset_option_only() {
        assert_eq!(
            parse("\\pset format").cmd,
            MetaCmd::Pset("format".to_owned(), None)
        );
    }

    #[test]
    fn parse_pset_option_value() {
        assert_eq!(
            parse("\\pset format csv").cmd,
            MetaCmd::Pset("format".to_owned(), Some("csv".to_owned()))
        );
    }

    #[test]
    fn parse_toggle_align() {
        assert_eq!(parse("\\a").cmd, MetaCmd::ToggleAlign);
    }

    #[test]
    fn parse_tuples_only_bare() {
        assert_eq!(parse("\\t").cmd, MetaCmd::TuplesOnly(None));
    }

    #[test]
    fn parse_tuples_only_on() {
        assert_eq!(parse("\\t on").cmd, MetaCmd::TuplesOnly(Some(true)));
    }

    #[test]
    fn parse_tuples_only_off() {
        assert_eq!(parse("\\t off").cmd, MetaCmd::TuplesOnly(Some(false)));
    }

    #[test]
    fn parse_field_sep_bare() {
        assert_eq!(parse("\\f").cmd, MetaCmd::FieldSep(None));
    }

    #[test]
    fn parse_field_sep_with_value() {
        assert_eq!(parse("\\f ,").cmd, MetaCmd::FieldSep(Some(",".to_owned())));
    }

    #[test]
    fn parse_toggle_html() {
        assert_eq!(parse("\\H").cmd, MetaCmd::ToggleHtml);
    }

    #[test]
    fn parse_set_title_bare() {
        assert_eq!(parse("\\C").cmd, MetaCmd::SetTitle(None));
    }

    #[test]
    fn parse_set_title_with_value() {
        assert_eq!(
            parse("\\C My Table").cmd,
            MetaCmd::SetTitle(Some("My Table".to_owned()))
        );
    }

    // timing must still work via the t-family dispatcher
    #[test]
    fn parse_timing_still_works() {
        assert_eq!(parse("\\timing on").cmd, MetaCmd::Timing(Some(true)));
        assert_eq!(parse("\\timing off").cmd, MetaCmd::Timing(Some(false)));
        assert_eq!(parse("\\timing").cmd, MetaCmd::Timing(None));
    }

    // \sf and \sv must still work after adding \set
    #[test]
    fn parse_sf_still_works_after_set() {
        let m = parse("\\sf my_func");
        assert_eq!(m.cmd, MetaCmd::ShowFunctionSource);
        assert_eq!(m.pattern, Some("my_func".to_owned()));
    }

    #[test]
    fn parse_sv_still_works_after_set() {
        let m = parse("\\sv my_view");
        assert_eq!(m.cmd, MetaCmd::ShowViewDef);
    }

    // -- I/O commands (#33) -------------------------------------------------

    #[test]
    fn parse_include_with_file() {
        let m = parse("\\i myfile.sql");
        assert_eq!(m.cmd, MetaCmd::Include);
        assert_eq!(m.pattern, Some("myfile.sql".to_owned()));
    }

    #[test]
    fn parse_include_no_file() {
        let m = parse("\\i");
        assert_eq!(m.cmd, MetaCmd::Include);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_include_relative_with_file() {
        let m = parse("\\ir ../other.sql");
        assert_eq!(m.cmd, MetaCmd::IncludeRelative);
        assert_eq!(m.pattern, Some("../other.sql".to_owned()));
    }

    #[test]
    fn parse_include_relative_not_confused_with_include() {
        // \ir must match before \i
        assert_eq!(parse("\\ir foo.sql").cmd, MetaCmd::IncludeRelative);
        assert_eq!(parse("\\i foo.sql").cmd, MetaCmd::Include);
    }

    #[test]
    fn parse_output_with_file() {
        let m = parse("\\o /tmp/out.txt");
        assert_eq!(m.cmd, MetaCmd::Output);
        assert_eq!(m.pattern, Some("/tmp/out.txt".to_owned()));
    }

    #[test]
    fn parse_output_no_file_restores_stdout() {
        let m = parse("\\o");
        assert_eq!(m.cmd, MetaCmd::Output);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_write_buffer() {
        let m = parse("\\w buf.sql");
        assert_eq!(m.cmd, MetaCmd::WriteBuffer);
        assert_eq!(m.pattern, Some("buf.sql".to_owned()));
    }

    #[test]
    fn parse_reset_buffer() {
        assert_eq!(parse("\\r").cmd, MetaCmd::ResetBuffer);
    }

    #[test]
    fn parse_print_buffer() {
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    #[test]
    fn parse_edit_no_args() {
        let m = parse("\\e");
        assert_eq!(m.cmd, MetaCmd::Edit);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_edit_with_file() {
        let m = parse("\\e myfile.sql");
        assert_eq!(m.cmd, MetaCmd::Edit);
        assert_eq!(m.pattern, Some("myfile.sql".to_owned()));
    }

    #[test]
    fn parse_edit_with_file_and_line() {
        let m = parse("\\e myfile.sql 42");
        assert_eq!(m.cmd, MetaCmd::Edit);
        assert_eq!(m.pattern, Some("myfile.sql 42".to_owned()));
    }

    #[test]
    fn parse_shell_with_command() {
        let m = parse("\\! echo hello");
        assert_eq!(m.cmd, MetaCmd::Shell);
        assert_eq!(m.pattern, Some("echo hello".to_owned()));
    }

    #[test]
    fn parse_shell_no_command() {
        let m = parse("\\!");
        assert_eq!(m.cmd, MetaCmd::Shell);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_chdir_with_dir() {
        let m = parse("\\cd /tmp");
        assert_eq!(m.cmd, MetaCmd::Chdir);
        assert_eq!(m.pattern, Some("/tmp".to_owned()));
    }

    #[test]
    fn parse_chdir_no_dir() {
        let m = parse("\\cd");
        assert_eq!(m.cmd, MetaCmd::Chdir);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_chdir_not_confused_with_reconnect() {
        // \cd must not match as \c
        assert_eq!(parse("\\cd /tmp").cmd, MetaCmd::Chdir);
        assert_eq!(parse("\\c mydb").cmd, MetaCmd::Reconnect);
    }

    #[test]
    fn parse_echo_with_text() {
        let m = parse("\\echo hello world");
        assert_eq!(m.cmd, MetaCmd::Echo);
        assert_eq!(m.pattern, Some("hello world".to_owned()));
    }

    #[test]
    fn parse_echo_no_text() {
        let m = parse("\\echo");
        assert_eq!(m.cmd, MetaCmd::Echo);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_qecho_with_text() {
        let m = parse("\\qecho output text");
        assert_eq!(m.cmd, MetaCmd::QEcho);
        assert_eq!(m.pattern, Some("output text".to_owned()));
    }

    #[test]
    fn parse_warn_with_text() {
        let m = parse("\\warn danger");
        assert_eq!(m.cmd, MetaCmd::Warn);
        assert_eq!(m.pattern, Some("danger".to_owned()));
    }

    #[test]
    fn parse_encoding_no_arg() {
        let m = parse("\\encoding");
        assert_eq!(m.cmd, MetaCmd::Encoding);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_encoding_utf8() {
        let m = parse("\\encoding UTF8");
        assert_eq!(m.cmd, MetaCmd::Encoding);
        assert_eq!(m.pattern, Some("UTF8".to_owned()));
    }

    #[test]
    fn parse_password_no_user() {
        let m = parse("\\password");
        assert_eq!(m.cmd, MetaCmd::Password);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_password_with_user() {
        let m = parse("\\password alice");
        assert_eq!(m.cmd, MetaCmd::Password);
        assert_eq!(m.pattern, Some("alice".to_owned()));
    }

    #[test]
    fn parse_warn_not_confused_with_write_buffer() {
        // \warn starts with 'w', must not match \w
        assert_eq!(parse("\\warn text").cmd, MetaCmd::Warn);
        assert_eq!(parse("\\w file.sql").cmd, MetaCmd::WriteBuffer);
    }

    #[test]
    fn parse_password_not_confused_with_print_buffer() {
        // \password starts with 'p', must not match \p
        assert_eq!(parse("\\password user").cmd, MetaCmd::Password);
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    // -- \if / \elif / \else / \endif (#37) ----------------------------------

    #[test]
    fn parse_if_with_expression() {
        let m = parse("\\if true");
        assert_eq!(m.cmd, MetaCmd::If);
        assert_eq!(m.pattern, Some("true".to_owned()));
    }

    #[test]
    fn parse_if_bare_no_expression() {
        let m = parse("\\if");
        assert_eq!(m.cmd, MetaCmd::If);
        assert!(m.pattern.is_none());
    }

    #[test]
    fn parse_elif_with_expression() {
        let m = parse("\\elif false");
        assert_eq!(m.cmd, MetaCmd::Elif);
        assert_eq!(m.pattern, Some("false".to_owned()));
    }

    #[test]
    fn parse_elif_bare() {
        let m = parse("\\elif");
        assert_eq!(m.cmd, MetaCmd::Elif);
        assert!(m.pattern.is_none());
    }

    #[test]
    fn parse_else() {
        assert_eq!(parse("\\else").cmd, MetaCmd::Else);
        assert!(parse("\\else").pattern.is_none());
    }

    #[test]
    fn parse_endif() {
        assert_eq!(parse("\\endif").cmd, MetaCmd::Endif);
    }

    #[test]
    fn parse_if_not_confused_with_include() {
        // \if starts with 'i'; must not match \i or \ir
        assert_eq!(parse("\\if true").cmd, MetaCmd::If);
        assert_eq!(parse("\\i file.sql").cmd, MetaCmd::Include);
        assert_eq!(parse("\\ir file.sql").cmd, MetaCmd::IncludeRelative);
    }

    #[test]
    fn parse_elif_not_confused_with_echo_or_encoding() {
        // \elif, \else, \endif start with 'e'; must not match \echo, \encoding, \e
        assert_eq!(parse("\\elif true").cmd, MetaCmd::Elif);
        assert_eq!(parse("\\else").cmd, MetaCmd::Else);
        assert_eq!(parse("\\endif").cmd, MetaCmd::Endif);
        assert_eq!(parse("\\echo hello").cmd, MetaCmd::Echo);
        assert_eq!(parse("\\encoding UTF8").cmd, MetaCmd::Encoding);
        assert_eq!(parse("\\e").cmd, MetaCmd::Edit);
    }

    #[test]
    fn parse_if_variable_expression() {
        // Variable interpolation has already occurred before parsing; the
        // parser stores the resulting string verbatim in `pattern`.
        let m = parse("\\if on");
        assert_eq!(m.cmd, MetaCmd::If);
        assert_eq!(m.pattern, Some("on".to_owned()));
    }

    // -- \g / \gx (issue #46) ------------------------------------------------

    #[test]
    fn parse_g_bare() {
        let m = parse("\\g");
        assert_eq!(m.cmd, MetaCmd::GoExecute(None));
    }

    #[test]
    fn parse_g_to_file() {
        let m = parse("\\g /tmp/out");
        assert_eq!(m.cmd, MetaCmd::GoExecute(Some("/tmp/out".to_owned())));
    }

    #[test]
    fn parse_g_piped() {
        let m = parse("\\g |sort");
        assert_eq!(m.cmd, MetaCmd::GoExecute(Some("|sort".to_owned())));
    }

    #[test]
    fn parse_gx_bare() {
        let m = parse("\\gx");
        assert_eq!(m.cmd, MetaCmd::GoExecuteExpanded(None));
    }

    #[test]
    fn parse_gx_to_file() {
        let m = parse("\\gx /tmp/out");
        assert_eq!(
            m.cmd,
            MetaCmd::GoExecuteExpanded(Some("/tmp/out".to_owned()))
        );
    }

    #[test]
    fn parse_g_not_confused_with_gset_gexec_gdesc() {
        // These longer g-prefixed commands must NOT parse as GoExecute.
        assert!(!matches!(parse("\\gexec").cmd, MetaCmd::GoExecute(_)));
        assert!(!matches!(parse("\\gset").cmd, MetaCmd::GoExecute(_)));
        assert!(!matches!(parse("\\gdesc").cmd, MetaCmd::GoExecute(_)));
    }

    // -- \gdesc (#52) --------------------------------------------------------

    #[test]
    fn parse_gdesc() {
        assert_eq!(parse("\\gdesc").cmd, MetaCmd::GDesc);
        // Trailing whitespace is fine.
        assert_eq!(parse("\\gdesc ").cmd, MetaCmd::GDesc);
        // Must NOT be confused with \g or \gx.
        assert!(!matches!(parse("\\gdesc").cmd, MetaCmd::GoExecute(_)));
        assert!(!matches!(
            parse("\\gdesc").cmd,
            MetaCmd::GoExecuteExpanded(_)
        ));
    }

    #[test]
    fn parse_gexec_is_gexec_not_go_execute() {
        // \gexec must parse as GExec, not GoExecute or Unknown.
        assert_eq!(parse("\\gexec").cmd, MetaCmd::GExec);
    }

    #[test]
    fn parse_gx_not_confused_with_g() {
        // \gx must not fall through to \g.
        assert!(matches!(parse("\\gx").cmd, MetaCmd::GoExecuteExpanded(_)));
        assert!(matches!(parse("\\g").cmd, MetaCmd::GoExecute(_)));
    }

    // -- \gset ---------------------------------------------------------------

    #[test]
    fn parse_gset_no_prefix() {
        let m = parse("\\gset");
        assert_eq!(m.cmd, MetaCmd::GSet(None));
    }

    #[test]
    fn parse_gset_with_prefix() {
        let m = parse("\\gset my_");
        assert_eq!(m.cmd, MetaCmd::GSet(Some("my_".to_owned())));
    }

    #[test]
    fn parse_gset_not_confused_with_g() {
        // \gset must NOT fall through to GoExecute.
        assert!(!matches!(parse("\\gset").cmd, MetaCmd::GoExecute(_)));
    }

    // -- \watch (#47) --------------------------------------------------------

    #[test]
    fn parse_watch_bare() {
        let m = parse("\\watch");
        assert_eq!(m.cmd, MetaCmd::Watch);
        assert!(m.pattern.is_none());
    }

    #[test]
    fn parse_watch_with_interval() {
        let m = parse("\\watch 5");
        assert_eq!(m.cmd, MetaCmd::Watch);
        assert_eq!(m.pattern, Some("5".to_owned()));
    }

    #[test]
    fn parse_watch_with_float_interval() {
        let m = parse("\\watch 0.5");
        assert_eq!(m.cmd, MetaCmd::Watch);
        assert_eq!(m.pattern, Some("0.5".to_owned()));
    }

    #[test]
    fn parse_watch_with_seconds_suffix() {
        let m = parse("\\watch 3s");
        assert_eq!(m.cmd, MetaCmd::Watch);
        assert_eq!(m.pattern, Some("3s".to_owned()));
    }

    #[test]
    fn parse_watch_not_confused_with_warn_or_write_buffer() {
        // \watch must not match \warn or \w
        assert_eq!(parse("\\watch").cmd, MetaCmd::Watch);
        assert_eq!(parse("\\warn text").cmd, MetaCmd::Warn);
        assert_eq!(parse("\\w file.sql").cmd, MetaCmd::WriteBuffer);
    }

    // -- \copy ---------------------------------------------------------------

    #[test]
    fn parse_copy_from_file() {
        let m = parse("\\copy my_table FROM '/tmp/data.txt'");
        assert_eq!(
            m.cmd,
            MetaCmd::Copy("my_table FROM '/tmp/data.txt'".to_owned())
        );
    }

    // -- \bind (#57) ---------------------------------------------------------

    #[test]
    fn parse_bind_no_params() {
        let m = parse("\\bind");
        assert_eq!(m.cmd, MetaCmd::Bind(vec![]));
    }

    #[test]
    fn parse_bind_one_param() {
        let m = parse("\\bind 42");
        assert_eq!(m.cmd, MetaCmd::Bind(vec!["42".to_owned()]));
    }

    #[test]
    fn parse_bind_two_params() {
        let m = parse("\\bind 3 4");
        assert_eq!(m.cmd, MetaCmd::Bind(vec!["3".to_owned(), "4".to_owned()]));
    }

    #[test]
    fn parse_bind_quoted_param_with_space() {
        let m = parse("\\bind 'hello world'");
        assert_eq!(m.cmd, MetaCmd::Bind(vec!["hello world".to_owned()]));
    }

    #[test]
    fn parse_bind_not_confused_with_bind_named() {
        // \bind_named must not fall through to \bind.
        assert!(!matches!(
            parse("\\bind_named my_stmt 1").cmd,
            MetaCmd::Bind(_)
        ));
    }

    // -- \bind_named (#57) ---------------------------------------------------

    #[test]
    fn parse_bind_named_no_params() {
        let m = parse("\\bind_named my_stmt");
        assert_eq!(m.cmd, MetaCmd::BindNamed("my_stmt".to_owned(), vec![]));
    }

    #[test]
    fn parse_bind_named_with_params() {
        let m = parse("\\bind_named my_stmt 1 2 3");
        assert_eq!(
            m.cmd,
            MetaCmd::BindNamed(
                "my_stmt".to_owned(),
                vec!["1".to_owned(), "2".to_owned(), "3".to_owned()],
            )
        );
    }

    #[test]
    fn parse_copy_to_stdout() {
        let m = parse("\\copy t TO stdout CSV");
        assert_eq!(m.cmd, MetaCmd::Copy("t TO stdout CSV".to_owned()));
    }

    #[test]
    fn parse_copy_bare_is_copy_with_empty_args() {
        let m = parse("\\copy");
        assert_eq!(m.cmd, MetaCmd::Copy(String::new()));
    }

    #[test]
    fn parse_copy_not_confused_with_conninfo_or_chdir() {
        // \copy must not silently become \c or \cd or \conninfo.
        assert!(matches!(parse("\\copy t FROM stdin").cmd, MetaCmd::Copy(_)));
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
        assert_eq!(parse("\\cd /tmp").cmd, MetaCmd::Chdir);
    }

    #[test]
    fn parse_bind_named_missing_name() {
        // No name: should parse as Unknown.
        let m = parse("\\bind_named");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    // -- \parse (#57) --------------------------------------------------------

    #[test]
    fn parse_parse_stmt() {
        let m = parse("\\parse my_stmt");
        assert_eq!(m.cmd, MetaCmd::Parse("my_stmt".to_owned()));
    }

    #[test]
    fn parse_parse_missing_name() {
        // No name: should parse as Unknown.
        let m = parse("\\parse");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_parse_not_confused_with_p() {
        // \parse must not fall through to PrintBuffer.
        assert!(!matches!(parse("\\parse stmt").cmd, MetaCmd::PrintBuffer));
    }

    // -- \close_prepared (#57) -----------------------------------------------

    #[test]
    fn parse_close_prepared() {
        let m = parse("\\close_prepared my_stmt");
        assert_eq!(m.cmd, MetaCmd::ClosePrepared("my_stmt".to_owned()));
    }

    #[test]
    fn parse_close_prepared_missing_name() {
        let m = parse("\\close_prepared");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_close_prepared_not_confused_with_conninfo() {
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
        assert!(matches!(
            parse("\\close_prepared stmt").cmd,
            MetaCmd::ClosePrepared(_)
        ));
    }

    // -- split_params helper -------------------------------------------------

    #[test]
    fn split_params_empty() {
        assert!(split_params("").is_empty());
    }

    #[test]
    fn split_params_whitespace_only() {
        assert!(split_params("   ").is_empty());
    }

    #[test]
    fn split_params_unquoted() {
        assert_eq!(split_params("a b c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_params_quoted_space() {
        assert_eq!(split_params("'hello world'"), vec!["hello world"]);
    }

    #[test]
    fn split_params_escaped_quote() {
        assert_eq!(split_params("'it''s'"), vec!["it's"]);
    }

    #[test]
    fn split_params_mixed() {
        assert_eq!(
            split_params("42 'foo bar' baz"),
            vec!["42", "foo bar", "baz"]
        );
    }

    // -- \crosstabview -------------------------------------------------------

    #[test]
    fn parse_crosstabview_bare() {
        let m = parse("\\crosstabview");
        assert_eq!(m.cmd, MetaCmd::CrosstabView(String::new()));
    }

    #[test]
    fn parse_crosstabview_with_args() {
        let m = parse("\\crosstabview row col val");
        assert_eq!(m.cmd, MetaCmd::CrosstabView("row col val".to_owned()));
    }

    #[test]
    fn parse_crosstabview_with_index_args() {
        let m = parse("\\crosstabview 0 1 2");
        assert_eq!(m.cmd, MetaCmd::CrosstabView("0 1 2".to_owned()));
    }

    #[test]
    fn parse_crosstabview_not_confused_with_reconnect() {
        // \c must still parse as Reconnect; \crosstabview uses the longer prefix.
        assert_eq!(parse("\\c").cmd, MetaCmd::Reconnect);
        assert_eq!(parse("\\c mydb").cmd, MetaCmd::Reconnect);
        assert!(matches!(
            parse("\\crosstabview").cmd,
            MetaCmd::CrosstabView(_)
        ));
    }

    #[test]
    fn parse_crosstabview_not_confused_with_conninfo() {
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
        assert!(matches!(
            parse("\\crosstabview").cmd,
            MetaCmd::CrosstabView(_)
        ));
    }

    // -- \copyright ----------------------------------------------------------

    #[test]
    fn parse_copyright() {
        assert_eq!(parse("\\copyright").cmd, MetaCmd::Copyright);
    }

    #[test]
    fn parse_copyright_not_confused_with_conninfo_or_copy() {
        assert_eq!(parse("\\copyright").cmd, MetaCmd::Copyright);
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
        assert_eq!(parse("\\cd /tmp").cmd, MetaCmd::Chdir);
    }

    // -- \dba (#66) ----------------------------------------------------------

    #[test]
    fn parse_dba_bare() {
        // `\dba` with no subcommand: cmd=Dba, pattern=None.
        let m = parse("\\dba");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, None);
    }

    #[test]
    fn parse_dba_help() {
        let m = parse("\\dba help");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("help".to_owned()));
    }

    #[test]
    fn parse_dba_activity() {
        let m = parse("\\dba activity");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("activity".to_owned()));
    }

    #[test]
    fn parse_dba_act_alias() {
        let m = parse("\\dba act");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("act".to_owned()));
    }

    #[test]
    fn parse_dba_locks() {
        let m = parse("\\dba locks");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("locks".to_owned()));
    }

    #[test]
    fn parse_dba_bloat() {
        let m = parse("\\dba bloat");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("bloat".to_owned()));
    }

    #[test]
    fn parse_dba_vacuum() {
        let m = parse("\\dba vacuum");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("vacuum".to_owned()));
    }

    #[test]
    fn parse_dba_tablesize() {
        let m = parse("\\dba tablesize");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("tablesize".to_owned()));
    }

    #[test]
    fn parse_dba_connections() {
        let m = parse("\\dba connections");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("connections".to_owned()));
    }

    #[test]
    fn parse_dba_unused_idx() {
        let m = parse("\\dba unused-idx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("unused-idx".to_owned()));
    }

    #[test]
    fn parse_dba_seq_scans() {
        let m = parse("\\dba seq-scans");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("seq-scans".to_owned()));
    }

    #[test]
    fn parse_dba_cache_hit() {
        let m = parse("\\dba cache-hit");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("cache-hit".to_owned()));
    }

    #[test]
    fn parse_dba_replication() {
        let m = parse("\\dba replication");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("replication".to_owned()));
    }

    #[test]
    fn parse_dba_config() {
        let m = parse("\\dba config");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("config".to_owned()));
    }

    #[test]
    fn parse_dba_verbose_plus() {
        // `\dba+ activity` — plus modifier is recognised.
        let m = parse("\\dba+ activity");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert!(m.plus, "expected plus=true for \\dba+");
        assert_eq!(m.pattern, Some("activity".to_owned()));
    }

    #[test]
    fn parse_dba_not_confused_with_db() {
        // `\db` must still parse as ListTablespaces; `\dba` is separate.
        assert_eq!(parse("\\db").cmd, MetaCmd::ListTablespaces);
        assert_eq!(parse("\\dba activity").cmd, MetaCmd::Dba);
    }

    #[test]
    fn parse_dba_unknown_subcommand_still_parses_as_dba() {
        // Unknown subcommands are handled at dispatch time, not parse time.
        let m = parse("\\dba nonexistent");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("nonexistent".to_owned()));
    }

    // -- Named queries (#69) ------------------------------------------------

    #[test]
    fn parse_named_save() {
        let m =
            parse("\\ns top_tables SELECT * FROM pg_stat_user_tables ORDER BY $1 DESC LIMIT $2");
        assert_eq!(
            m.cmd,
            MetaCmd::NamedSave(
                "top_tables".to_owned(),
                "SELECT * FROM pg_stat_user_tables ORDER BY $1 DESC LIMIT $2".to_owned(),
            )
        );
    }

    #[test]
    fn parse_named_exec_with_args() {
        let m = parse("\\n top_tables seq_scan 10");
        assert_eq!(
            m.cmd,
            MetaCmd::NamedExec(
                "top_tables".to_owned(),
                vec!["seq_scan".to_owned(), "10".to_owned()],
            )
        );
    }

    #[test]
    fn parse_named_exec_no_args() {
        let m = parse("\\n my_query");
        assert_eq!(m.cmd, MetaCmd::NamedExec("my_query".to_owned(), vec![]));
    }

    #[test]
    fn parse_named_list() {
        let m = parse("\\n+");
        assert_eq!(m.cmd, MetaCmd::NamedList);
    }

    #[test]
    fn parse_named_delete() {
        let m = parse("\\nd top_tables");
        assert_eq!(m.cmd, MetaCmd::NamedDelete("top_tables".to_owned()));
    }

    #[test]
    fn parse_named_save_ns_not_confused_with_n() {
        // `\ns` must not be mistaken for `\n` with arg `s`.
        let m = parse("\\ns my_q select 1");
        assert!(matches!(m.cmd, MetaCmd::NamedSave(_, _)));
    }

    #[test]
    fn parse_named_delete_nd_not_confused_with_n() {
        // `\nd` must not be mistaken for `\n` with arg `d`.
        let m = parse("\\nd my_q");
        assert!(matches!(m.cmd, MetaCmd::NamedDelete(_)));
    }

    #[test]
    fn parse_named_save_missing_query_is_unknown() {
        // `\ns name` with no query body should be Unknown.
        let m = parse("\\ns only_name");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_named_delete_missing_name_is_unknown() {
        let m = parse("\\nd");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    // -- Input mode commands ---------------------------------------------------

    #[test]
    fn parse_sql_mode() {
        assert_eq!(parse("\\sql").cmd, MetaCmd::SqlMode);
    }

    #[test]
    fn parse_text2sql_mode() {
        assert_eq!(parse("\\text2sql").cmd, MetaCmd::Text2SqlMode);
    }

    #[test]
    fn parse_t2s_alias() {
        assert_eq!(parse("\\t2s").cmd, MetaCmd::Text2SqlMode);
    }

    #[test]
    fn parse_show_mode() {
        assert_eq!(parse("\\mode").cmd, MetaCmd::ShowMode);
    }

    #[test]
    fn parse_mode_prefix_unknown() {
        // \modex should be unknown, not \mode
        assert!(matches!(parse("\\modex").cmd, MetaCmd::Unknown(_)));
    }

    // -- Execution mode commands -----------------------------------------------

    #[test]
    fn parse_plan_mode() {
        assert_eq!(parse("\\plan").cmd, MetaCmd::PlanMode);
    }

    #[test]
    fn parse_yolo_mode() {
        assert_eq!(parse("\\yolo").cmd, MetaCmd::YoloMode);
    }

    #[test]
    fn parse_observe_mode() {
        assert_eq!(parse("\\observe").cmd, MetaCmd::ObserveMode);
        assert!(parse("\\observe").pattern.is_none());
    }

    #[test]
    fn parse_observe_mode_with_duration() {
        let parsed = parse("\\observe 30s");
        assert_eq!(parsed.cmd, MetaCmd::ObserveMode);
        assert_eq!(parsed.pattern.as_deref(), Some("30s"));
    }

    #[test]
    fn parse_observe_mode_with_minutes() {
        let parsed = parse("\\observe 5m");
        assert_eq!(parsed.cmd, MetaCmd::ObserveMode);
        assert_eq!(parsed.pattern.as_deref(), Some("5m"));
    }

    #[test]
    fn parse_interactive_mode() {
        assert_eq!(parse("\\interactive").cmd, MetaCmd::InteractiveMode);
    }

    #[test]
    fn parse_plan_does_not_steal_p() {
        // Bare \p should still be PrintBuffer, not PlanMode
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    #[test]
    fn parse_observe_does_not_steal_o() {
        // Bare \o should still be Output, not ObserveMode
        assert_eq!(parse("\\o").cmd, MetaCmd::Output);
    }
}
