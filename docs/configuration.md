# Configuration reference

The bot's modules read one TOML file, `bot.toml`, given with
`mumbot webhook --config <file>`, `mumbot peer --config <file>`, `mumbot history --config <file>`, `mumbot music --config <file>`, `mumbot plugins --config <file>`
or `mumbot run --config <file>`,
or the `MUMBOT_CONFIG` environment variable. Modules that run together share the `[server]`
section and its connection ([modules.md](modules.md)). The test bot needs no file; it takes command-line
options ([cli.md](cli.md)) and a few environment variables (below).

`bot.toml` holds API keys and passwords: keep it private and never commit it (the repository's
`.gitignore` excludes every `bot.toml`). Commented starting points:
[`examples/bot.example.toml`](../examples/bot.example.toml) for a binary,
[`examples/webhook/bot.example.toml`](../examples/webhook/bot.example.toml) and
[`examples/music/bot.example.toml`](../examples/music/bot.example.toml) for Docker.

Rules that apply to the whole file:

- An unknown key or section is an error, so a typo is reported at startup instead of ignored.
- The file is read once at startup. After a change, restart the bot.
- Config values do not expand environment variables. Only the connect password has an
  environment fallback (`SERVER_PASSWORD`, see `server.password`).

## `[modules]`: which modules `mumbot run` starts (optional)

| Key | Type | Default | Env | Meaning |
| --- | --- | --- | --- | --- |
| `enabled` | list of `"webhook"`, `"peer"`, `"history"`, `"music"`, `"plugins"` | `[]` | `MUMBOT_MODULES` (comma-separated, via `--modules`) | Modules for `mumbot run`. `--modules`, `MUMBOT_MODULES`, `mumbot webhook`, `mumbot peer`, `mumbot history`, `mumbot music` and `mumbot plugins` replace it. Only the sections of the chosen modules are checked. |

## `[server]`: the Mumble server and the bot's identity

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `host` | string | `"127.0.0.1"` | | Server address (name or IP). |
| `port` | integer | `64738` | | Server port (TCP and UDP). |
| `name` | string | `"Mumbot"` | | User name people see in the channel. |
| `cert_name` | string | `name`, with every character other than `A-Z a-z 0-9 - _` replaced by `_` | | Name of the persistent client certificate. Keep it stable to keep the bot's identity. |
| `channel` | string | none: the bot stays in the root channel | | Channel to join at login: a name (`Lobby`) or a path (`Root/Ops/Alerts`). |
| `password` | string | none | `SERVER_PASSWORD` | Connect password, if the server has one. Without this key the bot uses `SERVER_PASSWORD`; prefer the environment variable so the file holds fewer secrets. |
| `data_dir` | path | `MUMBOT_DATA`, else `MUMCORD_BOT_DATA`, else `$XDG_DATA_HOME/mumbot`, else `~/.local/share/mumbot` | `MUMBOT_DATA` | Folder for the client certificate (`certs/<cert_name>/`). In Docker: `/data`, keep it on a volume. |
| `server_cert_sha256` | string | none | | Required with the peer and plugins modules: SHA-256 of the server's certificate, 64 hex digits, colons allowed (`openssl x509 -noout -fingerprint -sha256`); a server with another certificate is refused. Optional with the history module. The webhook module alone rejects this key (it does not pin yet); with the peer, history or plugins module it protects the shared connection. |
| `superuser` | boolean | `false` | `SUPERUSER_PASSWORD` | Log in as `SuperUser` with the password from `SUPERUSER_PASSWORD` instead of a certificate. Only for local test servers. |

When `SERVER_PASSWORD` or `SUPERUSER_PASSWORD` is not in the environment, the bot also looks in
the `.env` file of the test environment (`MUMBOT_TESTENV_DIR`, else `MUMCORD_TESTENV_DIR`, else
`./testenv`). That fallback exists for this repository's tests; production setups set the
environment variable.

