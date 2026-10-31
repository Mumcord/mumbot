# Configuration reference

The webhook bot reads one TOML file, `bot.toml`, given with `mumbot webhook --config <file>` or
the `MUMBOT_CONFIG` environment variable. The test bot needs no file; it takes command-line
options ([cli.md](cli.md)) and a few environment variables (below).

`bot.toml` holds API keys and passwords: keep it private and never commit it (the repository's
`.gitignore` excludes every `bot.toml`). Commented starting points:
[`examples/bot.example.toml`](../examples/bot.example.toml) for a binary,
[`examples/webhook/bot.example.toml`](../examples/webhook/bot.example.toml) for Docker.

Rules that apply to the whole file:

- An unknown key or section is an error, so a typo is reported at startup instead of ignored.
- The file is read once at startup. After a change, restart the bot.
- Config values do not expand environment variables. Only the connect password has an
  environment fallback (`SERVER_PASSWORD`, see `server.password`).

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
that key's channels. A config with a `[peer]` section may have no key at all; the bot then only
runs the peer module and answers `GET /health`.

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

## `[peer]`: relay discovery (optional)

Off unless the section is present. Announces a mumpeer relay to Mumcord clients and registers
their certificates and tokens with the relay's API; see [peer.md](peer.md).

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `udp` | list of `"host:port"`, not empty | required | Relay addresses for UDP, which clients try first. |
| `tcp` | list of `"host:port"` | `[]` | TCP fallback addresses, usually port 443, for networks that block UDP. |
| `fingerprint` | string | required | SHA-256 of the relay's TLS certificate: 64 hex digits, colons allowed (`openssl x509 -noout -fingerprint -sha256`). Clients only accept a relay with this certificate. |
| `api_url` | URL | required | Base URL of the relay's management API, for example `"http://127.0.0.1:8090"`. Plain HTTP to another host logs a warning; use `https` there. |
| `secret_file` | path | required | File with the API secret on its first line. Keep it private (`chmod 600`) and out of Git (`*.secret` is git-ignored). The secret is never logged. |
| `token_ttl_secs` | integer, 1 to 600 | `120` | How long a token for one pair of users is valid. |

All addresses together must fit one plugin data message (1000 bytes); the config check reports
it otherwise.

## Environment variables

| Variable | Used by | Meaning |
| --- | --- | --- |
| `MUMBOT_CONFIG` | webhook | Config file path when `--config` is not given. The Docker image sets `/config/bot.toml`. |
| `MUMBOT_DATA` | both | Certificate folder. The Docker image sets `/data`. |
| `MUMCORD_BOT_DATA` | both | Older name of `MUMBOT_DATA`, still honoured when that is unset. |
| `XDG_DATA_HOME` | both | Base of the default certificate folder (`$XDG_DATA_HOME/mumbot`). |
| `SERVER_PASSWORD` | both | Connect password of the server. Empty counts as unset. |
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
