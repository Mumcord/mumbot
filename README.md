<div align="center">

<img src="docs/assets/mumbot-banner.svg" alt="mumbot: headless bots for any Mumble server" width="560">

**Bots that join your Mumble server like a person would: post alerts, drive tests, fill channels.**

[![CI](https://img.shields.io/github/actions/workflow/status/Mumcord/mumbot/ci.yml?branch=main&label=CI&logo=github)](https://github.com/Mumcord/mumbot/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Mumcord/mumbot?include_prereleases&sort=semver&logo=github)](https://github.com/Mumcord/mumbot/releases)
[![Licence](https://img.shields.io/github/license/Mumcord/mumbot)](LICENSE)
[![Docker image](https://img.shields.io/badge/docker-ghcr.io%2Fmumcord%2Fmumbot-2496ED?logo=docker&logoColor=white)](https://github.com/Mumcord/mumbot/pkgs/container/mumbot)
<br>
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-b7410e?logo=rust)](Cargo.toml)
[![mumcord-protocol](https://img.shields.io/crates/v/mumcord-protocol?label=mumcord-protocol&logo=rust)](https://crates.io/crates/mumcord-protocol)
[![mumcord-codec](https://img.shields.io/crates/v/mumcord-codec?label=mumcord-codec&logo=rust)](https://crates.io/crates/mumcord-codec)

[Quick start](#quick-start) · [Install](#installation) · [Docs](docs/README.md) · [Examples](examples/README.md)

</div>

## What is mumbot?

One small binary (or Docker image) with headless bots for [Mumble](https://www.mumble.info/)
voice chat, built from modules: run one, or several together as one bot. Every bot logs in with its own certificate, exactly like a person with a Mumble
client. So it works with **any unmodified, standard Mumble server**: no server plugin, no server
config change, no protocol extension.

| Module | What it does | Status |
| --- | --- | --- |
| **Webhook** | Monitoring, CI or any script sends one HTTP request with an API key; the bot posts it into a channel. | Available |
| **Peer** | Connects the server to a [mumpeer](https://github.com/Mumcord/mumpeer) relay: announces the relay to Mumcord clients and hands out relay tokens. | Available |
| **Test bot** | A scripted second participant for automated tests: chat, user state, admin steps, `expect` checks, voice play/record/level checks. Exit code 0 = passed. | Available in the tester build |
| **Load test** | Fills a local server with up to 1000 users, with churn, chat and talkers. | Available in the tester build (source only) |
| Music | Plays an MP3 (or a URL) sent over HTTP as voice into a channel. | Coming: [#3](https://github.com/Mumcord/mumbot/issues/3) |
| **History** | Stays online, records the chat of the channels you choose and hands it to Mumcord clients that were offline (history sync). | Available |

Run one module with `mumbot webhook`, `mumbot peer` or `mumbot history`, or several on one
connection (one bot user) with `mumbot run --modules webhook,peer,history` or a `[modules]` list in the config; see
[docs/modules.md](docs/modules.md).

### Two builds

| Build | Contains | Archive | Docker image |
| --- | --- | --- | --- |
| **mumbot** | The production modules (webhook, peer, history). No test code. | `mumbot-<version>-<platform>` | `ghcr.io/mumcord/mumbot` |
| **mumbot-tester** | The same plus the scripted test bot (checks, voice, samples), for testing Mumble servers and clients in CI. | `mumbot-tester-<version>-<platform>` | `ghcr.io/mumcord/mumbot-tester` |

Both binaries are called `mumbot`. The main build answers a test command with a pointer to the
tester build.

## Quick start

Post a message into your Mumble server in 30 seconds with Docker:

```bash
KEY=$(openssl rand -hex 32)
cat > bot.toml <<EOF
[server]
host = "mumble.example.org"      # your server
channel = "Lobby"

[http]
listen = "0.0.0.0:8080"

[[keys]]
key = "$KEY"
label = "quickstart"
channels = ["Lobby"]
EOF
chmod 644 bot.toml

docker run -d --name mumbot -p 127.0.0.1:8080:8080 \
  -v "$PWD/bot.toml:/config/bot.toml:ro" -v mumbot-data:/data \
  ghcr.io/mumcord/mumbot:0.1.0
```

```bash
curl -s -H "X-API-Key: $KEY" http://127.0.0.1:8080/channels          # find the channel id
curl -s -X POST http://127.0.0.1:8080/message -H "X-API-Key: $KEY" \
  -d '{"channel": 1, "message": "**deploy** finished"}'
# {"delivered":true}
```

No server at hand? [`examples/webhook-with-server`](examples/webhook-with-server/) starts a
standard Mumble server next to the bot with one `docker compose up`.

## Installation

### Binary

Download from the [releases](https://github.com/Mumcord/mumbot/releases) and check it against
`SHA256SUMS` from the same release. For the test bot, take the same archive with the
`mumbot-tester-` prefix:

| Platform | Archive |
| --- | --- |
| Linux x86_64 (static) | `mumbot-<version>-linux-x86_64.tar.gz` |
| Linux arm64 (static) | `mumbot-<version>-linux-aarch64.tar.gz` |
| Windows x86_64 | `mumbot-<version>-windows-x86_64.zip` |
| macOS (Apple Silicon and Intel) | `mumbot-<version>-macos-universal.tar.gz` |

```bash
curl -LO https://github.com/Mumcord/mumbot/releases/download/v0.1.0/mumbot-0.1.0-linux-x86_64.tar.gz
curl -LO https://github.com/Mumcord/mumbot/releases/download/v0.1.0/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing && tar xzf mumbot-0.1.0-linux-x86_64.tar.gz
./mumbot --version
```

Each archive holds `mumbot` (`mumbot.exe` on Windows) at the top level, next to the licence, this
README, the changelog and `bot.example.toml`.

### Docker

```bash
docker pull ghcr.io/mumcord/mumbot:0.1.0
docker pull ghcr.io/mumcord/mumbot-tester:<version>   # with the test bot, from the next release
```

Multi-arch (`linux/amd64`, `linux/arm64`), non-root (uid 10001), health check on `/health`.
Config at `/config/bot.toml`, the bot's certificate in the `/data` volume. Pin a version tag
rather than `latest`. Ready-made setups: [`examples/`](examples/README.md).

### From source

```bash
git clone https://github.com/Mumcord/mumbot && cd mumbot
cargo build --release                     # main build, Rust 1.90+
cargo build --release --features tester   # tester build, also needs CMake (builds Opus)
target/release/mumbot --help
```

## Documentation

| | |
| --- | --- |
| [Modules](docs/modules.md) | Run one module or several on one connection; the two builds. |
| [Webhook bot](docs/webhook.md) | What it does, how to run it, the bot's identity. |
| [Peer bot](docs/peer.md) | Announce a mumpeer relay to Mumcord clients. |
| [History bot](docs/history.md) | Record channel chat for Mumcord clients that were offline. |
| [Configuration](docs/configuration.md) | Every `bot.toml` key, default and environment variable. |
| [CLI](docs/cli.md) | Every command and flag, test-bot scripts, voice checks, load test. |
| [HTTP API](docs/http-api.md) | Endpoints, authentication, errors, curl examples. |
| [Security](docs/security.md) | API keys, the client certificate, TLS, server certificate checking. |
| [Examples](docs/README.md#examples) | Docker Compose setups and how CI tests them. |

## Examples

| Example | Use it to |
| --- | --- |
| [`webhook/`](examples/webhook/) | Run the webhook bot against the Mumble server you already have. |
| [`webhook-with-server/`](examples/webhook-with-server/) | Try everything on one machine: bot plus a standard Mumble server. |
| [`peer-with-relay/`](examples/peer-with-relay/) | Try the peer bot with a mumpeer relay and a standard Mumble server. |
| [`history/`](examples/history/) | Run the history bot against the Mumble server you already have. |
| [`ci-test-bot/`](examples/ci-test-bot/) | Test a Mumble client in CI against a throwaway server (tester image). |

## Test bot at a glance

Needs the tester build (`mumbot-tester` archive or image).

```bash
# Is Alice in "Raid" and unmuted? Exit code 0 = yes.
mumbot --port 44738 --name Observer expect user Alice --in Raid --muted false

# One connection, several steps, one per line.
printf '%s\n' 'join Lobby' 'say "<b>hello</b>"' 'expect message --from Alice --contains hi' \
  | mumbot --port 44738 --name Bob script
```

Only point the test bot at local or throwaway test servers. All subcommands: [docs/cli.md](docs/cli.md).

## Mumbot and Mumcord

Mumbot comes from the makers of [Mumcord](https://github.com/Mumcord/mumcord), a modern Mumble
client. It is built on Mumcord's protocol and codec crates
([`mumcord-protocol`](https://crates.io/crates/mumcord-protocol),
[`mumcord-codec`](https://crates.io/crates/mumcord-codec)), and Mumcord's end-to-end tests use
its test bot. You do not need Mumcord: the bots work with any standard Mumble server and their
messages look the same in the official Mumble client and in Mumcord.

## Contributing

Issues and pull requests are welcome.

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings && cargo test                                    # main build
cargo clippy --all-targets --features tester -- -D warnings && cargo test --features tester  # tester build
cp testenv/.env.example testenv/.env && testenv/up.sh     # local test server (Docker)
MUMBOT_LIVE=1 cargo test --features tester -- --ignored --test-threads=1     # live tests against it
```

- Features start as a spec in [`specs/`](specs/); the [roadmap](specs/roadmap.md) lists what comes
  next.
- Small commits in [Conventional Commits](https://www.conventionalcommits.org/) style, user-visible
  changes in [`CHANGELOG.md`](CHANGELOG.md).
- Never test against public or third-party Mumble servers; use [`testenv/`](testenv/README.md).

## Licence

[AGPL-3.0-only](LICENSE)
