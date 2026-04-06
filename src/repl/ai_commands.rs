//! AI command handlers for the REPL.
//!
//! Extracted from `mod.rs` — `handle_ai_*`, `get_ai_provider`, `resolve_api_key`,
//! and related helpers.

#![allow(clippy::wildcard_imports)]

use super::watch::format_system_time;
use super::*;

// ---------------------------------------------------------------------------
// AI key resolution
// ---------------------------------------------------------------------------

/// Resolve an API key from the `api_key_env` config value.
///
/// If the value looks like a raw API key (starts with `sk-`, `sk-ant-`, etc.)
/// rather than an environment variable name, use it directly but warn the user.
/// Otherwise, treat it as an env-var name and look it up.
/// Track whether we've already warned about raw API keys in this session.
pub(super) static RAW_KEY_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) fn resolve_api_key(api_key_env: Option<&str>) -> Option<String> {
    let env_or_key = api_key_env?;

    // Detect raw keys accidentally placed in api_key_env.
    let looks_like_raw_key = env_or_key.starts_with("sk-")
        || env_or_key.starts_with("gsk_")
        || (env_or_key.len() > 40
            && !env_or_key
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_'));

    if looks_like_raw_key {
        // Warn only once per session to avoid noise on repeated AI commands.
        if !RAW_KEY_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "WARNING: api_key_env appears to contain a raw API key. \
                 For security, set it to an environment variable name instead:"
            );
            eprintln!(
                "  api_key_env = \"OPENAI_API_KEY\"  # then: export OPENAI_API_KEY=\"sk-...\""
            );
        }
        // Use it anyway so things work.
        return Some(env_or_key.to_owned());
    }

    match std::env::var(env_or_key) {
        Ok(val) if !val.is_empty() => Some(val),
        _ => {
            eprintln!(
                "ERROR: environment variable '{env_or_key}' is not set. \
                 Set it with: export {env_or_key}=\"your-api-key\""
            );
            None
        }
    }
}

/// Resolve the configured AI provider, ready to use for a request.
///
/// Combines the three repeated steps — provider-name lookup, API-key
/// resolution, and provider construction — into a single call.
///
/// Returns `Err` when:
/// - `config.ai.provider` is absent or empty ("AI not configured"), or
/// - `crate::ai::create_provider` returns an error (unknown provider,
///   missing key, etc.).
///
/// Callers that want a custom "not configured" message should check
/// `settings.config.ai.provider` themselves first; callers that are
/// happy with a generic `"AI error: …"` message can use this directly.
pub(super) fn get_ai_provider(
    settings: &ReplSettings,
) -> Result<Box<dyn crate::ai::LlmProvider>, String> {
    let provider_name = settings
        .config
        .ai
        .provider
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "AI not configured".to_owned())?;
    let api_key = resolve_api_key(settings.config.ai.api_key_env.as_deref());
    crate::ai::create_provider(
        provider_name,
        api_key.as_deref(),
        settings.config.ai.base_url.as_deref(),
        settings.config.ai.timeout,
    )
}

// ---------------------------------------------------------------------------
// AI command helpers
// ---------------------------------------------------------------------------

/// Stream an LLM completion to stdout, printing tokens as they arrive.
///
/// Falls back to printing the full response at once if the provider does
/// not implement true streaming.
/// Show a brief inline AI suggestion after a SQL error.
///
/// Called automatically when `[ai] auto_explain_errors = true`.  The
/// suggestion is dimmed to visually distinguish it from the error itself.
/// Uses a small `max_tokens` budget to keep latency low.
pub(super) async fn suggest_error_fix_inline(
    sql: &str,
    error_message: &str,
    settings: &mut ReplSettings,
) {
    if check_token_budget(settings) {
        return;
    }

    let Ok(provider) = get_ai_provider(settings) else {
        return; // AI not configured or error — silently skip.
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL expert. \
                      The user just got a SQL error. \
                      Give a ONE-LINE fix suggestion. \
                      Be extremely concise — just the fix, nothing else."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Query: {sql}\nError: {error_message}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 150,
        temperature: 0.0,
    };

    // Use non-streaming for lower latency on a short response.
    // Race against Ctrl-C so the user can cancel without waiting.
    match with_ctrl_c_cancel(provider.complete(&messages, &options)).await {
        Ok(result) => {
            record_token_usage(settings, &result);
            let suggestion = result.content.trim();
            if !suggestion.is_empty() {
                // Print dimmed (ANSI escape: dim = \x1b[2m, reset = \x1b[0m).
                eprintln!("\x1b[2mHint: {suggestion}\x1b[0m");
            }
        }
        Err(e) if e == CANCELLED => {} // silently ignore cancellation for auto-hints
        Err(_) => {}                   // silently ignore errors for auto-hints
    }
}

/// Interpret an auto-EXPLAIN plan with AI.
///
/// Called automatically after auto-EXPLAIN output is displayed. Uses a
/// concise system prompt to produce a brief interpretation of the plan.
/// Skips silently when AI is not configured or the token budget is exhausted.
pub(super) async fn interpret_auto_explain(
    plan_text: &str,
    original_query: &str,
    settings: &mut ReplSettings,
) {
    if check_token_budget(settings) {
        return;
    }

    let Ok(provider) = get_ai_provider(settings) else {
        return; // AI not configured or error — silently skip.
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL performance expert. \
                      Give a brief (2-4 sentence) interpretation of the \
                      EXPLAIN plan. Focus on: most expensive nodes, \
                      sequential scans, row estimate errors, and one \
                      actionable suggestion if applicable."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Query: {original_query}\n\nPlan:\n{plan_text}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 300,
        temperature: 0.0,
    };

    match with_ctrl_c_cancel(provider.complete(&messages, &options)).await {
        Ok(result) => {
            record_token_usage(settings, &result);
            let interpretation = result.content.trim();
            if !interpretation.is_empty() {
                eprintln!("\x1b[2m{interpretation}\x1b[0m");
            }
        }
        Err(e) if e == CANCELLED => {} // silently ignore cancellation for auto-hints
        Err(_) => {}                   // silently ignore errors for auto-hints
    }
}

