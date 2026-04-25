// Copyright 2026 Nikolay Samokhvalov / postgres.ai
// SPDX-License-Identifier: Apache-2.0

//! Slash command parser for rpg.
//!
//! Mirrors [`crate::metacmd`] for the `/`-namespace: a typed [`SlashCmd`]
//! enum and a [`parse`] function that turns a raw input string into a
//! [`ParsedSlash`].
//!
//! Backslash commands (`\`) are parsed by [`crate::metacmd`] — slash
//! commands (`/`) are parsed here.  See `docs/COMMANDS.md` for the
//! canonical command list and `docs/GLOSSARY.md` for vocabulary.

// ---------------------------------------------------------------------------
// SlashCmd enum
// ---------------------------------------------------------------------------

/// Recognised slash command types.
///
/// One variant per `/`-prefixed command currently dispatched by
/// [`crate::repl::ai_commands::dispatch_ai_command`].  See `docs/COMMANDS.md`
/// for the canonical list.  Variants carry only the fields actually consumed
/// by the existing handlers — additional state lives on [`crate::repl::ReplSettings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCmd {
    // -- AI commands -------------------------------------------------------
    /// `/ask <prompt>` — natural-language → SQL.
    ///
    /// `prompt` is the trimmed text after `/ask`; empty for a bare `/ask`,
    /// in which case the handler emits a usage message.
    Ask { prompt: String },
    /// `/fix [...]` — diagnose and fix the last error.
    ///
    /// Trailing arguments after `/fix` are ignored by the handler; the
    /// parser does not capture them.
    Fix,
    /// `/explain [query]` — explain the last (or given) query plan.
    ///
    /// `query` is empty for a bare `/explain`.
    Explain { query: String },
    /// `/optimize [query]` — suggest query optimisations.
    ///
    /// `query` is empty for a bare `/optimize`.
    Optimize { query: String },
    /// `/describe <table>` — AI-generated table description.
    ///
    /// `table` is empty for a bare `/describe`, in which case the handler
    /// emits a usage message.
    Describe { table: String },
    /// `/clear` — clear the AI conversation context.
    Clear,
    /// `/compact [focus]` — compact the conversation, optionally biasing
    /// retention toward a focus topic.
    ///
    /// `focus` is empty when omitted.
    Compact { focus: String },
    /// `/budget` — show token usage and remaining budget.
    Budget,
    /// `/init` — generate `.rpg.toml` and `POSTGRES.md`.
    Init,

    // -- DBA / diagnostics --------------------------------------------------
    /// `/dba [subcommand]` — database diagnostics.
    ///
    /// `subcommand` is empty for a bare `/dba`.  `plus` is `true` when the
    /// subcommand had a trailing `+` (e.g. `/dba activity+`).
    Dba { subcommand: String, plus: bool },
    /// `/ash [args]` — active-session-history TUI.
    ///
    /// `args` is the raw argument string after `/ash` (may be empty);
    /// flag parsing is delegated to the handler.
    Ash { args: String },

    // -- Modes -------------------------------------------------------------
    /// `/sql` — switch to SQL input mode.
    SqlMode,
    /// `/text2sql` / `/t2s` — switch to text-to-SQL input mode.
    Text2SqlMode,
    /// `/mode` — show current input and execution mode.
    ShowMode,
    /// `/plan` — enter plan execution mode.
    PlanMode,
    /// `/yolo` — YOLO mode: text2sql + auto-execute.
    YoloMode,
    /// `/interactive` — return to interactive (default) execution mode.
    InteractiveMode,

    // -- REPL management ---------------------------------------------------
    /// `/profiles` — list configured connection profiles.
    Profiles,
    /// `/refresh` — reload schema cache for tab completion.
    Refresh,
    /// `/session` or `/session list` — show recent sessions.
    SessionList,
    /// `/session save [name]` — save the current session.
    ///
    /// `name` is empty when omitted.
    SessionSave { name: String },
    /// `/session delete <id>` (alias `/session del <id>`) — delete a session.
    SessionDelete { id: String },
    /// `/session resume <id>` (alias `/session connect <id>`) — reconnect
    /// using a saved session.
    SessionResume { id: String },
    /// `/session <unrecognised>` — unknown subcommand.  The handler emits
    /// a usage hint mentioning the captured `sub`.
    SessionUnknown { sub: String },
    /// `/log-file [path]` — start (or stop, when `path` is `None`) query
    /// audit logging.
    LogFile { path: Option<String> },
    /// `/explain-share <service>` — upload last EXPLAIN plan to a visualiser.
    ///
    /// `service` is empty for a bare `/explain-share`, in which case the
    /// handler emits a usage message.
    ExplainShare { service: String },
    /// `/commands` — list custom Lua meta-commands.
    Commands,
    /// `/version` — show rpg version and build information.
    Version,
    /// `/rpg` — launch the rpg easter-egg game.
    Rpg,
    /// `/f2` — toggle schema-aware tab completion.
    F2,
    /// `/f3` — toggle single-line mode.
    F3,
    /// `/f4` — toggle Vi/Emacs editing mode.
    F4,
    /// `/f5` — toggle auto-EXPLAIN.
    F5,

    // -- Named queries -----------------------------------------------------
    /// `/ns <name> <query>` — save a named query.
    ///
    /// Both fields are empty for a bare `/ns`; the handler emits a usage
    /// message in that case.
    NamedSave { name: String, query: String },
    /// `/n+` — list all named queries.
    NamedList,
    /// `/nd <name>` — delete a named query.
    ///
    /// `name` is empty for a bare `/nd`.
    NamedDelete { name: String },
    /// `/np <name>` — print a named query without executing it.
    ///
    /// `name` is empty for a bare `/np`.
    NamedPrint { name: String },
    /// `/n <name> [args...]` — execute a named query.
    ///
    /// `rest` is the raw argument string after `/n`; the handler splits it
    /// into name and arguments.  Empty for a bare `/n`.
    NamedExec { rest: String },

    // -- Fallback ----------------------------------------------------------
    /// Unrecognised slash command.  Carries the original input verbatim so
    /// the handler can print a precise error message.
    Unknown { input: String },
}

