---
name: dev
description: Implement a finished specification (specs/{nr}-{name}-specification.md) of Mumbot (headless Mumble bots) end to end – task breakdown, build, test, verification in the running bot against the local Mumble test server. Team mode (lead only delegates, one fresh agent per task) is the default; pass --no-team to work in a single session.
argument-hint: "[spec number | name | path] [--no-team]"
disable-model-invocation: true
allowed-tools: Glob, TaskCreate, TaskUpdate, TaskList, TaskGet, TaskStop
---

# /dev – Build a specification

Arguments: **$ARGUMENTS**

You turn a finished specification into working, verified software for Mumbot, headless bots that are ordinary Mumble clients.
The user is a **product manager and not a technical person**, so report outcomes in plain, understandable language.

## Non-negotiable rules (both modes)

1. **Task list is mandatory.** Every piece of work lives in the task list (`TaskCreate` / `TaskUpdate` / `TaskList` / `TaskGet`): analysis, every implementation step, every verification, fixes, the report. Rules:
   - Nothing gets built without a task.
   - Set status `in_progress` on start, `completed` only when done **and** verified.
   - Newly discovered work becomes a new task.
   - Model the order with task dependencies.
   - If the Task tools are unavailable (they are missing in some Claude Code versions even with `CLAUDE_CODE_ENABLE_TASKS=1`), keep the task list as the file `specs/{nr}-{name}-tasks.md` instead: one table row per task with ID, title, status (`open` / `in progress` / `done` / `blocked`), owner, dependencies, complexity and covered `AC-…`/files. The owner column records which agent ran the task. The same rules apply. Only the lead edits this file; agents report status changes in their final report.
2. **The spec is the contract.** Build exactly what `specs/{nr}-{name}-specification.md` describes: MVP scope, no extras. On gaps or contradictions, ask the user (AskUserQuestion, plain language, recommended option first) instead of guessing. Earlier specifications and `CLAUDE.md` rules (if present) apply too.
3. **Done means verified in the running bot.** Every acceptance criterion (`AC-…`) of the spec must be checked against the running bot, connected to the **local Mumble test server** (`testenv/up.sh`, unmodified `mumble-server` in Docker) and, where the AC needs it, an observer client (`Bot` in a live test) or calls to the bot's API (`curl`, live tests with `MUMBOT_LIVE=1`). `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must also pass; Docker or release changes also need `docker build` plus a `/health` check. Failures are reported honestly. Audio aspects that can't be judged automatically (e.g. how a talking bot sounds) are verified as far as possible (levels, packets, codec numbers) and flagged for a manual listening check.
4. **Nothing outward-facing without explicit user approval:** no commit, push, release, publishing packages or app-store uploads, code signing with real keys, or connecting to public/third-party Mumble servers. Tests use local test servers only.
   - Commits only when the user asks, never `git add -A`, and branch first if on the default branch. Small, focused Conventional Commits; never any AI attribution (no `Co-Authored-By`, no "Generated with", no mention of Claude or AI).
   - Secrets (signing keys, server passwords, private certificates) never go into the repo.
5. **One task = one fresh agent.** Whenever work is delegated, each task gets its own fresh agent with a self-contained, short prompt. Never bundle several tasks into one agent. The agent ends after its final report (≤ 10 lines); follow-up work goes to a new agent.

## Step 1 – Parse arguments and resolve the spec

- `--no-team` anywhere in the arguments turns team mode **off**. Otherwise **team mode is on**.
- The remaining text identifies the spec: a number (`1` → `specs/01-*-specification.md`), a name fragment, or a path.
- Use Glob on `specs/*-specification.md` to resolve it. Only look at file names here, don't read file contents yet.
- **No identifier** → list the specification files and ask the user which one to build. Stop until they answer.
- **Only a brainstorming file exists** → tell the user to finish it with `/spec` first. Stop.
- **`specs/{nr}-{name}-implementation.md` exists** → this is a resume. The analysis step then also reads that report and the existing task list, and continues from there.

## Step 2 – Branch by mode

- **Team mode (default)** → follow [team-mode.md](team-mode.md) exactly. As team lead you **only delegate**, one fresh agent per task.
- **`--no-team`** → follow the single-session workflow below.

The phases are the same in both modes; only who does the work differs:

| Phase | Result |
|---|---|
| A · Analysis & breakdown | Spec, CLAUDE.md, earlier specs and existing code understood. Blocking open points clarified with the user. Task list created with dependencies, file ownership, and complexity (`simple`/`complex`) per task. Test server setup known (or a task to create it). |
| B · Build | Tasks implemented in dependency order. Tests are written alongside. |
| C · Verification | Tests, type checks and linters pass. Every `AC-…` checked against the running bot and the local test server (live tests against `testenv/`, `curl` against the bot's API, observer bots, the Docker image). Failures turn into fix tasks, then verification runs again. |
| D · Wrap-up | Report `specs/{nr}-{name}-implementation.md` written using [report-template.md](report-template.md). Final message to the user. |

**Complexity rule.** A task is `complex` if it matches any of these:
- It touches more than ~3 files.
- It introduces a new module, persisted data format/migration, or new dependency.
- It concerns voice (Opus encoding, transmission) or concurrency/async behaviour (locks, timeouts, reconnect).
- It concerns the Mumble protocol, the bot's HTTP/API surface, TLS/certificates, API keys, security or privacy.
- It involves platform-specific code, the `mumcord-protocol` dependency tag, or build/packaging/Docker/CI.
- It needs an architecture decision, or the spec leaves room for interpretation.

Everything else is `simple`.

## Single-session workflow (`--no-team`)

- Do the phases A–D yourself, strictly through the task list.
- Keep your own context lean: delegate broad codebase searches and long test or verification runs to subagents (Explore / general-purpose) and use only their conclusions.
- Heavy tasks may also go to a subagent. Same rule as team mode: one fresh subagent per task, self-contained short prompt, smallest suitable model, report ≤ 10 lines, no reuse.
- For `complex` tasks, write a short plan into the task description before touching code: approach, files, risks, how it gets verified. Then implement.
- For verification, prefer a fresh subagent that did not write the code. The subagent checks the acceptance criteria and reports pass/fail per `AC`.

## Final message to the user (both modes)

Short, plain language:
- ✅ / ❌ per acceptance criterion (mark ones that still need a manual listening check)
- How to try it yourself (start the test server, write `bot.toml`, start the bot, send a request, steps)
- What's open or deviating, and why
- Where the report lives
- Question: commit now? (and push/release, if relevant)