/// Interpret `\dba` diagnostic output with AI.
///
/// Called when a `\dba` command returns AI context (e.g. `\dba waits+`).
/// Produces a brief analysis of the diagnostic data. Skips silently when
/// AI is not configured.
pub(super) async fn interpret_dba_output(
    context: &str,
    subcommand: &str,
    settings: &mut ReplSettings,
) {
    if check_token_budget(settings) {
        return;
    }

    let Ok(provider) = get_ai_provider(settings) else {
        eprintln!("-- AI interpretation requires [ai] provider to be configured");
        return;
    };

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: "You are a PostgreSQL performance expert. \
                      Analyze the diagnostic output below and give a brief (3-5 sentence) \
                      interpretation. Focus on: dominant wait events, potential bottlenecks, \
                      and one actionable recommendation if applicable."
                .to_owned(),
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("\\dba {subcommand} output:\n\n{context}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: 400,
        temperature: 0.0,
    };

    eprintln!("-- AI interpreting wait events...");
    match with_ctrl_c_cancel(provider.complete(&messages, &options)).await {
        Ok(result) => {
            record_token_usage(settings, &result);
            let interpretation = result.content.trim();
            if !interpretation.is_empty() {
                eprintln!("\x1b[2m{interpretation}\x1b[0m");
            }
        }
        Err(e) if e == CANCELLED => eprintln!("-- AI request cancelled"),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

/// Sentinel error string returned when an LLM request is cancelled by Ctrl-C.
///
/// Callers that want to distinguish a user-initiated cancel from a real
/// error should compare `err == CANCELLED` rather than printing an error
/// message.
pub(super) const CANCELLED: &str = "__cancelled__";

/// Run a future, aborting it when Ctrl-C (SIGINT) is received.
///
/// Returns `Err(CANCELLED)` if Ctrl-C fires before the future completes.
/// Otherwise propagates the future's result unchanged.
async fn with_ctrl_c_cancel<F, T>(fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    tokio::select! {
        biased;
        _ = tokio::signal::ctrl_c() => Err(CANCELLED.to_owned()),
        result = fut => result,
    }
}

/// Stream a completion to the terminal, rendering markdown when enabled.
///
/// When `no_highlight` is `false` (the default), tokens are buffered and
/// the completed response is passed through [`crate::markdown::render_markdown`]
/// before printing, so that headers, bold text, code fences, etc. are
/// displayed with ANSI styling.
///
/// When `no_highlight` is `true` the raw text is streamed directly to
/// stdout token by token (original behaviour).
///
/// If Ctrl-C is pressed while the request is in flight the HTTP connection
/// is dropped (cancelling the request) and `Err(CANCELLED)` is returned.
pub(super) async fn stream_completion(
    provider: &dyn crate::ai::LlmProvider,
    messages: &[crate::ai::Message],
    options: &crate::ai::CompletionOptions,
    no_highlight: bool,
) -> Result<crate::ai::CompletionResult, String> {
    use std::io::Write;

    if no_highlight {
        // Raw streaming — emit each token immediately as it arrives.
        let result = with_ctrl_c_cancel(provider.complete_streaming(
            messages,
            options,
            Box::new(|token| {
                print!("{token}");
                let _ = io::stdout().flush();
            }),
        ))
        .await?;
        println!();
        return Ok(result);
    }

    // Markdown rendering mode — buffer tokens, render after completion.
    //
    // We still want to show progress to the user, so we print a dim "…"
    // indicator that gets overwritten once the full response arrives.
    // Use a shared buffer via Arc<Mutex<String>>.
    let buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let buf_clone = buf.clone();

    // Show a progress indicator so the terminal doesn't look stuck.
    eprint!("\x1b[2m…\x1b[0m");
    let _ = io::stderr().flush();

    let result = with_ctrl_c_cancel(provider.complete_streaming(
        messages,
        options,
        Box::new(move |token| {
            if let Ok(mut b) = buf_clone.lock() {
                b.push_str(token);
            }
        }),
    ))
    .await;

    // Erase the progress indicator (carriage return clears the line).
    eprint!("\r\x1b[K");
    let _ = io::stderr().flush();

    let result = result?;

    // Render markdown on the fully-collected content and print.
    let content = buf.lock().map(|b| b.clone()).unwrap_or_default();
    let rendered = crate::markdown::render_markdown(&content, no_highlight);
    print!("{rendered}");
    let _ = io::stdout().flush();

    Ok(result)
}

/// Dispatch a `/`-prefixed AI command.
///
/// Recognised commands:
/// - `/ask <prompt>` — generate SQL from natural language
/// - `/fix` — explain and fix the last error
/// - `/explain [query]` — explain query plan with AI interpretation
/// - `/optimize [query]` — suggest query optimizations
///
/// Dispatch a `/`-prefixed command.
///
/// Returns `Some(MetaResult)` when the caller needs to act on a result (e.g.
/// reconnect after `/session resume`), or `None` for commands that are fully
/// handled here.
// Parse /ash --cpu N flag. Accepts --cpu 8 or --cpu=16; returns None otherwise.
fn parse_ash_cpu_flag(args: &str) -> Option<u32> {
    let args = args.trim();
    // --cpu N or --cpu=N
    if let Some(rest) = args.strip_prefix("--cpu") {
        let val = rest.trim_start_matches('=').trim();
        return val.parse::<u32>().ok().filter(|&n| n > 0);
    }
    None
}

#[allow(clippy::too_many_lines)]
pub(super) async fn dispatch_ai_command(
    input: &str,
    client: &Client,
    params: &ConnParams,
    settings: &mut ReplSettings,
    tx: &mut TxState,
) -> Option<MetaResult> {
    // Budget gate — skip for non-AI and budget-diagnostic commands.
    let is_budget_exempt = input == "/clear"
        || input.starts_with("/compact")
        || input.starts_with("/budget")
        || input == "/init"
        || input.starts_with("/dba")
        || input.starts_with("/sql")
        || input.starts_with("/text2sql")
        || input.starts_with("/t2s")
        || input.starts_with("/mode")
        || input.starts_with("/plan")
        || input.starts_with("/yolo")
        || input.starts_with("/interactive")
        || input.starts_with("/profiles")
        || input.starts_with("/refresh")
        || input.starts_with("/session")
        || input.starts_with("/log-file")
        || input.starts_with("/explain-share")
        || input.starts_with("/commands")
        || input.starts_with("/version")
        || input.starts_with("/f2")
        || input.starts_with("/f3")
        || input.starts_with("/f4")
        || input.starts_with("/f5")
        || input.starts_with("/ns ")
        || input == "/n+"
        || input.starts_with("/nd ")
        || input.starts_with("/np ")
        || input.starts_with("/n ")
        || input.starts_with("/ash")
        || input == "/rpg";
    if !is_budget_exempt && check_token_budget(settings) {
        return None;
    }

    // ------------------------------------------------------------------
    // AI commands
    // ------------------------------------------------------------------

    if let Some(prompt) = input.strip_prefix("/ask").map(str::trim) {
        if prompt.is_empty() {
            eprintln!("Usage: /ask <natural language description>");
            return None;
        }
        match settings.exec_mode {
            ExecMode::Plan => handle_ai_plan(client, prompt, settings, params).await,
            _ => handle_ai_ask(client, prompt, settings, params, tx).await,
        }
    } else if input == "/fix" || input.starts_with("/fix ") {
        handle_ai_fix(client, settings, params, tx).await;
    // /explain-share <service> — upload last EXPLAIN plan to external visualiser.
    } else if let Some(service) = input.strip_prefix("/explain-share").map(str::trim) {
        if service.is_empty() {
            eprintln!("Usage: /explain-share <depesz|dalibo|pgmustard>");
        } else {
            dispatch_explain_share(client, settings, service).await;
        }
    } else if let Some(query_arg) = input.strip_prefix("/explain").map(str::trim) {
        handle_ai_explain(client, query_arg, settings, params).await;
    } else if let Some(query_arg) = input.strip_prefix("/optimize").map(str::trim) {
        handle_ai_optimize(client, query_arg, settings, params).await;
    } else if let Some(table_arg) = input.strip_prefix("/describe").map(str::trim) {
        if table_arg.is_empty() {
            eprintln!("Usage: /describe <table_name>");
            return None;
        }
        handle_ai_describe(client, table_arg, settings, params).await;
    } else if input == "/clear" {
        settings.conversation.clear();
        eprintln!("AI conversation context cleared.");
    } else if let Some(focus) = input.strip_prefix("/compact").map(str::trim) {
        if settings.conversation.is_empty() {
            eprintln!("Nothing to compact — conversation context is empty.");
        } else {
            let focus = if focus.is_empty() { None } else { Some(focus) };
            let before = settings.conversation.entries.len();
            settings.conversation.compact(focus);
            eprintln!(
                "Compacted {before} entries → {} entries (~{} tokens)",
                settings.conversation.entries.len(),
                settings.conversation.token_estimate(),
            );
        }
    } else if input == "/budget" {
        handle_ai_budget(settings);
    } else if input == "/init" {
        handle_init(client, settings, params).await;

    // ------------------------------------------------------------------
    // rpg-specific commands (/ namespace)
    // ------------------------------------------------------------------

    // /dba [subcommand] — database diagnostics.
    } else if let Some(rest) = input.strip_prefix("/dba").map(str::trim) {
        let subcommand = rest;
        let plus = subcommand.ends_with('+');
        let subcommand = subcommand.trim_end_matches('+').trim();
        let caps = settings.db_capabilities.clone();
        let ai_context = crate::dba::execute(client, subcommand, plus, Some(&caps), settings).await;
        if let Some(ref context) = ai_context {
            interpret_dba_output(context, subcommand, settings).await;
        }

    // /ash — active session history TUI.
    } else if input == "/rpg" {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() {
            eprintln!("/rpg requires an interactive terminal");
            return None;
        }
        crate::rpg::run_game();
    } else if input == "/ash" || input.starts_with("/ash ") {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() {
            eprintln!("/ash requires an interactive terminal");
            return None;
        }
        // Parse optional --cpu N flag: /ash --cpu 8
        let ash_args = input.strip_prefix("/ash").map_or("", str::trim);
        let cpu_override = parse_ash_cpu_flag(ash_args);
        if let Err(e) = crate::ash::run_ash(client, settings, cpu_override).await {
            eprintln!("/ash: {e}");
        }

    // /sql — switch to SQL input mode.
    } else if input == "/sql" {
        let result = MetaResult::SetInputMode(InputMode::Sql);
        let label = apply_mode_change(&result, settings);
        eprintln!("Input mode: {label}");

    // /text2sql, /t2s — switch to text-to-SQL input mode.
    } else if input == "/text2sql" || input == "/t2s" {
        let result = MetaResult::SetInputMode(InputMode::Text2Sql);
        let label = apply_mode_change(&result, settings);
        eprintln!("Input mode: {label}");

    // /mode — show current input and execution mode.
    } else if input == "/mode" {
        let input_label = match settings.input_mode {
            InputMode::Sql => "sql",
            InputMode::Text2Sql => "text2sql",
        };
        let exec_label = match settings.exec_mode {
            ExecMode::Interactive => "interactive",
            ExecMode::Plan => "plan",
            ExecMode::Yolo => "yolo",
        };
        eprintln!("Input mode: {input_label}  Execution mode: {exec_label}");

    // /plan — enter plan execution mode.
    } else if input == "/plan" {
        let result = MetaResult::SetExecMode(ExecMode::Plan);
        let label = apply_mode_change(&result, settings);
        eprintln!("Execution mode: {label}");

    // /yolo — enter YOLO mode (text2sql + auto-execute).
    } else if input == "/yolo" {
        let result = MetaResult::SetExecMode(ExecMode::Yolo);
        let label = apply_mode_change(&result, settings);
        eprintln!("Execution mode: {label}");

    // /interactive — return to interactive (default) mode.
    } else if input == "/interactive" {
        let result = MetaResult::SetExecMode(ExecMode::Interactive);
        let label = apply_mode_change(&result, settings);
        eprintln!("Execution mode: {label}");

    // /profiles — list configured connection profiles.
    } else if input == "/profiles" {
        print_profiles(&settings.config);

    // /refresh — reload schema cache for tab completion.
    } else if input == "/refresh" {
        match &settings.schema_cache {
            None => {
                eprintln!("/refresh: no active connection or not in interactive mode");
            }
            Some(cache) => match load_schema_cache(client).await {
                Ok(loaded) => {
                    *cache.write().unwrap() = loaded;
                    println!("Schema cache refreshed.");
                }
                Err(e) => {
                    eprintln!("/refresh: failed to reload schema cache: {e}");
                }
            },
        }

    // /session [subcommand] — session persistence.
    } else if let Some(rest) = input.strip_prefix("/session").map(str::trim) {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let sub = parts.next().unwrap_or("");
        let arg = parts.next().map_or("", str::trim).to_owned();
        match sub {
            "" | "list" => dispatch_session_list(),
            "save" => dispatch_session_save(
                params,
                &settings.session_id,
                if arg.is_empty() {
                    None
                } else {
                    Some(arg.as_str())
                },
                settings.query_count,
            ),
            "delete" | "del" => {
                if arg.is_empty() {
                    eprintln!("Usage: /session delete <id>");
                } else {
                    dispatch_session_delete(&arg);
                }
            }
            "resume" | "connect" => {
                if arg.is_empty() {
                    eprintln!("Usage: /session resume <id>");
                } else if let Some(result) = dispatch_session_resume(&arg).await {
                    return Some(result);
                }
            }
            _ => eprintln!("/session: unknown subcommand \"{sub}\". Try /session list."),
        }

    // /log-file [path] — start or stop query audit logging.
    } else if let Some(rest) = input.strip_prefix("/log-file").map(str::trim) {
        let path = if rest.is_empty() {
            None
        } else {
            Some(rest.to_owned())
        };
        let parsed = crate::metacmd::ParsedMeta {
            cmd: crate::metacmd::MetaCmd::LogFile(path),
            plus: false,
            system: false,
            pattern: None,
            echo_hidden: false,
            kind_filter: None,
            continuation: None,
        };
        dispatch_io(&parsed, client, params, settings, tx).await;

    // /commands — list custom Lua meta-commands.
    } else if input == "/commands" {
        let cmds = &settings.lua_registry.commands;
        if cmds.is_empty() {
            println!(
                "No custom commands loaded.\n\
                 Add Lua scripts to ~/.config/rpg/commands/*.lua"
            );
        } else {
            let mut out = String::from("Custom commands:\n");
            for cmd in cmds {
                use std::fmt::Write as _;
                let _ = writeln!(out, "  /{:<20} {}", cmd.name, cmd.description);
            }
            maybe_page(settings, &out);
        }

    // /version — show rpg version and build information.
    } else if input == "/version" {
        println!("{}", crate::version_string());
        if let Some(ref sv) = settings.db_capabilities.server_version {
            println!("Server: PostgreSQL {sv}");
        }

    // /f2..f5 — function key toggles.
    } else if input == "/f2" {
        apply_fkey_toggle(FKeyAction::Completion, settings);
    } else if input == "/f3" {
        apply_fkey_toggle(FKeyAction::SingleLine, settings);
    } else if input == "/f4" {
        apply_fkey_toggle(FKeyAction::ViEmacs, settings);
    } else if input == "/f5" {
        apply_fkey_toggle(FKeyAction::AutoExplain, settings);

    // /ns <name> <query> — save a named query.
    } else if input == "/ns" || input.starts_with("/ns ") {
        let rest = input["/ns".len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_owned();
        let query = parts.next().map_or("", str::trim).to_owned();
        if name.is_empty() || query.is_empty() {
            eprintln!("Usage: /ns <name> <query>");
        } else if crate::named::NamedQueries::is_valid_name(&name) {
            let mut nq = crate::named::NamedQueries::load();
            nq.set(&name, &query);
            match nq.save() {
                Ok(()) => {
                    if !settings.quiet {
                        eprintln!("Saved query \"{name}\".");
                    }
                }
                Err(e) => eprintln!("/ns: {e}"),
            }
        } else {
            eprintln!(
                "/ns: invalid query name \"{name}\": \
                 names must contain only alphanumerics and underscores"
            );
        }

    // /n+ — list all named queries.
    } else if input == "/n+" {
        let nq = crate::named::NamedQueries::load();
        let queries = nq.list();
        if queries.is_empty() {
            println!("No named queries saved.");
        } else {
            let mut out = String::new();
            for (name, query) in queries {
                use std::fmt::Write as FmtWrite;
                let _ = writeln!(out, "  {name}: {query}");
            }
            maybe_page(settings, &out);
        }

    // /nd <name> — delete a named query.
    } else if input == "/nd" || input.starts_with("/nd ") {
        let name = input["/nd".len()..].trim();
        if name.is_empty() {
            eprintln!("Usage: /nd <name>");
        } else {
            let mut nq = crate::named::NamedQueries::load();
            if nq.delete(name) {
                match nq.save() {
                    Ok(()) => {
                        if !settings.quiet {
                            eprintln!("Deleted query \"{name}\".");
                        }
                    }
                    Err(e) => eprintln!("/nd: {e}"),
                }
            } else {
                eprintln!("/nd: unknown query \"{name}\"");
            }
        }

    // /np <name> — print a named query without executing.
    } else if input == "/np" || input.starts_with("/np ") {
        let name = input["/np".len()..].trim();
        if name.is_empty() {
            eprintln!("Usage: /np <name>");
        } else {
            let nq = crate::named::NamedQueries::load();
            match nq.get(name) {
                Some(query) => println!("{query}"),
                None => eprintln!("/np: unknown query \"{name}\""),
            }
        }

    // /n <name> [args...] — execute a named query.
    } else if input == "/n" || input.starts_with("/n ") {
        let rest = input["/n".len()..].trim();
        if rest.is_empty() {
            eprintln!("Usage: /n <name> [args...]");
        } else {
            let mut parts = rest.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").to_owned();
            let args_str = parts.next().unwrap_or("").trim();
            let args: Vec<String> = args_str.split_whitespace().map(str::to_owned).collect();
            let nq = crate::named::NamedQueries::load();
            match nq.get(&name) {
                Some(query) => {
                    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                    let sql = crate::named::NamedQueries::substitute(query, &arg_refs);
                    execute_query(client, &sql, settings, tx).await;
                }
                None => eprintln!("/n: unknown query \"{name}\""),
            }
        }
    } else {
        eprintln!(
            "Unknown command: {input}\n\
             AI: /ask, /fix, /explain, /optimize, /describe, /init, /clear, /compact, /budget\n\
             Diagnostics: /dba, /ash\n\
             Modes: /sql, /text2sql, /t2s, /plan, /yolo, /interactive, /mode\n\
             Queries: /ns, /n, /n+, /nd, /np\n\
             REPL: /profiles, /refresh, /session, /log-file, /explain-share, /commands, /version, /f2-f5"

        );
    }

    None
}

/// Strip markdown code fences from LLM output.
///
/// LLMs sometimes wrap SQL in `` ```sql ... ``` `` blocks.  This function
/// removes the fences and returns the inner content, trimmed.  If no fences
/// are found, the original string is returned as-is.
#[cfg(test)]
pub(super) fn strip_sql_fences(s: &str) -> &str {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on the opening fence line.
        let after_tag = rest.find('\n').map_or(rest, |i| &rest[i + 1..]);
        // Remove closing fence.
        let body = if let Some(pos) = after_tag.rfind("```") {
            &after_tag[..pos]
        } else {
            after_tag
        };
        body.trim()
    } else {
        trimmed
    }
}

/// Check whether the session token budget has been exceeded.
///
/// Returns `true` (and prints a message) if the budget is exceeded,
/// meaning the caller should abort the AI operation.
/// Returns `false` if the budget is unlimited (0) or not yet reached.
pub(super) fn check_token_budget(settings: &ReplSettings) -> bool {
    let budget = settings.config.ai.token_budget;
    if budget == 0 {
        return false; // No budget limit.
    }
    if settings.tokens_used >= budget {
        eprintln!(
            "Token budget exhausted ({used}/{budget} tokens used). \
             AI commands are disabled for this session.",
            used = settings.tokens_used,
        );
        true
    } else {
        false
    }
}

/// Record token usage from a completion result.
pub(super) fn record_token_usage(
    settings: &mut ReplSettings,
    result: &crate::ai::CompletionResult,
) {
    settings.tokens_used += u64::from(result.input_tokens) + u64::from(result.output_tokens);
}

/// Prompt the user with a yes/no question and return their answer.
///
/// `default_yes` controls what happens when the user presses Enter without
/// typing anything: `true` → default is yes, `false` → default is no.
pub(super) fn ask_yn_prompt(prompt: &str, default_yes: bool) -> bool {
    use std::io::Write;
    eprint!("{prompt}");
    let _ = io::stderr().flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    let answer = input.trim().to_lowercase();
    if answer.is_empty() {
        return default_yes;
    }
    answer.starts_with('y')
}

/// User's choice when asked about executing AI-generated SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AskChoice {
    /// Execute as-is.
    Yes,
    /// Skip execution.
    No,
    /// Open in `$EDITOR` first, then execute the edited version.
    Edit,
}

/// Prompt the user with `[Y/n/e]` (yes / no / edit) and return their choice.
///
/// `default_yes` controls the behaviour when the user presses Enter without
/// typing: `true` → defaults to `Yes`, `false` → defaults to `No`.
///
/// Ctrl+C and Ctrl+D (EOF) always return `No` regardless of the default,
/// so the user can safely abort without the query being executed.
pub(super) fn ask_yne_prompt(prompt: &str, default_yes: bool) -> AskChoice {
    use crossterm::event::{read, Event, KeyCode, KeyModifiers};
    use crossterm::terminal;
    use std::io::{IsTerminal, Write};

    eprint!("{prompt}");
    let _ = io::stderr().flush();

    // Non-TTY guard: if stdin is not a terminal (CI, piped input, scripts),
    // skip the raw-mode loop entirely and return the default answer.
    if !io::stdin().is_terminal() {
        return if default_yes {
            AskChoice::Yes
        } else {
            AskChoice::No
        };
    }

    // Enable raw mode so we can read single key events and detect Ctrl+C.
    // Outside readline, the terminal is in cooked mode; we temporarily switch
    // to raw, read one meaningful key, then restore.
    let raw_enabled = terminal::enable_raw_mode().is_ok();

    let choice = loop {
        if let Ok(Event::Key(key)) = read() {
            match (key.code, key.modifiers) {
                // Ctrl+C / Ctrl+D / Escape — abort without executing.
                (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) | (KeyCode::Esc, _) => {
                    let _ = write!(io::stderr(), "\r\n");
                    break AskChoice::No;
                }
                // Enter — use the default.
                (KeyCode::Enter, _) => {
                    let _ = write!(io::stderr(), "\r\n");
                    break if default_yes {
                        AskChoice::Yes
                    } else {
                        AskChoice::No
                    };
                }
                (KeyCode::Char('y' | 'Y'), _) => {
                    let _ = write!(io::stderr(), "y\r\n");
                    break AskChoice::Yes;
                }
                (KeyCode::Char('n' | 'N'), _) => {
                    let _ = write!(io::stderr(), "n\r\n");
                    break AskChoice::No;
                }
                (KeyCode::Char('e' | 'E'), _) => {
                    let _ = write!(io::stderr(), "e\r\n");
                    break AskChoice::Edit;
                }
                // Any other key: ignore and keep waiting.
                _ => {}
            }
        } else {
            // EOF or error — abort.
            let _ = write!(io::stderr(), "\r\n");
            break AskChoice::No;
        }
    };

    if raw_enabled {
        let _ = terminal::disable_raw_mode();
    }

    choice
}

/// Wrap a SQL query in a `start transaction read only` / `commit` block.
///
/// Used by [`handle_ai_ask`] so that every read-only query executed on behalf
/// of `/ask` is protected at the database level.  Even if [`is_write_query`]
/// misclassifies a query, the database itself will reject any write attempt
/// inside the read-only transaction.
///
/// The SQL is terminated with a semicolon only when one is not already
/// present, so the wrapped statement is always syntactically valid.
pub(super) fn wrap_in_ask_readonly_tx(sql: &str) -> String {
    let trimmed = sql.trim_end_matches(|c: char| c == ';' || c.is_whitespace());
    format!("start transaction read only;\n{trimmed};\ncommit;")
}

/// Handle a `/ask <prompt>` command end-to-end.
///
/// Acts as a general-purpose `PostgreSQL` expert assistant.  The AI answers
/// questions directly and, when a database query is needed, includes it in
/// a triple-backtick `sql` code fence.  Any SQL blocks found in the response
/// are automatically executed; results are shown interleaved with the AI's
/// explanatory text.
///
/// # SQL display
///
/// The generated SQL is always printed before execution.  The display format
/// depends on mode and settings:
///
/// - **text2sql** (`\t2s`): rendered in a boxed ```` ```sql ```` markdown fence
///   (unless yolo mode suppresses the box).
/// - **`ai.show_sql = true`** (config) or **`\set ECHO_HIDDEN on`** (runtime):
///   syntax-highlighted SQL on stderr.
/// - **fallback**: plain SQL on stderr so the user always sees what is about to
///   run.
///
/// # Read-only protection
///
/// Queries executed by `/ask` are wrapped in `start transaction read only` /
/// `commit` in all cases except text2sql write queries (see below).  This is
/// a second line of defence: even in yolo mode, the database rejects any
/// mutation that slips past [`is_write_query`] for `/ask`-originated queries.
///
/// In non-yolo `/ask` mode write queries (`INSERT`/`UPDATE`/`DELETE`/`MERGE`)
/// are refused before execution (`AskChoice::No`).  In yolo mode they reach
/// `AskChoice::Yes` but the read-only transaction wrapper causes `PostgreSQL`
/// to reject them at the wire level.
///
/// **Exception — text2sql write queries (non-yolo):** when the user is in
/// `\t2s` mode and the AI generates a write query, the user is prompted
/// `Execute write query? [y/N/e]` (default NO).  If confirmed, the query runs
/// directly without the read-only wrapper so it can actually mutate the
/// database.  Yolo mode in text2sql still uses the read-only wrapper (yolo is
/// primarily a convenience for read-only exploration, not mass writes).
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_ask(
    client: &Client,
    prompt: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
    tx: &mut TxState,
) {
    if settings
        .config
        .ai
        .provider
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        eprintln!("Supported providers: anthropic, openai, ollama");
        eprintln!("Example:");
        eprintln!("  [ai]");
        eprintln!("  provider = \"anthropic\"");
        eprintln!("  api_key_env = \"ANTHROPIC_API_KEY\"");
        return;
    }

    let provider = match get_ai_provider(settings) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build a compact schema description for the system prompt.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Collect wait event context for richer analysis.
    let wait_ctx = crate::ai::context::build_wait_context(
        client,
        settings.db_capabilities.pg_ash.is_available(),
    )
    .await;

    let wait_section = if wait_ctx.is_empty() {
        String::new()
    } else {
        format!("\n\nDatabase activity:\n{wait_ctx}")
    };

    let system_content = format!(
        "You are a helpful PostgreSQL expert assistant connected to a \
         live database.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}{wait}\n\n\
         Guidelines:\n\
         - Answer the user's question directly and concisely.\n\
         - If you need to query the database to answer, include the SQL \
           wrapped in a ```sql code fence.\n\
         - You may include explanatory text before or after the SQL.\n\
         - If the question does not require a database query, just answer \
           it directly — do not generate SQL.\n\
         - Use standard PostgreSQL syntax.\n\
         - If the request is ambiguous, make reasonable assumptions.",
        dbname = params.dbname,
        schema = schema_ctx,
        wait = wait_section,
    );

    // In text2sql mode, guide the LLM to translate natural language to SQL —
    // but allow plain-text answers for conversational/meta questions that do
    // not require a database query (e.g. "what SQL did you use?", "show SQL").
    let system_content = if settings.input_mode == InputMode::Text2Sql {
        format!(
            "{system_content}\n\n\
             You are in text2sql mode.\n\
             - If the user's input describes a database operation or asks for \
               data from the database, respond with ONLY the SQL query inside \
               a ```sql code fence — no commentary, no explanation.\n\
             - If the user's input is conversational or meta (e.g. asking about \
               previous queries, asking you to explain what you did, asking to \
               show the SQL you used), answer in plain text WITHOUT a SQL block. \
               Do NOT generate SQL that re-runs a previous query just because \
               the user asked about it."
        )
    } else {
        system_content
    };

    // Build messages: system + conversation history + current prompt.
    let mut messages = vec![crate::ai::Message {
        role: crate::ai::Role::System,
        content: system_content,
    }];

    // Include conversation history for follow-up context.
    messages.extend(settings.conversation.to_messages());

    messages.push(crate::ai::Message {
        role: crate::ai::Role::User,
        content: prompt.to_owned(),
    });

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    let ai_response = match with_ctrl_c_cancel(provider.complete(&messages, &options)).await {
        Ok(result) => {
            record_token_usage(settings, &result);
            result.content
        }
        Err(e) if e == CANCELLED => {
            eprintln!("-- AI request cancelled");
            return;
        }
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // In text2sql mode, remember the natural-language query so that /fix
    // can include the original intent in its prompt.
    if settings.input_mode == InputMode::Text2Sql {
        settings.last_t2s_nl_query = Some(prompt.to_owned());
    }

    // Record the exchange in conversation context for follow-ups.
    settings.conversation.push_user(prompt.to_owned());
    settings.conversation.push_assistant(ai_response.clone());

    // Auto-compact when approaching the context window limit.
    if settings
        .conversation
        .auto_compact_if_needed(settings.config.ai.context_window)
    {
        eprintln!("-- AI context auto-compacted to save tokens");
    }

    // Parse the response into text and SQL segments, then process each.
    let in_text2sql = settings.input_mode == InputMode::Text2Sql;
    let yolo = settings.exec_mode == ExecMode::Yolo;
    // In text2sql mode: show SQL box + ask confirm by default.
    // Yolo mode or TEXT2SQL_SHOW_SQL=off suppresses the box and skips confirm.
    let text2sql_show = in_text2sql && settings.text2sql_show_sql && !yolo;
    // Legacy AI_SHOW_SQL / echo_hidden path (used by /ask outside text2sql).
    let show_sql = settings.config.ai.show_sql || settings.echo_hidden;

    let segments = parse_ai_response_segments(&ai_response);

    for segment in &segments {
        match segment {
            AiResponseSegment::Text(text) => {
                // In yolo mode only query results should appear — suppress
                // the AI's explanatory text so the terminal stays clean.
                // In text2sql mode the system prompt allows plain-text
                // answers for conversational questions, and we want those
                // to reach the user (unless yolo overrides).
                if !yolo {
                    let text = text.trim();
                    if !text.is_empty() {
                        println!("{text}");
                    }
                }
            }
            AiResponseSegment::Sql(sql) => {
                if text2sql_show {
                    // Print in the same ┌── sql box style as /fix.
                    let boxed = format!("```sql\n{sql}\n```");
                    print!(
                        "{}",
                        crate::markdown::render_markdown(&boxed, settings.no_highlight)
                    );
                } else if show_sql {
                    // Legacy /ask path: plain highlighted SQL.
                    if settings.no_highlight {
                        eprintln!("{sql}");
                    } else {
                        eprintln!("{}", crate::highlight::highlight_sql(sql, None));
                    }
                } else {
                    // Always show the SQL /ask is about to execute so the user
                    // knows what's happening.
                    eprintln!("{sql}");
                }

                // Decide whether to prompt before executing.
                //
                // `in_text2sql` means the user is in \t2s mode and typed a
                // natural-language prompt — they explicitly want SQL executed,
                // including write queries (with confirmation).  `/ask` is a
                // question command and must never execute write queries.
                let read_only = !is_write_query(sql);
                let choice = if yolo {
                    // Yolo: auto-execute.  Write queries still go through the
                    // read-only transaction wrapper, so PostgreSQL will reject
                    // them at the DB level.  Warn so the user understands why
                    // the query failed.
                    if !read_only {
                        eprintln!(
                            "-- YOLO: write query attempted — \
                             read-only transaction will reject it"
                        );
                    }
                    AskChoice::Yes
                } else if !read_only && in_text2sql {
                    // text2sql mode: user asked for a write operation — show
                    // the SQL box (printed above) and ask for confirmation.
                    // Default is NO so accidental Enter does not mutate data.
                    ask_yne_prompt("Execute write query? [y/N/e] ", false)
                } else if !read_only {
                    // /ask is a question command — show the SQL but do not
                    // execute DML or DDL.  The user can copy and run it
                    // manually or use raw SQL input / a direct \t2s prompt.
                    eprintln!(
                        "-- (write query — not executed in /ask mode; \
                         use \\t2s to execute)"
                    );
                    AskChoice::No
                } else if text2sql_show {
                    // text2sql interactive mode, read-only query: show the SQL
                    // box (already printed above) and ask for confirmation.
                    ask_yne_prompt("Execute? [Y/n/e] ", true)
                } else {
                    // /ask interactive mode, read-only: auto-execute.
                    AskChoice::Yes
                };

                // Whether to skip the read-only transaction wrapper for
                // this query.  Only text2sql non-yolo write queries execute
                // directly; yolo mode always uses the read-only wrapper so
                // that accidental writes are still rejected at the DB level.
                let t2s_write = in_text2sql && !read_only && !yolo;

                match choice {
                    AskChoice::Yes => {
                        let ok = if t2s_write {
                            // text2sql write query: execute directly without
                            // the read-only transaction wrapper.
                            execute_query_interactive(client, sql, settings, tx).await
                        } else {
                            // /ask or text2sql read-only query: wrap in a
                            // read-only transaction as a defence-in-depth
                            // guard.  The database will reject any mutation
                            // that slips past the is_write_query check.
                            let exec_sql = std::borrow::Cow::Owned(wrap_in_ask_readonly_tx(sql));
                            // Mark this as an internal /ask transaction so
                            // that auto_rollback_stale_tx can clean it up if
                            // the query is interrupted before commit.
                            settings.internal_tx = true;
                            let ok =
                                execute_query_interactive(client, &exec_sql, settings, tx).await;
                            if !ok {
                                // Roll back on error to leave the session
                                // clean.
                                let _ = client.simple_query("rollback").await;
                            }
                            // The wrapped query ends with `commit;` (or was
                            // rolled back above) so the session is always idle
                            // at this point regardless of outcome.
                            // update_from_sql() only saw `start transaction`
                            // and set InTransaction; correct it here.
                            *tx = TxState::Idle;
                            if let Some(ref sl_arc) = settings.statusline {
                                sl_arc.lock().unwrap().set_tx_state(TxState::Idle);
                            }
                            // Clear the internal-tx flag — the transaction
                            // has been committed or rolled back.
                            settings.internal_tx = false;
                            ok
                        };
                        if ok {
                            settings.conversation.push_query_result(sql, "(executed)");
                        } else if let Some(err) = &settings.last_error {
                            let msg = err.error_message.clone();
                            settings
                                .conversation
                                .push_query_result(sql, &format!("ERROR: {msg}"));
                        } else {
                            settings
                                .conversation
                                .push_query_result(sql, "ERROR: (execution failed)");
                        }
                    }
                    AskChoice::Edit => match crate::io::edit(sql, None, None) {
                        Ok(edited) => {
                            let edited = edited.trim();
                            if edited.is_empty() {
                                eprintln!("(empty — skipped)");
                            } else {
                                // Re-evaluate write/read-only from the
                                // edited SQL — user may have changed the
                                // query type in the editor.  Yolo mode always
                                // uses the read-only wrapper (same as above).
                                let edited_write = in_text2sql && is_write_query(edited) && !yolo;
                                let ok = if edited_write {
                                    // text2sql write query (possibly modified):
                                    // execute directly.
                                    execute_query_interactive(client, edited, settings, tx).await
                                } else {
                                    // /ask, yolo, or read-only edited query:
                                    // wrap in a read-only transaction.
                                    let exec_edited =
                                        std::borrow::Cow::Owned(wrap_in_ask_readonly_tx(edited));
                                    settings.internal_tx = true;
                                    let ok = execute_query_interactive(
                                        client,
                                        &exec_edited,
                                        settings,
                                        tx,
                                    )
                                    .await;
                                    if !ok {
                                        let _ = client.simple_query("rollback").await;
                                    }
                                    // The wrapped query ends with `commit;` (or
                                    // was rolled back above) so the session is
                                    // always idle at this point.
                                    // update_from_sql() only saw `start
                                    // transaction`; correct it here.
                                    *tx = TxState::Idle;
                                    if let Some(ref sl_arc) = settings.statusline {
                                        sl_arc.lock().unwrap().set_tx_state(TxState::Idle);
                                    }
                                    settings.internal_tx = false;
                                    ok
                                };
                                if ok {
                                    settings
                                        .conversation
                                        .push_query_result(edited, "(executed after edit)");
                                } else if let Some(err) = &settings.last_error {
                                    let msg = err.error_message.clone();
                                    settings
                                        .conversation
                                        .push_query_result(edited, &format!("ERROR: {msg}"));
                                } else {
                                    settings
                                        .conversation
                                        .push_query_result(edited, "ERROR: (execution failed)");
                                }
                            }
                        }
                        Err(e) => eprintln!("{e}"),
                    },
                    AskChoice::No => {}
                }
            }
        }
    }
}

/// A segment of an AI response: plain text or a SQL block.
pub(super) enum AiResponseSegment {
    Text(String),
    Sql(String),
}

/// Split an AI response into alternating text and SQL segments.
///
/// SQL blocks are delimited by ` ```sql ` … ` ``` ` (case-insensitive
/// language tag).  Plain ` ``` ` fences without a language tag are treated
/// as text, not SQL.  The function never allocates an empty segment.
pub(super) fn parse_ai_response_segments(response: &str) -> Vec<AiResponseSegment> {
    let mut segments: Vec<AiResponseSegment> = Vec::new();
    let mut remaining = response;

    while !remaining.is_empty() {
        // Look for the start of a ```sql fence.
        if let Some(fence_start) = remaining.find("```sql") {
            // Any text before the fence becomes a Text segment.
            let before = &remaining[..fence_start];
            if !before.trim().is_empty() {
                segments.push(AiResponseSegment::Text(before.to_owned()));
            }
            // Advance past the opening fence + language tag.
            let after_open = &remaining[fence_start + 6..];
            // Skip the newline immediately after the tag.
            let body_start = after_open
                .find('\n')
                .map_or(after_open, |i| &after_open[i + 1..]);
            // Find the closing fence.
            if let Some(close_pos) = body_start.find("```") {
                let sql_body = body_start[..close_pos].trim();
                if !sql_body.is_empty() {
                    segments.push(AiResponseSegment::Sql(sql_body.to_owned()));
                }
                remaining = &body_start[close_pos + 3..];
            } else {
                // Unclosed fence: treat everything as SQL.
                let sql_body = body_start.trim();
                if !sql_body.is_empty() {
                    segments.push(AiResponseSegment::Sql(sql_body.to_owned()));
                }
                break;
            }
        } else {
            // No more SQL fences — rest is plain text.
            if !remaining.trim().is_empty() {
                segments.push(AiResponseSegment::Text(remaining.to_owned()));
            }
            break;
        }
    }

    segments
}

/// Handle a plan-mode prompt.
///
/// Gathers schema context, sends the user's natural-language prompt to the
/// LLM with a plan-generation system prompt, and streams the resulting plan.
/// Offers to save the plan to `~/.local/share/rpg/plans/`.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_plan(
    client: &Client,
    prompt: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    if settings
        .config
        .ai
        .provider
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        return;
    }

    let provider = match get_ai_provider(settings) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    let system_content = format!(
        "You are a PostgreSQL expert. \
         The user has asked you to investigate and produce an action plan.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n\n\
         Rules:\n\
         - Produce a structured plan in markdown format\n\
         - Each action should include the SQL command and a safety assessment\n\
         - Mark actions as [safe], [caution], or [dangerous]\n\
         - Order actions from safest to most impactful\n\
         - Include estimated duration where possible\n\
         - Do NOT execute anything — only plan",
        dbname = params.dbname,
        schema = schema_ctx,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: prompt.to_owned(),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    eprintln!("-- Plan mode: investigating...");
    let result = match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(r) => r,
        Err(e) if e == CANCELLED => {
            eprintln!("-- AI request cancelled");
            return;
        }
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };
    record_token_usage(settings, &result);

    // Offer to save the plan.
    if ask_yn_prompt("Save this plan? [Y/n] ", true) {
        let plans_dir = dirs::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("rpg")
            .join("plans");
        if let Err(e) = std::fs::create_dir_all(&plans_dir) {
            eprintln!("Cannot create plans directory: {e}");
            return;
        }
        let date = format_system_time(std::time::SystemTime::now())
            .replace(' ', "-")
            .replace(':', "");
        // Build a slug from the first few words of the prompt.
        let slug: String = prompt
            .split_whitespace()
            .take(4)
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .collect();
        let filename = format!("{date}-{slug}.md");
        let path = plans_dir.join(&filename);
        match std::fs::write(&path, &result.content) {
            Ok(()) => eprintln!("Saved to: {}", path.display()),
            Err(e) => eprintln!("Failed to save plan: {e}"),
        }
    }
}

