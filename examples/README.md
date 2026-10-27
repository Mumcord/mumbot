# Examples

| Path | What it is |
| --- | --- |
| [`webhook/`](webhook/) | Docker Compose: the webhook bot against a Mumble server you already run. |
| [`webhook-with-server/`](webhook-with-server/) | Docker Compose: the webhook bot plus a standard Mumble server, to try it on one machine. |
| [`ci-test-bot/`](ci-test-bot/) | Docker Compose: the scripted test bot in a CI pipeline against a throwaway server. |
| [`bot.example.toml`](bot.example.toml) | Config for the webhook bot as a plain binary. |
| [`loadtest.rs`](loadtest.rs) | Load generator for the local load-test server (`cargo run --release --example loadtest`). |

Coming with their modules: `music/` ([#3](https://github.com/Mumcord/mumbot/issues/3)),
`history/` ([#2](https://github.com/Mumcord/mumbot/issues/2)) and `multi-module/`
([#5](https://github.com/Mumcord/mumbot/issues/5)).

Overview, configuration, CLI and API reference: [docs/](../docs/README.md).
