# Example: music bot for an existing Mumble server

Runs mumbot's music bot in Docker next to a Mumble server you already have. A script, a CI job or
a person then plays an MP3 into a channel with one HTTP request, and everyone in the channel hears
it as normal voice, in any Mumble client:

```bash
curl -s -X POST http://127.0.0.1:8081/play \
  -H "X-API-Key: $KEY" \
  -d '{"file": "jingle.mp3"}'
```

The bot is an ordinary Mumble client. Nothing is installed or changed on the server; it works
with any standard Mumble server (murmur / mumble-server).

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- A Mumble server you run or may use for a bot, and its address. If it has a connect password,
  you need that too.
- The bot needs the "Enter" and "Speak" permissions in the channels it plays into (granted to
  everyone by default).
- Port 8081 free on 127.0.0.1, or another one set in `.env`.

## Steps

```bash
cd examples/music

# 1. Settings: the server's connect password, if any.
cp .env.example .env
$EDITOR .env

# 2. Bot config: your server's address, the channel, and a random API key.
cp bot.example.toml bot.toml
KEY=$(openssl rand -hex 32)
sed -i "s/REPLACE-WITH-A-LONG-RANDOM-KEY/$KEY/" bot.toml
$EDITOR bot.toml                 # host, port, channel, the key's channels
chmod 644 bot.toml               # the container user (uid 10001) must be able to read it

# 3. Music: copy MP3s into media/ (readable for uid 10001).
cp ~/Music/jingle.mp3 media/

# 4. Start and wait until the bot is connected.
docker compose up -d --wait

# 5. Play, look at the queue, skip.
curl -s -X POST http://127.0.0.1:8081/play -H "X-API-Key: $KEY" -d '{"file": "jingle.mp3"}'
curl -s http://127.0.0.1:8081/queue -H "X-API-Key: $KEY"
curl -s -X POST http://127.0.0.1:8081/skip -H "X-API-Key: $KEY"
```

Upload an MP3 instead of using the media folder:

```bash
curl -s -X POST "http://127.0.0.1:8081/play?channel=1" -H "X-API-Key: $KEY" \
  -H "Content-Type: audio/mpeg" --data-binary @song.mp3
```

All endpoints, limits and errors: [docs/music.md](../../docs/music.md).

## Run it with the webhook bot

One bot user can do both: set `command: ["run"]`, add the webhook's `[http]` and `[[keys]]`
sections to `bot.toml`, list `[modules] enabled = ["music", "webhook"]` and publish port 8080 as
well. See [docs/modules.md](../../docs/modules.md).

## Stop

```bash
docker compose down        # keeps the bot's certificate volume
docker compose down -v     # also forgets the bot's identity
```
