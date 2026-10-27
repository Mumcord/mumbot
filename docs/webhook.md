# Webhook bot: messages from an outside system into a channel

The webhook bot is a small headless program (`mumbot webhook`) that behaves exactly like a
normal Mumble client. An outside system — monitoring, CI, a script — sends one HTTP request with
an API key, and the bot posts that text into a channel. Nothing is installed, configured or
changed on the Mumble server: the server only sees a client that writes chat messages.

Specification: `specs/01-webhook-specification.md`. Docker and release binaries: see the README.

## Run it

```bash
cargo build --release
target/release/mumbot webhook --config bot.toml
```

The config path can also come from the environment: `MUMBOT_CONFIG=bot.toml mumbot webhook`.
Logging follows `RUST_LOG` (for example `RUST_LOG=info`).

The bot connects, keeps the connection up (it reconnects after a dropped link) and answers HTTP
requests until you stop it.

## Config file

`bot.toml` holds the API keys, so keep it private — it is git-ignored and must never be
committed. Use a long random key, not a word someone can guess.

```toml
[server]
host = "127.0.0.1"
port = 64738
name = "Mumbot"               # the user people see in the channel
# cert_name = "Mumbot"        # keeps the same identity; spaces are not allowed
channel = "Lobby"             # channel the bot joins at login
# password = ""               # only if the server needs a connect password
                              # (or set SERVER_PASSWORD in the environment)
# data_dir = "/path/to/certs" # default: MUMBOT_DATA, else ~/.local/share/mumbot

[http]
listen = "127.0.0.1:8080"     # loopback by default; set explicitly for a LAN
rate_limit_per_minute = 20
delivery_timeout_secs = 3

[[keys]]
key = "REPLACE-WITH-A-LONG-RANDOM-KEY"
label = "ci"                  # name used in the log
channels = ["Lobby", 42]      # name or channel id, mixed allowed
```

A key may only write to the channels listed for it. Add one `[[keys]]` block per outside system.

## Channel passwords

Passwords belong to the channel, not to a key, so they are a separate list (FR-1.22):

```toml
[[token]]
channel = "Secret"            # name or channel id
password = "hunter2"
```

The bot presents these passwords to the server at login, like any normal Mumble client. A channel
that only the token group may enter becomes usable for the bot then. Without the configured
password the bot stays outside that channel and nothing is written there. Passwords are secrets:
the config file is git-ignored, and they are never logged.

## Post a message

```bash
curl -s -X POST http://127.0.0.1:8080/message \
  -H 'X-API-Key: REPLACE-WITH-A-LONG-RANDOM-KEY' \
  -d '{"channel": 42, "message": "build failed"}'
```

Answer: `200 {"delivered":true}` — the message was written to the server and the bot had the
chat permission for that channel. A standard Mumble server does not acknowledge chat messages, so
`delivered` means "accepted by the server", not "read by someone".

Status codes:

| Code  | Meaning                                                                                                 |
| ----- | ------------------------------------------------------------------------------------------------------- |
| `200` | written to the server                                                                                   |
| `401` | key missing or unknown (checked before the body is read)                                                |
| `403` | channel not allowed for this key                                                                        |
| `404` | channel does not exist                                                                                  |
| `400` | bad JSON, empty message, or message over 5000 characters (or over the smaller limit the server reports) |
| `413` | request body over 20 KB                                                                                 |
| `405` | wrong HTTP method                                                                                       |
| `429` | rate limit reached (20 requests per minute per key)                                                     |
| `503` | bot not connected, or the server refused the message                                                    |

Error bodies carry a short reason only, never a channel list.

## Other endpoints

```bash
curl -s -H 'X-API-Key: <key>' http://127.0.0.1:8080/channels
# {"channels":[{"id":42,"name":"Lobby"}]}   only the channels this key may write to

curl -s http://127.0.0.1:8080/health
# {"connected":true,"uptime_seconds":312,"server":"127.0.0.1"}
```

## What the message may contain

Plain text is escaped, so markup sent by the caller never becomes markup in the chat. Only this
small subset is converted: `**bold**`, `*italic*`, `` `code` ``, fenced code blocks, `- list`,
`# heading`, plain `http`/`https` links, and line breaks. Links with other schemes stay text.

## The bot's identity

On first start the bot creates its own client certificate in `data_dir` (default `MUMBOT_DATA`,
else `$XDG_DATA_HOME/mumbot`, else `~/.local/share/mumbot`). The certificate is the bot's
identity on the server: keep the directory (in Docker: the `/data` volume) so the bot keeps its
registration and permissions. It is a private key — never commit or share it.

## Security notes

- TLS to the Mumble server is always on, with the bot's own client certificate (its identity).
- The listener binds to `127.0.0.1`; serving a LAN must be set in the config, and the bot warns
  at startup because the key then travels in clear text. Put a TLS reverse proxy in front.
- Keys are compared in constant time, so the answer time does not reveal them.
- Logs contain time, key label, channel id and status — never the key, never the message text.