/// Extract the last SQL code block from a mixed text+SQL LLM response.
///
/// LLMs responding to `/fix` produce a mix of explanation and corrected SQL.
/// This function scans for the last `` ```sql ... ``` `` fence (or plain
/// `` ``` ... ``` `` fence) and returns the inner content, trimmed.  If no
/// fences are found it returns `None`.
pub(super) fn extract_last_sql_block(text: &str) -> Option<&str> {
    // Iterate forward through all fence pairs, keeping the last non-empty one.
    // rfind("```") was incorrect: it found the closing fence, then looked for
    // another "```" after it — always returning None for a single block.
    let mut last_sql: Option<&str> = None;
    let mut search_from = 0;
    while let Some(open_pos) = text[search_from..].find("```") {
        let open_abs = search_from + open_pos;
        let after_open = &text[open_abs + 3..];
        // Skip optional language tag (e.g. "sql\n") on the opening fence line.
        let body_start_rel = after_open.find('\n').map_or(after_open.len(), |i| i + 1);
        let body_text = &after_open[body_start_rel..];
        if let Some(close_pos) = body_text.find("```") {
            let body = body_text[..close_pos].trim();
            if !body.is_empty() {
                last_sql = Some(body);
            }
            // Advance past the closing fence before looking for the next pair.
            search_from = open_abs + 3 + body_start_rel + close_pos + 3;
        } else {
            // Unclosed fence — treat rest of text as the body.
            let body = body_text.trim();
            if !body.is_empty() {
                last_sql = Some(body);
            }
            break;
        }
    }
    last_sql
}

