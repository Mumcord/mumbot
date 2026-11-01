# Modules and builds

Mumbot is one bot built from modules. Each module is a job the bot does while it is online; you
choose which ones run. Several modules in one process share **one Mumble connection**: one bot
user, one client certificate, one reconnect loop.

| Module | What it does | Needs in `bot.toml` |
| --- | --- | --- |
| `webhook` | HTTP API that posts messages into channels ([webhook.md](webhook.md)). | `[http]`, at least one `[[keys]]` |
| `peer` | Announces a mumpeer relay to Mumcord clients and hands out relay tokens ([peer.md](peer.md)). | `[peer]`, `server.server_cert_sha256` |
| `history` | Records the chat of chosen channels and answers Mumcord's history sync ([history.md](history.md)). | `[history]` |
| `music` | HTTP API that plays MP3s as voice into a channel, with a queue ([music.md](music.md)). | `[music]` with a `media_dir` or URLs, at least one `[[music.keys]]` |
| `plugins` | Announces the Mumcord plugins the server community uses to Mumcord clients ([plugins.md](plugins.md)). | `[[plugins.announce]]`, `server.server_cert_sha256`, a registered bot user |

## Choosing modules

One module:

```bash
mumbot webhook --config bot.toml
mumbot peer --config bot.toml
mumbot music --config bot.toml
mumbot history --config bot.toml
mumbot plugins --config bot.toml
```

Several modules, on the command line:

```bash
mumbot run --config bot.toml --modules webhook,peer
MUMBOT_MODULES=webhook,peer mumbot run --config bot.toml    # same, handy in Docker
```

Or in the config file, then just `mumbot run --config bot.toml`:

```toml
[modules]
enabled = ["webhook", "peer"]
```

- The command line wins: `--modules`, `MUMBOT_MODULES`, `mumbot webhook`, `mumbot peer` and
  `mumbot history`, `mumbot music` and `mumbot plugins` replace the config's list.
- The music module moves the shared bot user into the channel of the track it plays and back to
  `server.channel` afterwards; the other modules work the same from any channel.
- Only the sections of the chosen modules are checked, so one `bot.toml` can hold settings for
  modules that do not run.
- `mumbot run` without any choice stops with an error that names both ways to choose.
- When one module fails, the whole bot stops (Docker's restart policy brings it back).
- `server.server_cert_sha256` is required with `peer` and `plugins`, optional with `history`, and then protects
  the shared connection; the webhook module alone rejects it (it does not pin yet).

Existing webhook setups need no change: `[modules]` is optional and the Docker image still runs
`webhook` by default. To run several modules in Docker, set the command to `run` and list the
modules in `[modules]` or in `MUMBOT_MODULES`.

The image's health check asks the webhook module's `/health` on port 8080; when the webhook
module does not run, disable it (as [`examples/history/`](../examples/history/) does) or replace it
(the music module answers `/health` on its own port, see [`examples/music/`](../examples/music/)).

## The two builds

| Build | Contains | Release archive | Docker image |
| --- | --- | --- | --- |
| **mumbot** | The production modules only. | `mumbot-<version>-<platform>` | `ghcr.io/mumcord/mumbot:<version>` |
| **mumbot-tester** | The same plus the scripted test bot: every test subcommand of [cli.md](cli.md), voice checks, samples and the load test. | `mumbot-tester-<version>-<platform>` | `ghcr.io/mumcord/mumbot-tester:<version>` |

Both binaries are called `mumbot`. Use the main build for production bots; it carries no test
code (no voice analysis, no samples, no test commands). Use the tester build to test Mumble servers or clients, for
example in CI ([`examples/ci-test-bot/`](../examples/ci-test-bot/)).

A test command in the main build prints which build it needs and exits with code 2:

```text
$ mumbot say hi
mumbot: `say` is not a command of this build. The test commands (say, expect, play, script, …) are only in the tester build: the mumbot-tester release archive or the ghcr.io/mumcord/mumbot-tester image.
```

`register` is in both builds: the peer bot's setup needs it once.

From source, the tester build is the Cargo feature `tester`:

```bash
cargo build --release                     # mumbot (needs CMake for Opus, which the music module uses)
cargo build --release --features tester   # mumbot-tester
```
