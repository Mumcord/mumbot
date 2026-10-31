# Changelog

All notable changes to Mumbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Peer bot `mumbot peer --config bot.toml` (Mumcord#62): announces a mumpeer relay to Mumcord
  clients over plugin data (`mumcord.peer`), fetches pair tokens from the relay's API on request
  and tells the relay when someone leaves. Needs the new `[peer]` section and a pinned server
  certificate. See `docs/peer.md` (#8).
- `server.server_cert_sha256` for the peer bot (required): pins the Mumble server certificate;
  any other certificate is refused. The webhook bot rejects the key for now.
- Docker Compose example `examples/peer-with-relay/` (Mumble server, mumpeer relay, peer bot),
  using the released `ghcr.io/mumcord/mumpeer:0.1.0` image.

## [0.1.1] - 2026-10-27

### Fixed

- Test bot: `--hold` keeps the bot connected again for the whole time after its command (it
  disconnected before the hold since the move from the Mumcord repository), and every run waits
  until the server has received all sent messages before it disconnects, so a `say` or `pm`
  right before the exit (also with `--hold 0`, or a burst beyond the client-side rate limit) is
  no longer lost (#14).

### Added

- Docker Compose examples in `examples/`: `webhook/`, `webhook-with-server/` and `ci-test-bot/`,
  each with a commented config, `.env.example`, health check and a volume for the bot's
  certificate; CI validates them and smoke-tests `webhook-with-server`.
- Reference documentation in `docs/`: configuration, CLI, HTTP API and security notes, plus an
  overview page; a new README.

## [0.1.0] – Unreleased

### Changed

- `mumcord-protocol` comes from crates.io (`0.4`) instead of a git dependency.

### Added

- Webhook bot (`mumbot webhook --config bot.toml`): an HTTP API that posts messages from outside
  systems into Mumble channels, carried over from the Mumcord client repository (Epic 1).
  - `POST /message`, `GET /channels` and `GET /health`, API keys in the `X-API-Key` header,
    per-key channel lists (names or ids), rate limit and synchronous delivery answer.
  - Plain text with a small Markdown subset, escaped before it reaches the chat.
  - Channel passwords (Mumble access tokens) presented at login.
  - Reconnects after a dropped connection; persistent client certificate as the bot's identity.
- Test bot: every scripted subcommand of Mumcord's `mumcord-bot` (connect, chat, user state,
  moderation and ACL helpers, `expect` checks, `script` mode), carried over from the Mumcord
  client repository with the same flags, output and exit codes, so Mumcord's E2E and smoke tests
  can download the `mumbot` release binary.
  - Voice: `play`, `record`, `expect-audio` and `game-context`, with Opus from `mumcord-codec`.
  - The old environment variables `MUMCORD_BOT_DATA` and `MUMCORD_TESTENV_DIR` still work.
  - Load generator `examples/loadtest.rs` and a separate load-test server (`testenv/scale/`).
- Multi-arch Docker image (`ghcr.io/mumcord/mumbot`) and release binaries for Linux, Windows
  and macOS.
