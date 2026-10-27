---
name: spec
description: Run a round-based brainstorming with a non-technical product manager for an epic from specs/roadmap.md or any other feature of Mumbot (headless Mumble bots), recorded in specs/{nr}-{name}-brainstorming.md, then write specs/{nr}-{name}-specification.md and commit it. No implementation.
argument-hint: "[epic number | feature name or description]"
disable-model-invocation: true
allowed-tools: Read, Write, Edit, Glob, Grep, Bash(ls *), Bash(git status *), Bash(git init *), Bash(git add *), Bash(git commit *), Bash(git log *), Bash(git rev-parse *)
---

# /spec – Brainstorming → Specification

Target: **$ARGUMENTS**

You guide the user through a structured brainstorming for one epic or feature of **Mumbot** (headless bots that speak the Mumble protocol as ordinary clients and stay compatible with existing Mumble servers) and turn the result into a specification.
The user is a **product manager and not a technical person**. They rely on you for technology decisions (networking, protocol, APIs, hosting, frameworks), so explain technical topics in plain language, always give a clear recommendation, and keep the reasoning short.

**This skill is about thinking and writing documents only. Never start implementation.** Don't scaffold, install or write code.

## Hard rules

1. **Just one round at a time.** Ask me one round of questions, I answer, then we repeat until it's done.
   Append exactly one round to the brainstorming file, tell the user it is ready, then **stop and wait**. Never write two rounds in one turn, and never answer questions on the user's behalf.
2. **Questions only live in the brainstorming file.** Ask them only in `specs/{nr}-{name}-brainstorming.md`, not in chat and not with AskUserQuestion. Chat messages stay short: what the round covers and that it's waiting for answers.
3. **Append only.** Never change, reorder or delete existing questions, answers or notes in the brainstorming file. New content always goes at the end. Corrections, follow-ups and explanations become new questions in the next round. So don't put mutable elements such as status checkboxes or "open/done" tables into the file.
4. **Options use `[ ]` checkboxes.** Mark the recommended option with ⭐ and **preselect** it with `[x]`. Every question gets a `✍️ Note:` line for free-text answers. See [question-format.md](question-format.md).
5. **MVP scope.** Keep to the roadmap's scope. Offer nice-to-haves as clearly labeled "later" options, not as recommendations.
6. **Pure client, protocol compatibility is a given.** Every Mumbot bot is a client only and must work with unmodified standard Mumble servers (murmur / mumble-server). Never offer options that need anything installed, changed or configured on the server side (no companion services, plugins or server settings). A bot's own interface (e.g. an HTTP API on the bot's machine) is fine. If a wish can only be met server-side, say so plainly and offer the best client-only alternative.
7. **Git commit when the specification is written.** See step 6.

## Step 1 – Determine the target

Read `specs/roadmap.md` (if it exists) and list `specs/`.

- `$ARGUMENTS` is a number (e.g. `2`) → the matching epic in the roadmap.
- `$ARGUMENTS` names or describes a feature → match it to a roadmap epic if it clearly belongs to one. Otherwise treat it as a standalone feature, even if it is not in the roadmap.
- `$ARGUMENTS` is empty → reply with a short list of the roadmap epics and the existing spec files, ask in chat which topic to do, and stop. This is the only case where you ask something in chat.
- **No roadmap exists yet** → treat the target as a standalone feature. If it is the very first spec, it is the foundation: round 1 also covers the product vision, target platforms and the overall tech stack.

**File naming** `specs/{nr}-{name}-brainstorming.md` / `specs/{nr}-{name}-specification.md`:
- `{nr}` is two digits. Epics use their epic number. Standalone features take the next free number above both the highest existing spec number and the number of roadmap epics.
- `{name}` is short English kebab-case, e.g. `01-foundation-connection-voice`, `02-channel-tree-chat`.

## Step 2 – Resume or start

- **Specification already exists** → ask in chat whether to revise it. If yes, append a new round `Addendum` to the brainstorming file and later update the specification, followed by a new commit.
- **Brainstorming file exists** → read it completely and continue with the next round (step 4). If the user's latest round looks unanswered, i.e. unchanged defaults and no comments, briefly ask in chat whether they're done with it before appending anything.
- **Nothing exists** → gather context (step 3), then create the file with a header and round 1.

## Step 3 – Gather context before round 1

