# Changelog

All notable changes to Mumbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

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
  - Voice: `play`, `record`, `expect-audio` and `game-context`, with Opus from `mumcord-audio`.
  - The old environment variables `MUMCORD_BOT_DATA` and `MUMCORD_TESTENV_DIR` still work.
  - Load generator `examples/loadtest.rs` and a separate load-test server (`testenv/scale/`).
- Multi-arch Docker image (`ghcr.io/mumcord/mumbot`) and release binaries for Linux, Windows
  and macOS.
