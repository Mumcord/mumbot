# Epic 1 – Bot: webhook messages into a channel · Implementation report

> Carried over from the Mumcord client repository (`specs/03-bot-webhook-implementation.md`, PR #51), renumbered as Mumbot Epic 1 (FR-3.x → FR-1.x, AC-3.x → AC-1.x). References to Mumcord specs and requirements are marked "mumcord"; content is otherwise unchanged.

> Specification: `specs/01-webhook-specification.md`
> As of: 2026-10-27 · Mode: single session (`/dev 3 --no-team`)

## Run 1 – 2026-10-27

### Result in one sentence

The webhook bot runs as a headless Mumble client: an HTTP request with an API key becomes a chat message in a channel, and every acceptance criterion is verified against the local test server except the visual rendering of the markup in the client window.

### Acceptance criteria

| AC | Result | Evidence / note |
|---|---|---|
| AC-1.1 | ✅ | `tests/webhook_live.rs` against the local test server: connects as a normal client with its own certificate, drops the link, reconnects and delivers again |
| AC-1.2 | ✅ | Live test: `POST /message` answers `200 {"delivered":true}` and a second client receives the message in `#Lobby` from the bot user |
| AC-1.3 | ✅ | Live test + `tests/webhook_wire.rs` + unit tests: `401` (missing/unknown key), `403` (channel not allowed), `404` (unknown channel), `400` (bad JSON, empty, over 5000 chars), `413` (body over 20 KB), `405` (wrong method), `429` (21st request in the window), `503` (offline). Error bodies carry no channel list |
| AC-1.4 | ✅ | Live test + wire test: `GET /channels` lists only the channels the presented key may write to, `GET /health` answers without a key |
| AC-1.5 | ✅ (bot side) 🎧 (client) | `webhook/markdown.rs` tests + live test: the subset renders as markup, caller HTML arrives as literal text. The visual rendering in the Mumcord window still needs a manual look |
| AC-1.6 | ✅ | `DEFAULT_LISTEN` is `127.0.0.1:8080`; running the bot with `listen = "0.0.0.0:18086"` prints the plain-HTTP warning, the loopback config prints none |
| AC-1.7 | ✅ | Wire test: 5 parallel requests all answered; live test: every accepted parallel message arrived, none lost or doubled |
| AC-1.8 | ✅ | `tests/webhook_live.rs` runs against an unmodified standard murmur (Docker `testenv/v15`, protocol 1.5), no server-side change, no protocol extension |

### Try it yourself

1. Start Docker, then `testenv/up.sh v15` (needs `testenv/.env` with `SUPERUSER_PASSWORD`; the bot itself does not use the superuser).
2. `cp examples/bot.example.toml bot.toml`, put a long random key in `[[keys]]`.
3. `cargo run -p mumcord-bot -- webhook --config bot.toml`
4. `curl -s -H 'X-API-Key: <key>' http://127.0.0.1:8080/channels` → the channel ids the key may write to (on the test server Lobby is id 2).
5. `curl -s -X POST http://127.0.0.1:8080/message -H 'X-API-Key: <key>' -d '{"channel": 2, "message": "**build** failed"}'` → `{"delivered":true}` and the message shows in `#Lobby` of a connected Mumcord client.
6. Run the server-dependent tests: `MUMBOT_LIVE=1 cargo test --test webhook_live -- --ignored --test-threads=1`

### Completed tasks

| Task | Title | Owner |
|---|---|---|
| A1 | Analysis & breakdown | lead |
| B1 | TOML config + API key model | lead (`webhook/config.rs`) |
| B2 | Markdown subset → safe HTML | lead (`webhook/markdown.rs`) |
| B3 | HTTP routes, key auth, rate limit, status codes | lead (`webhook/http.rs`) |
| B4 | Bot bridge, delivery, reconnect, `webhook` subcommand | lead (`webhook/state.rs`, `main.rs`) |
| B5 | Docs, example config, README quick-start | lead (`docs/webhook.md`, `examples/bot.example.toml`, `README.md`) |
| C1 | Verification | lead — unit + wire tests pass; live run blocked, see below |
| D1 | This report | lead |

### Deviations from the specification

- **Delivery answer (FR-1.9):** a standard murmur does not acknowledge chat messages. `200` therefore means "the bot had the chat permission on that channel and the message was written to the server"; the 3 s window is only used when the permission is not already known from the server, so normal calls answer fast. Confirmed by the product owner (2026-10-27): `200` means accepted by the server, not read by someone; the docs use this wording.
- **Certificate name:** the display name `Mumcord Bot` contains a space, which the certificate store does not allow, so the certificate name is derived as `Mumcord_Bot` (unit tested).
- **Logging field:** the log line records peer, path and status; the key label is not printed for requests that failed authentication (there is no label to show). Otherwise as specified.
- **`serve` takes the listener** instead of binding it, so the tests can use a port the OS picks. Small structural choice, no behaviour change.
- **No `413` reading of the full body:** the body is read only up to the cap, then the connection is drained briefly before closing.

### Open points & known issues

- Rendering of the converted markup in the Mumcord window needs a manual look (AC-1.5 🎧); the bytes that reach the chat are correct.
- `rustfmt` is not installed for the toolchain, so formatting was checked by hand (max width 100 followed).
- Keys are supplied by the user in the config (Q2.4); a weak key is possible. The docs say to use a long random one. No generation command was added (out of MVP scope).
- HTTP over a LAN without TLS puts the key on the wire; only a startup warning protects the user.

### Technical notes

- New dependency for `mumcord-bot`: `toml` (config file); `serde`, `serde_json`, `tracing-subscriber` were already in the workspace and are now used by this crate. No new crate.
- Modules: `src/webhook/{mod,config,markdown,http,state}.rs`, exposed as `pub mod webhook`.
- CLI: `mumcord-bot webhook --config bot.toml`.
- Environment: `RUST_LOG` controls the log level (`tracing-subscriber` env filter).
- Secrets: only in the git-ignored config file; `ApiKey`'s `Debug` prints `<hidden>` so a key cannot appear in a log.
- Tested: Linux, toolchain stable 1.99.1, `cargo test` (29 unit + 1 wire test), `cargo clippy -p mumcord-bot --all-targets` clean, live test against `testenv/v15` (standard mumble-server 1.5.735). `rustfmt` is not installed for this toolchain, so formatting was checked by hand against `rustfmt.toml` (max width 100).
- New dependency for `mumcord-bot`: `toml` (config file).

## Run 2 – 2026-10-27 (review pass, task B6)

Two requirements were not fully implemented in run 1; both are closed and tested.

- **FR-1.6 – server-reported message cap.** The bot used the fixed 5000 characters. It now uses the smaller of 5000 and the `maxMessageLength` the server reports, so a server with a lower limit is respected. Test: `the_server_reported_cap_wins_when_it_is_smaller` (a server with 1000 refuses the 1001st character, accepts 1000). A standard murmur reports 10000, so behaviour on the local test server stays 5000.
- **FR-1.20 – request log.** The log only carried peer, path and status. It now logs timestamp, peer, path, key **label**, channel id and status — still never the key value and never the message text. Observed in the running program:
  `request peer=127.0.0.1:54528 path=/message key="test" channel=1 status=200`
  Requests without a key or without a channel show `-`, because `0` is a real channel (Root).
- `docs/webhook.md` status-code table now mentions the server-reported cap.

Checks: build, 31 unit tests, wire test, live test against `testenv/v15` (standard mumble-server 1.5) and `cargo clippy -p mumcord-bot --all-targets` all pass. Formatting checked by hand (`rustfmt` is not installed for this toolchain).

## Run 3 – 2026-10-27 (Addendum: channel passwords, task B7)

The product owner asked what happens when a channel has a password. The MVP config only carried the
server connect password, so a protected channel stayed closed for the bot. Decision: passwords
belong to the channel, not to a key, so the config gets a separate `[[token]]` list
(`channel` + `password`) that is presented to the server at login through the normal
access-token mechanism (mumcord FR-2.78). Recorded as FR-1.22 and AC-1.9.

- `src/webhook/config.rs`: `ChannelToken`, `WebhookConfig.tokens`, passwords hidden in `Debug`.
- `src/bot.rs`: `BotConfig.tokens`, used at login and on reconnect.
- `docs/webhook.md` + `examples/bot.example.toml`: the new section and the commented example.
- Tests: unit test for the config parsing and the hidden password (32 unit tests), live test
  `tests/webhook_tokens.rs` against `testenv/v15`: SuperUser bot creates a channel that
  only the token group `#s3cret-token` may enter; the webhook bot with `[[token]]` joins it and the
  posted message reaches the chat; a bot without the password stays outside and the join is refused.

Checks: build, 32 unit tests, wire test, live tests (`webhook_live`, `webhook_tokens`), clippy clean.