## `[http]`: the HTTP API listener

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `listen` | `"ip:port"` | `"127.0.0.1:8080"` | | Address the API listens on. Loopback by default; any other address logs a warning at startup because the key then travels over plain HTTP. In Docker use `"0.0.0.0:8080"` and publish the port to `127.0.0.1`. |
| `rate_limit_per_minute` | integer, above 0 | `20` | | Requests per minute per API key; more get `429`. |
| `delivery_timeout_secs` | integer, above 0 | `3` | | Seconds a `POST /message` waits for a possible denial when the bot does not yet know whether it may write to the channel. No denial within this time counts as delivered ([http-api.md](http-api.md#post-message)). |

## `[[keys]]`: API keys (at least one)

One block per outside system. A request is only accepted with one of these keys, and only for
that key's channels. The peer bot ignores this section.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `key` | string, not empty | required | The secret sent in the `X-API-Key` header. Use a long random value, for example `openssl rand -hex 32`. |
| `label` | string, not empty | required | Name used in the log instead of the key. |
| `channels` | list of names and ids, not empty | required | Channels this key may write to, for example `["Lobby", 42]`. Names and numeric ids may be mixed. |

## `[[token]]`: channel passwords (optional)

Passwords belong to a channel, not to a key. The bot presents every listed password to the
server at login (Mumble access tokens), so channels that need one become writable. `[[tokens]]`
is accepted as well.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `channel` | name or id | required | The channel the password is for. |
| `password` | string | required | The channel password. Never logged. |

## `[peer]`: the peer bot

Used only by `mumbot peer`, which needs it together with `server.server_cert_sha256`; the webhook
bot ignores it. See [peer.md](peer.md).

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `api_url` | URL | required | | Base URL of the mumpeer relay's API, for example `"http://127.0.0.1:7480"`. Plain `http` to a host other than loopback stops the bot at start (the secret would travel in clear text); use `https` with `api_fp` there. |
| `allow_insecure_http` | bool | `false` | | Allows plain `http` to another host anyway, for example inside a private container network; logs a warning. |
| `api_secret_file` | path | none | `MUMBOT_PEER_API_SECRET` | File with the API secret on its first line. The environment variable wins when set. One of both is required; the secret must have at least 32 characters and no control characters. The file must be private (`chmod 600`): one that group or others can read stops the bot at start. Keep it out of Git (`*.secret` is git-ignored). Never logged. |
| `api_fp` | string | none | | SHA-256 of the API's TLS certificate (64 hex digits, colons allowed). Required for `https`: the relay's certificate is self-signed, so it is pinned instead of checked against certificate authorities. |
| `registered_only` | bool | `true` | | Only registered users get relay tokens. `false` lets guests use the relay too and logs a warning at start: anyone can then use the relay as a bandwidth tunnel with fresh certificates. See [peer.md](peer.md#who-gets-tokens). |
| `cert_requests_per_minute` | integer | `10` | | Token requests per certificate and minute, across all sessions with that certificate (besides 6 per session). At least 1. |
| `max_pairs_per_cert` | integer | `4` | | Open relay pairs per certificate. At least 1. |

## `[music]`: the music bot

Used only when the music module runs; it then needs at least one `[[music.keys]]` entry and a
`media_dir` or enabled URLs. See [music.md](music.md).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | `host:port` | `"127.0.0.1:8081"` | The music API's own listener. Anything but loopback logs a plain-HTTP warning at start. |
| `channel` | name or id | none | Where a track plays when the request names no channel; without it the bot's current channel. |
| `media_dir` | path | none | Folder that `{"file": …}` plays from; nothing outside it (also not through symlinks). |
| `return_home` | bool | `true` | Move back to `server.channel` when the queue is empty. |
| `volume` | 0–100 | `60` | Start volume in percent. |
| `max_queue` | integer | `10` | Waiting tracks; one more answers 409. |
| `max_track_mb` | integer | `20` | Size cap of a file, upload or download (413). |
| `max_track_secs` | integer | `900` | Length cap (422; a track without a known length is cut). |
| `rate_limit_per_minute` | integer | `30` | Requests per key (429). |
| `bitrate`, `frame_ms` | integer | from `[voice]` | The music's voice settings, above `[voice]` (below `--bitrate` and the environment). |

`[music.urls]`: MP3s from the web.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | bool | `false` | URL sources are refused (403) unless this is `true`. |
| `allow_hosts` | list of strings | `[]` | Host names a URL may point to; `"*.example.com"` allows every subdomain. Required when enabled. |
| `allow_private` | bool | `false` | Allow hosts that resolve to private, loopback or link-local addresses. |
| `timeout_secs` | integer | `20` | Time for the whole download (502 after it). |

`[[music.keys]]`: one entry per caller, the same fields as [`[[keys]]`](#keys-api-keys-at-least-one)
(`key`, `label`, `channels`). The channels are the ones the key may play into. Music keys and
webhook keys are separate lists.
## `[history]`: the history bot

Used only by the history module (`mumbot history`, or `history` in `mumbot run`). What is stored
and who gets it: [history.md](history.md).

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `channels` | list of names, `Root/Paths` or ids | required | | Channels to record; `["*"]` records every channel. Nothing else is recorded, private messages never. Resolved at every login: the bot listens to each recorded channel other than `server.channel`. |
| `exclude` | list of names, `Root/Paths` or ids | `[]` | | Channels never recorded; wins over `channels`. |
| `retention_days` | number | `7` | | Days a message is kept, 1 to 7 (history sync never asks for older messages). |
| `max_messages_per_channel` | number | `5000` | | Newest messages kept per channel, 1 to 100000. |
| `database` | path | `history.sqlite3` in the data directory | | SQLite file with the recorded messages; created readable only by the bot's user. Use one per server. |

## `[plugins]`: the plugins bot

Used only by the plugins module (`mumbot plugins`, or `plugins` in `mumbot run`). Needs
`server.server_cert_sha256` and a registered bot user. How clients get the list:
[plugins.md](plugins.md).

`[[plugins.announce]]`: one entry per suggested plugin, 1 to 8 entries, in the order Mumcord
shows them. The whole announcement must fit into 1000 bytes; all of it is checked at start.

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `id` | string | required | | Plugin id from its manifest: 3 to 32 characters, lowercase letters, digits and `-`, starting with a letter. Each id once. |
| `min` | string | required | | Lowest version the community uses, `MAJOR.MINOR.PATCH`. |
| `source` | string | required | | `official`, or the `https://` address of a third-party repository's `index.json`. |
| `key` | string | none | | The third-party repository's minisign public key (56 characters, starting with `RW`). Required for a third-party source, not allowed for `official`. |
| `for` | string | required | | What the community uses the plugin for, 1 to 40 characters, shown to users. |

The announcement counter `seq` is kept in `plugins-seq.json` in the data directory and grows by
one whenever the list changes between two starts.

## `[voice]`: bitrate of voice senders (optional)

Applies to every module that sends voice: the music module (which may override it in `[music]`)
and the tester build's `play` command.

| Key | Type | Default | Environment | Meaning |
| --- | --- | --- | --- | --- |
| `bitrate` | integer | `96000` | `MUMBOT_VOICE_BITRATE` | Opus bitrate in bit/s, 6000 to 510000. The default is Mumcord's own default. Out-of-range values stop the start with an error. |
| `frame_ms` | integer | `20` | `MUMBOT_VOICE_FRAME_MS` | Packet length in ms: `10` or `20` (the packet sizes of the codec; 40 and 60 ms are not supported). |

**Precedence**, highest first: command line (`--bitrate`, `--frame-ms`), environment variable, the
module's own section, `[voice]`, the default. Each key is resolved on its own.

**The server limit wins.** The server announces a maximum bandwidth (`ServerConfig`/`ServerSync`;
Murmur's default is 72000 bit/s). Mumble counts the packet headers against it, so the bot sends at
most *limit minus header overhead* (about 14800 bit/s for 20 ms packets over UDP, more for 10 ms,
the TCP tunnel or positional audio). If the configured bitrate is higher, the bot lowers it and
logs a warning with the configured, effective and server values (`voice bitrate lowered to fit the
server's bandwidth limit`). It never sends above the limit, and never below 6000 bit/s.

## Environment variables

| Variable | Used by | Meaning |
| --- | --- | --- |
| `MUMBOT_CONFIG` | modules | Config file path when `--config` is not given. The Docker image sets `/config/bot.toml`. |
| `MUMBOT_MODULES` | modules | Modules for `mumbot run`, comma-separated (`webhook,peer,music`); replaces `[modules] enabled`. |
| `MUMBOT_DATA` | both | Certificate folder. The Docker image sets `/data`. |
| `MUMCORD_BOT_DATA` | both | Older name of `MUMBOT_DATA`, still honoured when that is unset. |
| `XDG_DATA_HOME` | both | Base of the default certificate folder (`$XDG_DATA_HOME/mumbot`). |
| `SERVER_PASSWORD` | both | Connect password of the server. Empty counts as unset. |
| `MUMBOT_VOICE_BITRATE` | both | Voice bitrate in bit/s; wins over the config, loses to `--bitrate`. See `[voice]`. |
| `MUMBOT_VOICE_FRAME_MS` | both | Voice packet length (10 or 20 ms); same precedence. |
| `MUMBOT_PEER_API_SECRET` | peer | API secret of the mumpeer relay; wins over `peer.api_secret_file`. |
| `SUPERUSER_PASSWORD` | both | SuperUser password for `--superuser` (test bot) or `superuser = true` (webhook). |
| `MUMBOT_TESTENV_DIR` | both | Folder whose `.env` is the fallback for the two passwords (default `./testenv`). |
| `MUMCORD_TESTENV_DIR` | both | Older name of `MUMBOT_TESTENV_DIR`. |
| `RUST_LOG` | webhook | Log level: `error`, `warn`, `info` (the Docker image's default) or `debug`. Without it the binary logs errors only. |
| `MUMBOT_PORT_BASE` | tests | Port of the local test server for this repository's tests (default `44738`). |

## A complete example

```toml
[server]
host = "mumble.example.org"
port = 64738
name = "Build Bot"
cert_name = "build-bot"
channel = "Root/Dev"

[http]
listen = "127.0.0.1:8080"
rate_limit_per_minute = 20
delivery_timeout_secs = 3

[[keys]]
key = "REPLACE-WITH-A-LONG-RANDOM-KEY"
label = "ci"
channels = ["Dev", "Releases"]

[[keys]]
key = "REPLACE-WITH-ANOTHER-LONG-RANDOM-KEY"
label = "monitoring"
channels = [42]

[[token]]
channel = "Releases"
password = "REPLACE-WITH-THE-CHANNEL-PASSWORD"
```

The connect password comes from `SERVER_PASSWORD` in the environment.
