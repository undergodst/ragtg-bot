# Guide for AI agents (Claude, Devin, Codex, Cursor, Aider, …)

This file is the entry point for **any** AI coding agent picking up work on this repo. It does not duplicate `CLAUDE.md` — it points you at the right artifacts and lists the few rules that aren't obvious from reading the code.

## What this project is

Telegram bot for a 20–50 person group chat. Acts as a chat member (not a Q&A assistant), trolls in 2ch style, remembers users and context. Stack: Rust (teloxide) + SQLite (sqlx) + Qdrant + Redis + OpenRouter LLMs (DeepSeek Flash main, Nemotron Omni for vision/audio).

**Source of truth for design:** [`CLAUDE.md`](CLAUDE.md) — read it before non-trivial changes. Architecture, schema, prompts, decision logic, all there.

**Step-by-step prompts the human originally followed:** [`PROMTS.md`](PROMTS.md). Useful for understanding scope of each step.

**Quick stack digest:** [`CHEATSHEET.md`](CHEATSHEET.md).

## Where we are right now

The roadmap in `CLAUDE.md` has 14 numbered steps. **Don't trust this file** for the current cursor — query git directly:

```bash
git log --oneline --grep='^feat(step' main
```

Each completed step lands as a single `feat(stepN): <slug>` commit on `main`, merged from a feature branch. The next open step is the lowest number missing.

## What to work on next

If the human hasn't told you a specific step, the default is: **the lowest-numbered roadmap step that doesn't yet have a `feat(stepN):` commit on main.** Open `CLAUDE.md` § "Roadmap (порядок реализации)" for the description.

If the human asks "что осталось" / "what's left", run the grep above and reply with the gap list — don't recite this file.

Steps may have explicitly-deferred sub-pieces. Search the codebase for `v1` / `v2` / `TODO` / `step6b` markers before assuming something is new work versus already-scoped follow-up. Examples currently in tree:
- video notes (kruzhki) and animated stickers — perception pipeline v1 skips them on purpose, see `src/bot/handlers.rs::classify_media`.

## Hard rules — read these before your first commit

### 1. Never commit directly to `main`

The repo uses a branch-and-merge flow for everything, even one-line fixes. Pattern:

```bash
git checkout -b <feat|fix|docs>/<slug>
# ... work ...
git add <specific files>
git commit -m "<conventional message>"
git push -u origin <branch>
git checkout main
git merge --ff-only <branch>
git push origin main
```

Use a real PR if your platform mandates one (Devin), but the merge target is always `main` — there is no `develop`. Existing commits follow `feat(stepN): ...` for roadmap work, `fix: ...` for bug fixes, `docs: ...` for documentation, `chore: ...` for plumbing.

### 2. The sqlx offline cache MUST stay in sync with `query!` macros

Compile-time checked queries live in `.sqlx/`. Every `sqlx::query!` / `sqlx::query_as!` invocation in source needs a matching `.sqlx/query-<hash>.json` or `cargo check` fails under `SQLX_OFFLINE=true` (which CI and Dockerfile use).

When you add or change a `query!` invocation:

```bash
# one-time setup if data/bot.db doesn't exist:
DATABASE_URL='sqlite:./data/bot.db?mode=rwc' sqlx database create
DATABASE_URL='sqlite:./data/bot.db' sqlx migrate run --source ./migrations

# regenerate cache:
DATABASE_URL='sqlite:./data/bot.db' cargo sqlx prepare
git add .sqlx/
```

If `sqlx-cli` isn't installed: `cargo install sqlx-cli --no-default-features --features sqlite,rustls --version "^0.8"`.

**Trick that avoids regenerating the cache:** if your new query differs from an existing cached one only by hard-coded literals, parameterise the literals so both queries share a hash. Done once already in `persist_bot_reply` — see git history.

### 3. Build / test commands

```bash
SQLX_OFFLINE=true cargo check                     # fast loop
SQLX_OFFLINE=true cargo test storage::redis       # rate-limit + media cache (live Redis)
SQLX_OFFLINE=true cargo test memory::working      # working-memory window (live Redis)
SQLX_OFFLINE=true cargo test                      # everything
```

Integration tests are written in a **skip-if-unreachable** style: they noop when Redis isn't on `localhost:6379`. Don't replace them with mocks. If you need a live Redis: `docker compose up -d redis`.

### 4. Code style guardrails (project-specific, not generic Rust)

- No `unwrap()` / `expect()` in non-test code. Either `?` or explicit `match`. Tests can use `expect("...")`.
- All SQL goes through `sqlx::query!` / `query_as!` for compile-time checking. **No** raw query strings via `query()`.
- LLM prompts live in `src/llm/prompts/*.rs` as `pub const &str`. Don't inline them into call sites.
- Personality is a **separate concern** from infrastructure. Don't sprinkle "haha funny" hardcoded lines into handlers — that goes through `SYSTEM_PROMPT_BASE` in `src/llm/prompts/system.rs` and (eventually) few-shots in `src/llm/prompts/examples/`.
- Don't log API keys or full message bodies. Truncate user text to ~100 chars in tracing fields.

### 5. Things deliberately NOT in scope yet

CLAUDE.md describes the full target system. Several pieces are intentionally stubbed (1-line `// TODO` files) and SHOULD NOT be built ahead of their roadmap step:

- `src/decision.rs` — step 9
- `src/personality.rs` — step 13
- `src/memory/{episodic,semantic,lore}.rs` — steps 7, 8, 10
- `src/llm/embeddings.rs` — step 7 (used from there)
- `src/tasks/{summarize,extract_facts}.rs` — steps 7, 8
- `src/llm/prompts/{summary,facts,decision}.rs` — paired with their tasks

If you're tempted to build one preemptively because "I noticed it's empty", don't. The numbered scope is the contract.

## Run / deploy

See [`README.md`](README.md) § "Локальный запуск". TL;DR: fill `.env`, `cp config/config.toml.example config/config.toml`, `mkdir -p data`, `docker compose up --build`.

**One thing missing from README:** for the bot to receive non-command messages in groups, set Privacy Mode = OFF in BotFather (`/setprivacy` → choose bot → Disable). Otherwise teloxide only sees `/commands` and direct mentions miss.

## Memory / state notes for agents that have a memory layer

If you're an agent with persistent memory across sessions (Claude Code, Cursor with memory, …):

- **Don't memorize the roadmap snapshot.** It rots. Use the `git log` query above.
- **Do memorize project-specific gotchas** — sqlx cache rule, branch workflow, integration-test pattern — that aren't obvious from a single file read.
- **Don't memorize stack/architecture facts** — they're in `CLAUDE.md` and the code, both authoritative.

## Who's working in this repo

- The human owner makes the design calls and the merges.
- Multiple AI agents have contributed: commits authored as `Devin AI`, `Claude` co-authors, etc. Don't assume any single contributor's commit pattern reflects house style — `CLAUDE.md` and this file are the contract.
