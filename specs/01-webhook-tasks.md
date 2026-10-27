# Epic 1 – Bot: webhook messages into a channel · Task list

> Carried over from the Mumcord client repository (`specs/03-bot-webhook-tasks.md`, PR #51), renumbered as Mumbot Epic 1 (FR-3.x → FR-1.x, AC-3.x → AC-1.x). References to Mumcord specs and requirements are marked "mumcord"; content is otherwise unchanged.

> Mode: single session (`/dev 3 --no-team`). Owner column names who ran the task.
> Spec: `specs/01-webhook-specification.md` · Report: `specs/01-webhook-implementation.md`

| ID | Title | Status | Owner | Depends on | Complexity | Files / covered AC |
|---|---|---|---|---|---|---|
| A1 | Analysis & breakdown | done | lead | – | complex | spec + `crates/bot`, contracts below |
| B1 | TOML config + API key model | done | lead | A1 | complex | `src/webhook/config.rs`, `Cargo.toml` (new `toml` dep), tests |
| B2 | Markdown subset → safe HTML | done | lead | A1 | complex | `src/webhook/markdown.rs`, tests (AC-1.5) |
| B3 | HTTP server: routes, key auth, rate limit, status codes | done | lead | B1, B2 | complex | `src/webhook/http.rs`, tests (AC-1.3, AC-1.4, AC-1.7) |
| B4 | Bot bridge: live channel tree, delivery, reconnect, `webhook` subcommand | done | lead | B1, B3 | complex | `src/webhook/state.rs`, `src/lib.rs`, `src/main.rs`, tests (AC-1.1) |
| B5 | Docs, example config, README quick-start | done | lead | B1–B4 | simple | `docs/webhook.md`, `examples/bot.example.toml`, `README.md` (AC-1.6 warning wording) |
| C1 | Verification against the local test server | done | lead | B1–B4 | complex | `tests/webhook_live.rs` (AC-1.1–1.8) |
| B6 | Close two spec gaps found in review: server-reported message cap (FR-1.6) and key label + channel id in the request log (FR-1.20) | done | lead | B3 | simple | `src/webhook/http.rs`, `src/webhook/state.rs`, `docs/webhook.md`, tests |
| B7 | Channel passwords (`[[token]]` list, presented at login) per Addendum FR-1.22 | done | lead | B4 | medium | `src/webhook/config.rs`, `src/bot.rs`, `docs/webhook.md`, `examples/bot.example.toml`, tests (AC-1.9) |
| D1 | Implementation report | done | lead | C1 | simple | `specs/01-webhook-implementation.md` |

## Contracts between tasks (decisions that must not drift)

- **Modules:** `src/webhook/{config.rs, markdown.rs, http.rs, state.rs}`, exposed as `pub mod webhook` from `src/lib.rs`.
- **Config (TOML, git-ignored):** `[server]` host/port/name/cert/channel, `[http]` listen (`127.0.0.1:8080` default), `[[key]]` `key`, `label`, `channels` (name or numeric ID, mixed allowed).
- **Hard rules:** message cap 5000 chars, body cap 20 KB → `413`, rate limit 20 requests/min per key → `429`, delivery wait 3 s, key compared in constant time, key checked before the body is parsed.
- **Channel passwords:** a separate `[[token]]` list (`channel` + `password`), presented at login; the login code takes the passwords from the config, nothing else. Passwords are never logged.
- **Status codes:** `200 {"delivered":true}` · `401` unknown key · `403` channel not allowed for key · `404` unknown channel · `400` bad/empty/too long body · `413` body too large · `405` wrong method · `429` rate limited · `503` not connected. No channel list in error bodies.
- **Delivery answer:** murmur does not echo a client's own chat message, so "delivered" means the server accepted it: sent on the control channel and no `PermissionDenied` for chat within the 3 s window. This wording goes into the docs and the report (spec §13).
- **Logging:** timestamp, key label, channel id, status. Never the key, never the message text.

## Verification status

Build, unit tests, wire test and clippy pass. The live test `tests/webhook_live.rs` ran against the local standard server (`testenv/up.sh v15`) and passed: AC-1.1–1.4, AC-1.6, AC-1.7, AC-1.8 ✅, AC-1.5 ✅ on the bot side with a manual look at the client rendering still open.