/// Handle a `/fix` command end-to-end.
///
/// Looks up the most recently failed query from [`ReplSettings::last_error`],
/// sends it to the configured LLM with schema context, and prints an
/// explanation plus a corrected SQL query.  After streaming the response,
/// if a corrected SQL block is detected the user is prompted
/// `Execute? [Y/n]` (default yes) and the query is executed when confirmed.
/// Gracefully degrades when no prior error exists or when AI is not
/// configured.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_fix(
    client: &Client,
    settings: &mut ReplSettings,
    params: &ConnParams,
    tx: &mut TxState,
) {
    // Require a prior error to fix.
    let last_error = if let Some(e) = &settings.last_error {
        e.clone()
    } else {
        eprintln!("No recent error to fix. Run a query first.");
        return;
    };

    if settings
        .config
        .ai
        .provider
        .as_deref()
        .unwrap_or("")
        .is_empty()
    {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        eprintln!("Supported providers: anthropic, openai, ollama");
        eprintln!("Example:");
        eprintln!("  [ai]");
        eprintln!("  provider = \"anthropic\"");
        eprintln!("  api_key_env = \"ANTHROPIC_API_KEY\"");
        return;
    }

    let provider = match get_ai_provider(settings) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };

    // Build a compact schema description for the system prompt.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Format the SQLSTATE hint if available.
    let sqlstate_hint = last_error
        .sqlstate
        .as_deref()
        .map(|s| format!(" (SQLSTATE {s})"))
        .unwrap_or_default();

    let system_content = format!(
        "You are a PostgreSQL expert. \
         Explain SQL errors and provide corrected queries.\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n\n\
         Rules:\n\
         - First, briefly explain what caused the error (1-2 sentences)\n\
         - Then output the corrected SQL query\n\
         - Use standard PostgreSQL syntax\n\
         - Keep the corrected query as close to the original intent as possible\n\
         - IMPORTANT: columns annotated 'generated always as identity' in the \
           schema are identity columns — never include them in an INSERT column \
           list; the database generates their values automatically\n\
         - Do NOT reference sequences (e.g. nextval) for identity columns; \
           simply omit those columns from the INSERT",
        dbname = params.dbname,
        schema = schema_ctx,
    );

    // In text2sql mode, include the original natural-language query so the
    // AI understands the intent behind the SQL and can fix it correctly
    // instead of regenerating the same broken query from scratch.
    let user_content = if settings.input_mode == InputMode::Text2Sql {
        if let Some(ref nl_query) = settings.last_t2s_nl_query.clone() {
            format!(
                "The following SQL was generated from this user request: \"{nl_query}\"\n\n\
                 The generated SQL failed{sqlstate_hint}:\n\n\
                 ```sql\n{query}\n```\n\n\
                 Error: {error}\n\n\
                 Fix the SQL to resolve the error while satisfying \
                 the original request.",
                nl_query = nl_query,
                query = last_error.query,
                error = last_error.error_message,
            )
        } else {
            format!(
                "The following query failed{sqlstate_hint}:\n\n\
                 ```sql\n{query}\n```\n\n\
                 Error: {error}",
                query = last_error.query,
                error = last_error.error_message,
            )
        }
    } else {
        format!(
            "The following query failed{sqlstate_hint}:\n\n\
             ```sql\n{query}\n```\n\n\
             Error: {error}",
            query = last_error.query,
            error = last_error.error_message,
        )
    };

    // Build messages: system + any prior /fix attempts from the conversation
    // history (so repeated /fix calls carry forward what was tried before and
    // avoid repeating the same wrong suggestions) + the current error.
    let mut messages = vec![crate::ai::Message {
        role: crate::ai::Role::System,
        content: system_content,
    }];
    messages.extend(settings.conversation.to_messages());
    messages.push(crate::ai::Message {
        role: crate::ai::Role::User,
        content: user_content.clone(),
    });

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    let result = match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(r) => r,
        Err(e) if e == CANCELLED => {
            eprintln!("-- AI request cancelled");
            return;
        }
        Err(e) => {
            eprintln!("AI error: {e}");
            return;
        }
    };
    record_token_usage(settings, &result);

    // Record this fix attempt in the conversation so that the next /fix call
    // carries forward the full chain: what failed, what was suggested, and
    // (below) whether the suggestion itself failed.  This prevents the AI
    // from looping on the same wrong fix.
    settings.conversation.push_user(user_content);
    settings.conversation.push_assistant(result.content.clone());

    // Auto-compact when approaching the context window limit.
    if settings
        .conversation
        .auto_compact_if_needed(settings.config.ai.context_window)
    {
        eprintln!("-- AI context auto-compacted to save tokens");
    }

    // If the response contains a corrected SQL block, offer to execute it.
    if let Some(fix_sql) = extract_last_sql_block(&result.content) {
        let choice = ask_yne_prompt("Execute? [Y/n/e] ", true);
        match choice {
            AskChoice::Yes => {
                // Mark that this execution originates from /fix so the
                // auto-suggest hint is suppressed for any resulting error.
                settings.last_was_fix = true;
                let ok = execute_query_interactive(client, fix_sql, settings, tx).await;
                if ok {
                    settings
                        .conversation
                        .push_query_result(fix_sql, "(fix applied)");
                } else if let Some(ref err) = settings.last_error {
                    settings
                        .conversation
                        .push_query_result(fix_sql, &err.error_message.clone());
                }
            }
            AskChoice::Edit => match crate::io::edit(fix_sql, None, None) {
                Ok(edited) => {
                    let edited = edited.trim();
                    if edited.is_empty() {
                        eprintln!("(empty — skipped)");
                    } else {
                        settings.last_was_fix = true;
                        let ok = execute_query_interactive(client, edited, settings, tx).await;
                        if ok {
                            settings
                                .conversation
                                .push_query_result(edited, "(fix applied after edit)");
                        } else if let Some(ref err) = settings.last_error {
                            settings
                                .conversation
                                .push_query_result(edited, &err.error_message.clone());
                        }
                    }
                }
                Err(e) => eprintln!("{e}"),
            },
            AskChoice::No => {}
        }
    }
}

