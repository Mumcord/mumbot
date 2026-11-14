# Contributing to Mumbot

Mumbot builds headless bots that speak the Mumble protocol and stay compatible with standard Mumble servers. Please read this before opening a pull request. Security problems go to the private channel in [SECURITY.md](SECURITY.md), never into a public issue.

## Rules

- Write everything in English: code, comments, docs, commit messages and log text.
- Keep it simple. No speculative abstractions, no dead code, no commented-out blocks. Comments explain the why, not the what. Follow the style of the surrounding code.
- Never swallow errors silently: log them or report them cleanly to the caller. No `unwrap` outside tests, no unchecked casts and no `unsafe` without a reason.
- Stay compatible with standard Mumble servers: no private protocol extensions, and nothing may be required on the server side. Every bot must work fully without optional companion services.
- Keep the command line of the tester build (subcommands, flags, output, exit codes) compatible: Mumcord's tests use it as a release binary.
- Commits are small and focused, one logical change each, in Conventional Commits style (`feat:`, `fix:`, `docs:`, `chore:` ...).

## Security and privacy

- TLS to the Mumble server stays on. The bot's client certificate is its identity: never log it, never commit it.
- API keys and channel passwords are secrets: never logged, compared in constant time.
- No chat contents, message texts, keys or personal data in logs.
- Treat everything from the server and from HTTP callers (names, comments, chat HTML, request bodies) as untrusted input: validate, cap sizes and escape before sending it on.
- Never commit secrets (API keys, passwords, certificates, private keys). Use git-ignored local files (`bot.toml`, `testenv/.env`).

## Tests

- New logic gets tests. A bug is first reproduced with a test, then fixed.
- Never test against public or third-party Mumble servers. Use the local test server (`testenv/up.sh`).
- Before opening a pull request, run these, both without and with `--features tester`:
  - `cargo fmt --all --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
- Critical flows also get a live test: `MUMBOT_LIVE=1 cargo test --features tester -- --ignored --test-threads=1`.
- Docker changes also need a `docker build` and a `/health` check.

## Changelog and docs

User-visible changes get a line in `CHANGELOG.md` under the unreleased version, and the docs (README, `docs/`) are updated.
