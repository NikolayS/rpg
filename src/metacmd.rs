//! Backslash (meta) command parser for Rpg.
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
    /// `\dy [pattern]` — list event triggers.
    ListEventTriggers,
    /// `\do [pattern]` — list operators.
    ListOperators,

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
    /// `\prompt [text] name` — prompt the user for input and store in a variable.
    ///
    /// Payload: `(prompt_text, var_name)`.  When only one argument is given,
    /// `prompt_text` is the empty string and `var_name` holds the argument.
    /// The optional prompt text may be single- or double-quoted.
    Prompt(String, String),
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

    // -- Conditional execution (#393) -------------------------------------
    /// `\if <expression>` — begin a conditional block.
    ///
    /// The expression is variable-interpolated by the caller before parsing;
    /// this variant carries the final string to be evaluated for truthiness.
    If(String),
    /// `\elif <expression>` — alternate branch of a conditional block.
    ///
    /// Like `If`, the expression has already been variable-interpolated.
    Elif(String),
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
    /// `\version` — show rpg version and build information.
    Version,

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
    /// `\np name` — print (show) a named query without executing.
    NamedPrint(String),

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
    /// `\interactive` — return to interactive mode.
    InteractiveMode,

    // -- Connection profiles -----------------------------------------------
    /// `\profiles` — list all configured connection profiles.
    ListProfiles,

    // -- Schema cache (#301) -----------------------------------------------
    /// `\refresh` — reload the schema cache for tab completion.
    RefreshSchema,

    // -- Session persistence (#247) ----------------------------------------
    /// `\session list` — show recent sessions.
    SessionList,
    /// `\session save [name]` — save the current session with an optional name.
    ///
    /// `name` is `None` when omitted.
    SessionSave(Option<String>),
    /// `\session delete <id>` — delete a session by id.
    SessionDelete(String),
    /// `\session resume <id>` — reconnect using a saved session.
    SessionResume(String),

    // -- Function key toggles (#321) ---------------------------------------
    /// `\f2` — toggle schema-aware completion on/off (F2).
    ToggleCompletion,
    /// `\f3` — toggle single-line mode on/off (F3).
    ToggleSingleLine,
    /// `\f4` — toggle Vi/Emacs editing mode (F4, #325).
    ToggleViEmacs,
    /// `\f5` — toggle auto-EXPLAIN on/off (F5).
    ToggleAutoExplain,

    // -- Query audit logging (FR-23) ---------------------------------------
    /// `\log-file <path>` — start logging queries to `path` (append mode).
    ///
    /// `\log-file` with no argument closes the current log file.
    ///
    /// Each query entry written to the log looks like:
    /// ```text
    /// -- 2026-03-12 14:23:01 UTC | mydb | user=nik | duration=12ms
    /// SELECT * FROM users WHERE id = 42;
    /// -- (1 row)
    /// ```
    LogFile(Option<String>),

    // -- Large object commands (#400) --------------------------------------
    /// `\lo_import <filename> [<comment>]` — import a file as a large object.
    ///
    /// `filename` is the local path to read from.  The optional `comment`
    /// string is stored as an object comment in the catalog.
    ///
    /// Payload: `(filename, comment)`.  `comment` is empty when omitted.
    LoImport(String, String),
    /// `\lo_export <loid> <filename>` — export a large object to a local file.
    ///
    /// Payload: `(loid, filename)`.
    LoExport(String, String),
    /// `\lo_list` / `\dl` — list all large objects with their OIDs and
    /// descriptions.
    LoList,
    /// `\lo_unlink <loid>` — delete a large object by OID.
    ///
    /// Payload: the OID as a string.
    LoUnlink(String),

    // -- Explain share (#655) ---------------------------------------------
    /// `\explain share <service>` — upload the last EXPLAIN plan to an
    /// external visualiser and print the resulting URL.
    ///
    /// `service` is either `"depesz"` (explain.depesz.com) or
    /// `"dalibo"` (explain.dalibo.com).
    ExplainShare(String),

    // -- Custom Lua commands (#659) ----------------------------------------
    /// `\commands` — list all custom Lua meta-commands loaded from
    /// `~/.config/rpg/commands/*.lua`.
    ListCustomCommands,

    // -- History (#history) ------------------------------------------------
    /// `\s [filename | pattern]` — display or save command history.
    ///
    /// Behaviour:
    /// - `\s` — display numbered history on screen (through pager).
    /// - `\s filename` — save history to a file (path contains `/` or `.`
    ///   or starts with `~`; or the argument has a file extension).
    /// - `\s pattern` — filter and display history entries that contain
    ///   the pattern (case-insensitive grep-like search).
    ///
    /// Unlike plain psql, rpg numbers entries and applies SQL syntax
    /// highlighting when writing to the screen.
    ///
    /// The argument is `None` for bare `\s`; otherwise it carries the raw
    /// argument string (filename or pattern).
    History(Option<String>),

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
            Self::ListEventTriggers => "\\dy",
            Self::ListOperators => "\\do",
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
            Self::Prompt(_, _) => "\\prompt",
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
    /// Optional prokind filter for `\df` variants:
    /// `'f'` = normal functions (`\dfn`), `'p'` = procedures (`\dfp`),
    /// `'a'` = aggregates (`\dfa`), `'w'` = window functions (`\dfw`).
    /// `None` means show all kinds (`\df`).
    pub kind_filter: Option<char>,
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
            kind_filter: None,
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
                        kind_filter: None,
                    };
                }
            }
            parse_simple_or_unknown(input, "q", MetaCmd::Quit)
        }
        Some('?') => parse_simple_or_unknown(input, "?", MetaCmd::Help),
        Some('a') => parse_a_family(input),
        Some('c') => parse_c_family(input),
        Some('C') => parse_set_title(input),
        Some('e') => parse_e_family(input),
        Some('f') => parse_f_family(input),
        Some('h') => parse_h(input),
        Some('H') => parse_simple_or_unknown(input, "H", MetaCmd::ToggleHtml),
        Some('i') => parse_i_family(input),
        Some('o') => parse_o(input),
        Some('p') => parse_p_family(input),
        Some('r') => parse_r_family(input),
        Some('s') => parse_s_family(input),
        Some('t') => parse_t_family(input),
        Some('u') => parse_unset(input),
        Some('v') => parse_v_family(input),
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