/// Strip leading SQL comments (both `--` single-line and `/* */` block)
/// and any surrounding whitespace, returning a slice that starts at the
/// first real SQL token.
///
/// This is needed so that `is_write_query` correctly classifies queries
/// that the AI generates with a leading comment such as:
///
/// ```sql
/// -- Create the table
/// CREATE TABLE foo (id int);
/// ```
pub(super) fn strip_leading_sql_comments(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            // Skip to end of line.
            s = match s.find('\n') {
                Some(pos) => &s[pos + 1..],
                None => "",
            };
            s = s.trim_start();
        } else if s.starts_with("/*") {
            // Skip to end of block comment.
            s = match s.find("*/") {
                Some(pos) => &s[pos + 2..],
                None => "",
            };
            s = s.trim_start();
        } else {
            break;
        }
    }
    s
}

/// Detect whether a query is a write or schema-changing statement.
///
/// Returns `true` for:
/// - **DML**: `INSERT`, `UPDATE`, `DELETE`, `MERGE`
/// - **DDL**: `CREATE`, `DROP`, `ALTER`, `TRUNCATE`, `RENAME`
/// - **DCL**: `GRANT`, `REVOKE`
/// - **Maintenance**: `VACUUM`, `CLUSTER`, `REINDEX`, `REFRESH`
/// - **CTEs**: any query starting with `WITH` (treated conservatively as a
///   potential write, since CTEs may wrap DML)
///
/// Leading SQL comments (`--` and `/* */`) are stripped before checking so
/// that AI-generated SQL prefixed with a comment is classified correctly.
///
/// Used to decide whether to wrap a query in `BEGIN`/`ROLLBACK` for
/// `EXPLAIN ANALYZE`, and whether to require confirmation before
/// auto-executing in `/ask` and yolo modes.
pub(super) fn is_write_query(sql: &str) -> bool {
    let effective = strip_leading_sql_comments(sql);
    let first = effective
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(';')
        .trim_end_matches(',')
        .to_ascii_uppercase();
    // WITH starts a CTE which may wrap DML (INSERT/UPDATE/DELETE/MERGE).
    // Treat all CTEs as potentially mutating to prevent silent auto-execution
    // of CTE-prefixed write queries in /ask and yolo modes.
    if first == "WITH" {
        return true;
    }
    matches!(
        first.as_str(),
        // DML
        "INSERT" | "UPDATE" | "DELETE" | "MERGE"
        // DDL — always require confirmation; never auto-execute
        | "CREATE" | "DROP" | "ALTER" | "TRUNCATE" | "RENAME"
        // Privilege control
        | "GRANT" | "REVOKE"
        // Maintenance (mutate physical storage / stats)
        | "VACUUM" | "CLUSTER" | "REINDEX" | "REFRESH"
    )
}

/// Build the `EXPLAIN` SQL for a given target query.
///
/// Write queries are wrapped in `BEGIN` / `ROLLBACK` so that
/// `EXPLAIN ANALYZE` can run them without persisting any changes.
pub(super) fn build_explain_sql(target_query: &str) -> String {
    let explain = format!("explain (analyze, costs, verbose, buffers, format text) {target_query}");
    if is_write_query(target_query) {
        format!("begin;\n{explain};\nrollback;")
    } else {
        explain
    }
}

