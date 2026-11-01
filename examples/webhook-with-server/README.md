# Example: webhook bot with its own Mumble server

Starts a standard Mumble server and mumbot's webhook bot side by side with one
`docker compose up`. Send an HTTP request, and the message shows up in the server's root
channel. Ideal for trying the bot out on your own machine before you point it at a real server.

| Service | Image | Reachable at |
| --- | --- | --- |
| `mumble-server` | `mumblevoip/mumble-server:v1.5.735` (unmodified upstream image) | `127.0.0.1:64738` (Mumble client) |
| `mumbot` | `ghcr.io/mumcord/mumbot:0.3.0` | `http://127.0.0.1:8080` (HTTP API) |

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- `curl` and `openssl` for the steps below.
- Optional: a Mumble client (official Mumble or Mumcord) to watch the messages arrive.
- Ports 64738 and 8080 free on 127.0.0.1, or other ones set in `.env`.

## Steps

```bash
cd examples/webhook-with-server

# 1. Settings: a random SuperUser password for the server.
cp .env.example .env
sed -i "s/^SUPERUSER_PASSWORD=.*/SUPERUSER_PASSWORD=$(openssl rand -hex 16)/" .env

# 2. Bot config: a random API key. The container user (uid 10001) must be able to read it.
cp bot.example.toml bot.toml
KEY=$(openssl rand -hex 32)
sed -i "s/REPLACE-WITH-A-LONG-RANDOM-KEY/$KEY/" bot.toml
chmod 644 bot.toml

# 3. Start both and wait until they report healthy.
docker compose up -d --wait

# 4. Post a message into the root channel (id 0).
curl -s -X POST http://127.0.0.1:8080/message \
  -H "X-API-Key: $KEY" \
  -d '{"channel": 0, "message": "**hello** from mumbot"}'
# {"delivered":true}
```

On macOS, use `sed -i ''` instead of `sed -i`. Connect a Mumble client to `127.0.0.1`, port
`64738`, to see the bot (`Mumbot`) and its messages. The server uses a self-signed certificate,
so the client asks you to accept it once.

## Check that it works

```bash
docker compose ps                           # both services "healthy"
curl -s http://127.0.0.1:8080/health        # {"connected":true,"uptime_seconds":12,"server":"mumble-server"}
curl -s -H "X-API-Key: $KEY" http://127.0.0.1:8080/channels   # {"channels":[{"id":0,"name":"Root"}]}
docker compose logs -f mumbot               # one line per request: path, key label, channel, status
```

The log never shows the key or the message text, only the key's label and the status code.
A warning about plain HTTP at startup is expected: the bot listens on all interfaces inside the
container, and the port is published to `127.0.0.1` only.

## Stop and clean up

```bash
docker compose down        # stop; the server database and the bot's certificate stay
docker compose down -v     # stop and delete both volumes (new server, new bot identity)
```

## Going further

- More channels: connect a Mumble client, log in as `SuperUser` with the password from `.env`,
  create channels, then add their names or ids to `channels` in `bot.toml` and run
  `docker compose restart mumbot`.
- A real server instead of the bundled one: [`../webhook/`](../webhook/).
- Every config key: [docs/configuration.md](../../docs/configuration.md). The API:
  [docs/http-api.md](../../docs/http-api.md).

## Common problems

| Symptom | Cause and fix |
| --- | --- |
| `SUPERUSER_PASSWORD` error on `up` | `.env` is missing: copy `.env.example` to `.env`. |
| `mumbot` restarts with `cannot read config /config/bot.toml` | `bot.toml` is missing or not readable for uid 10001: copy it and run `chmod 644 bot.toml`. If Docker created a `bot.toml` folder, delete it first. |
| `invalid config` in the log | A typo or an unknown key in `bot.toml`; the log names the line. |
| `401 {"error":"unknown or missing key"}` | The `X-API-Key` header does not match a key in `bot.toml`. |
| `403 {"error":"channel not allowed for this key"}` | Add the channel to the key's `channels`, then restart the bot. |
| `404 {"error":"channel not found"}` | No channel with that id; list them with `GET /channels`. |
| `503 {"error":"not connected"}` | The bot is reconnecting; check `docker compose logs mumbot`. |
| `port is already allocated` | Another program uses 64738 or 8080: set `MUMBLE_PORT` or `HTTP_PORT` in `.env`. |
| `up --wait` reports `mumbot` unhealthy | The bot runs but is not connected: check the server password and the logs of both services. |
