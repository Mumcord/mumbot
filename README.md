# Mumbot

Headless bots for [Mumble](https://www.mumble.info/) voice chat servers, from the makers of the
[Mumcord](https://github.com/Mumcord/mumcord) client.

Every Mumbot bot is an ordinary Mumble client: it logs in with its own certificate like a person
would, so it works against any unmodified, standard Mumble server (murmur / mumble-server). No
server plugin, no server config change, no protocol extension.

The first bot is the **webhook bot**: monitoring, CI or any script sends one HTTP request with an
API key, and the bot posts that text into a Mumble channel.

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
