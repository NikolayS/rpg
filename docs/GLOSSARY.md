# GLOSSARY.md — rpg vocabulary

Shared vocabulary for design discussions, code review, issues, and PRs. When
multiple terms exist for the same concept, use the one in this file. New terms
go here first, code second.

Scope: terms needed to talk about the **command-dispatch refactor** and the
**architecture review** vocabulary that frames it. Not exhaustive of the
codebase — entries are added as needs arise.

## Architecture vocabulary

These are the only words used to talk about structure. Do not substitute
synonyms ("component", "service", "layer", "boundary") — consistency is the
point.

- **Module** — anything with an interface and an implementation: a function, a struct + its `impl`, a file, a `mod`, a crate.
- **Interface** — everything a caller must know to use the module: signatures, invariants, error modes, ordering, configuration. Not just types.
- **Implementation** — the code inside the module.
- **Depth** — leverage at the interface: lots of behaviour behind a small interface.
- **Deep module** — small interface, large implementation. High leverage.
- **Shallow module** — interface nearly as complex as the implementation. Low leverage; usually a pass-through.
- **Seam** — where an interface lives; a place behaviour can be altered without editing in place.
- **Adapter** — a concrete type satisfying an interface at a seam.
- **Leverage** — what callers gain from depth: terse, intent-shaped call sites.
- **Locality** — what maintainers gain from depth: change, bugs, knowledge concentrated in one place.
- **Deletion test** — imagine deleting the module. If complexity vanishes, it was a pass-through. If complexity reappears across N callers, it was earning its keep.
- **Earned complexity** — complexity that survives the deletion test.
- **Hypothetical seam** — one adapter today, no second one in sight. Likely premature.
- **Real seam** — two or more adapters. Worth designing for.

## Project framing

- **rpg** — this project: a Postgres terminal in Rust, psql-compatible, with built-in DBA diagnostics and an AI assistant. Single static binary.
- **REPL** — the rpg interactive loop: read input → parse → dispatch → execute → render.
- **psql-compat** — the contract that `\`-commands behave identically to psql. Verified by `tests/compat/test-psql-regress.sh` (regression suite) and `tests/compat/test-compat.sh` (golden output diff).
- **Wire protocol** — the Postgres frontend protocol; rpg speaks it via `tokio-postgres`.

## Command system

The two namespaces are surface vocabulary; downstream code uses different
names — keep both straight.

- **Backslash command** — a `\`-prefixed command. psql-compatible by definition. Any command psql has uses `\`.
- **Slash command** — a `/`-prefixed command. rpg-native extension. Any command rpg adds uses `/`.
- **Meta-command** — synonym for backslash command in the parser layer. The Rust type is `MetaCmd` (`src/metacmd.rs`). Do not use "meta-command" to mean any non-SQL input — only backslash commands.
- **`MetaCmd`** — typed enum of recognized backslash commands.
- **`ParsedMeta`** — the parser output: `MetaCmd` plus modifiers (`+`, `S`), pattern argument, and `echo_hidden` flag.
- **Dispatcher** — function that maps a parsed command to a handler invocation. Today: `dispatch_meta` (backslash) and `dispatch_ai_command` (slash).
- **Handler** — the function that executes one specific command (e.g., `handle_ai_ask`, `apply_timing`).
- **Modifier** — psql-style flags `+` (extra detail) and `S` (include system objects). Carried on `ParsedMeta`.
- **Pattern** — the optional argument to `\d*` commands matching schema-qualified names (e.g., `\dt public.*`).
- **Conditional block** — `\if` / `\elif` / `\else` / `\endif` state, owned by `settings.cond`.
- **Token-budget gate** — the AI cost-control check that blocks AI commands when the conversation has exhausted its token budget. Currently expressed as a hand-curated exemption list (`src/repl/ai_commands.rs:399`).

## Command-dispatch refactor terms

Terms introduced by the refactor (issue #827). Not in code yet — used in
design discussion and PR descriptions.

- **`SlashCmd`** — proposed typed enum mirroring `MetaCmd` for `/`-commands. Lives in `src/slashcmd.rs`.
- **`ParsedSlash`** — proposed parser output for slash commands.
- **`Command`** — proposed unified abstraction (trait or wrapping enum) covering both `MetaCmd` and `SlashCmd`. The seam at which both dispatch paths converge.
- **Command Registry** — the table that maps a parsed `Command` to its handler. Replaces the two `match` statements in today's dispatchers.
- **`CommandCtx`** — proposed bundle of execution dependencies (`client`, `params`, `settings`, `tx`) passed to every handler. Single place to extend handler capabilities.
- **`MetaResult`** — the existing handler return enum (`Continue` / `Quit` / `Reconnected` / `ClearBuffer` / `PrintBuffer` / …). Stays.
- **Category** — grouping of commands for behaviours that vary across them: AI, DBA, session, output, conditional, …. The token-budget gate becomes a property of the category, not a hand-curated list.

## REPL state

- **`ReplSettings`** — the struct threaded through every handler. Mixes configuration (timing, format) and state-machine fields (conversation, conditional stack, output target). Slated for a future split (separate refactor — out of scope for #827).
- **`TxState`** — current transaction state (idle, in-block, failed). Tracked alongside `ReplSettings`.
- **`ExecMode`** — `Plan` / `Interactive` / `Yolo`. Determines how `/ask` and text2sql forwarding behave.
- **`InputMode`** — `Sql` / `Text2Sql`. Determines whether bare lines are treated as SQL or natural-language prompts.
- **`ConversationContext`** — AI chat history plus token accounting. Lives on `ReplSettings` as the `conversation` field. Shorthand "the conversation" refers to this struct.

## Subsystems referenced in design

Brief — full descriptions live in code or `docs/blueprints/SPEC.md`.

- **DBA diagnostics** — the `/dba` family: activity, locks, bloat, indexes, vacuum, replication, config. Implemented in `src/dba.rs`.
- **Active Session History (`/ash`)** — wait-event timeline; depends on the `pg_ash` Postgres extension.
- **AI assistant** — `/ask`, `/fix`, `/explain`, `/optimize`, `/describe`, `/clear`, `/compact`, `/budget`, `/init`. Multi-provider (Anthropic, OpenAI, Ollama).
- **Provider** — an LLM backend behind the `LlmProvider` trait (`src/ai/`).
- **Catalog reader** — proposed future module returning typed Postgres catalog data instead of raw rows. Out of scope for #827.

## Disambiguations — say this, not that

| Don't say | Say instead | Why |
|---|---|---|
| "AI command" (when you mean any `/`-command) | "slash command" | `/dba`, `/session`, `/ash`, `/profiles` are not AI. The dispatcher is misnamed. |
| "Component" / "service" / "layer" | "module" | Architecture vocabulary uses one word. |
| "Boundary" | "seam" | Specific term with a specific meaning. |
| "Refactor for cleanliness" | "deepen" / "make less shallow" | Cleanliness is unmeasurable; depth has the deletion test. |
| "Helper" / "utility" | name what it does | "helper" is a smell; if you can't name it, the abstraction isn't earning its keep. |
| "Meta-command" (for slash commands) | "slash command" | `MetaCmd` is the backslash type. Don't overload. |

## Conventions

- **Copyright** — always `Copyright 2026`. Never a year range.
- **Units** — binary in docs (GiB, MiB), PG format in PG config (`shared_buffers = '32GB'`).
- **Timestamps** — ISO 8601 in static content; relative + ISO tooltip in dynamic UI.
