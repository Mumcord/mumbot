# Example: webhook bot for an existing Mumble server

Runs mumbot's webhook bot in Docker next to a Mumble server you already have. Monitoring, CI or
any script then posts into a channel with one HTTP request:

```bash
curl -s -X POST http://127.0.0.1:8080/message \
  -H "X-API-Key: $KEY" \
  -d '{"channel": 1, "message": "**deploy** finished"}'
```

The bot is an ordinary Mumble client. Nothing is installed or changed on the server; it works
with any standard Mumble server (murmur / mumble-server).

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- A Mumble server you run or may use for a bot, and its address. If it has a connect password,
  you need that too.
- The bot needs the "Text message" permission in the channels it writes to (granted to everyone
  by default).
- Port 8080 free on 127.0.0.1, or another one set in `.env`.

## Steps

```bash
cd examples/webhook

# 1. Settings: the server's connect password, if any.
cp .env.example .env
$EDITOR .env

# 2. Bot config: your server's address, the channel, and a random API key.
cp bot.example.toml bot.toml
KEY=$(openssl rand -hex 32)
sed -i "s/REPLACE-WITH-A-LONG-RANDOM-KEY/$KEY/" bot.toml
$EDITOR bot.toml                 # host, port, channel, the key's channels
chmod 644 bot.toml               # the container user (uid 10001) must be able to read it

# 3. Start and wait until the bot is connected.
docker compose up -d --wait

# 4. Find the channel ids this key may write to, then post.
curl -s -H "X-API-Key: $KEY" http://127.0.0.1:8080/channels
curl -s -X POST http://127.0.0.1:8080/message \
  -H "X-API-Key: $KEY" \
  -d '{"channel": 1, "message": "hello from mumbot"}'
# {"delivered":true}
```

On macOS, use `sed -i ''` instead of `sed -i`. If the Mumble server runs on the same machine
as Docker, set `host = "host.docker.internal"` in `bot.toml`; the compose file maps that name
to the host.

## Check that it works

```bash
docker compose ps                      # mumbot "healthy" = HTTP up and connected to the server
curl -s http://127.0.0.1:8080/health   # {"connected":true,"uptime_seconds":42,"server":"mumble.example.org"}
docker compose logs -f mumbot          # one line per request: path, key label, channel, status
```

The health check of the container only turns healthy while the bot is connected to the server.
The log never shows the key or the message text. A warning about plain HTTP at startup is
expected: the bot listens on all interfaces inside the container, and the port is published to
`127.0.0.1` only.

## Keep it running

- `restart: unless-stopped` brings the bot back after a reboot or a crash; a dropped server
  connection is retried by the bot itself.
- The `mumbot-data` volume holds the bot's client certificate, its identity on the server.
  Keep it: with a new certificate the server sees a new user, and a registration or
  permissions given to the bot are lost. `docker compose down -v` deletes it.
- After editing `bot.toml`: `docker compose restart mumbot`.
- Other machines should reach the API through a TLS reverse proxy (Caddy, nginx, Traefik) in
  front of `127.0.0.1:8080`, not by opening the port: plain HTTP sends the key in clear text.

Every config key: [docs/configuration.md](../../docs/configuration.md). The API:
[docs/http-api.md](../../docs/http-api.md). Security: [docs/security.md](../../docs/security.md).

## Common problems

| Symptom | Cause and fix |
| --- | --- |
| `mumbot` restarts with `cannot read config /config/bot.toml` | `bot.toml` is missing or not readable for uid 10001: copy it and run `chmod 644 bot.toml`. If Docker created a `bot.toml` folder, delete it first. |
| `invalid config` in the log | A typo or an unknown key in `bot.toml`; the log names the line. |
| The bot restarts and the log shows a connection error | Wrong `host` or `port`, the server is down, or a firewall blocks it. A server on this machine: `host = "host.docker.internal"`. |
| The log shows a rejected login | Wrong or missing `SERVER_PASSWORD`, the name is taken by a registered user, or the server is full. |
| `401 {"error":"unknown or missing key"}` | The `X-API-Key` header does not match a key in `bot.toml`. |
| `403 {"error":"channel not allowed for this key"}` | Add the channel to the key's `channels`, then restart the bot. |
| `404 {"error":"channel not found"}` | No channel with that id; list them with `GET /channels`. |
| `503 {"error":"server refused the message"}` | The bot lacks the text permission there, or the channel needs a password: add a `[[token]]` block. |
| `503 {"error":"not connected"}` | The bot is reconnecting; check `docker compose logs mumbot`. |
| `429 {"error":"rate limit reached"}` | More than `rate_limit_per_minute` requests from one key; wait or raise the limit. |
