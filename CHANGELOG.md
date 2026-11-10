# Changelog

All notable changes to Mumbot are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- FreeBSD (issue #64): the build no longer fails because `mumcord-protocol` finds no bundled
  `protoc`. From `mumcord-protocol` 0.8 on it takes the compiler from `PROTOC` or from the system
  (`/usr/local/bin`, `/usr/bin`), so `pkg install protobuf` is all FreeBSD needs before
  `cargo build --release`.

## [0.5.4] - 2026-11-10

### Fixed

- The bot no longer panics (`JoinHandle polled after completion`) when the Mumble server restarts
  and a reconnect attempt cannot log in: after such an attempt it kept the already finished
  forwarding task and waited on it again on the next try, which killed the whole process and left
  the peer module without a relay announcement until someone restarted the bot. Reconnects are now
  retried until the server is back (issue #65).
- The history module compiles against `mumcord-protocol` 0.7.1 and later again: the permissions
  event gained the `flush` flag, which the match there did not list, so a build whose lock
  resolves a newer protocol patch release failed. The bot ignores the flag, as it changes nothing
  the history proof relies on.
## [0.5.3] - 2026-11-08

### Changed

- The peer module names why it refused a camera, screen share, file transfer or token instead of
  answering a bare `denied`: `not-registered` (a guest while only registered users get tokens),
  `group-not-allowed` (registered, but in none of the `allowed_groups`), `no-hello` (the bot has
  not seen this client's hello, so reconnecting helps) and, for broadcasts, `not-in-channel`. The
  reason is logged at debug level without any personal data. A Mumcord client older than 0.8
  reads the unknown reasons as `denied` as before. The relay announcement now says `reg` when
  guests get no token, so a guest's client can explain itself before trying. Needs
  `mumcord-p2p` 0.7.2.

## [0.5.2] - 2026-11-07

### Fixed

- The peer module refuses to start when its bot user is not registered, instead of announcing a
  relay beneath its clients: Mumcord clients ignore relay announcements from anyone else, so those
  setups showed "this server has no relay" (and with it "file transfer is not available") while the
  bot logged a reachable relay. `docs/peer.md` now lists what else to check when a client reports
  no relay (a changed bot or relay certificate, two announcing bots).

## [0.5.1] - 2026-11-06

### Added

- Example [`examples/webhook-and-history/`](examples/webhook-and-history/): two modules in one bot (`command: ["run"]`), so the config's `[modules]` list decides what runs.

### Fixed

- A container that lists several modules in `[modules] enabled` now says why only one of them runs: the image's default command (`webhook`) replaces the list, and mumbot logs a warning naming both. Set the command to `run` to run them together.
- An empty `MUMBOT_MODULES` (a Compose file's `MUMBOT_MODULES: ${MUMBOT_MODULES:-}`) counts as unset instead of stopping `mumbot run` with "a value is required for '--modules'", so the config's `[modules]` list decides.

## [0.5.0] - 2026-11-06

### Security

- History module: the chat history of locked channels (a password or an ACL that restricts entering) is no longer given to people who only listen to the channel. Only people inside a locked channel get it, checked again before messages are sent. With channel passwords in the config, the bot also asks for Mumcord's password proof and answers only while the server confirms that its own password is still valid (Mumcord issue #382).

### Added

- The viewer token of a broadcast (`bcast-tok`) carries the broadcast kind in `k` (`scr`, `plg` or `cam`), so a viewer of a sharer who runs several broadcasts at once knows which one the token is for (Mumcord PR #488). See `docs/peer.md`.
- `server.avatar` sets the bot's avatar from an `https` image URL or a base64 string (Mumcord issue #460). The bot loads it once at startup, checks that it is a real PNG or JPEG, scales it to Mumble's 600 x 60 pixel limit and sets it as its user texture, so every client shows it. If loading fails, it logs a warning and runs without an avatar until the next start. See `docs/configuration.md`.
- The bot's Mumcord hello carries `product` ("Mumbot") and `voice` in its `bot` object, and the bot reports "Mumbot <version>" as its client to the server, so Mumble and Mumcord show "Mumbot 0.x.y" instead of "Mumcord 0.x.y". `voice` is true when the music module runs, so Mumcord can show music bots in their channel and other bots only in its bot list (Mumcord issues #458 and #459). The scripted test bot still sends no `bot` object and looks like a regular Mumcord user.
- The peer module announces which uses the relay allows (images, files, screen sharing) and its broadcast viewer limit to clients.
- Token requests carry the use they are for; the bot passes it to the relay and answers `use-not-allowed` when the relay has switched that use off. A request without a use counts as screen sharing.
- Screen-share broadcasts: the peer module creates a relay broadcast when a Mumcord sharer asks for one (`bcast-req`), admits the viewers the sharer lets in (`bcast-admit`) after checking that both use Mumcord with screen sharing, have a certificate, may use the relay and are in the same channel, and removes viewers on `bcast-kick`. A viewer who leaves or moves to another channel is removed at the relay, and a broadcast ends when its sharer leaves.
- Plugin data relay (Mumcord spec 09): the peer module passes the relay's `plugin_data` use and its per-sender kbps limit to clients, gives out pair tokens for plugin data, and creates plugin data broadcasts (`bcast-req` with `k = plg`) for Mumcord clients that run plugins. At most 8 people per channel send plugin data broadcasts at the same time; admission, rate limit and clean-up work as for screen sharing, and the bot never learns which plugin the data belongs to. Needs a mumpeer relay with `plugin_data` switched on.
- Camera broadcasts (Mumcord spec 08): the peer module passes the relay's `camera` use to clients and creates camera broadcasts (`bcast-req` with `k = cam`) for Mumcord clients whose hello lists `camera`. A person can share the screen and the camera at the same time. At most `max_cameras_per_channel` cameras (default 9) run in one channel; one more is answered with `room-full`. A camera ends when its sender changes channel, and camera admits have their own rate limit (24 at once, 60 per minute). Needs a mumpeer relay with `camera` switched on.
- Peer bot: `allowed_groups` restricts relay tokens to the members of chosen Mumble groups ([#31](https://github.com/Mumcord/mumbot/issues/31)), with `group_channel` naming the channel whose ACL defines them; both users of a pair must be members. The bot reads the members from the server's answer to an ACL query, which a standard server gives only to people with the Write permission on that channel: without it, or with a group the channel does not define, the bot refuses to start instead of allowing everyone. Because the server never reports a change, the members are read again per token request (at most one query per 10 seconds) and every 60 seconds; someone who loses access is dropped at the relay, with its open pair and its broadcast viewings. See `docs/peer.md`.

### Changed

- Updated the Mumcord crates (`mumcord-protocol`, `mumcord-codec`, `mumcord-p2p`) to 0.7, all from crates.io. Protocol 0.6 already added the ACL query (`Event::Acl` and `Handle::query_acl`) that group access needs.
- `resolve_channel` takes `Root` alone as the root channel, whatever the server calls it.
- Updated the MP3 decoder library (symphonia) to 0.6.

## [0.4.1] - 2026-11-01

### Changed

- Updated the Mumcord crates to 0.5.
- The peer module uses the shared `mumcord.peer` types from `mumcord-p2p` instead of its own copies.

## [0.4.0] - 2026-11-01

### Added

- Every module: the bot answers a Mumcord client's hello with its own `mumcord.hello` that says
  it is a bot, which one and which modules it runs:
  `{"app":"mumcord","v":1,"features":[],"bot":{"name":"mumbot","version":…,"modules":[…]}}`, to
  that client only, at most once every 10 seconds per client. Until now the bot never sent a
  hello. See `docs/modules.md`.

## [0.3.0] - 2026-11-01

### Added

- Plugins bot (`mumbot plugins`, or `plugins` in `mumbot run`): announces up to 8 Mumcord plugins
  the server community uses (`[[plugins.announce]]`) to Mumcord clients whose hello lists
  `server-plugins`, at most once a minute per client, as plugin data `mumcord.srvplugins`. Needs a
  registered bot user and `server.server_cert_sha256`; the list is checked at start and a changed
  list is announced with a higher `seq` after a restart. See `docs/plugins.md` (#20).

## [0.2.1] - 2026-11-01

### Security

Hardening of the `peer` bot. Upgrading is recommended for everyone who runs it; guests are now
refused by default, so allow them again only knowingly (`registered_only = false` can let anyone
use the relay as a bandwidth tunnel).

- Peer bot: relay tokens only for registered users, as requester and as peer (`[peer]
  registered_only`, default `true`; `false` lets guests in again and logs a warning) (#29).
- Peer bot: the relay drops a certificate only when its last session leaves, so a second device
  keeps its relay session (#29).
- Peer bot: token requests are also limited per certificate (`cert_requests_per_minute`, default
  10) and open pairs per certificate are capped (`max_pairs_per_cert`, default 4), so reconnecting
  does not reset the budget (#29).
- Peer bot: plain `http` to a relay API on another host now stops the bot unless
  `allow_insecure_http = true`; an API secret with control characters or a secret file that
  others can read is refused (#29).

## [0.2.0] - 2026-10-31

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
- Docker Compose example `examples/peer-with-relay/` (Mumble server, mumpeer relay, peer bot),
  using the released `ghcr.io/mumcord/mumpeer:0.1.0` image.
- Modules (#5): `mumbot run --config bot.toml` runs several modules (`webhook`, `peer`) on one
  Mumble connection and one bot user. Choose them with `--modules webhook,peer`, the environment
  variable `MUMBOT_MODULES` or the new optional `[modules] enabled = [...]` list in the config;
  the command line wins. `mumbot webhook` and `mumbot peer` still run one module. See
  `docs/modules.md`.
- Music module (#3): `mumbot music --config bot.toml` (or `--modules music,…`) plays MP3s as
  voice into a channel. Its own HTTP API on `127.0.0.1:8081` with `[[music.keys]]`:
  `POST /play` (a file from `media_dir`, an `audio/mpeg` upload, or a URL from `allow_hosts` when
  `[music.urls]` is enabled), `GET /queue`, `GET /now-playing`, `POST /skip`, `POST /stop`,
  `POST /volume`, `GET /health`. A queue with interrupt, the bot moves to the track's channel and
  back, its comment shows the current track. Size, length, queue, timeout and rate limits; no
  requests to private addresses. Uses `[music] bitrate`/`frame_ms` above `[voice]`. See
  `docs/music.md` and `examples/music/`.
- Tester build (#16): every release ships `mumbot-tester-<version>-<platform>` archives and the
  image `ghcr.io/mumcord/mumbot-tester` next to the main ones. Built with the Cargo feature
  `tester`.
- History bot `mumbot history --config bot.toml` (#2): records the chat of the channels listed in
  the new `[history]` section in a local SQLite file and answers Mumcord's history sync over
  plugin data (`mumcord.history`), so Mumcord users who were offline can load what they missed.
  Never records private messages or voice, keeps messages at most 7 days (`retention_days`,
  `max_messages_per_channel`) and says so in its comment. Runs alone or with other modules
  (`mumbot run --modules history,…`). See `docs/history.md` and the Docker Compose example
  `examples/history/`.
- `server.server_cert_sha256` is also accepted with the history module.

### Changed

- The main build contains the Opus codec again (the music module sends voice), so building it
  from source needs CMake like the tester build.

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

## [0.1.0] - 2026-10-27

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