impl SlashCmd {
    /// Whether this command must pass the AI token-budget gate before
    /// executing.
    ///
    /// Replaces the hand-curated `is_budget_exempt` chain that lived in
    /// [`crate::repl::ai_commands::dispatch_ai_command`].  Only the five
    /// actual AI requests (`/ask`, `/fix`, `/explain`, `/optimize`,
    /// `/describe`) consume tokens; everything else — including the
    /// AI-management commands `/clear`, `/compact`, `/budget`, `/init` —
    /// is exempt so the user can still inspect or recover the conversation
    /// after exhausting the budget.
    pub fn requires_ai_budget(&self) -> bool {
        matches!(
            self,
            Self::Ask { .. }
                | Self::Fix
                | Self::Explain { .. }
                | Self::Optimize { .. }
                | Self::Describe { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// ParsedSlash
// ---------------------------------------------------------------------------

/// A fully parsed slash command.
///
/// Mirrors [`crate::metacmd::ParsedMeta`] in shape but carries only the
/// fields the existing handlers actually need.  The original input string
/// is preserved in `raw` so handlers (and `Unknown` error messages) can
/// quote what the user typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSlash {
    /// The recognised command type.
    pub cmd: SlashCmd,
    /// The original input string, trimmed of surrounding whitespace.
    pub raw: String,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a slash command string into a [`ParsedSlash`].
///
/// `input` must include the leading `/`; surrounding whitespace is trimmed
/// before parsing.  Unrecognised commands return [`SlashCmd::Unknown`].
///
/// The parser is recognise-only: it does not check semantic validity (e.g.
/// non-empty arguments, valid query names).  Handlers continue to perform
/// those checks and emit usage messages.
#[allow(clippy::too_many_lines)]
pub fn parse(input: &str) -> ParsedSlash {
    let trimmed = input.trim();
    let raw = trimmed.to_owned();

    // Helper: build a result with a given variant.
    let mk = |cmd: SlashCmd| ParsedSlash {
        cmd,
        raw: raw.clone(),
    };

    // Bare-only matches — exact equality.
    let cmd = match trimmed {
        "/clear" => return mk(SlashCmd::Clear),
        "/budget" => return mk(SlashCmd::Budget),
        "/init" => return mk(SlashCmd::Init),
        "/sql" => return mk(SlashCmd::SqlMode),
        "/text2sql" | "/t2s" => return mk(SlashCmd::Text2SqlMode),
        "/mode" => return mk(SlashCmd::ShowMode),
        "/plan" => return mk(SlashCmd::PlanMode),
        "/yolo" => return mk(SlashCmd::YoloMode),
        "/interactive" => return mk(SlashCmd::InteractiveMode),
        "/profiles" => return mk(SlashCmd::Profiles),
        "/refresh" => return mk(SlashCmd::Refresh),
        "/commands" => return mk(SlashCmd::Commands),
        "/version" => return mk(SlashCmd::Version),
        "/rpg" => return mk(SlashCmd::Rpg),
        "/f2" => return mk(SlashCmd::F2),
        "/f3" => return mk(SlashCmd::F3),
        "/f4" => return mk(SlashCmd::F4),
        "/f5" => return mk(SlashCmd::F5),
        "/n+" => return mk(SlashCmd::NamedList),
        _ => None::<SlashCmd>,
    };
    let _ = cmd; // unused arm reachability marker

    // -- AI commands -------------------------------------------------------

    if trimmed == "/ask" || trimmed.starts_with("/ask ") {
        let prompt = trimmed["/ask".len()..].trim().to_owned();
        return mk(SlashCmd::Ask { prompt });
    }
    if trimmed == "/fix" || trimmed.starts_with("/fix ") {
        return mk(SlashCmd::Fix);
    }
    // `/explain-share` must be checked before `/explain` so the longer
    // prefix wins.
    if trimmed == "/explain-share" || trimmed.starts_with("/explain-share ") {
        let service = trimmed["/explain-share".len()..].trim().to_owned();
        return mk(SlashCmd::ExplainShare { service });
    }
    if trimmed == "/explain" || trimmed.starts_with("/explain ") {
        let query = trimmed["/explain".len()..].trim().to_owned();
        return mk(SlashCmd::Explain { query });
    }
    if trimmed == "/optimize" || trimmed.starts_with("/optimize ") {
        let query = trimmed["/optimize".len()..].trim().to_owned();
        return mk(SlashCmd::Optimize { query });
    }
    if trimmed == "/describe" || trimmed.starts_with("/describe ") {
        let table = trimmed["/describe".len()..].trim().to_owned();
        return mk(SlashCmd::Describe { table });
    }
    if trimmed == "/compact" || trimmed.starts_with("/compact ") {
        let focus = trimmed["/compact".len()..].trim().to_owned();
        return mk(SlashCmd::Compact { focus });
    }

    // -- DBA / diagnostics -------------------------------------------------

    if trimmed == "/dba" || trimmed.starts_with("/dba ") {
        let rest = trimmed["/dba".len()..].trim();
        let plus = rest.ends_with('+');
        let subcommand = rest.trim_end_matches('+').trim().to_owned();
        return mk(SlashCmd::Dba { subcommand, plus });
    }
    if trimmed == "/ash" || trimmed.starts_with("/ash ") {
        let args = trimmed["/ash".len()..].trim().to_owned();
        return mk(SlashCmd::Ash { args });
    }

    // -- Session -----------------------------------------------------------

    if trimmed == "/session" || trimmed.starts_with("/session ") {
        let rest = trimmed["/session".len()..].trim();
        if rest.is_empty() {
            return mk(SlashCmd::SessionList);
        }
        let mut parts = rest.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("");
        let arg = parts.next().map_or("", str::trim).to_owned();
        let cmd = match sub {
            "list" => SlashCmd::SessionList,
            "save" => SlashCmd::SessionSave { name: arg },
            "delete" | "del" => SlashCmd::SessionDelete { id: arg },
            "resume" | "connect" => SlashCmd::SessionResume { id: arg },
            other => SlashCmd::SessionUnknown {
                sub: other.to_owned(),
            },
        };
        return mk(cmd);
    }

    // -- Log file ----------------------------------------------------------

    if trimmed == "/log-file" || trimmed.starts_with("/log-file ") {
        let rest = trimmed["/log-file".len()..].trim();
        let path = if rest.is_empty() {
            None
        } else {
            Some(rest.to_owned())
        };
        return mk(SlashCmd::LogFile { path });
    }

    // -- Named queries -----------------------------------------------------
    //
    // Order matters: `/ns`, `/nd`, `/np`, `/n+` (handled above as bare),
    // then `/n` last so `/ns ...` doesn't fall into `/n`.

    if trimmed == "/ns" || trimmed.starts_with("/ns ") {
        let rest = trimmed["/ns".len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_owned();
        let query = parts.next().map_or("", str::trim).to_owned();
        return mk(SlashCmd::NamedSave { name, query });
    }
    if trimmed == "/nd" || trimmed.starts_with("/nd ") {
        let name = trimmed["/nd".len()..].trim().to_owned();
        return mk(SlashCmd::NamedDelete { name });
    }
    if trimmed == "/np" || trimmed.starts_with("/np ") {
        let name = trimmed["/np".len()..].trim().to_owned();
        return mk(SlashCmd::NamedPrint { name });
    }
    if trimmed == "/n" || trimmed.starts_with("/n ") {
        let rest = trimmed["/n".len()..].trim().to_owned();
        return mk(SlashCmd::NamedExec { rest });
    }

    // -- Fallback ----------------------------------------------------------

    mk(SlashCmd::Unknown { input: raw.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- AI commands -------------------------------------------------------

    #[test]
    fn parse_ask_with_prompt() {
        let p = parse("/ask hello world");
        assert_eq!(
            p.cmd,
            SlashCmd::Ask {
                prompt: "hello world".to_owned()
            }
        );
        assert_eq!(p.raw, "/ask hello world");
    }

    #[test]
    fn parse_ask_bare() {
        let p = parse("/ask");
        assert_eq!(
            p.cmd,
            SlashCmd::Ask {
                prompt: String::new()
            }
        );
    }

    #[test]
    fn parse_fix_bare() {
        assert_eq!(parse("/fix").cmd, SlashCmd::Fix);
    }

    #[test]
    fn parse_fix_with_trailing() {
        // `/fix something` is still recognised as Fix — the trailing text
        // is ignored by the existing handler.
        assert_eq!(parse("/fix me").cmd, SlashCmd::Fix);
    }

    #[test]
    fn parse_explain_bare() {
        let p = parse("/explain");
        assert_eq!(
            p.cmd,
            SlashCmd::Explain {
                query: String::new()
            }
        );
    }

    #[test]
    fn parse_explain_with_query() {
        let p = parse("/explain select 1");
        assert_eq!(
            p.cmd,
            SlashCmd::Explain {
                query: "select 1".to_owned()
            }
        );
    }

    #[test]
    fn parse_explain_share_disambiguated_from_explain() {
        // `/explain-share <service>` must not be mis-parsed as Explain.
        let p = parse("/explain-share depesz");
        assert_eq!(
            p.cmd,
            SlashCmd::ExplainShare {
                service: "depesz".to_owned()
            }
        );
    }

    #[test]
    fn parse_explain_share_bare() {
        let p = parse("/explain-share");
        assert_eq!(
            p.cmd,
            SlashCmd::ExplainShare {
                service: String::new()
            }
        );
    }

    #[test]
    fn parse_optimize_bare() {
        let p = parse("/optimize");
        assert_eq!(
            p.cmd,
            SlashCmd::Optimize {
                query: String::new()
            }
        );
    }

    #[test]
    fn parse_optimize_with_query() {
        let p = parse("/optimize select 1");
        assert_eq!(
            p.cmd,
            SlashCmd::Optimize {
                query: "select 1".to_owned()
            }
        );
    }

    #[test]
    fn parse_describe_with_table() {
        let p = parse("/describe users");
        assert_eq!(
            p.cmd,
            SlashCmd::Describe {
                table: "users".to_owned()
            }
        );
    }

    #[test]
    fn parse_describe_bare() {
        let p = parse("/describe");
        assert_eq!(
            p.cmd,
            SlashCmd::Describe {
                table: String::new()
            }
        );
    }

    #[test]
    fn parse_clear() {
        assert_eq!(parse("/clear").cmd, SlashCmd::Clear);
    }

    #[test]
    fn parse_compact_bare() {
        let p = parse("/compact");
        assert_eq!(
            p.cmd,
            SlashCmd::Compact {
                focus: String::new()
            }
        );
    }

    #[test]
    fn parse_compact_with_focus() {
        let p = parse("/compact pricing");
        assert_eq!(
            p.cmd,
            SlashCmd::Compact {
                focus: "pricing".to_owned()
            }
        );
    }

    #[test]
    fn parse_budget() {
        assert_eq!(parse("/budget").cmd, SlashCmd::Budget);
    }

    #[test]
    fn parse_init() {
        assert_eq!(parse("/init").cmd, SlashCmd::Init);
    }

    // -- DBA / diagnostics --------------------------------------------------

    #[test]
    fn parse_dba_bare() {
        let p = parse("/dba");
        assert_eq!(
            p.cmd,
            SlashCmd::Dba {
                subcommand: String::new(),
                plus: false
            }
        );
    }

    #[test]
    fn parse_dba_subcommand() {
        let p = parse("/dba activity");
        assert_eq!(
            p.cmd,
            SlashCmd::Dba {
                subcommand: "activity".to_owned(),
                plus: false
            }
        );
    }

    #[test]
    fn parse_dba_subcommand_plus() {
        let p = parse("/dba activity+");
        assert_eq!(
            p.cmd,
            SlashCmd::Dba {
                subcommand: "activity".to_owned(),
                plus: true
            }
        );
    }

    #[test]
    fn parse_ash_bare() {
        let p = parse("/ash");
        assert_eq!(
            p.cmd,
            SlashCmd::Ash {
                args: String::new()
            }
        );
    }

    #[test]
    fn parse_ash_with_cpu() {
        let p = parse("/ash --cpu 8");
        assert_eq!(
            p.cmd,
            SlashCmd::Ash {
                args: "--cpu 8".to_owned()
            }
        );
    }

    // -- Modes -------------------------------------------------------------

    #[test]
    fn parse_sql_mode() {
        assert_eq!(parse("/sql").cmd, SlashCmd::SqlMode);
    }

    #[test]
    fn parse_text2sql_mode() {
        assert_eq!(parse("/text2sql").cmd, SlashCmd::Text2SqlMode);
    }

    #[test]
    fn parse_t2s_mode() {
        assert_eq!(parse("/t2s").cmd, SlashCmd::Text2SqlMode);
    }

    #[test]
    fn parse_mode_show() {
        assert_eq!(parse("/mode").cmd, SlashCmd::ShowMode);
    }

    #[test]
    fn parse_plan() {
        assert_eq!(parse("/plan").cmd, SlashCmd::PlanMode);
    }

    #[test]
    fn parse_yolo() {
        assert_eq!(parse("/yolo").cmd, SlashCmd::YoloMode);
    }

    #[test]
    fn parse_interactive() {
        assert_eq!(parse("/interactive").cmd, SlashCmd::InteractiveMode);
    }

    // -- REPL management ---------------------------------------------------

    #[test]
    fn parse_profiles() {
        assert_eq!(parse("/profiles").cmd, SlashCmd::Profiles);
    }

    #[test]
    fn parse_refresh() {
        assert_eq!(parse("/refresh").cmd, SlashCmd::Refresh);
    }

    #[test]
    fn parse_session_bare() {
        let p = parse("/session");
        assert_eq!(p.cmd, SlashCmd::SessionList);
    }

    #[test]
    fn parse_session_list() {
        let p = parse("/session list");
        assert_eq!(p.cmd, SlashCmd::SessionList);
    }

    #[test]
    fn parse_session_save_bare() {
        let p = parse("/session save");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionSave {
                name: String::new()
            }
        );
    }

    #[test]
    fn parse_session_save_named() {
        let p = parse("/session save my-work");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionSave {
                name: "my-work".to_owned()
            }
        );
    }

    #[test]
    fn parse_session_delete() {
        let p = parse("/session delete abc123");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionDelete {
                id: "abc123".to_owned()
            }
        );
    }

    #[test]
    fn parse_session_del_alias() {
        let p = parse("/session del abc123");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionDelete {
                id: "abc123".to_owned()
            }
        );
    }

    #[test]
    fn parse_session_resume() {
        let p = parse("/session resume xyz789");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionResume {
                id: "xyz789".to_owned()
            }
        );
    }

    #[test]
    fn parse_session_connect_alias() {
        let p = parse("/session connect xyz789");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionResume {
                id: "xyz789".to_owned()
            }
        );
    }

    #[test]
    fn parse_session_unknown_subcommand() {
        let p = parse("/session frobnicate xyz");
        assert_eq!(
            p.cmd,
            SlashCmd::SessionUnknown {
                sub: "frobnicate".to_owned()
            }
        );
    }

    #[test]
    fn parse_log_file_bare() {
        let p = parse("/log-file");
        assert_eq!(p.cmd, SlashCmd::LogFile { path: None });
    }

    #[test]
    fn parse_log_file_with_path() {
        let p = parse("/log-file /tmp/queries.log");
        assert_eq!(
            p.cmd,
            SlashCmd::LogFile {
                path: Some("/tmp/queries.log".to_owned())
            }
        );
    }

    #[test]
    fn parse_commands() {
        assert_eq!(parse("/commands").cmd, SlashCmd::Commands);
    }

    #[test]
    fn parse_version() {
        assert_eq!(parse("/version").cmd, SlashCmd::Version);
    }

    #[test]
    fn parse_rpg() {
        assert_eq!(parse("/rpg").cmd, SlashCmd::Rpg);
    }

    #[test]
    fn parse_f2() {
        assert_eq!(parse("/f2").cmd, SlashCmd::F2);
    }

    #[test]
    fn parse_f3() {
        assert_eq!(parse("/f3").cmd, SlashCmd::F3);
    }

    #[test]
    fn parse_f4() {
        assert_eq!(parse("/f4").cmd, SlashCmd::F4);
    }

    #[test]
    fn parse_f5() {
        assert_eq!(parse("/f5").cmd, SlashCmd::F5);
    }

    // -- Named queries -----------------------------------------------------

    #[test]
    fn parse_ns_save() {
        let p = parse("/ns top_users select * from users");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedSave {
                name: "top_users".to_owned(),
                query: "select * from users".to_owned()
            }
        );
    }

