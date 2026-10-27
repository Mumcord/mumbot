# Mumbot – headless Mumble bots

Mumbot builds headless bots that speak the Mumble protocol as ordinary clients and stay compatible with standard Mumble servers (murmur / mumble-server). Products: the webhook bot (`mumbot webhook --config bot.toml`) and the scripted test bot (every other subcommand), which Mumcord's E2E and smoke tests download as a release binary, so its CLI (subcommands, flags, output, exit codes) must stay compatible. Sister project: the Mumcord desktop client (`Mumcord/mumcord`), whose `mumcord-protocol` and `mumcord-audio` crates Mumbot uses.
Overview: `specs/roadmap.md`. Each epic gets `specs/{nr}-{name}-specification.md`; specs are created with the `/spec` skill and built with `/dev`.
The user is a product manager and not a technical person → make technology decisions with a clear recommendation, explained simply.
Everything is in English: chat, specs, docs, code, comments, commit messages and log/API text.

## Answer style: caveman mode (always)

- Chat answers extremely short: no filler, no intro, no repetition. Sentence fragments are fine.
- Only the result and next steps. Bullet points instead of prose. For code changes, `file:line` is enough instead of code excerpts.
- Must still be understandable for a non-technical person.
- **Applies to chat only.** Specs, brainstorming files, docs, commit messages and code comments are written fully and cleanly.

## Context budget: only 200k tokens → be frugal

- Search before reading (Grep/Glob). Read large files only in parts (`offset`/`limit`).
- Never read anything twice. Don't re-read your own changes to check them.
- Delegate broad searches across many files to a subagent (Explore) and take only its result.
- Keep agents short: concise brief, smallest suitable model, final report ≤ 10 lines. No long-lived agents, no status pings.
- Limit command output: `--quiet`, `| tail -n 50`, for tests print only the failures.
- Don't copy file contents, logs or diffs into the chat.
- Run independent tool calls in parallel. No unnecessary interim reports.

## Way of working

- Specs are the truth: only build what the specification says. Unclear or contradictory → ask instead of guessing. No scope creep.
- Delete your own build cache in ~/.cache/mumcord-build/<agent> when done (Rust targets are several GB each).
- One task = small, verifiable steps. Every epic must be verifiable in the running bot against the local test server (`testenv/`, acceptance criteria of the spec).
- One fresh agent per subtask with a self-contained brief. Never bundle several tasks into one agent. The agent ends after its final report; follow-up work gets a new agent.
- Commit only on request (exception: `/spec` commits the finished spec). Never `git add -A`. Never push without being asked.
- Commits are small and focused: one logical change per commit, Conventional Commits style (`feat:`, `fix:`, `docs:`, `chore:` …), clear English message. Never add AI attribution: no `Co-Authored-By` trailers, no "Generated with" lines, no mention of Claude or AI.
- User-visible changes get a line in `CHANGELOG.md` under the unreleased version.
- Secrets never go into the repo (no API keys, passwords, bot certificates or private keys in files, docs or examples). Use only local git-ignored files (`bot.toml`, `testenv/.env`) and CI variables.
- Never connect to public or third-party Mumble servers for testing; use the local test server (`testenv/up.sh`).
- **Client only.** Never build, require or modify anything on the server side (no companion services, server plugins or server config changes). Every bot must work against an unmodified standard Mumble server. A bot's own listener (e.g. the webhook HTTP API) runs on the bot's side and is fine.
- Releases: a `v*` tag builds binaries, the Docker image and a draft GitHub release; the user publishes it by hand. Never create tags or releases without being asked.

## Stack

- **Language:** Rust (edition 2024, MSRV 1.90), one crate `mumbot` (library + `mumbot` binary).
- **Mumble protocol and voice:** `mumcord-protocol` and `mumcord-audio` from crates.io (published by `Mumcord/mumcord`); a version bump (by hand or a Dependabot pull request) must pass the live tests.
- **Async runtime:** tokio. TLS: rustls with ring (via `mumcord-protocol`).
- **HTTP:** the webhook API is a small HTTP/1.1 handler on `tokio::net` (no web framework), JSON via serde/serde_json.
- **Config:** TOML (`toml` + serde). CLI: clap. Logs: tracing (`RUST_LOG`).
- **Packaging:** static musl binaries for Linux (cross-built with cargo-zigbuild), native builds for Windows and macOS, a multi-arch Docker image `ghcr.io/mumcord/mumbot` (Alpine runtime, non-root).
- **CI:** GitHub Actions (`ci.yml` checks, `release.yml` on tags), Dependabot for cargo, GitHub Actions and Docker.

Don't introduce anything without a reason.

## Coding rules

**General**
- Simple over clever. No speculative abstractions. No dead code, no commented-out blocks.
- Comments explain the *why*, not the *what*.
- Follow the style of the surrounding code.
- Never swallow errors silently: log them or report them cleanly to the caller.
- Strict typing; no escape hatches (`unwrap` outside tests, unchecked casts, `unsafe`) without a reason.

**Networking & runtime**
- Keep protocol handling, bot logic and outside interfaces (HTTP API, CLI) in separate modules; the HTTP handler talks to the bot through a narrow trait, never to the protocol directly.
- A bot is long-running: reconnect after a dropped link, never block the async runtime (no blocking I/O or long locks in async code), bound every wait with a timeout.
- Stay compatible with standard Mumble servers; no private protocol extensions.
- Anything a bot needs that is missing in `mumcord-protocol` is first discussed for Mumcord, not patched around here.

**Tests**
- New logic gets tests. Critical flows get a live test along the acceptance criteria, against the local test server (`MUMBOT_LIVE=1 cargo test -- --ignored --test-threads=1`).
- A bug is first reproduced with a test, then fixed.
- Before "done": `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` and the live tests pass; Docker changes also `docker build` plus a `/health` check. If not, say so honestly.

**Security & privacy**
- TLS to the Mumble server always on. Protect the bot's client certificate (it is its identity): private file permissions, never logged, never committed.
- API keys and channel passwords are secrets: never logged (`Debug` hides them), compared in constant time.
- Listeners bind to loopback by default; serving a network must be explicit and warns about plain HTTP.
- No chat contents, message texts, keys or personal data in logs.
- Treat everything from the server and from HTTP callers (names, comments, chat HTML, request bodies) as untrusted input: validate, cap sizes and escape before sending it on.
