# Changelog

All notable changes to Mumbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.0] – Unreleased

### Added

- Webhook bot (`mumbot webhook --config bot.toml`): an HTTP API that posts messages from outside
  systems into Mumble channels, carried over from the Mumcord client repository (Epic 1).
  - `POST /message`, `GET /channels` and `GET /health`, API keys in the `X-API-Key` header,
    per-key channel lists (names or ids), rate limit and synchronous delivery answer.
  - Plain text with a small Markdown subset, escaped before it reaches the chat.
  - Channel passwords (Mumble access tokens) presented at login.
  - Reconnects after a dropped connection; persistent client certificate as the bot's identity.
- Multi-arch Docker image (`ghcr.io/mumcord/mumbot`) and release binaries for Linux, Windows
  and macOS.
