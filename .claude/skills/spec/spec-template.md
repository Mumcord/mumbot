# Specification template

Write the specification in English. Fill every section. Where a section doesn't apply, write "Not applicable, because …" in one line. Be concrete and decisive: a specification states what will be built, not options.

```markdown
# Epic {N} – {Title} · Specification

> Status: Final (brainstorming completed) · As of: {Date}
> Sources: `specs/roadmap.md` (Epic {N}), `specs/{nr}-{name}-brainstorming.md`
> Builds on: {earlier specs, e.g. `specs/01-…-specification.md`}

## 1. Goal & context
Why does this epic/feature exist, which problem does it solve, what becomes possible afterwards (2–5 sentences).

## 2. Scope
### 2.1 In scope (MVP)
### 2.2 Out of scope / later

## 3. Roles & permissions
Who may do what? Refer to Mumble ACLs/groups (e.g. guest, registered user, channel admin, server admin). As a table: role × action. What the bot checks itself (e.g. API keys) vs. what the server enforces.

## 4. Functional requirements
Grouped by area. Each requirement gets an ID (`FR-{N}.{n}`), is stated concisely and names its rules and edge cases.
Where helpful, as a user story: "As a {role} I want {…} so that {…}."

## 5. Interfaces
- What people in Mumble see from the bot (user name, chat messages, commands it answers)
- What outside systems send and get back (HTTP endpoints, request/response examples, status codes)
- Config file keys with an example, CLI and environment variables
- Log lines and error messages (e.g. server unreachable, certificate rejected, bad key)
- Design notes (light/dark theme, scaling, accessibility)

## 6. Audio & network
- Audio input/output, push-to-talk / voice activation, processing (noise suppression, echo cancellation, volume)
- Codec and quality levels, latency target
- Connection flow (Mermaid `sequenceDiagram`), reconnect, behaviour on packet loss, UDP/TCP fallback
- Protocol compatibility: minimum server version, protocol messages used (functional level only)

## 7. Data & state model
- Locally stored data (favorites, settings, certificates, chat history…): Mermaid `erDiagram` or table
- Per entity a table: field · type · required · rules/notes
- States and transitions (Mermaid `stateDiagram-v2`, e.g. connection, talk state)

## 8. Technology & architecture
Decisions with a one-sentence plain-language rationale: language/frameworks and libraries (network, HTTP, config, audio if the bot talks, crypto), module structure, threads/concurrency, platform-specific code, configuration and secrets.

## 9. Non-functional requirements
Latency, CPU/memory usage, uptime and reconnect behaviour, security (TLS, certificate verification, API keys), privacy/GDPR, target platforms/OS versions, logging/diagnostics.

## 10. Build, packaging & release
Supported platforms, release binaries and the Docker image, CI steps, one-time setup steps, test server setup. Not applicable if nothing new is added.

## 11. Acceptance criteria (verifiable in the running bot)
Numbered checklist (`AC-{N}.{n}`), each as a concrete flow: starting point (incl. test server / an observer client if needed) → action → expected result.

## 12. Decision log
| Question | Decision | Note |
|---|---|---|
| Q1.1 | … | … |

## 13. Open points & risks
What still needs clarification (with owner, if known), known risks.

## 14. Impact on the roadmap
Changes to the big picture or to later epics that follow from this brainstorming (or "None").
```
