# Example: history bot for an existing Mumble server

Runs mumbot's history bot in Docker next to a Mumble server you already have. The bot records
the chat of the channels you list and hands it to Mumcord users who were offline, through
Mumcord's history sync. What it stores and for how long: [docs/history.md](../../docs/history.md).

The bot is an ordinary Mumble client. Nothing is installed or changed on the server; it works
with any standard Mumble server 1.4 or newer (murmur / mumble-server).

## Prerequisites

- Docker with the Compose plugin.
- A Mumble server 1.4 or newer you run or may use for a bot, and its address. If it has a
  connect password, you need that too.
- In the recorded channels other than its own, the bot needs the "Listen" permission (granted
  to everyone by default).
- Tell the people on the server that a bot records the channel chat. The bot's comment says so
  too.

## Steps

```bash
cd examples/history

# 1. Settings: the server's connect password, if any.
cp .env.example .env
$EDITOR .env

# 2. Bot config: your server's address, the bot's channel and the channels to record.
cp bot.example.toml bot.toml
$EDITOR bot.toml
chmod 644 bot.toml               # the container user (uid 10001) must be able to read it

# 3. Start.
docker compose up -d
```

If the Mumble server runs on the same machine as Docker, set `host = "host.docker.internal"` in
`bot.toml`; the compose file maps that name to the host.

## Check that it works

```bash
docker compose logs -f mumbot    # "history: recording channels=…" and "bot connected"
```

In a Mumble client, the bot sits in its channel and its comment names the recorded channels.
Post a message, then join that channel later with Mumcord: the message appears marked
"from History's history".

The log never shows message texts, names or certificate hashes. A warning
`configured channel not found` means a name in `channels` or `exclude` does not exist on the
server.

## Keep it running

- `restart: unless-stopped` brings the bot back after a reboot or a crash; a dropped server
  connection is retried by the bot itself.
- The `mumbot-data` volume holds the bot's client certificate and `history.sqlite3`. Keep it
  private. `docker compose down -v` deletes both: the history is gone and the server sees a new
  user.
- After editing `bot.toml`: `docker compose restart mumbot`. New channels on the server are
  picked up at the next start too.
- To run the history bot together with the webhook bot as one user, use `command: ["run"]`,
  set `MUMBOT_MODULES: history,webhook`, add the webhook sections to `bot.toml` and keep the
  image's health check (see [examples/webhook](../webhook/)).

Every config key: [docs/configuration.md](../../docs/configuration.md).