/// Parse `\a` (toggle align).
fn parse_a_family(input: &str) -> ParsedMeta {
    // Bare `\a` — toggle aligned/unaligned output.
    parse_simple_or_unknown(input, "a", MetaCmd::ToggleAlign)
}

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

/// Dispatch `s`-family commands: `\s`, `\set`, `\sf`, `\sv`, `\session`.
fn parse_s_family(input: &str) -> ParsedMeta {
    // \sql — switch to SQL mode (check before \set)
    if let Some(rest) = input.strip_prefix("sql") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::SqlMode);
        }
    }
    // \session … — session persistence commands (check before \set)
    if let Some(rest) = input.strip_prefix("session") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return parse_session(rest.trim());
        }
    }
    if let Some(after) = input.strip_prefix("set") {
        if after.is_empty() || after.starts_with(char::is_whitespace) {
            return parse_set(input);
        }
    }
    // \s [filename | pattern] — show/save/filter history.
    // Must be checked after the longer prefixes above (sql, session, set,
    // sf, sv) so those still match.
    if input == "s" || input.starts_with("s ") || input.starts_with("s\t") {
        let arg = input.strip_prefix("s").unwrap_or("").trim();
        return ParsedMeta::simple(MetaCmd::History(if arg.is_empty() {
            None
        } else {
            Some(arg.to_owned())
        }));
    }
    parse_sf_sv(input)
}

/// Parse `\session [subcommand [args]]`.
fn parse_session(rest: &str) -> ParsedMeta {
    // Bare `\session` — same as `\session list`.
    if rest.is_empty() {
        return ParsedMeta::simple(MetaCmd::SessionList);
    }

    let mut parts = rest.splitn(2, char::is_whitespace);
    let sub = parts.next().unwrap_or("");
    let arg = parts.next().map_or("", str::trim).to_owned();

    match sub {
        "list" => ParsedMeta::simple(MetaCmd::SessionList),
        "save" => ParsedMeta::simple(MetaCmd::SessionSave(if arg.is_empty() {
            None
        } else {
            Some(arg)
        })),
        "delete" | "del" => {
            if arg.is_empty() {
                ParsedMeta::simple(MetaCmd::Unknown("session delete".to_owned()))
            } else {
                ParsedMeta::simple(MetaCmd::SessionDelete(arg))
            }
        }
        "resume" | "connect" => {
            if arg.is_empty() {
                ParsedMeta::simple(MetaCmd::Unknown("session resume".to_owned()))
            } else {
                ParsedMeta::simple(MetaCmd::SessionResume(arg))
            }
        }
        _ => ParsedMeta::simple(MetaCmd::Unknown(format!("session {sub}"))),
    }
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
    // `\set name` or `\set name value…`
    // The name is the first whitespace-delimited token; value tokens are
    // parsed with `split_params` (strips surrounding single-quotes, handles
    // `\'`/`''` escapes) and concatenated *without* spaces, matching psql:
    //   `\set d '\'' :var '\''`  →  d = "'<var>'"  (after interpolation)
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").to_owned();
    let raw_value = parts.next().map_or("", str::trim);
    let value = if raw_value.is_empty() {
        String::new()
    } else {
        split_params(raw_value).concat()
    };
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
///
/// The value may be single-quoted (matching psql: `\pset null '(null)'`
/// sets the null display string to `(null)` without the quotes).
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
    let value = parts.next().map(|s| {
        let v = s.trim();
        // Strip surrounding single quotes (matching psql behaviour).
        if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
            v[1..v.len() - 1].to_owned()
        } else {
            v.to_owned()
        }
    });
    ParsedMeta::simple(MetaCmd::Pset(option, value))
}

/// Parse `\prompt [text] name`.
///
/// The optional prompt text may be quoted (single or double quotes).  When the
/// first non-whitespace character is a quote, the text extends to the matching
/// closing quote; otherwise the first whitespace-delimited token is the prompt
/// text and the second is the variable name.  When only one unquoted token is
/// present it is the variable name (empty prompt text).
fn parse_prompt(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix("prompt") else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    let rest = rest.trim();
    if rest.is_empty() {
        // Bare `\prompt` with no variable name — unknown/error.
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    }

    // Check for a quoted prompt string.
    if rest.starts_with('"') || rest.starts_with('\'') {
        let quote = rest.chars().next().unwrap();
        let after_open = &rest[1..];
        if let Some(close) = after_open.find(quote) {
            let prompt_text = after_open[..close].to_owned();
            let var_name = after_open[close + 1..].trim().to_owned();
            if var_name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::Prompt(prompt_text, var_name));
        }
        // Unterminated quote — treat everything after opening quote as prompt,
        // which means no variable name: unknown.
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    }

    // No quotes: split on whitespace.
    let mut parts = rest.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").to_owned();
    match parts.next().map(str::trim) {
        Some(second) if !second.is_empty() => {
            // Two tokens: first is prompt text, second is variable name.
            ParsedMeta::simple(MetaCmd::Prompt(first, second.to_owned()))
        }
        _ => {
            // One token: it is the variable name, no prompt text.
            ParsedMeta::simple(MetaCmd::Prompt(String::new(), first))
        }
    }
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