- Read the roadmap section for the target and the cross-cutting decisions in `specs/roadmap.md`, if present.
- Read **all existing specifications** in `specs/`. Decisions made there, such as the tech stack, target platforms, packaging, API style or data model, are settled. Don't ask about them again. Reference them instead ("settled in Spec 01") and only ask what is new or different.
- If code exists in the project, skim the relevant parts so questions match reality.
- Keep the Mumble domain in mind: servers, channel tree, users, ACLs/permissions, access tokens (channel passwords), certificates as identity, registration, text chat and its HTML subset, whisper/shout, Opus voice (only for bots that talk), plugin data between clients. And the bot side: outside systems calling the bot, API keys, rate limits, running as a service or container.
- **Document language:** English, always – specs, brainstorming files, roadmap and chat.

## Step 4 – Rounds

Plan the rounds up front and show the plan once in the file header as a simple list without status markers. A typical sequence, adapted to the topic:

| Round | Topic |
|---|---|
| 1 | Functionality & scope: what exactly, which cases, which rules, which Mumble features |
| 2 | Interfaces: what people and outside systems see and send (chat output, commands, HTTP API, config file, logs), with examples |
| 3 | Network & runtime: connection, reconnect behaviour, rate limits, timeouts, audio only if the bot uses voice, in plain language |
| 4 | Technology & operations: libraries, target OS, packaging (binaries, Docker), running as a service, security, privacy (DSGVO), testing |
| 5 | Data/state model review (Mermaid ER or state diagram) & remaining open points |

Skip or merge rounds whose topics are already settled. Add rounds when answers open new topics.

**Before writing each new round,** read the user's answers to the previous round carefully:
- Take changed checkboxes and every `✍️ Note` seriously.
- If a note asks for an explanation ("explain this to me"), open the next round with a short section `Follow-ups & explanations for round N`. Give the explanation and ask the question again as a new question.
- If the answers contradict each other or contradict earlier specs, ask about it in a follow-up question.
- If an answer makes a planned question obsolete, drop that question.

**Good questions:**
- Cover all aspects of the topic across the rounds: functionality, interfaces (chat, API, config), networking, protocol compatibility, data/state, technologies, security and privacy, platforms and distribution, acceptance criteria.
- Ask each question once and make it concrete, with 2–4 options, including "later / not in MVP" where it fits.
- Keep technical questions understandable for a non-technical person. Explain what the choice means for them (effort, cost, risk, user experience, how it sounds or feels), not just the jargon.
- **Visualize when it helps:** Mermaid flowcharts for flows, Mermaid `sequenceDiagram` for connection/protocol flows, `stateDiagram-v2` for connection states, `erDiagram` for data models, example requests/config/log lines, tables for comparisons.

End each turn with a short chat message: which round was added, the main topics, and "let me know when the round is answered".

## Step 5 – Finish: write the specification

When the last planned round is answered and no follow-ups remain:
1. Append a closing note to the brainstorming file (`## Closing` with the date, noting that the specification was created from it).
2. Write `specs/{nr}-{name}-specification.md` following [spec-template.md](spec-template.md).
   - Use the final answers: the checked boxes plus every `✍️ Note`.
   - Where a later round overrides an earlier answer, the later one wins.
   - Reference question IDs (e.g. `Q2.3`) in the decision log.
   - Every acceptance criterion must be verifiable in the running bot, against the local test server (`testenv/`) where needed.
   - Stay at specification level. No code and no implementation tasks beyond what the template asks for.
3. If the outcome changes the roadmap's big picture, don't edit the roadmap silently. Mention it in chat and list it under "Impact on the roadmap" in the spec.

## Step 6 – Git commit

Right after the specification is written:
1. `git rev-parse --is-inside-work-tree`. If this is not a git repository, run `git init -b main`.
2. Stage **only** the brainstorming and specification files of this topic (plus the roadmap if the user explicitly changed it). Never `git add -A`.
3. Commit with a message like `docs(spec): Epic 2 – channel tree & chat specification`. Add a body line naming both files. Keep commits small and clean (e.g. brainstorming and specification may be separate commits). **Never add AI attribution** – no `Co-Authored-By` trailers, no "Generated with" lines, no mention of Claude or AI.
4. Don't push unless the user asks. If the commit fails, for example because no git user is configured, report the exact error and don't work around it.
5. Tell the user the spec is done, where it lives, and the commit hash.
