# Mumbot

Headless bots for [Mumble](https://www.mumble.info/) voice chat servers, from the makers of the
[Mumcord](https://github.com/Mumcord/mumcord) client.

Every Mumbot bot is an ordinary Mumble client: it logs in with its own certificate like a person
would, so it works against any unmodified, standard Mumble server (murmur / mumble-server). No
server plugin, no server config change, no protocol extension.

Two bots ship in one binary, `mumbot`:

- the **webhook bot**: monitoring, CI or any script sends one HTTP request with an API key, and
  the bot posts that text into a Mumble channel;
- the **test bot**: a scripted second participant for automated tests of Mumble clients (chat,
  user state, ACL changes, waiting for conditions, playing and recording voice). Mumcord's E2E
  and smoke tests use it, and a load generator fills a local server with up to 1000 users.

## Quick start

### Binary

Download the archive for your platform from the
[releases](https://github.com/Mumcord/mumbot/releases) (Linux x86_64/aarch64, Windows x86_64,
macOS universal; check it against `SHA256SUMS`), or build it yourself with Rust 1.90+:

```bash
cargo build --release
cp examples/bot.example.toml bot.toml     # then set the server and a long random key
target/release/mumbot webhook --config bot.toml
```

### Docker

```bash
cp examples/bot.example.toml bot.toml     # set the server, a key and listen = "0.0.0.0:8080"
docker run -d --name mumbot \
  -v "$PWD/bot.toml:/config/bot.toml:ro" \
  -v mumbot-data:/data \
  -p 127.0.0.1:8080:8080 \
  ghcr.io/mumcord/mumbot:latest
```

- The image is multi-arch (`linux/amd64`, `linux/arm64`) and runs as a non-root user.
- Config: mounted at `/config/bot.toml` (another path via `MUMBOT_CONFIG`). The server password
  can come from `SERVER_PASSWORD` instead of the file.
- `/data` holds the bot's client certificate (its identity on the server); keep the volume.
- Inside the container the bot must listen on `0.0.0.0:8080` so the port can be published; it
  then warns that HTTP is unencrypted. Publish the port to `127.0.0.1` or put a TLS reverse
  proxy in front.
- The image health check calls `GET /health` on port 8080.

## Config

```toml
[server]
host = "mumble.example.org"
port = 64738
name = "Mumbot"            # the user people see in the channel
channel = "Lobby"          # channel the bot joins at login

[http]
listen = "127.0.0.1:8080"  # loopback by default

[[keys]]
key = "REPLACE-WITH-A-LONG-RANDOM-KEY"
label = "ci"               # name used in the log
channels = ["Lobby", 42]   # names or channel ids this key may write to
```

`bot.toml` holds secrets: keep it private and never commit it. All options, channel passwords and
the full list of status codes: [docs/webhook.md](docs/webhook.md).

## API

All requests except `/health` need the header `X-API-Key: <key>`.

| Request | Answer |
| --- | --- |
| `POST /message` with `{"channel": 42, "message": "build failed"}` (channel id, see `GET /channels`) | `200 {"delivered":true}` once the server accepted the message |
| `GET /channels` | `{"channels":[{"id":42,"name":"Lobby"}]}`: only the channels this key may write to |
| `GET /health` | `{"connected":true,"uptime_seconds":312,"server":"…"}` |

```bash
curl -s -X POST http://127.0.0.1:8080/message \
  -H 'X-API-Key: REPLACE-WITH-A-LONG-RANDOM-KEY' \
  -d '{"channel": 1, "message": "**deploy** finished"}'
```

Messages are plain text with a small Markdown subset (`**bold**`, `*italic*`, `` `code` ``, code
blocks, lists, headings, `http`/`https` links). Error codes: `401` key, `403` channel not
allowed, `404` no such channel, `400`/`413` bad or too large body, `429` rate limit, `503` not
delivered.

## Test bot

Every subcommand except `webhook` drives one scripted bot: it connects, runs the subcommand (or a
whole script), stays connected for `--hold` seconds and exits. **Exit code 0 means the step or
check passed**; an error or a timeout prints the reason on stderr and exits with 1, so a test only
has to run the command and look at the exit code. What a subcommand prints on stdout is listed
below.

Only point it at local test servers (`testenv/`), never at public or third-party servers.

### Connection options

Given before the subcommand:

| Option | Default | Meaning |
| --- | --- | --- |
| `--host <host>` | `127.0.0.1` | server address |
| `--port <port>` | `64738` | server port (this repository's test server: `44738`) |
| `--name <name>` | `Bot` | user name |
| `--cert <name>` | the `--name` | persistent certificate name; reuse it to keep the identity |
| `--superuser` | off | log in as `SuperUser` (password see below) |
| `--timeout <s>` | `10` | seconds for the login and for each wait |
| `--hold <s>` | `0` | stay connected this long after the command(s) |

- Certificates are generated once per name and kept in `MUMBOT_DATA`, else `MUMCORD_BOT_DATA`,
  else `$XDG_DATA_HOME/mumbot` (usually `~/.local/share/mumbot`).
- The superuser password is `SUPERUSER_PASSWORD` from the environment, else from the `.env` file
  in `MUMBOT_TESTENV_DIR`, `MUMCORD_TESTENV_DIR` or `./testenv`. `SERVER_PASSWORD` (same lookup)
  is the connect password, if the server has one.
- Channels are given by name (`Raid`) or path (`Root/Raid`).

### Subcommands

| Subcommand | Does | Prints |
| --- | --- | --- |
| `connect` | connect only | `session=<n> name=<name> cert_hash=<hash>` |
| `join <channel>` | move into a channel | |
| `say [--channel <ch>] <html>` | send HTML to a channel (default: the current one) | |
| `say-sample [--channel <ch>] <sample>` | send a canned message for formatting and sanitizer tests: `bold`, `link`, `image`, `rich`, `script`, `external-image` | |
| `pm <user> <html>` | private message | |
| `rename <new-name>` | reconnect under a new name, same certificate | like `connect` |
| `avatar <file>`, `avatar --sample`, `avatar --clear` | set the avatar from a file or a built-in PNG, or remove it | |
| `comment <html>` | set the comment (`""` removes it) | |
| `register` | self-register under the current name | |
| `listen <ch>...`, `unlisten <ch>...` | follow channels, stop following | |
| `mute on/off`, `deaf on/off` | self mute, self deafen | |
| `moderate <user> [--mute on/off] [--deaf on/off]` | server-mute or -deafen someone (superuser) | |
| `move <user> <channel>` | move someone (superuser) | |
| `deny-speak <ch>`, `allow-speak <ch>` | deny "Speak" for everyone there (suppresses them), undo (superuser) | |
| `deny-listen <ch>`, `allow-listen <ch>` | deny "Listen" for everyone there, undo (superuser) | |
| `describe <ch> <html>` | set a channel description, `""` removes it (superuser) | |
| `expect user <name> [checks]` | wait until a user matches all checks | |
| `expect user-gone <name>` | wait until no user with that name is connected | |
| `expect message [checks]` | wait for a received text message (since this run connected) | the message HTML |
| `expect denied [--permission <p>]` | wait until a request of this bot is denied; `<p>` is one of `write`, `traverse`, `enter`, `speak`, `whisper`, `text`, `listen`, `register`, `self-register` | the denial kind |
| `play <wav> [--loop] [--seconds <s>] [--position x,y,z]` | play a WAV file as voice into the current channel | `played <n> packets in <ms> ms` |
| `game-context <context> [--identity <id>]` | announce a Mumble Link game context (`""` = no game) | |
| `record <out.wav> --seconds <s> [--from <user>]` | record received voice into a 48 kHz mono WAV | `recorded <s> s to <path>` |
| `expect-audio [--from <user>] [--min-rms <dBFS>] [--timeout <s>]` | wait until voice reaches the level (default -40 dBFS, 100 ms windows) | `<level dB> <latency ms>` |
| `sleep <s>` | wait (pacing in scripts) | |
| `script` | run subcommands from stdin, one per line, on one connection | `ok <line>` after each line |

Checks of `expect user` (all optional, all must hold; booleans take `true` or `false`): `--in <ch>`,
`--muted`, `--deaf` (self or by an admin), `--listening <ch>`, `--not-listening <ch>`,
`--registered`, `--comment-contains <text>`, `--has-avatar`, `--avatar-file <file>` (avatar equals
the file), `--cert-hash <sha1-hex>`. Checks of `expect message`: `--from <user>`,
`--contains <text>`, `--private true/false`, `--channel <ch>`.

```bash
# Is the client under test in "Raid" and unmuted?
mumbot --port 44738 --name Observer expect user Alice --in Raid --muted false

# One connection, several steps; quoting works like a shell, # starts a comment.
printf '%s\n' 'join Lobby' 'say "<b>hello</b>"' 'expect message --from Alice --contains hi' \
  | mumbot --port 44738 --name Bob script

# Admin step.
mumbot --port 44738 --superuser deny-listen Root/Raid
```

### Voice checks

The test bot sends and receives real Opus voice (`mumcord-codec`, the codec settings of the
Mumcord client), over UDP with the TCP tunnel as fallback.

- `play` sends a WAV file (any rate or channel count, converted to 48 kHz mono) in 20 ms packets
  and ends with a terminator packet. `--position` adds positional-audio coordinates.
- `record` writes the sum of all speakers (or only `--from`) as received, silence included.
- `expect-audio` measures the level in 100 ms windows and passes as soon as one window reaches
  `--min-rms`; the latency counts from the start of the command. Start it before the sender plays.
- `testenv/audio/speech.wav` is a self-made, speech-like 6 s reference clip (about -23 dBFS).

```bash
printf '%s\n' 'join Lobby' 'expect-audio --from Mouth --min-rms -50 --timeout 12' \
  | mumbot --port 44738 --name Ear script &
sleep 3
printf '%s\n' 'join Lobby' 'play testenv/audio/speech.wav' | mumbot --port 44738 --name Mouth script
wait $!   # exit 0: the voice arrived loud enough
```

### Load test

`examples/loadtest.rs` connects many users (default 1000) spread over the channels of the local
load-test server, then adds churn (moves, leaves and joins, chat) and a few talkers on demand.
Churn and talking switch on and off while it runs by creating or removing the files given by
`--churn-file` and `--talk-file`, so one connected crowd serves both the steady and the churn
phase.

```bash
testenv/scale/up.sh                 # separate server on port 44850, ~300 channels, 1500 slots
cargo run --release --example loadtest -- --users 1000
testenv/scale/down.sh
```

Background and measured numbers: `specs/00-test-bot-carried-over.md` (scale notes).

### Use from other projects

Release archives are named `mumbot-<version>-<platform>` and hold the `mumbot` binary
(`mumbot.exe` on Windows) at the top level, next to the licence and docs:

| Platform | Archive |
| --- | --- |
| Linux x86_64 (static) | `mumbot-<version>-linux-x86_64.tar.gz` |
| Linux arm64 (static) | `mumbot-<version>-linux-aarch64.tar.gz` |
| Windows x86_64 | `mumbot-<version>-windows-x86_64.zip` |
| macOS (Apple Silicon and Intel) | `mumbot-<version>-macos-universal.tar.gz` |

Download: `https://github.com/Mumcord/mumbot/releases/download/v<version>/<archive>`, checked
against `SHA256SUMS` from the same release. There is no `mumcord-bot` alias: Mumcord's tests pass
the bot's path explicitly (`e2e/helpers/paths.ts`, `smoke.sh --bot`), so pointing that path at
`mumbot` is enough. The old environment variables `MUMCORD_BOT_DATA` and `MUMCORD_TESTENV_DIR`
keep working.

## Development

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cp testenv/.env.example testenv/.env && testenv/up.sh      # local test server (Docker)
MUMBOT_LIVE=1 cargo test -- --ignored --test-threads=1      # live tests against it
```

Specs live in `specs/` (`specs/roadmap.md` for what comes next). Never point tests at a public or
third-party Mumble server.

## Licence

[AGPL-3.0-only](LICENSE)