/// Parse `\y` family: `\yolo` and `\dy [+] [pattern]`.
///
/// Note: `\dy` starts with `d`, so it is handled in `parse_d_family` via the
/// `D_SUBCMDS` table.  This function handles remaining `y`-prefixed commands.
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
/// Dispatch `\f…` commands: function-key aliases and field separator.
///
/// - `\f2` → toggle completion (F2)
/// - `\f3` → toggle single-line mode (F3)
/// - `\f4` → toggle Vi/Emacs mode (F4, #325)
/// - `\f5` → toggle auto-EXPLAIN (F5)
/// - `\f [sep]` → set field separator for unaligned output
fn parse_f_family(input: &str) -> ParsedMeta {
    let Some(rest) = input.strip_prefix('f') else {
        return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
    };
    // Function-key aliases: `\f2`, `\f3`, `\f4`, `\f5` (no trailing chars allowed).
    match rest.trim() {
        "2" => return ParsedMeta::simple(MetaCmd::ToggleCompletion),
        "3" => return ParsedMeta::simple(MetaCmd::ToggleSingleLine),
        "4" => return ParsedMeta::simple(MetaCmd::ToggleViEmacs),
        "5" => return ParsedMeta::simple(MetaCmd::ToggleAutoExplain),
        _ => {}
    }
    // Reject if `f` is immediately followed by another letter/digit
    // that isn't one of the known aliases above (e.g. `\foo`).
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

/// Parse `\version` or unknown `\v…`.
fn parse_v_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("version") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::Version);
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\conninfo`, `\crosstabview`, `\copy`, `\close_prepared`, `\copyright`, `\cd`, `\c`, or unknown `\c…`.
fn parse_c_family(input: &str) -> ParsedMeta {
    if let Some(rest) = input.strip_prefix("conninfo") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ConnInfo);
        }
        // `\conninfo+` — verbose variant: shows psql line plus pooler/provider.
        if let Some(after_plus) = rest.strip_prefix('+') {
            if after_plus.is_empty() || after_plus.starts_with(char::is_whitespace) {
                return ParsedMeta {
                    cmd: MetaCmd::ConnInfo,
                    plus: true,
                    system: false,
                    pattern: None,
                    echo_hidden: false,
                    kind_filter: None,
                };
            }
        }
    }
    // `\commands` — list custom Lua meta-commands.  Must be checked before
    // `\copy`, `\cd`, and `\c` to avoid a spurious match.
    if let Some(rest) = input.strip_prefix("commands") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ListCustomCommands);
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
                kind_filter: None,
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
                kind_filter: None,
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
        kind_filter: None,
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
            kind_filter: None,
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
            kind_filter: None,
        };
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\l [pattern]`, `\log-file [path]`, and `\lo_*` large object commands.
///
/// `\log-file <path>` starts appending an audit log to `path`.
/// `\log-file` (no argument) stops the current audit log.
///
/// Large object commands (#400):
///   `\lo_import <filename> [<comment>]`
///   `\lo_export <loid> <filename>`
///   `\lo_list`
///   `\lo_unlink <loid>`
fn parse_l(input: &str) -> ParsedMeta {
    // `\lo_import`, `\lo_export`, `\lo_list`, `\lo_unlink` — checked before
    // `\log-file` and bare `\l` (longest prefix first).
    if let Some(rest) = input.strip_prefix("lo_") {
        return parse_lo_family(rest);
    }

    // `\log-file [path]` — must be checked before bare `\l` (longer prefix).
    if let Some(rest) = input.strip_prefix("log-file") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let path = rest.trim();
            return ParsedMeta::simple(MetaCmd::LogFile(if path.is_empty() {
                None
            } else {
                Some(path.to_owned())
            }));
        }
    }
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
        kind_filter: None,
    }
}

/// Parse `\lo_import`, `\lo_export`, `\lo_list`, `\lo_unlink`.
///
/// `input` is the string after the `lo_` prefix, e.g. `"import /tmp/file.bin"`.
fn parse_lo_family(input: &str) -> ParsedMeta {
    // `\lo_import <filename> [<comment>]`
    if let Some(rest) = input.strip_prefix("import") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let rest = rest.trim();
            if rest.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown("lo_import".to_owned()));
            }
            // Split into filename and optional comment.
            let (filename, comment) = split_lo_args(rest);
            return ParsedMeta::simple(MetaCmd::LoImport(filename, comment));
        }
    }

    // `\lo_export <loid> <filename>`
    if let Some(rest) = input.strip_prefix("export") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let rest = rest.trim();
            if rest.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown("lo_export".to_owned()));
            }
            let (loid, filename) = split_lo_args(rest);
            if filename.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown("lo_export".to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::LoExport(loid, filename));
        }
    }

    // `\lo_list`
    if let Some(rest) = input.strip_prefix("list") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::LoList);
        }
    }

    // `\lo_unlink <loid>`
    if let Some(rest) = input.strip_prefix("unlink") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let loid = rest.trim().to_owned();
            if loid.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown("lo_unlink".to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::LoUnlink(loid));
        }
    }

    ParsedMeta::simple(MetaCmd::Unknown(format!("lo_{input}")))
}