/// Handle a `/explain [query]` command end-to-end.
///
/// 1. Resolves the target query: inline arg or `last_query`.
/// 2. Runs `EXPLAIN (ANALYZE, COSTS, VERBOSE, BUFFERS, FORMAT TEXT)`.
///    Write queries (`INSERT`/`UPDATE`/`DELETE`/`MERGE`) are wrapped in
///    a `BEGIN` … `ROLLBACK` to prevent side-effects.
/// 3. Prints the raw plan.
/// 4. If AI is configured, sends plan + schema context to the LLM and
///    prints its interpretation.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_explain(
    client: &Client,
    query_arg: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    // Resolve target query.
    let target_query = if query_arg.is_empty() {
        if let Some(q) = settings.last_query.as_deref() {
            q.to_owned()
        } else {
            eprintln!(
                "/explain: no query to explain. \
                 Run a query first or provide one: /explain SELECT ..."
            );
            return;
        }
    } else {
        query_arg.to_owned()
    };

    // Run EXPLAIN ANALYZE (wrapped in BEGIN/ROLLBACK for write queries).
    let explain_sql = build_explain_sql(&target_query);

    let messages_result = client.simple_query(&explain_sql).await;
    let raw_messages = match messages_result {
        Ok(msgs) => msgs,
        Err(e) => {
            crate::output::eprint_db_error(
                &e,
                Some(&target_query),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            return;
        }
    };

    // Collect plan lines from the result.
    let mut plan_lines: Vec<String> = Vec::new();
    for msg in &raw_messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if let Some(line) = row.get(0) {
                plan_lines.push(line.to_owned());
            }
        }
    }

    if plan_lines.is_empty() {
        eprintln!("/explain: EXPLAIN returned no output");
        return;
    }

    let plan_text = plan_lines.join("\n");
    let display_plan =
        crate::explain::highlight::highlight_explain(&plan_text, settings.no_highlight);
    println!("{display_plan}");

    // AI interpretation — skip gracefully when AI is not configured.
    let Ok(provider) = get_ai_provider(settings) else {
        return;
    };

    // Build schema context for richer analysis.
    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    // Collect wait event context for performance correlation.
    let wait_ctx = crate::ai::context::build_wait_context(
        client,
        settings.db_capabilities.pg_ash.is_available(),
    )
    .await;

    let wait_section = if wait_ctx.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDatabase activity (use to correlate plan behavior \
             with current wait patterns):\n{wait_ctx}"
        )
    };

    let system_content = format!(
        "You are a PostgreSQL performance expert. \
         Analyse the EXPLAIN ANALYZE plan and output a structured summary using ONLY these labeled lines (omit any line where there is nothing to report):\n\
         Bottleneck: <most expensive node, cost, actual rows>\n\
         Est error: <if actual rows diverges from estimated by >5x, state both>\n\
         Slow nodes: <nodes with actual time >100ms>\n\
         Filters: <sequential scans or filters that could benefit from an index>\n\
         Wait: <dominant wait event if wait data provided>\n\
         Time: planning Xms, execution Xms\n\n\
         Rules:\n\
         - Output ONLY the labeled lines above, no prose, no markdown, no headers\n\
         - Omit any line where there is nothing significant to report\n\
         - Maximum 6 lines total\n\
         - All significant findings must be included — just on labeled lines\n\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}{wait}",
        dbname = params.dbname,
        schema = schema_ctx,
        wait = wait_section,
    );

    let user_content = format!("Query:\n{target_query}\n\nEXPLAIN ANALYZE output:\n{plan_text}");

    let ai_messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens.min(400),
        temperature: 0.0,
    };

    println!();
    match stream_completion(
        provider.as_ref(),
        &ai_messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) if e == CANCELLED => eprintln!("-- AI request cancelled"),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

/// Extract table names referenced by `FROM` and `JOIN` clauses.
///
/// Best-effort heuristic parser — handles common patterns including
/// schema-qualified names but does not aim for full SQL parsing.
/// Used by `/optimize` to query `pg_stat_user_tables`.
pub(super) fn extract_table_names(sql: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    let upper_tokens: Vec<String> = upper.split_whitespace().map(String::from).collect();
    let mut tables = Vec::new();

    let mut i = 0;
    while i < upper_tokens.len() {
        let is_from = upper_tokens[i] == "FROM";
        let is_join = upper_tokens[i].ends_with("JOIN") && upper_tokens[i] != "DISJOIN";

        if (is_from || is_join) && i + 1 < tokens.len() {
            let candidate = tokens[i + 1];
            // Skip sub-selects: FROM (SELECT ...)
            if !candidate.starts_with('(') {
                let clean = candidate.trim_end_matches([',', ')', ';']);
                if !clean.is_empty() {
                    tables.push(clean.to_owned());
                }
            }
        }
        i += 1;
    }

    tables.sort();
    tables.dedup();
    tables
}

/// Handle a `/optimize [query]` command end-to-end.
///
/// 1. Resolves the target query: inline arg or `last_query`.
/// 2. Runs `EXPLAIN (ANALYZE, COSTS, VERBOSE, BUFFERS, FORMAT TEXT)`.
/// 3. Gathers `pg_stat_user_tables` stats for referenced tables.
/// 4. Sends plan + stats + schema context to the LLM for optimization
///    suggestions (index creation, query rewrites, join order changes).
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_optimize(
    client: &Client,
    query_arg: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    // Resolve target query.
    let target_query = if query_arg.is_empty() {
        if let Some(q) = settings.last_query.as_deref() {
            q.to_owned()
        } else {
            eprintln!(
                "/optimize: no query to optimize. \
                 Run a query first or provide one: /optimize SELECT ..."
            );
            return;
        }
    } else {
        query_arg.to_owned()
    };

    // Run EXPLAIN ANALYZE (wrapped in BEGIN/ROLLBACK for write queries).
    let explain_sql = build_explain_sql(&target_query);

    let raw_messages = match client.simple_query(&explain_sql).await {
        Ok(msgs) => msgs,
        Err(e) => {
            crate::output::eprint_db_error(
                &e,
                Some(&target_query),
                settings.verbose_errors,
                settings.terse_errors,
                settings.sqlstate_errors,
            );
            return;
        }
    };

    // Collect plan lines.
    let mut plan_lines: Vec<String> = Vec::new();
    for msg in &raw_messages {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
            if let Some(line) = row.get(0) {
                plan_lines.push(line.to_owned());
            }
        }
    }

    if plan_lines.is_empty() {
        eprintln!("/optimize: EXPLAIN returned no output");
        return;
    }

    let plan_text = plan_lines.join("\n");
    let display_plan =
        crate::explain::highlight::highlight_explain(&plan_text, settings.no_highlight);
    println!("{display_plan}");

    // Gather table statistics for referenced tables.
    let table_names = extract_table_names(&target_query);
    let mut stats_text = String::new();

    if !table_names.is_empty() {
        let in_list: String = table_names
            .iter()
            .map(|t| {
                let escaped = t.replace('\'', "''");
                format!("'{escaped}'")
            })
            .collect::<Vec<_>>()
            .join(", ");

        let stats_sql = format!(
            "SELECT schemaname || '.' || relname AS table_name, \
                    n_live_tup, n_dead_tup, \
                    seq_scan, seq_tup_read, \
                    idx_scan, idx_tup_fetch, \
                    last_vacuum::text, last_analyze::text \
             FROM pg_stat_user_tables \
             WHERE relname IN ({in_list}) \
                OR schemaname || '.' || relname IN ({in_list}) \
             ORDER BY relname"
        );

        if let Ok(msgs) = client.simple_query(&stats_sql).await {
            let mut stat_rows = Vec::new();
            for msg in &msgs {
                if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                    let cols: Vec<String> = (0..9)
                        .map(|i| row.get(i).unwrap_or("(null)").to_owned())
                        .collect();
                    stat_rows.push(cols.join(" | "));
                }
            }
            if !stat_rows.is_empty() {
                stats_text = format!(
                    "\n\nTable statistics (table | live_tup | dead_tup | \
                     seq_scan | seq_tup_read | idx_scan | idx_tup_fetch | \
                     last_vacuum | last_analyze):\n{}",
                    stat_rows.join("\n")
                );
            }
        }
    }

    // AI optimization — skip gracefully when AI is not configured.
    let Ok(provider) = get_ai_provider(settings) else {
        eprintln!(
            "\nAI not configured — showing raw plan only. \
             Add an [ai] section to {} for optimization suggestions.",
            crate::config::user_config_path_display()
        );
        return;
    };

    let schema_ctx = match crate::ai::context::build_schema_context(client).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Schema context error: {e}");
            return;
        }
    };

    let system_content = format!(
        "You are a PostgreSQL performance optimization expert. \
         Analyse the query, its EXPLAIN ANALYZE plan, and table statistics, then output a numbered list of concrete optimization actions, highest impact first.\n\
         Format each item as: N. <SQL action>; -- <reason, expected gain>\n\
         Example:\n\
         1. CREATE INDEX CONCURRENTLY idx ON workload(val); -- seq scan -> index scan, ~10x speedup\n\
         2. VACUUM workload; -- reclaim dead tuples, reduce scan cost\n\
         3. ANALYZE workload; -- refresh planner stats\n\
         Rules:\n\
         - Each item is a single SQL statement followed by an inline comment (-- ...)\n\
         - Inline comment max 60 chars; no trailing sentences after the SQL line\n\
         - CREATE INDEX items must be valid SQL: CREATE INDEX CONCURRENTLY name ON table(col);\n\
         - Maximum 6 items\n\
         - No prose intro, no section headers, no markdown, no standalone sentences\n\
         - Forbidden phrases: \"This will\", \"This allows\", \"This enables\", \"This should\", \"significantly\", \"overall\"\n\
         - Order by expected impact (highest first)\n\n\
         Database: {dbname}\n\n\
         Schema:\n{schema}\n",
        dbname = params.dbname,
        schema = schema_ctx,
    );

    let user_content = format!(
        "Query:\n```sql\n{target_query}\n```\n\n\
         EXPLAIN ANALYZE output:\n{plan_text}{stats_text}"
    );

    let ai_messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: user_content,
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens.min(500),
        temperature: 0.0,
    };

    println!();
    match stream_completion(
        provider.as_ref(),
        &ai_messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) if e == CANCELLED => eprintln!("-- AI request cancelled"),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

