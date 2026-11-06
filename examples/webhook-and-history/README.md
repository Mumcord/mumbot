# Example: webhook and history modules in one bot

Runs two of mumbot's modules in one process next to a Mumble server you already have:

- the **webhook** module lets monitoring, CI or any script post into a channel with one HTTP
  request ([docs/webhook.md](../../docs/webhook.md));
- the **history** module records the chat of the channels you list and hands it to Mumcord users
  who were offline ([docs/history.md](../../docs/history.md)).

Both run on one connection, so the server sees **one** bot user with one certificate. Which
modules run comes from `[modules] enabled` in `bot.toml`, because the container starts with the
`run` command. How choosing modules works, and what the container's default `webhook` command
does instead: [docs/modules.md](../../docs/modules.md).

The bot is an ordinary Mumble client. Nothing is installed or changed on the server; it works
with any standard Mumble server (murmur / mumble-server).

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- A Mumble server 1.4 or newer you run or may use for a bot, and its address. If it has a
  connect password, you need that too.
- The bot needs the "Text message" permission in the channels it writes to and the "Listen"
  permission in the channels the history records (both granted to everyone by default).
- Port 8080 free on 127.0.0.1, or another one set in `.env`.
- Tell the people on the server that a bot records the channel chat. The bot's comment says so
  too.

## Steps

```bash
cd examples/webhook-and-history

# 1. Settings: the server's connect password, if any.
cp .env.example .env
$EDITOR .env

# 2. Bot config: the server, the channel, a random API key and the channels to record.
cp bot.example.toml bot.toml
KEY=$(openssl rand -hex 32)
sed -i "s/REPLACE-WITH-A-LONG-RANDOM-KEY/$KEY/" bot.toml
$EDITOR bot.toml                 # host, port, channel, [modules], the key's and [history] channels
chmod 644 bot.toml               # the container user (uid 10001) must be able to read it

# 3. Start and wait until the bot is connected.
docker compose up -d --wait

# 4. Post a message, then read it back.
curl -s -X POST http://127.0.0.1:8080/message \
  -H "X-API-Key: $KEY" \
  -d '{"channel": 1, "message": "hello from mumbot"}'
# {"delivered":true}
```

On macOS, use `sed -i ''` instead of `sed -i`. If the Mumble server runs on the same machine as
Docker, set `host = "host.docker.internal"` in `bot.toml`; the compose file maps that name to the
host.

## Check that it works

```bash
docker compose logs mumbot | grep "bot connected"
# … modules=[Webhook, History]   ← both modules, one bot user

docker compose ps                      # mumbot "healthy" = HTTP up and connected to the server
curl -s http://127.0.0.1:8080/health   # {"connected":true,…}
```

With `modules=[Webhook]` instead, only the webhook runs: the config's list was replaced by a
single-module command, and mumbot logs a warning saying so. The container's command must be
`run` for the list to count.

In a Mumble client the bot sits in its channel. Post a message, then join that channel later with
Mumcord: the message appears marked "from Mumbot's history".

## Keep it running

- `restart: unless-stopped` brings the bot back after a reboot or a crash; a dropped server
  connection is retried by the bot itself.
- The `mumbot-data` volume holds the bot's client certificate and `history.sqlite3`. Keep it
  private. `docker compose down -v` deletes both: the history is gone and the server sees a new
  user.
- After editing `bot.toml`: `docker compose restart mumbot`. A change of the module list needs
  that restart as well.
- To change the modules for one run without editing `bot.toml`, set `MUMBOT_MODULES` in `.env`
  (comma-separated, for example `webhook,history`). An empty value means the config decides.
- When the webhook module is not in the list, its `/health` is gone: then drop the `ports` entry
  or disable the health check, as [`examples/history/`](../history/) does.

Every config key: [docs/configuration.md](../../docs/configuration.md). Every module:
[docs/modules.md](../../docs/modules.md).