/// Split a two-token argument string into `(first, rest)`.
///
/// The first token is whitespace-delimited.  Everything after the first token
/// (trimmed) is returned as the second element.  When only one token is
/// present the second element is an empty string.
///
/// Quoted first arguments (single-quoted) are supported to allow filenames
/// with spaces, matching psql behaviour.
fn split_lo_args(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(after_open) = s.strip_prefix('\'') {
        // Quoted first argument.
        if let Some(close) = after_open.find('\'') {
            let first = after_open[..close].to_owned();
            let rest = after_open[close + 1..].trim().to_owned();
            return (first, rest);
        }
        // Unterminated quote — treat whole string as first arg.
        return (s.to_owned(), String::new());
    }
    let mut parts = s.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").to_owned();
    let rest = parts.next().map_or("", str::trim).to_owned();
    (first, rest)
}

// ---------------------------------------------------------------------------
// I/O and utility command parsers (#33)
// ---------------------------------------------------------------------------

/// Parse `\e [file [line]]`, `\echo text`, `\encoding [enc]`, `\elif expr`,
/// `\else`, and `\endif`.
///
/// Longer prefixes are checked first to avoid false prefix matches.
fn parse_e_family(input: &str) -> ParsedMeta {
    // `\explain share <service>` — upload last EXPLAIN plan to a visualiser.
    // Must be checked before bare `\e` (longer prefix first).
    if let Some(rest) = input.strip_prefix("explain") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let args = rest.trim();
            // `\explain share depesz` / `\explain share dalibo`
            if let Some(share_rest) = args.strip_prefix("share") {
                let service = share_rest.trim().to_lowercase();
                return ParsedMeta::simple(MetaCmd::ExplainShare(service));
            }
            // Bare `\explain` or unknown sub-command — fall through to Unknown.
            return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
        }
    }

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
                kind_filter: None,
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
                kind_filter: None,
            };
        }
    }

    // `\elif <expression>` — expression carried in the variant.
    if let Some(rest) = input.strip_prefix("elif") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let expr = rest.trim().to_owned();
            return ParsedMeta::simple(MetaCmd::Elif(expr));
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
                kind_filter: None,
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
    // `\if <expression>` — expression carried in the variant.
    if let Some(rest) = input.strip_prefix("if") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let expr = rest.trim().to_owned();
            return ParsedMeta::simple(MetaCmd::If(expr));
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
                kind_filter: None,
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
                kind_filter: None,
            };
        }
    }
    ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()))
}

/// Parse `\o [file]`.
fn parse_o(input: &str) -> ParsedMeta {
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
            kind_filter: None,
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
    // `\refresh` — reload schema cache (check before bare `\r`).
    if let Some(rest) = input.strip_prefix("refresh") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::RefreshSchema);
        }
    }
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
                kind_filter: None,
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
                kind_filter: None,
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
                kind_filter: None,
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
                kind_filter: None,
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
    // `\profiles` — list all configured connection profiles.
    if let Some(rest) = input.strip_prefix("profiles") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return ParsedMeta::simple(MetaCmd::ListProfiles);
        }
    }
    // `\prompt [text] name` — prompt for input into a variable.
    if let Some(rest) = input.strip_prefix("prompt") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return parse_prompt(input);
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
        kind_filter: None,
    }
}

// ---------------------------------------------------------------------------
// \b family parser — extended query protocol commands (#57)
// ---------------------------------------------------------------------------

