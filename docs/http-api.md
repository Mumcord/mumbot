# HTTP API reference

The webhook bot (`mumbot webhook`) serves a small HTTP/1.1 JSON API on `http.listen`
(default `127.0.0.1:8080`). It is the bot's own listener: nothing runs on the Mumble server.

| Request | Key needed | Answer |
| --- | --- | --- |
| [`POST /message`](#post-message) | yes | Posts a message into a channel. |
| [`GET /channels`](#get-channels) | yes | Lists the channels this key may write to. |
| [`GET /health`](#get-health) | no | Connection state, for health checks and monitoring. |

## Authentication

Send the key in the `X-API-Key` header:

```text
X-API-Key: <one of the keys in bot.toml>
```

- Every key in `[[keys]]` of `bot.toml` may only write to its own `channels`.
- The key is checked first, before the body is read; a missing or unknown key gets `401`.
- Keys are compared in constant time, and the log shows the key's `label`, never the key.
- There is no other authentication and no TLS on this listener: keep it on loopback or behind a
  TLS reverse proxy ([security.md](security.md)).

## `POST /message`

Posts one message into one channel and answers once the Mumble server has accepted it.

Body (JSON, at most 20 KB):

| Field | Type | Meaning |
| --- | --- | --- |
| `channel` | integer | Channel id (from `GET /channels`). |
| `message` | string | The text, 1 to 5000 characters (or the server's smaller limit, if it reports one). |

```bash
curl -s -X POST http://127.0.0.1:8080/message \
  -H 'X-API-Key: REPLACE-WITH-A-LONG-RANDOM-KEY' \
  -d '{"channel": 42, "message": "**build failed** on `main`\nhttps://ci.example.org/run/123"}'
```

```json
{"delivered":true}
```

`delivered` means "accepted by the server". A standard Mumble server does not acknowledge chat
messages, so the bot answers right away when the server already granted it the text permission
for that channel. Otherwise it waits up to `delivery_timeout_secs` (default 3) for a denial: a
denial answers `503`, no denial within that time counts as delivered. It cannot tell whether
someone read the message.

### Message format

The text is escaped, so markup sent by the caller never becomes markup in the chat. Only this
Markdown subset is converted:

| Write | Becomes |
| --- | --- |
| `**bold**` | **bold** |
| `*italic*` | *italic* |
| `` `code` `` | `code` |
| a fenced code block (three backticks) | a code block |
| `- item` | a list |
| `# heading` | a heading |
| `https://…` or `http://…` | a link (other schemes stay text) |
| a line break (`\n` in JSON) | a line break |

## `GET /channels`

```bash
curl -s -H 'X-API-Key: REPLACE-WITH-A-LONG-RANDOM-KEY' http://127.0.0.1:8080/channels
```

```json
{"channels":[{"id":42,"name":"Lobby"}]}
```

Only the channels this key may write to and that exist on the server right now.

## `GET /health`

No key needed, so a container health check or monitoring can call it.

```bash
curl -s http://127.0.0.1:8080/health
```

```json
{"connected":true,"uptime_seconds":312,"server":"mumble.example.org"}
```

| Field | Meaning |
| --- | --- |
| `connected` | `true` while the bot is logged in to the server; `false` while it reconnects. |
| `uptime_seconds` | Seconds since the bot started. |
| `server` | The configured `server.host`. |

The status code is `200` in both states, so check `connected`, not only the status. The
examples' health checks do: `wget -q -O - http://127.0.0.1:8080/health | grep -q '"connected":true'`.

## Errors

Errors answer with a status code and a short reason, never with a channel list or a key:

```json
{"error":"channel not allowed for this key"}
```

| Code | Reason text | When |
| --- | --- | --- |
| `400` | `invalid JSON body` | The body is not JSON with an integer `channel` and a string `message`. |
| `400` | `empty message` | `message` is empty. |
| `400` | `message too long` | Over 5000 characters, or over the smaller limit the server reports. |
| `401` | `unknown or missing key` | No `X-API-Key` header, or no key matches. |
| `403` | `channel not allowed for this key` | The channel exists but is not in this key's `channels`. |
| `404` | `channel not found` | No channel with that id. |
| `404` | `not found` | Unknown path. |
| `405` | `wrong method` | For example `GET /message`. |
| `413` | `request body too large` | Body over 20 KB. |
| `429` | `rate limit reached` | More than `rate_limit_per_minute` requests (default 20) from this key in a minute. |
| `503` | `not connected` | The bot is not connected, or lost the connection during delivery (it reconnects by itself). |
| `503` | `server refused the message` | The server denied it: no text permission there, or the channel needs a password (`[[token]]`). |

The checks run in this order: key, rate limit, body size, JSON, message, channel, permission
for the key, connection.

## Examples

```bash
API=http://127.0.0.1:8080
KEY=REPLACE-WITH-A-LONG-RANDOM-KEY

# Find the id of a channel by name.
curl -s -H "X-API-Key: $KEY" "$API/channels" | jq '.channels[] | select(.name == "Alerts") | .id'

# Post from a shell script and fail when the message was not delivered.
curl -sf -X POST "$API/message" -H "X-API-Key: $KEY" \
  -d "$(jq -n --arg m "Backup finished on $(hostname)" '{channel: 42, message: $m}')"

# Monitoring probe: exit 0 only while the bot is connected.
curl -s "$API/health" | grep -q '"connected":true'
```

`jq -n --arg` builds the JSON safely, so quotes or newlines in the text cannot break the body.
