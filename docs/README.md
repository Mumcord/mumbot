# Mumbot documentation

Mumbot runs headless bots that log in to any standard Mumble server as ordinary clients. This
folder is the reference; the [README](../README.md) has the quick start.

## Guides and reference

| Page | What it covers |
| --- | --- |
| [Modules and builds](modules.md) | Run one module or several on one connection (`mumbot run`); the main and the tester build. |
| [Webhook bot](webhook.md) | What the webhook bot does, how to run it, the bot's identity. |
| [Peer bot](peer.md) | Relay discovery for Mumcord clients (`mumbot peer`): announcement, tokens, the relay API. |
| [Music bot](music.md) | Play MP3s into a channel (`mumbot music`): endpoints, sources, URL allowlist, limits. |
| [Plugins bot](plugins.md) | Mumcord plugin suggestions for everyone who joins (`mumbot plugins`): what is announced, how often, what the bot learns. |
| [History bot](history.md) | Channel history for Mumcord clients that were offline (`mumbot history`): what is stored, for how long, who gets it. |
| [Configuration](configuration.md) | Every `bot.toml` key with its default and environment variable. |
| [CLI](cli.md) | Every command and flag of `mumbot`, plus the load test. |
| [HTTP API](http-api.md) | `POST /message`, `GET /channels`, `GET /health`, keys, errors, curl examples. |
| [Security](security.md) | API keys, the client certificate, TLS and the server certificate check. |

## Examples

Ready-to-run Docker Compose setups in [`examples/`](../examples/). Each has a commented config,
an `.env.example`, a health check, a restart policy, the non-root user and a volume for the bot's
certificate.

| Example | What it shows | Status |
| --- | --- | --- |
| [`webhook/`](../examples/webhook/) | The webhook bot against a Mumble server you already run. | Available |
| [`webhook-with-server/`](../examples/webhook-with-server/) | The webhook bot plus a standard Mumble server, to try it on one machine. | Available |
| [`ci-test-bot/`](../examples/ci-test-bot/) | The scripted test bot (tester image) in a CI pipeline against a throwaway server. | Available |
| [`peer-with-relay/`](../examples/peer-with-relay/) | The peer bot with a mumpeer relay and a standard Mumble server. | Available (needs the released mumpeer relay image) |
| [`music/`](../examples/music/) | The music bot against a Mumble server you already run, with a read-only media folder. | Available |
| [`history/`](../examples/history/) | The history bot against a Mumble server you already run, with its database on a volume. | Available |

The examples use the pinned image `ghcr.io/mumcord/mumbot:0.2.1`; `ci-test-bot` uses the tester
image `ghcr.io/mumcord/mumbot-tester` ([modules.md](modules.md#the-two-builds)). When you upgrade,
change the tag in `docker-compose.yml` and read the [changelog](../CHANGELOG.md) first.

### How the examples are tested

CI checks the examples on every pull request that touches them, the image or the code
(`.github/workflows/ci.yml`, job "Examples"):

- `docker compose config` validates every example's `docker-compose.yml` with its `.env.example`.
- `webhook-with-server` is started from the image built from the pull request (tagged locally as
  `ghcr.io/mumcord/mumbot:0.2.1`), must report itself healthy and connected on `/health`, and
  must answer `{"delivered":true}` to a `POST /message`.

Files in `examples/<name>/` only trigger that job, and `examples/*.md` triggers nothing.
`examples/*.rs` and `examples/bot.example.toml` are Rust build and test inputs and trigger the
full CI (`scripts/ci/code-changed.sh`).

## Development

Specs live in [`specs/`](../specs/), with the [roadmap](../specs/roadmap.md) for what comes next.
The local test server is in [`testenv/`](../testenv/README.md).