    #[test]
    fn parse_ns_bare() {
        // Bare `/ns` (no args) — name and query both empty; the handler
        // emits a usage message.
        let p = parse("/ns");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedSave {
                name: String::new(),
                query: String::new()
            }
        );
    }

    #[test]
    fn parse_n_plus_list() {
        // `/n+` (no space) lists named queries.
        assert_eq!(parse("/n+").cmd, SlashCmd::NamedList);
    }

    #[test]
    fn parse_nd_delete() {
        let p = parse("/nd top_users");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedDelete {
                name: "top_users".to_owned()
            }
        );
    }

    #[test]
    fn parse_nd_bare() {
        let p = parse("/nd");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedDelete {
                name: String::new()
            }
        );
    }

    #[test]
    fn parse_np_print() {
        let p = parse("/np top_users");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedPrint {
                name: "top_users".to_owned()
            }
        );
    }

    #[test]
    fn parse_n_exec() {
        let p = parse("/n top_users 42");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedExec {
                rest: "top_users 42".to_owned()
            }
        );
    }

    #[test]
    fn parse_n_bare() {
        let p = parse("/n");
        assert_eq!(
            p.cmd,
            SlashCmd::NamedExec {
                rest: String::new()
            }
        );
    }

    // -- Unknown -----------------------------------------------------------

    #[test]
    fn parse_unknown() {
        let p = parse("/foo bar");
        assert_eq!(
            p.cmd,
            SlashCmd::Unknown {
                input: "/foo bar".to_owned()
            }
        );
    }

    // -- requires_ai_budget ------------------------------------------------

    #[test]
    fn ai_commands_require_budget() {
        assert!(SlashCmd::Ask {
            prompt: String::new()
        }
        .requires_ai_budget());
        assert!(SlashCmd::Fix.requires_ai_budget());
        assert!(SlashCmd::Explain {
            query: String::new()
        }
        .requires_ai_budget());
        assert!(SlashCmd::Optimize {
            query: String::new()
        }
        .requires_ai_budget());
        assert!(SlashCmd::Describe {
            table: String::new()
        }
        .requires_ai_budget());
    }

    #[test]
    fn ai_management_commands_are_budget_exempt() {
        // /clear, /compact, /budget, /init manage the AI conversation
        // and must work even when the budget is exhausted.
        assert!(!SlashCmd::Clear.requires_ai_budget());
        assert!(!SlashCmd::Compact {
            focus: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::Budget.requires_ai_budget());
        assert!(!SlashCmd::Init.requires_ai_budget());
    }

    #[test]
    fn non_ai_commands_are_budget_exempt() {
        assert!(!SlashCmd::Dba {
            subcommand: String::new(),
            plus: false
        }
        .requires_ai_budget());
        assert!(!SlashCmd::Ash {
            args: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::SqlMode.requires_ai_budget());
        assert!(!SlashCmd::Text2SqlMode.requires_ai_budget());
        assert!(!SlashCmd::ShowMode.requires_ai_budget());
        assert!(!SlashCmd::PlanMode.requires_ai_budget());
        assert!(!SlashCmd::YoloMode.requires_ai_budget());
        assert!(!SlashCmd::InteractiveMode.requires_ai_budget());
        assert!(!SlashCmd::Profiles.requires_ai_budget());
        assert!(!SlashCmd::Refresh.requires_ai_budget());
        assert!(!SlashCmd::SessionList.requires_ai_budget());
        assert!(!SlashCmd::SessionSave {
            name: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::LogFile { path: None }.requires_ai_budget());
        assert!(!SlashCmd::ExplainShare {
            service: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::Commands.requires_ai_budget());
        assert!(!SlashCmd::Version.requires_ai_budget());
        assert!(!SlashCmd::Rpg.requires_ai_budget());
        assert!(!SlashCmd::F2.requires_ai_budget());
        assert!(!SlashCmd::F3.requires_ai_budget());
        assert!(!SlashCmd::F4.requires_ai_budget());
        assert!(!SlashCmd::F5.requires_ai_budget());
        assert!(!SlashCmd::NamedSave {
            name: String::new(),
            query: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::NamedList.requires_ai_budget());
        assert!(!SlashCmd::NamedDelete {
            name: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::NamedPrint {
            name: String::new()
        }
        .requires_ai_budget());
        assert!(!SlashCmd::NamedExec {
            rest: String::new()
        }
        .requires_ai_budget());
    }

    #[test]
    fn unknown_does_not_require_budget() {
        // Unknown slash commands should not block on the budget gate —
        // they fall through to a usage message regardless.
        assert!(!SlashCmd::Unknown {
            input: "/foo".to_owned()
        }
        .requires_ai_budget());
    }
}