/// Split a whitespace-separated argument string into individual tokens.
///
/// A token may be single-quoted to include spaces: `'hello world'` → one
/// token.  Inside a single-quoted token both `''` (doubled) and `\'`
/// (backslash-escaped) produce a literal `'`, matching psql behaviour.
/// Unquoted tokens are delimited by ASCII whitespace.
pub(crate) fn split_params(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = s.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '\'' {
            // Quoted token: consume until closing `'`.
            // Both `''` (doubled) and `\'` (backslash) escape a literal `'`.
            chars.next(); // consume opening quote
            let mut token = String::new();
            loop {
                match chars.next() {
                    None => break,
                    Some('\'') => {
                        if chars.peek() == Some(&'\'') {
                            // Doubled single-quote → literal `'`.
                            chars.next();
                            token.push('\'');
                        } else {
                            break;
                        }
                    }
                    Some('\\') => {
                        if chars.peek() == Some(&'\'') {
                            // Backslash-escaped single-quote → literal `'`.
                            chars.next();
                            token.push('\'');
                        } else {
                            token.push('\\');
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

/// Parse `\ns name query`, `\nd name`, `\np name`, `\n+`, and `\n name [args...]`.
///
/// Disambiguation order (longest match first):
///   `ns` → [`MetaCmd::NamedSave`]
///   `nd` → [`MetaCmd::NamedDelete`]
///   `np` → [`MetaCmd::NamedPrint`]
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

    // `\np name` — print a named query without executing.  Must come before bare `\n`.
    if let Some(rest) = input.strip_prefix("np") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let name = rest.trim().to_owned();
            if name.is_empty() {
                return ParsedMeta::simple(MetaCmd::Unknown(input.to_owned()));
            }
            return ParsedMeta::simple(MetaCmd::NamedPrint(name));
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
                kind_filter: None,
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
                kind_filter: None,
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
    ("dl", MetaCmd::LoList),
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
    ("dy", MetaCmd::ListEventTriggers),
    ("do", MetaCmd::ListOperators),
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

    // \dfn / \dfp / \dfa / \dfw — psql function-kind filter variants.
    // Check before the generic D_SUBCMDS loop so the longer prefix wins.
    if let Some(rest) = input.strip_prefix("df") {
        // The next char (if present) may be a kind qualifier: n, p, a, w.
        // Anything else falls through to the normal \df handling below.
        let (kind_char, rest2) = match rest.chars().next() {
            Some(k @ ('n' | 'p' | 'a' | 'w')) => (Some(k), &rest[1..]),
            _ => (None, rest),
        };
        if kind_char.is_some() || rest2.starts_with(|c: char| c.is_whitespace() || c == '+' || c == 'S') || rest2.is_empty() {
            let (plus, system, pattern) = parse_modifiers_and_pattern(rest2);
            return ParsedMeta {
                cmd: MetaCmd::ListFunctions,
                plus,
                system,
                pattern,
                echo_hidden: false,
                kind_filter: kind_char,
            };
        }
    }

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
                kind_filter: None,
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
        kind_filter: None,
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
        assert!(
            !parse("\\conninfo").plus,
            "bare \\conninfo must not set plus"
        );
    }

    #[test]
    fn parse_conninfo_plus() {
        let m = parse("\\conninfo+");
        assert_eq!(m.cmd, MetaCmd::ConnInfo);
        assert!(m.plus, "\\conninfo+ must set plus=true");
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
    fn parse_dy_event_triggers() {
        assert_eq!(parse("\\dy").cmd, MetaCmd::ListEventTriggers);
    }

    #[test]
    fn parse_dy_plus_modifier() {
        let m = parse("\\dy+");
        assert_eq!(m.cmd, MetaCmd::ListEventTriggers);
        assert!(m.plus);
    }

    #[test]
    fn parse_dy_with_pattern() {
        let m = parse("\\dy my_trigger");
        assert_eq!(m.cmd, MetaCmd::ListEventTriggers);
        assert_eq!(m.pattern, Some("my_trigger".to_owned()));
    }

    #[test]
    fn parse_do_operators() {
        assert_eq!(parse("\\do").cmd, MetaCmd::ListOperators);
    }

    #[test]
    fn parse_do_operators_plus_modifier() {
        let m = parse("\\do+");
        assert_eq!(m.cmd, MetaCmd::ListOperators);
        assert!(m.plus);
    }

    #[test]
    fn parse_do_operators_system_modifier() {
        let m = parse("\\doS");
        assert_eq!(m.cmd, MetaCmd::ListOperators);
        assert!(m.system);
    }

    #[test]
    fn parse_do_operators_with_pattern() {
        let m = parse("\\do my_op");
        assert_eq!(m.cmd, MetaCmd::ListOperators);
        assert_eq!(m.pattern, Some("my_op".to_owned()));
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
        // Multiple unquoted value tokens are concatenated without separators,
        // matching psql behaviour: `\set X hello world` → X = "helloworld".
        assert_eq!(
            parse("\\set X hello world").cmd,
            MetaCmd::Set("X".to_owned(), "helloworld".to_owned())
        );
    }

    #[test]
    fn parse_set_value_quoted_concat() {
        // Single-quoted tokens strip quotes; `\'` inside a quoted token
        // escapes a single-quote.  Adjacent tokens are concatenated:
        //   `\set d '\'' val '\''`  →  d = "'val'"
        // This is the pattern used by postgres_dba to wrap user input in
        // single quotes for safe SQL interpolation.
        assert_eq!(
            parse("\\set d '\\'' val '\\''").cmd,
            MetaCmd::Set("d".to_owned(), "'val'".to_owned())
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

    // -- \prompt (#203) -------------------------------------------------------

    #[test]
    fn parse_prompt_var_only() {
        // One argument — it is the variable name, no prompt text.
        assert_eq!(
            parse("\\prompt myvar").cmd,
            MetaCmd::Prompt(String::new(), "myvar".to_owned())
        );
    }

    #[test]
    fn parse_prompt_text_and_var() {
        // Two unquoted tokens: first is prompt text, second is var name.
        assert_eq!(
            parse("\\prompt Enter: myvar").cmd,
            MetaCmd::Prompt("Enter:".to_owned(), "myvar".to_owned())
        );
    }

    #[test]
    fn parse_prompt_single_quoted_text() {
        assert_eq!(
            parse("\\prompt 'Enter value: ' myvar").cmd,
            MetaCmd::Prompt("Enter value: ".to_owned(), "myvar".to_owned())
        );
    }

    #[test]
    fn parse_prompt_double_quoted_text() {
        assert_eq!(
            parse("\\prompt \"Enter value: \" myvar").cmd,
            MetaCmd::Prompt("Enter value: ".to_owned(), "myvar".to_owned())
        );
    }

    #[test]
    fn parse_prompt_bare_is_unknown() {
        // Bare `\prompt` with no variable name is an error.
        assert!(matches!(parse("\\prompt").cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_prompt_not_confused_with_print_buffer() {
        // \prompt starts with 'p', must not match \p
        assert!(matches!(parse("\\prompt myvar").cmd, MetaCmd::Prompt(_, _)));
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    // -- \if / \elif / \else / \endif (#393) ---------------------------------

    #[test]
    fn parse_if_with_expression() {
        let m = parse("\\if true");
        assert_eq!(m.cmd, MetaCmd::If("true".to_owned()));
    }

    #[test]
    fn parse_if_bare_no_expression() {
        // Bare `\if` without an expression results in an empty string payload.
        let m = parse("\\if");
        assert_eq!(m.cmd, MetaCmd::If(String::new()));
    }

    #[test]
    fn parse_elif_with_expression() {
        let m = parse("\\elif false");
        assert_eq!(m.cmd, MetaCmd::Elif("false".to_owned()));
    }

    #[test]
    fn parse_elif_bare() {
        // Bare `\elif` — empty expression string.
        let m = parse("\\elif");
        assert_eq!(m.cmd, MetaCmd::Elif(String::new()));
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
        assert!(matches!(parse("\\if true").cmd, MetaCmd::If(_)));
        assert_eq!(parse("\\i file.sql").cmd, MetaCmd::Include);
        assert_eq!(parse("\\ir file.sql").cmd, MetaCmd::IncludeRelative);
    }

    #[test]
    fn parse_elif_not_confused_with_echo_or_encoding() {
        // \elif, \else, \endif start with 'e'; must not match \echo, \encoding, \e
        assert!(matches!(parse("\\elif true").cmd, MetaCmd::Elif(_)));
        assert_eq!(parse("\\else").cmd, MetaCmd::Else);
        assert_eq!(parse("\\endif").cmd, MetaCmd::Endif);
        assert_eq!(parse("\\echo hello").cmd, MetaCmd::Echo);
        assert_eq!(parse("\\encoding UTF8").cmd, MetaCmd::Encoding);
        assert_eq!(parse("\\e").cmd, MetaCmd::Edit);
    }

    #[test]
    fn parse_if_variable_expression() {
        // Variable interpolation has already occurred before parsing; the
        // parser stores the resulting string in the variant payload.
        let m = parse("\\if on");
        assert_eq!(m.cmd, MetaCmd::If("on".to_owned()));
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

    // -- \version ------------------------------------------------------------

    #[test]
    fn parse_version() {
        assert_eq!(parse("\\version").cmd, MetaCmd::Version);
    }

    #[test]
    fn parse_version_not_confused_with_other_v_prefixes() {
        // \version must parse cleanly; a partial match like \v alone is Unknown.
        assert_eq!(parse("\\version").cmd, MetaCmd::Version);
        assert!(matches!(parse("\\v").cmd, MetaCmd::Unknown(_)));
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

    #[test]
    fn parse_dba_progress() {
        let m = parse("\\dba progress");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("progress".to_owned()));
    }

    #[test]
    fn parse_dba_progress_vacuum() {
        // Two-word subcommand: entire rest of line is the pattern.
        let m = parse("\\dba progress vacuum");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("progress vacuum".to_owned()));
    }

    #[test]
    fn parse_dba_invalid_idx() {
        let m = parse("\\dba invalid-idx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("invalid-idx".to_owned()));
    }

    #[test]
    fn parse_dba_iidx_alias() {
        let m = parse("\\dba iidx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("iidx".to_owned()));
    }

    #[test]
    fn parse_dba_redundant_idx() {
        let m = parse("\\dba redundant-idx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("redundant-idx".to_owned()));
    }

    #[test]
    fn parse_dba_ridx_alias() {
        let m = parse("\\dba ridx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("ridx".to_owned()));
    }

    #[test]
    fn parse_dba_missing_fk_idx() {
        let m = parse("\\dba missing-fk-idx");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("missing-fk-idx".to_owned()));
    }

    #[test]
    fn parse_dba_mfk_alias() {
        let m = parse("\\dba mfk");
        assert_eq!(m.cmd, MetaCmd::Dba);
        assert_eq!(m.pattern, Some("mfk".to_owned()));
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

    #[test]
    fn parse_named_print() {
        let m = parse("\\np top_tables");
        assert_eq!(m.cmd, MetaCmd::NamedPrint("top_tables".to_owned()));
    }

    #[test]
    fn parse_named_print_missing_name_is_unknown() {
        let m = parse("\\np");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_named_print_np_not_confused_with_n() {
        // `\np` must not be mistaken for `\n` with arg `p`.
        let m = parse("\\np my_q");
        assert!(matches!(m.cmd, MetaCmd::NamedPrint(_)));
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
    fn parse_interactive_mode() {
        assert_eq!(parse("\\interactive").cmd, MetaCmd::InteractiveMode);
    }

    #[test]
    fn parse_plan_does_not_steal_p() {
        // Bare \p should still be PrintBuffer, not PlanMode
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    #[test]
    fn parse_o_bare_is_output() {
        assert_eq!(parse("\\o").cmd, MetaCmd::Output);
    }

    // -- \profiles -----------------------------------------------------------

    #[test]
    fn parse_profiles_bare() {
        assert_eq!(parse("\\profiles").cmd, MetaCmd::ListProfiles);
    }

    #[test]
    fn parse_profiles_with_leading_backslash() {
        assert_eq!(parse("profiles").cmd, MetaCmd::ListProfiles);
    }

    #[test]
    fn parse_profiles_does_not_steal_p() {
        // Bare \p should still be PrintBuffer, not ListProfiles
        assert_eq!(parse("\\p").cmd, MetaCmd::PrintBuffer);
    }

    #[test]
    fn parse_profiles_not_confused_with_pset() {
        assert_eq!(
            parse("\\pset format").cmd,
            MetaCmd::Pset("format".to_owned(), None)
        );
        assert_eq!(parse("\\profiles").cmd, MetaCmd::ListProfiles);
    }

    #[test]
    fn parse_profiles_not_confused_with_plan() {
        assert_eq!(parse("\\plan").cmd, MetaCmd::PlanMode);
        assert_eq!(parse("\\profiles").cmd, MetaCmd::ListProfiles);
    }

    // -- Function-key aliases (#321) ----------------------------------------

    #[test]
    fn parse_f2_toggle_completion() {
        assert_eq!(parse("\\f2").cmd, MetaCmd::ToggleCompletion);
    }

    #[test]
    fn parse_f3_toggle_single_line() {
        assert_eq!(parse("\\f3").cmd, MetaCmd::ToggleSingleLine);
    }

    #[test]
    fn parse_f4_toggle_vi_emacs() {
        assert_eq!(parse("\\f4").cmd, MetaCmd::ToggleViEmacs);
    }

    #[test]
    fn parse_f5_toggle_auto_explain() {
        assert_eq!(parse("\\f5").cmd, MetaCmd::ToggleAutoExplain);
    }

    #[test]
    fn parse_f2_not_confused_with_field_sep() {
        // `\f` bare → FieldSep; `\f2` → ToggleCompletion
        assert_eq!(parse("\\f").cmd, MetaCmd::FieldSep(None));
        assert_eq!(parse("\\f2").cmd, MetaCmd::ToggleCompletion);
    }

    #[test]
    fn parse_field_sep_still_works_after_f_family_refactor() {
        assert_eq!(parse("\\f").cmd, MetaCmd::FieldSep(None));
        assert_eq!(parse("\\f ,").cmd, MetaCmd::FieldSep(Some(",".to_owned())));
    }

    // -- \log-file (FR-23) --------------------------------------------------

    #[test]
    fn parse_log_file_with_path() {
        let m = parse("\\log-file foo.log");
        assert_eq!(m.cmd, MetaCmd::LogFile(Some("foo.log".to_owned())));
    }

    #[test]
    fn parse_log_file_with_absolute_path() {
        let m = parse("\\log-file /tmp/queries.log");
        assert_eq!(m.cmd, MetaCmd::LogFile(Some("/tmp/queries.log".to_owned())));
    }

    #[test]
    fn parse_log_file_with_tilde_path() {
        let m = parse("\\log-file ~/.local/share/rpg/queries.log");
        assert_eq!(
            m.cmd,
            MetaCmd::LogFile(Some("~/.local/share/rpg/queries.log".to_owned()))
        );
    }

    #[test]
    fn parse_log_file_no_arg_stops_logging() {
        let m = parse("\\log-file");
        assert_eq!(m.cmd, MetaCmd::LogFile(None));
    }

    #[test]
    fn parse_log_file_not_confused_with_list_databases() {
        // `\l` must still work after `\log-file` is added.
        assert_eq!(parse("\\l").cmd, MetaCmd::ListDatabases);
        assert_eq!(parse("\\l mydb").cmd, MetaCmd::ListDatabases);
    }

    #[test]
    fn parse_log_file_not_confused_with_list_databases_plus() {
        assert_eq!(parse("\\l+").cmd, MetaCmd::ListDatabases);
        let m = parse("\\l+");
        assert!(m.plus);
    }

    // -- \lo_* large object commands (#400) ---------------------------------

    #[test]
    fn parse_lo_import_basic() {
        let m = parse("\\lo_import /tmp/file.bin");
        assert_eq!(
            m.cmd,
            MetaCmd::LoImport("/tmp/file.bin".to_owned(), String::new())
        );
    }

    #[test]
    fn parse_lo_import_with_comment() {
        let m = parse("\\lo_import /tmp/file.bin my comment");
        assert_eq!(
            m.cmd,
            MetaCmd::LoImport("/tmp/file.bin".to_owned(), "my comment".to_owned())
        );
    }

    #[test]
    fn parse_lo_import_quoted_filename() {
        let m = parse("\\lo_import '/path/with spaces/file.bin' a comment");
        assert_eq!(
            m.cmd,
            MetaCmd::LoImport(
                "/path/with spaces/file.bin".to_owned(),
                "a comment".to_owned()
            )
        );
    }

    #[test]
    fn parse_lo_import_no_args_is_unknown() {
        let m = parse("\\lo_import");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_lo_export_basic() {
        let m = parse("\\lo_export 12345 /tmp/out.bin");
        assert_eq!(
            m.cmd,
            MetaCmd::LoExport("12345".to_owned(), "/tmp/out.bin".to_owned())
        );
    }

    #[test]
    fn parse_lo_export_no_args_is_unknown() {
        let m = parse("\\lo_export");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_lo_export_one_arg_is_unknown() {
        let m = parse("\\lo_export 12345");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_lo_list() {
        assert_eq!(parse("\\lo_list").cmd, MetaCmd::LoList);
    }

    #[test]
    fn parse_dl_alias_for_lo_list() {
        assert_eq!(parse("\\dl").cmd, MetaCmd::LoList);
    }

    #[test]
    fn parse_lo_unlink_basic() {
        let m = parse("\\lo_unlink 12345");
        assert_eq!(m.cmd, MetaCmd::LoUnlink("12345".to_owned()));
    }

    #[test]
    fn parse_lo_unlink_no_args_is_unknown() {
        let m = parse("\\lo_unlink");
        assert!(matches!(m.cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_lo_commands_not_confused_with_log_file() {
        // `\log-file` must still work after `\lo_*` is added.
        let m = parse("\\log-file /tmp/q.log");
        assert_eq!(m.cmd, MetaCmd::LogFile(Some("/tmp/q.log".to_owned())));
    }

    #[test]
    fn parse_lo_commands_not_confused_with_list_databases() {
        // `\l` must still work after `\lo_*` is added.
        assert_eq!(parse("\\l").cmd, MetaCmd::ListDatabases);
    }

    // -- \explain share (#655) -----------------------------------------------

    #[test]
    fn parse_explain_share_depesz() {
        let m = parse("\\explain share depesz");
        assert_eq!(m.cmd, MetaCmd::ExplainShare("depesz".to_owned()));
    }

    #[test]
    fn parse_explain_share_dalibo() {
        let m = parse("\\explain share dalibo");
        assert_eq!(m.cmd, MetaCmd::ExplainShare("dalibo".to_owned()));
    }

    #[test]
    fn parse_explain_share_normalises_case() {
        // Service name is lowercased by the parser.
        let m = parse("\\explain share DEPESZ");
        assert_eq!(m.cmd, MetaCmd::ExplainShare("depesz".to_owned()));
    }

    #[test]
    fn parse_explain_bare_is_unknown() {
        // `\explain` with no subcommand is Unknown (no sub-command given).
        assert!(matches!(parse("\\explain").cmd, MetaCmd::Unknown(_)));
    }

    #[test]
    fn parse_explain_share_pgmustard() {
        let m = parse("\\explain share pgmustard");
        assert_eq!(m.cmd, MetaCmd::ExplainShare("pgmustard".to_owned()));
    }

    #[test]
    fn parse_explain_share_pgmustard_normalises_case() {
        let m = parse("\\explain share PGMUSTARD");
        assert_eq!(m.cmd, MetaCmd::ExplainShare("pgmustard".to_owned()));
    }

    #[test]
    fn parse_explain_share_not_confused_with_echo() {
        // `\echo` must still parse correctly.
        let m = parse("\\echo hello");
        assert_eq!(m.cmd, MetaCmd::Echo);
    }

    #[test]
    fn parse_explain_share_not_confused_with_encoding() {
        // `\encoding` must still parse correctly.
        let m = parse("\\encoding UTF8");
        assert_eq!(m.cmd, MetaCmd::Encoding);
    }

    // -- Custom Lua commands (#659) -----------------------------------------

    #[test]
    fn parse_list_custom_commands() {
        assert_eq!(parse("\\commands").cmd, MetaCmd::ListCustomCommands);
    }

    #[test]
    fn parse_list_custom_commands_bare() {
        // `\commands` with trailing whitespace is still valid.
        assert_eq!(parse("\\commands ").cmd, MetaCmd::ListCustomCommands);
    }

    #[test]
    fn parse_commands_not_confused_with_conninfo() {
        // `\conninfo` must not match `\commands`.
        assert_eq!(parse("\\conninfo").cmd, MetaCmd::ConnInfo);
    }

    #[test]
    fn parse_commands_not_confused_with_copy() {
        // `\copy` must still work.
        let m = parse("\\copy t from stdin");
        assert_eq!(m.cmd, MetaCmd::Copy("t from stdin".to_owned()));
    }

    #[test]
    fn parse_commands_not_confused_with_c_reconnect() {
        // Bare `\c` must still be Reconnect.
        assert_eq!(parse("\\c").cmd, MetaCmd::Reconnect);
    }

    // -- History (#history) -------------------------------------------------

    #[test]
    fn parse_history_bare() {
        // Bare `\s` — show all history.
        assert_eq!(parse("\\s").cmd, MetaCmd::History(None));
    }

    #[test]
    fn parse_history_with_filename() {
        // `\s /tmp/history.txt` — save to file.
        assert_eq!(
            parse("\\s /tmp/history.txt").cmd,
            MetaCmd::History(Some("/tmp/history.txt".to_owned()))
        );
    }

    #[test]
    fn parse_history_with_pattern() {
        // `\s orders` — filter history by pattern.
        assert_eq!(
            parse("\\s orders").cmd,
            MetaCmd::History(Some("orders".to_owned()))
        );
    }

    #[test]
    fn parse_history_not_confused_with_set() {
        // `\set` must not match `\s`.
        let m = parse("\\set FOO bar");
        assert_eq!(m.cmd, MetaCmd::Set("FOO".to_owned(), "bar".to_owned()));
    }

    #[test]
    fn parse_history_not_confused_with_session() {
        // `\session` must not match `\s`.
        assert_eq!(parse("\\session").cmd, MetaCmd::SessionList);
    }

    #[test]
    fn parse_history_not_confused_with_sql_mode() {
        // `\sql` must not match `\s`.
        assert_eq!(parse("\\sql").cmd, MetaCmd::SqlMode);
    }

    #[test]
    fn parse_history_not_confused_with_sf() {
        // `\sf` must not match `\s`.
        assert_eq!(parse("\\sf myfunc").cmd, MetaCmd::ShowFunctionSource);
    }

    #[test]
    fn parse_history_not_confused_with_sv() {
        // `\sv` must not match `\s`.
        assert_eq!(parse("\\sv myview").cmd, MetaCmd::ShowViewDef);
    }
}