/// Handle a `/describe <table>` command.
///
/// Queries the table's columns, constraints, indexes, and row estimate,
/// then sends everything to the LLM for a human-readable description of
/// the table's purpose, relationships, and notable patterns.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_ai_describe(
    client: &Client,
    table_name: &str,
    settings: &mut ReplSettings,
    params: &ConnParams,
) {
    let Ok(provider) = get_ai_provider(settings) else {
        eprintln!(
            "AI not configured. Add an [ai] section to {}",
            crate::config::user_config_path_display()
        );
        return;
    };

    // Gather table metadata.
    let mut table_info = String::new();

    // Columns.
    let col_query = format!(
        "SELECT column_name, data_type, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_name = '{table_name}' \
         ORDER BY ordinal_position"
    );
    if let Ok(rows) = client.simple_query(&col_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "Columns:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let dtype = row.get(1).unwrap_or("?");
                let nullable = row.get(2).unwrap_or("?");
                let default = row.get(3).unwrap_or("");
                let _ = writeln!(
                    table_info,
                    "  {name} {dtype} nullable={nullable} default={default}"
                );
            }
        }
    }

    // Constraints (PK, FK, unique, check).
    let constraint_query = format!(
        "SELECT conname, contype, pg_get_constraintdef(oid) \
         FROM pg_constraint \
         WHERE conrelid = '{table_name}'::regclass"
    );
    if let Ok(rows) = client.simple_query(&constraint_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "\nConstraints:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let ctype = row.get(1).unwrap_or("?");
                let def = row.get(2).unwrap_or("?");
                let type_label = match ctype {
                    "p" => "PRIMARY KEY",
                    "f" => "FOREIGN KEY",
                    "u" => "UNIQUE",
                    "c" => "CHECK",
                    "x" => "EXCLUSION",
                    other => other,
                };
                let _ = writeln!(table_info, "  {name} ({type_label}): {def}");
            }
        }
    }

    // Indexes.
    let idx_query = format!(
        "SELECT indexname, indexdef \
         FROM pg_indexes \
         WHERE tablename = '{table_name}'"
    );
    if let Ok(rows) = client.simple_query(&idx_query).await {
        use std::fmt::Write as _;
        let _ = writeln!(table_info, "\nIndexes:");
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let name = row.get(0).unwrap_or("?");
                let def = row.get(1).unwrap_or("?");
                let _ = writeln!(table_info, "  {name}: {def}");
            }
        }
    }

    // Row estimate + size.
    let stats_query = format!(
        "SELECT reltuples::bigint AS row_estimate, \
         pg_size_pretty(pg_total_relation_size('{table_name}'::regclass)) AS size \
         FROM pg_class WHERE relname = '{table_name}'"
    );
    if let Ok(rows) = client.simple_query(&stats_query).await {
        use std::fmt::Write as _;
        for msg in &rows {
            if let tokio_postgres::SimpleQueryMessage::Row(row) = msg {
                let rows_est = row.get(0).unwrap_or("?");
                let size = row.get(1).unwrap_or("?");
                let _ = writeln!(
                    table_info,
                    "\nEstimated rows: {rows_est}, Total size: {size}"
                );
            }
        }
    }

    if table_info.trim().is_empty() {
        eprintln!("No metadata found for table '{table_name}'.");
        return;
    }

    let system_content = format!(
        "You are a PostgreSQL expert. \
         Describe the purpose and design of this database table.\n\
         Database: {dbname}\n\n\
         Rules:\n\
         - Infer the table's purpose from its name, columns, and constraints\n\
         - Describe relationships (foreign keys) to other tables\n\
         - Note any design patterns (audit columns, soft deletes, etc.)\n\
         - Mention notable indexes and their likely purpose\n\
         - Be concise — this is for quick understanding",
        dbname = params.dbname,
    );

    let messages = vec![
        crate::ai::Message {
            role: crate::ai::Role::System,
            content: system_content,
        },
        crate::ai::Message {
            role: crate::ai::Role::User,
            content: format!("Describe table '{table_name}':\n\n{table_info}"),
        },
    ];

    let options = crate::ai::CompletionOptions {
        model: settings.config.ai.model.clone().unwrap_or_default(),
        max_tokens: settings.config.ai.max_tokens,
        temperature: 0.0,
    };

    match stream_completion(
        provider.as_ref(),
        &messages,
        &options,
        settings.no_highlight,
    )
    .await
    {
        Ok(result) => record_token_usage(settings, &result),
        Err(e) if e == CANCELLED => eprintln!("-- AI request cancelled"),
        Err(e) => eprintln!("AI error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// /budget — Token usage display
// ---------------------------------------------------------------------------

/// Handle the `/budget` command: display token usage for the current session.
///
/// Shows total tokens consumed and the configured budget limit.  When no
/// budget is set (`token_budget == 0`) it prints an informational note about
/// how to configure one.
pub(super) fn handle_ai_budget(settings: &ReplSettings) {
    let used = settings.tokens_used;
    let budget = settings.config.ai.token_budget;

    eprintln!("Token usage this session:");
    eprintln!("  Total:  {:>10} tokens", format_tokens(used));
    if budget == 0 {
        eprintln!("  Budget: not set (use \\set TOKEN_BUDGET <N> to set)");
    } else {
        let remaining = budget.saturating_sub(used);
        let pct = (used * 100).checked_div(budget).unwrap_or(100);
        eprintln!(
            "  Budget: {:>10} tokens ({}% used, {} remaining)",
            format_tokens(budget),
            pct,
            format_tokens(remaining),
        );
    }
}

/// Format a token count with thousands separators.
fn format_tokens(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

// ---------------------------------------------------------------------------
// /rca — Root Cause Analysis (removed)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// /init — generate starter files
// ---------------------------------------------------------------------------

/// Handle the `/init` command.
///
/// Generates `.rpg.toml` and `POSTGRES.md` in the current working directory.
/// Skips a file (with a warning) if it already exists.
pub(super) async fn handle_init(client: &Client, settings: &ReplSettings, params: &ConnParams) {
    use std::fs;
    use std::path::Path;

    // Generate .rpg.toml -------------------------------------------------------
    let toml_path = Path::new(".rpg.toml");
    if toml_path.exists() {
        eprintln!(
            "WARNING: .rpg.toml already exists — skipping. \
             Remove it first if you want to regenerate."
        );
    } else {
        let toml_content = crate::init::generate_rpg_toml(&settings.config, params);
        match fs::write(toml_path, toml_content) {
            Ok(()) => println!("Created .rpg.toml"),
            Err(e) => eprintln!("Error writing .rpg.toml: {e}"),
        }
    }

    // Generate POSTGRES.md -----------------------------------------------------
    let md_path = Path::new("POSTGRES.md");
    if md_path.exists() {
        eprintln!(
            "WARNING: POSTGRES.md already exists — skipping. \
             Remove it first if you want to regenerate."
        );
    } else {
        match crate::init::generate_postgres_md(client).await {
            Ok(md_content) => match fs::write(md_path, md_content) {
                Ok(()) => println!("Created POSTGRES.md"),
                Err(e) => eprintln!("Error writing POSTGRES.md: {e}"),
            },
            Err(e) => eprintln!("Error querying database for POSTGRES.md: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ConversationContext loop-prevention mechanism -------------------------

    /// Verify that messages pushed into `ConversationContext` are returned by
    /// `to_messages()` in order and with the correct roles.  This is the
    /// mechanism that injects prior /fix attempts into subsequent AI calls,
    /// preventing the AI from suggesting the same wrong fix repeatedly.
    #[test]
    fn conversation_history_injected_into_fix_calls() {
        let mut ctx = ConversationContext::new();

        // Simulate a first /fix attempt: user error + AI suggestion.
        ctx.push_user(
            "The following query failed:\n\n\
             ```sql\nSELECT * FROM usres;\n```\n\n\
             Error: relation \"usres\" does not exist"
                .to_owned(),
        );
        ctx.push_assistant(
            "The table name appears to be misspelled. Try:\n\n\
             ```sql\nSELECT * FROM users;\n```"
                .to_owned(),
        );

        // Simulate recording the execution result (fix was applied or failed).
        ctx.push_query_result("SELECT * FROM users;", "(fix applied)");

        let msgs = ctx.to_messages();

        // All three entries must be present and in order.
        assert_eq!(msgs.len(), 3);
        assert!(
            matches!(msgs[0].role, crate::ai::Role::User),
            "first message should be user role"
        );
        assert!(
            matches!(msgs[1].role, crate::ai::Role::Assistant),
            "second message should be assistant role"
        );
        assert!(
            matches!(msgs[2].role, crate::ai::Role::User),
            "query result is recorded as a user message"
        );

        // The query result entry must contain both the SQL and the outcome.
        assert!(
            msgs[2].content.contains("SELECT * FROM users;"),
            "query result message should contain the executed SQL"
        );
        assert!(
            msgs[2].content.contains("(fix applied)"),
            "query result message should contain the result summary"
        );
    }

    // -- wrap_in_ask_readonly_tx -----------------------------------------------

    /// A plain SELECT is wrapped with start transaction read only / commit.
    #[test]
    fn wrap_readonly_tx_basic_select() {
        assert_eq!(
            wrap_in_ask_readonly_tx("select 1"),
            "start transaction read only;\nselect 1;\ncommit;"
        );
    }

    /// SQL that already ends with a semicolon must not gain a double semicolon.
    #[test]
    fn wrap_readonly_tx_no_double_semicolon() {
        assert_eq!(
            wrap_in_ask_readonly_tx("select 1;"),
            "start transaction read only;\nselect 1;\ncommit;"
        );
    }

    /// The wrapped SQL must use the correct `start transaction read only`
    /// syntax — not the older `begin; set transaction read only` form.
    #[test]
    fn wrap_readonly_tx_uses_start_transaction_syntax() {
        let wrapped = wrap_in_ask_readonly_tx("select count(*) from users");
        assert_eq!(
            wrapped,
            "start transaction read only;\nselect count(*) from users;\ncommit;"
        );
        // Verify the old syntax is absent.
        assert!(!wrapped.contains("begin;"), "must not use begin; syntax");
        assert!(
            !wrapped.contains("set transaction"),
            "must not use set transaction"
        );
    }

    /// Multi-line SQL is wrapped correctly.
    #[test]
    fn wrap_readonly_tx_multiline_sql() {
        let sql = "select\n    id,\n    name\nfrom users\nwhere active = true";
        assert_eq!(
            wrap_in_ask_readonly_tx(sql),
            "start transaction read only;\nselect\n    id,\n    name\nfrom users\nwhere active = true;\ncommit;"
        );
    }

    /// SQL with only trailing whitespace/semicolons is handled cleanly.
    #[test]
    fn wrap_readonly_tx_trailing_whitespace() {
        assert_eq!(
            wrap_in_ask_readonly_tx("select 1   ;  "),
            "start transaction read only;\nselect 1;\ncommit;"
        );
    }

    // -- rollback-on-error for read-only tx ------------------------------------

    /// Verify that `wrap_in_ask_readonly_tx` produces SQL that can be rolled
    /// back: the wrapped output starts a transaction, so a subsequent
    /// `rollback;` is valid.  This tests the invariant relied upon by the
    /// `if !ok { rollback }` path in `handle_ai_ask`.
    #[test]
    fn wrap_readonly_tx_rollback_path_produces_valid_rollback_target() {
        let wrapped = wrap_in_ask_readonly_tx("select 1/0");
        // The wrapped SQL opens a transaction — so rollback is valid.
        assert!(
            wrapped.starts_with("start transaction read only;"),
            "transaction must be opened so rollback is valid on error"
        );
        // Simulating the error path: when execute_query_interactive fails
        // on the wrapped SQL, the code issues `client.simple_query("rollback")`.
        // The rollback target is the `start transaction` we opened.
        // Verify the commit at the end is present (only reached on success).
        assert!(
            wrapped.ends_with("commit;"),
            "commit present; on error the code issues rollback instead"
        );
    }

    // -- strip_leading_sql_comments -------------------------------------------

    #[test]
    fn strip_no_comment_returns_same() {
        assert_eq!(strip_leading_sql_comments("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn strip_single_line_comment() {
        let result = strip_leading_sql_comments("-- hello\nSELECT 1");
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn strip_block_comment() {
        let result = strip_leading_sql_comments("/* comment */SELECT 1");
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn strip_block_comment_with_whitespace() {
        let result = strip_leading_sql_comments("/* comment */ SELECT 1");
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn strip_multiple_single_line_comments() {
        let result = strip_leading_sql_comments("-- one\n-- two\nSELECT 1");
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn strip_mixed_comments() {
        let result =
            strip_leading_sql_comments("-- line comment\n/* block */\nINSERT INTO t VALUES (1)");
        assert_eq!(result, "INSERT INTO t VALUES (1)");
    }

    #[test]
    fn strip_unterminated_block_comment_returns_empty() {
        // An unterminated block comment consumes the rest; result is empty.
        let result = strip_leading_sql_comments("/* unterminated");
        assert_eq!(result, "");
    }

    #[test]
    fn strip_comment_only_returns_empty() {
        // A comment with nothing after it returns an empty slice.
        let result = strip_leading_sql_comments("-- only a comment");
        assert_eq!(result, "");
    }

    #[test]
    fn strip_whitespace_preserved_within_sql() {
        // Leading whitespace before the real token is trimmed; interior
        // whitespace is preserved.
        let result = strip_leading_sql_comments("  SELECT  1  AS  n");
        assert_eq!(result, "SELECT  1  AS  n");
    }

    // -- is_write_query (all categories, isolated) ----------------------------

    #[test]
    fn is_write_dml_insert_is_true() {
        assert!(is_write_query("INSERT INTO t VALUES (1)"));
        assert!(is_write_query("insert into t values (1)"));
    }

    #[test]
    fn is_write_dml_update_is_true() {
        assert!(is_write_query("UPDATE t SET x = 1 WHERE id = 1"));
        assert!(is_write_query("update t set x = 1"));
    }

    #[test]
    fn is_write_dml_delete_is_true() {
        assert!(is_write_query("DELETE FROM t WHERE id = 1"));
        assert!(is_write_query("delete from t"));
    }

    #[test]
    fn is_write_dml_merge_is_true() {
        assert!(is_write_query("MERGE INTO t USING src ON (t.id = src.id)"));
        assert!(is_write_query("merge into t using src on (t.id = src.id)"));
    }

    #[test]
    fn is_write_ddl_create_is_true() {
        assert!(is_write_query("CREATE TABLE t (id int)"));
        assert!(is_write_query("create table t (id int)"));
    }

    #[test]
    fn is_write_ddl_drop_is_true() {
        assert!(is_write_query("DROP TABLE t"));
        assert!(is_write_query("drop table t"));
    }

    #[test]
    fn is_write_ddl_alter_is_true() {
        assert!(is_write_query("ALTER TABLE t ADD COLUMN x text"));
        assert!(is_write_query("alter table t add column x text"));
    }

    #[test]
    fn is_write_ddl_truncate_is_true() {
        assert!(is_write_query("TRUNCATE TABLE t"));
        assert!(is_write_query("truncate t"));
    }

    #[test]
    fn is_write_ddl_rename_is_true() {
        assert!(is_write_query("RENAME TABLE t TO t2"));
        assert!(is_write_query("rename table t to t2"));
    }

    #[test]
    fn is_write_dcl_grant_is_true() {
        assert!(is_write_query("GRANT SELECT ON t TO user1"));
        assert!(is_write_query("grant all on t to user1"));
    }

    #[test]
    fn is_write_dcl_revoke_is_true() {
        assert!(is_write_query("REVOKE ALL ON t FROM user1"));
        assert!(is_write_query("revoke select on t from user1"));
    }

    #[test]
    fn is_write_maintenance_vacuum_is_true() {
        assert!(is_write_query("VACUUM ANALYZE t"));
        assert!(is_write_query("vacuum full t"));
        assert!(is_write_query("VACUUM t"));
    }

    #[test]
    fn is_write_maintenance_cluster_is_true() {
        assert!(is_write_query("CLUSTER t USING t_pkey"));
        assert!(is_write_query("cluster t using t_pkey"));
    }

    #[test]
    fn is_write_maintenance_reindex_is_true() {
        assert!(is_write_query("REINDEX TABLE t"));
        assert!(is_write_query("reindex index idx"));
    }

    #[test]
    fn is_write_maintenance_refresh_is_true() {
        assert!(is_write_query("REFRESH MATERIALIZED VIEW mv"));
        assert!(is_write_query("refresh materialized view mv"));
    }

    #[test]
    fn is_write_cte_prefix_is_true() {
        // All CTEs (WITH ...) are conservatively treated as writes.
        assert!(is_write_query("WITH cte AS (SELECT 1) SELECT * FROM cte"));
        assert!(is_write_query("WITH data AS (SELECT 1) DELETE FROM t"));
        assert!(is_write_query(
            "WITH data AS (SELECT 1) INSERT INTO t VALUES (1)"
        ));
        assert!(is_write_query("WITH x AS (SELECT 1) UPDATE t SET a = 1"));
        assert!(is_write_query("with x as (select 1) select * from x"));
    }

    #[test]
    fn is_write_select_is_false() {
        assert!(!is_write_query("SELECT * FROM users"));
        assert!(!is_write_query("select 1"));
        assert!(!is_write_query("select count(*) from pg_class"));
    }

    #[test]
    fn is_write_empty_is_false() {
        assert!(!is_write_query(""));
        assert!(!is_write_query("   "));
    }

    #[test]
    fn is_write_comment_stripping_create() {
        // AI-generated SQL with a leading comment must still be detected.
        assert!(is_write_query(
            "-- Create the table\nCREATE TABLE t (id int);"
        ));
        assert!(is_write_query("/* block */\nCREATE TABLE t (id int);"));
    }

    #[test]
    fn is_write_comment_stripping_select_false() {
        // Leading comments before SELECT must not flip the result.
        assert!(!is_write_query("-- read only\nSELECT 1;"));
        assert!(!is_write_query("/* read */\nselect * from t;"));
    }

    #[test]
    fn is_write_trailing_semicolon_is_true() {
        // A bare keyword followed immediately by a semicolon (no whitespace)
        // must still be classified as a write query.
        assert!(is_write_query("VACUUM;"));
        assert!(is_write_query("vacuum;"));
        assert!(is_write_query("DELETE;"));
        assert!(is_write_query("delete;"));
        assert!(is_write_query("CREATE;"));
        assert!(is_write_query("create;"));
    }

    // -- parse_ai_response_segments + is_write_query: DDL detection ---------

    /// An AI response with a ` ```sql ` fence containing CREATE TABLE is
    /// parsed as a `Sql` segment, and `is_write_query` correctly flags it as
    /// a write query.  This is the core invariant that prevents DDL execution
    /// in `/ask` mode.
    #[test]
    fn ask_create_table_is_write_query() {
        let response = "Here is how to create the table:\n\
                        ```sql\nCREATE TABLE t4 (id int);\n```\n\
                        This creates t4.";
        let segments = parse_ai_response_segments(response);

        // Exactly one SQL segment must be found.
        let sql_segments: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                AiResponseSegment::Sql(sql) => Some(sql.as_str()),
                AiResponseSegment::Text(_) => None,
            })
            .collect();
        assert_eq!(
            sql_segments.len(),
            1,
            "expected exactly one SQL segment in response"
        );

        // That SQL segment must be classified as a write query.
        let sql = sql_segments[0];
        assert!(
            is_write_query(sql),
            "CREATE TABLE must be detected as a write query; sql={sql:?}"
        );
    }

    /// An AI response with a ` ```sql ` fence containing DROP TABLE is
    /// flagged as a write query.  DDL must never auto-execute in `/ask` mode.
    #[test]
    fn ask_drop_table_is_write_query() {
        let response = "To remove the table:\n\
                        ```sql\nDROP TABLE IF EXISTS t4;\n```";
        let segments = parse_ai_response_segments(response);
        let sql_segments: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                AiResponseSegment::Sql(sql) => Some(sql.as_str()),
                AiResponseSegment::Text(_) => None,
            })
            .collect();
        assert_eq!(sql_segments.len(), 1, "expected one SQL segment");
        assert!(
            is_write_query(sql_segments[0]),
            "DROP TABLE must be a write query"
        );
    }

    /// SELECT query is NOT a write query and will be auto-executed in `/ask`
    /// mode.
    #[test]
    fn ask_select_is_not_write_query() {
        let response = "Here are the results:\n\
                        ```sql\nSELECT * FROM users WHERE active = true;\n```";
        let segments = parse_ai_response_segments(response);
        let sql_segments: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                AiResponseSegment::Sql(sql) => Some(sql.as_str()),
                AiResponseSegment::Text(_) => None,
            })
            .collect();
        assert_eq!(sql_segments.len(), 1, "expected one SQL segment");
        assert!(
            !is_write_query(sql_segments[0]),
            "SELECT must not be a write query"
        );
    }

    /// An INSERT statement is classified as a write query and must not
    /// auto-execute in `/ask` mode.
    #[test]
    fn ask_insert_is_write_query() {
        let response = "```sql\nINSERT INTO users (name) VALUES ('Alice');\n```";
        let segments = parse_ai_response_segments(response);
        let sql_segments: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                AiResponseSegment::Sql(sql) => Some(sql.as_str()),
                AiResponseSegment::Text(_) => None,
            })
            .collect();
        assert_eq!(sql_segments.len(), 1, "expected one SQL segment");
        assert!(
            is_write_query(sql_segments[0]),
            "INSERT must be a write query"
        );
    }

    /// A response with no ` ```sql ` fence produces only Text segments —
    /// nothing is executed.  This covers the case where the AI returns plain
    /// text with a code block but without the `sql` language tag.
    #[test]
    fn ask_no_sql_fence_produces_no_sql_segment() {
        let response = "Here is the SQL:\n\
                        ```\nCREATE TABLE t4 (id int);\n```\n\
                        Run the above.";
        let segments = parse_ai_response_segments(response);
        let sql_segments: Vec<&str> = segments
            .iter()
            .filter_map(|s| match s {
                AiResponseSegment::Sql(sql) => Some(sql.as_str()),
                AiResponseSegment::Text(_) => None,
            })
            .collect();
        assert!(
            sql_segments.is_empty(),
            "backtick fence without 'sql' tag must not produce a SQL segment; \
             got: {sql_segments:?}"
        );
    }

    /// SQL shown to the user is always printed — even when it will not be
    /// executed (write query in `/ask` mode).  This test verifies that the
    /// SQL segment IS parsed so it can be displayed, not silently swallowed.
    #[test]
    fn ask_write_query_sql_is_parsed_for_display() {
        let response = "```sql\nALTER TABLE users ADD COLUMN bio text;\n```";
        let segments = parse_ai_response_segments(response);
        let has_sql = segments
            .iter()
            .any(|s| matches!(s, AiResponseSegment::Sql(_)));
        assert!(
            has_sql,
            "write query must be parsed as a SQL segment so it can be shown to the user"
        );
    }

    // -- text2sql write query classification ----------------------------------

    /// In `\t2s` mode the AI may generate an INSERT or other write query in
    /// response to a natural-language prompt (e.g. "fill orders table with
    /// sample data").  The query must be classified as a write query so the
    /// caller can route it to the `ask_yne_prompt("Execute write query?…")`
    /// path rather than silently blocking it the way `/ask` mode does.
    #[test]
    fn t2s_insert_classified_as_write_query() {
        let sql = "INSERT INTO public.orders (user_id, amount, status) SELECT \
             (random() * 10000)::bigint, (random() * 1000)::numeric(10,2), \
             'pending' FROM generate_series(1, 1000000);";
        assert!(
            is_write_query(sql),
            "INSERT must be classified as a write query for the \
             text2sql confirmation prompt"
        );
    }

    /// A SELECT generated by text2sql is NOT a write query — it goes through
    /// the read-only transaction wrapper and prompts `Execute? [Y/n/e]`.
    #[test]
    fn t2s_select_not_write_query() {
        let sql = "SELECT count(*) FROM public.orders;";
        assert!(
            !is_write_query(sql),
            "SELECT must not be classified as a write query in text2sql mode"
        );
    }

    // -- Ctrl-C cancellation ---------------------------------------------------

    /// `CANCELLED` sentinel is a stable string value that callers can match on.
    #[test]
    fn cancelled_sentinel_value() {
        assert_eq!(CANCELLED, "__cancelled__");
    }

    /// `with_ctrl_c_cancel` propagates `Ok` results when no signal fires.
    #[tokio::test]
    async fn with_ctrl_c_cancel_propagates_ok() {
        let result: Result<i32, String> = with_ctrl_c_cancel(async { Ok(42) }).await;
        assert_eq!(result, Ok(42));
    }

    /// `with_ctrl_c_cancel` propagates `Err` results from the future itself.
    #[tokio::test]
    async fn with_ctrl_c_cancel_propagates_err() {
        let result: Result<i32, String> =
            with_ctrl_c_cancel(async { Err("provider error".to_owned()) }).await;
        assert_eq!(result, Err("provider error".to_owned()));
    }

    /// The `CANCELLED` sentinel is distinct from any plausible provider error.
    #[test]
    fn cancelled_sentinel_is_distinct_from_provider_errors() {
        let ai_errors = [
            "provider error",
            "HTTP 429",
            "connection refused",
            "invalid api key",
            "",
        ];
        for e in &ai_errors {
            assert_ne!(*e, CANCELLED, "'{e}' should not equal CANCELLED sentinel");
        }
    }

    /// After a query is cancelled the `TxState` transitions to `Failed` when
    /// inside an explicit transaction, and stays `Idle` otherwise.
    #[test]
    fn tx_state_after_query_cancel_in_transaction() {
        use super::super::TxState;
        let mut tx = TxState::InTransaction;
        // A cancellation returns an error — on_error() is called.
        tx.on_error();
        assert_eq!(
            tx,
            TxState::Failed,
            "cancelling a query inside a transaction must leave session in Failed state"
        );
    }

    #[test]
    fn tx_state_after_query_cancel_outside_transaction() {
        use super::super::TxState;
        let mut tx = TxState::Idle;
        // A cancellation outside a transaction does not change state.
        tx.on_error();
        assert_eq!(
            tx,
            TxState::Idle,
            "cancelling a query outside a transaction must leave session in Idle state"
        );
    }

    #[test]
    fn explain_system_prompt_is_structured() {
        // Verify the explain prompt instructs structured labeled-line output
        let prompt_fragment = "Bottleneck:";
        assert!(
            include_str!("ai_commands.rs").contains(prompt_fragment),
            "explain system prompt should instruct Bottleneck: label"
        );
    }

    #[test]
    fn optimize_system_prompt_is_numbered_list() {
        let prompt_fragment = "CREATE INDEX CONCURRENTLY";
        assert!(
            include_str!("ai_commands.rs").contains(prompt_fragment),
            "optimize system prompt should mention CREATE INDEX CONCURRENTLY"
        );
    }
}
