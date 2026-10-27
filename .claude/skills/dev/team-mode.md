# Team mode – protocol for the team lead

You are the **team lead**. Your job is coordination only. Your context must stay free for coordinating, so everything else is delegated.

## Core principle: one task = one fresh agent

Long-lived agents that own many tasks grow huge contexts (every resume replays them) and flood the lead with status messages. So:

- **One agent per task.** Every task (analysis, build step, fix, verification, report) gets its own **fresh** agent. Never assign several tasks to one agent – no batches.
- **The agent ends after its task.** It sends exactly one final report (≤ 10 lines), then you stop it with `TaskStop` and the agent's name. No standby, no reusing a finished agent for new work. A stopped agent could be resumed via `SendMessage`, but only to finish its own task (e.g. a question about its report) – never for a new task.
- **Follow-up work = new agent.** Fixes, re-verification or a changed plan go to a new fresh agent that gets the facts it needs in its prompt.
- **Plan and build stay in one agent.** For a `complex` task the same agent explores read-only, sends a plan, waits for `APPROVED`, then implements. That is one task, not a batch.

## What the lead may and may not do

**Allowed – coordination only:**
- Resolve the spec file name (Glob on file names).
- Use the task list tools (`TaskCreate`, `TaskUpdate`, `TaskList`, `TaskGet`), or maintain the fallback task list file `specs/{nr}-{name}-tasks.md` when those tools are unavailable (see SKILL.md).
- Spawn one agent per task with the Agent tool **with a `name`**, answer its plan (`SendMessage`), and stop it after its final report (`TaskStop` with the agent's name).
- Review short plans and final reports.
- Ask the user questions (AskUserQuestion) and report to the user.

**Forbidden – always delegate:**
- **Coding:** writing, editing or creating any file, including the implementation report.
- **Research:** reading the spec, code, docs or logs, searching the codebase, or fetching web pages (including Mumble protocol docs).
- **Verification and testing:** running builds, tests, linters, the bot, the test server, or reviewing diffs.
- **Bash** for anything but trivial orchestration checks.

If you catch yourself about to do one of these, create a task and spawn an agent for it instead.

## Preconditions

- Agent teams must be enabled (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`). If you cannot spawn named agents, tell the user and offer `/dev <spec> --no-team`. Don't silently fall back.
- Agent teams are experimental. If an agent hangs or dies, stop it (`TaskStop`) and spawn a fresh agent for the same task, with the facts of what is already done.

## Keeping token use low

- **Communication goes through the lead only.** Agents don't message each other, send no idle or status pings and no repeated summaries. The only messages are: a plan (complex tasks), a blocking question, and the final report.
- **Contracts between tasks** (module interfaces, message/event types, API request/response shapes, config keys) travel through the lead – copied into the next agent's prompt – or through a file in the repo/`specs/` that the next agent reads.
- **Short prompts.** Copy in only what this task needs: decisions, contracts, file paths. No history, no full spec excerpts when a path is enough.
- **Smallest suitable model.** Set the Agent tool's `model` per agent: a small/cheap model for simple, well-described tasks and plain verification runs; the default model for `complex` tasks, the analysis and plan reviews.
- **Reports ≤ 10 lines**, file paths instead of file contents, no dumps.

## Parallelism and file ownership

- Independent tasks run **in parallel as separate agents** at the same time.
- **File ownership per agent:** no two running agents edit the same files. Put shared files, such as build config, dependency manifests, `src/lib.rs` or the CLI definition (`src/main.rs`) shell, in their own task, and make dependent tasks wait for it.
- **Shared resources:** parallel QA agents must not fight over the same test server port, HTTP port or build directory. Give each its own instance (`MUMBOT_PORT_BASE`, `MUMBOT_PROJECT`, own `CARGO_TARGET_DIR`), or serialize them.
- **Dependent tasks start only after** their dependency's agent has sent its final report and the task is `completed`.
- **Verifiers are independent:** the agent that verifies a task must not be the one that implemented it (automatically true with fresh agents, but don't pass it the builder's reasoning – only what was built and where).
- Name agents after their task so messages stay clear, e.g. `b3-http-api`, `b5-config`, `c2-qa`, `f1-fix-reconnect`.

## Phase flow

### A · Analysis & breakdown
1. Create task `A1 Analysis & breakdown`.
2. Spawn one **analyst** agent for A1. Its job:
   - Read the spec, CLAUDE.md (if present), earlier specs, the implementation report (on resume) and the existing code.
   - Return a compact breakdown. Per task: title, 1–3 sentence description, covered `FR-`/`AC-` IDs, owned files/areas, dependencies, `simple`/`complex`, key decisions/contracts the task's agent must know.
   - Return blocking open questions for the user in plain language, each with a recommended answer.
   - Return the verification approach: which tests, how to start the local Mumble test server, how to drive the bot (live tests, `curl` against its API, observer bots), what needs a manual check.
3. Stop the analyst after its report (`TaskStop`).
4. Ask the user the blocking questions (AskUserQuestion). If the answers change the breakdown substantially, spawn a fresh analyst with the previous breakdown and the answers.
5. Create all tasks in the task list with dependencies:
   - Keep tasks small enough for one agent: one coherent change with clear file ownership.
   - If no reproducible test server setup exists yet, add it as an early task.
   - Every `AC-…` is covered by at least one verification task.
   - Always add a final task `D1 Implementation report` that depends on all verification tasks.

### B · Build
1. For every task whose dependencies are `completed`, spawn a fresh agent with the spawn prompt below. Start all such independent tasks in parallel.
2. Record in the task list which agent ran the task (task owner / fallback file column "owner").
3. **Plan approval for `complex` tasks (enforced):**
   - The agent must not edit files before its plan is approved.
   - It explores read-only, then sends you a plan of ≤ 15 lines: approach, files, data formats/dependencies, concurrency concerns, risks, how it will be verified.
   - Check the plan against spec scope, file ownership, protocol compatibility, CLAUDE.md rules and dependencies. Reply `APPROVED` or `REJECTED: <reason/changes>`.
   - If judging the plan needs deep technical review, spawn a separate reviewer agent (read-only, one plan) instead of reading code yourself.
4. On a final report: check that the task is `completed` in the task list, stop the agent (`TaskStop`), note contracts the next tasks need, then spawn the agents for newly unblocked tasks.
5. Discovered work from a report becomes a new task, run later by a new agent.
6. Anything outward-facing (commit, push, release, signing, public servers, external accounts) → ask the user first.

### C · Verification
1. Spawn one fresh **QA agent per verification task**, never one that built the code. It:
   - Builds the bot and starts its own local Mumble test server instance (`MUMBOT_PORT_BASE=<port> MUMBOT_PROJECT=<name> testenv/up.sh`).
   - Runs tests, type checks and linters.
   - Checks its `AC-…` IDs against the running bot (observer bot or API calls where needed).
   - Reports per `AC` in one line: `AC-1.3 ✅`, `AC-1.3 🎧 <what to listen for>` (automated checks pass, manual listening check needed) or `AC-1.3 ❌ <symptom>`.
2. Every ❌ becomes a fix task, run by a new fresh agent that gets the symptom, the affected files and the relevant contracts. Re-verification is another new QA agent.
3. Repeat until every AC passes, or it is clear that something cannot be done without the user. In that case, ask the user.

### D · Wrap-up
1. A fresh agent writes `specs/{nr}-{name}-implementation.md` using [report-template.md](report-template.md). Give it the task list location and the key facts from the final reports.
2. Make sure no agent and no test server instance is still running.
3. Send the final message to the user (see SKILL.md).

## Spawn prompt template (one task)

The agent has no shared history. It sees only CLAUDE.md, skills and this prompt, so the prompt must be self-contained – but short.

```text
You are agent "<name>" building ONE task of <spec path> for Mumbot (headless Mumble bots).
Team lead: coordinates only – it does not read code, so report conclusions, not dumps.

Task <ID> – <title> (<simple|complex>): <description, covered FR-/AC- IDs>
You own these files/areas: <paths>. Don't edit anything else; if you need a change elsewhere, tell the lead in your report.
Decisions/contracts you must follow: <copied facts, or path to the file that holds them>
Results of dependencies: <task ID → file paths / contracts>
Verification to run: <build, tests, type checks, linters, check against the running bot and the local test server on port <port>>

Rules:
- Read CLAUDE.md (if present) and only the spec sections relevant to this task. The spec is the contract – MVP scope, no extras. Unclear? Ask the lead, don't guess.
- Every Mumbot bot is a pure client: stay compatible with unmodified standard Mumble servers; never build or require anything server-side, no protocol extensions.
- Mark the task in_progress on start and completed only when done AND verified (fallback task list file: the lead updates it, you just report).
- COMPLEX task only: explore read-only first, then SendMessage the lead a plan (≤15 lines: approach, files, data formats/deps, concurrency concerns, risks, verification). Do NOT edit any file until the lead replies APPROVED.
- Write tests alongside code. Build, tests, type checks and linters must pass for your changes.
- Talk to the lead only. No messages to other agents, no status or idle pings.
- Never commit, push, release, sign with real keys, or connect to public Mumble servers. Never put secrets in files. Stop any test server you started.
- Finish with ONE final report to the lead (≤10 lines): status, task ID, changed file paths, contracts the next tasks need, new work discovered, anything open. Then stop – take no further work.
```

Adapt the template per task type. QA agents get the `AC-…` IDs to check and the rule "you did not build this; verify it independently". Fix agents get the ❌ symptom. The analyst gets the output format from phase A.
