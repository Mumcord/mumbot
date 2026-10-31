# Changelog

All notable changes to Mumbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Configurable voice bitrate (#7): `[voice] bitrate` and `frame_ms` in the config, `--bitrate` and
  `--frame-ms` on the command line and `MUMBOT_VOICE_BITRATE` / `MUMBOT_VOICE_FRAME_MS` in the
  environment (command line, then environment, then a module's own section, then `[voice]`, then
  the default of 96000 bit/s, Mumcord's default). The server's maximum bandwidth wins: the bot
  lowers the bitrate to fit it with the packet overhead and logs a warning. The tester build's
  `play` uses it; its default changes from 40000 to 96000 bit/s (capped by the server). 10 ms
  packets are supported; 40 and 60 ms are not. See `docs/configuration.md`.
- Peer bot `mumbot peer --config bot.toml` (Mumcord#62): announces a mumpeer relay to Mumcord
  clients over plugin data (`mumcord.peer`), fetches pair tokens from the relay's API on request
  and tells the relay when someone leaves. Needs the new `[peer]` section and a pinned server
  certificate. See `docs/peer.md` (#8).
- `server.server_cert_sha256` for the peer bot (required): pins the Mumble server certificate;
  any other certificate is refused. The webhook bot rejects the key for now.
- Docker Compose example `examples/peer-with-relay/` (Mumble server, mumpeer relay, peer bot);
  the relay image is a placeholder until mumpeer is released.
- Modules (#5): `mumbot run --config bot.toml` runs several modules (`webhook`, `peer`) on one
  Mumble connection and one bot user. Choose them with `--modules webhook,peer`, the environment
  variable `MUMBOT_MODULES` or the new optional `[modules] enabled = [...]` list in the config;
  the command line wins. `mumbot webhook` and `mumbot peer` still run one module. See
  `docs/modules.md`.
- Tester build (#16): every release ships `mumbot-tester-<version>-<platform>` archives and the
  image `ghcr.io/mumcord/mumbot-tester` next to the main ones. Built with the Cargo feature
  `tester`.

### Changed

- The main build (`mumbot-<version>-<platform>`, `ghcr.io/mumcord/mumbot`) no longer contains the
  scripted test bot (#16): test subcommands such as `say`, `expect`, `play` or `script` print a
  pointer to the tester build and exit with code 2. `register` stays in the main build.
  **Migration:** test and CI setups (like Mumcord's E2E and smoke tests) download
  `mumbot-tester-<version>-<platform>` or use `ghcr.io/mumcord/mumbot-tester` instead; the binary
  inside is still called `mumbot` and every command, flag and output is unchanged. Webhook and
  peer configs need no change.
- From source, the load test needs `--features tester`
  (`cargo run --release --features tester --example loadtest`).
- The bot logs `bot connected` with its modules instead of `peer bot connected` or
  `webhook bot listening` (now `webhook module listening`).

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
