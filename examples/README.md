# Examples

| Path | What it is |
| --- | --- |
| [`webhook/`](webhook/) | Docker Compose: the webhook bot against a Mumble server you already run. |
| [`webhook-with-server/`](webhook-with-server/) | Docker Compose: the webhook bot plus a standard Mumble server, to try it on one machine. |
| [`peer-with-relay/`](peer-with-relay/) | Docker Compose: the peer bot plus a mumpeer relay and a standard Mumble server. |
| [`history/`](history/) | Docker Compose: the history bot against a Mumble server you already run. |
| [`ci-test-bot/`](ci-test-bot/) | Docker Compose: the scripted test bot (tester image) in a CI pipeline against a throwaway server. |
| [`bot.example.toml`](bot.example.toml) | Config for the webhook bot as a plain binary. |
| [`loadtest.rs`](loadtest.rs) | Load generator for the local load-test server (`cargo run --release --features tester --example loadtest`). |

Coming with its module: `music/` ([#3](https://github.com/Mumcord/mumbot/issues/3)). To run several modules in one
bot, see [docs/modules.md](../docs/modules.md).

Overview, configuration, CLI and API reference: [docs/](../docs/README.md).
