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
// SlashCmd enum (Red phase: tests only, no implementation yet)
// ---------------------------------------------------------------------------

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
        assert!(
            SlashCmd::Ask {
                prompt: String::new()
            }
            .requires_ai_budget()
        );
        assert!(SlashCmd::Fix.requires_ai_budget());
        assert!(
            SlashCmd::Explain {
                query: String::new()
            }
            .requires_ai_budget()
        );
        assert!(
            SlashCmd::Optimize {
                query: String::new()
            }
            .requires_ai_budget()
        );
        assert!(
            SlashCmd::Describe {
                table: String::new()
            }
            .requires_ai_budget()
        );
    }

    #[test]
    fn ai_management_commands_are_budget_exempt() {
        // /clear, /compact, /budget, /init manage the AI conversation
        // and must work even when the budget is exhausted.
        assert!(!SlashCmd::Clear.requires_ai_budget());
        assert!(
            !SlashCmd::Compact {
                focus: String::new()
            }
            .requires_ai_budget()
        );
        assert!(!SlashCmd::Budget.requires_ai_budget());
        assert!(!SlashCmd::Init.requires_ai_budget());
    }

    #[test]
    fn non_ai_commands_are_budget_exempt() {
        assert!(
            !SlashCmd::Dba {
                subcommand: String::new(),
                plus: false
            }
            .requires_ai_budget()
        );
        assert!(
            !SlashCmd::Ash {
                args: String::new()
            }
            .requires_ai_budget()
        );
        assert!(!SlashCmd::SqlMode.requires_ai_budget());
        assert!(!SlashCmd::Text2SqlMode.requires_ai_budget());
        assert!(!SlashCmd::ShowMode.requires_ai_budget());
        assert!(!SlashCmd::PlanMode.requires_ai_budget());
        assert!(!SlashCmd::YoloMode.requires_ai_budget());
        assert!(!SlashCmd::InteractiveMode.requires_ai_budget());
        assert!(!SlashCmd::Profiles.requires_ai_budget());
        assert!(!SlashCmd::Refresh.requires_ai_budget());
        assert!(!SlashCmd::SessionList.requires_ai_budget());
        assert!(
            !SlashCmd::SessionSave {
                name: String::new()
            }
            .requires_ai_budget()
        );
        assert!(!SlashCmd::LogFile { path: None }.requires_ai_budget());
        assert!(
            !SlashCmd::ExplainShare {
                service: String::new()
            }
            .requires_ai_budget()
        );
        assert!(!SlashCmd::Commands.requires_ai_budget());
        assert!(!SlashCmd::Version.requires_ai_budget());
        assert!(!SlashCmd::Rpg.requires_ai_budget());
        assert!(!SlashCmd::F2.requires_ai_budget());
        assert!(!SlashCmd::F3.requires_ai_budget());
        assert!(!SlashCmd::F4.requires_ai_budget());
        assert!(!SlashCmd::F5.requires_ai_budget());
        assert!(
            !SlashCmd::NamedSave {
                name: String::new(),
                query: String::new()
            }
            .requires_ai_budget()
        );
        assert!(!SlashCmd::NamedList.requires_ai_budget());
        assert!(
            !SlashCmd::NamedDelete {
                name: String::new()
            }
            .requires_ai_budget()
        );
        assert!(
            !SlashCmd::NamedPrint {
                name: String::new()
            }
            .requires_ai_budget()
        );
        assert!(
            !SlashCmd::NamedExec {
                rest: String::new()
            }
            .requires_ai_budget()
        );
    }

    #[test]
    fn unknown_does_not_require_budget() {
        // Unknown slash commands should not block on the budget gate —
        // they fall through to a usage message regardless.
        assert!(
            !SlashCmd::Unknown {
                input: "/foo".to_owned()
            }
            .requires_ai_budget()
        );
    }
}
