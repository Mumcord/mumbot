# Music bot: play MP3s into a voice channel

The music module plays MP3 files as normal Mumble voice into a channel, so everyone there hears
them, in the official Mumble client, in Mumcord and in any other client. Scripts, CI jobs or
people control it over an HTTP API with an API key: play, queue, skip, stop, volume.

Sources:

- **A file** from a media folder you configure (`{"file": "jingle.mp3"}`).
- **An upload**: the MP3 itself as the request body.
- **A URL** (`{"url": "https://…"}`): off by default, and then only from hosts you list.

The bot is an ordinary Mumble client; nothing is installed on the server. Spec:
[`specs/05-music-specification.md`](../specs/05-music-specification.md).

## Run it

```bash
mumbot music --config bot.toml                      # the music module alone
mumbot run --config bot.toml --modules music,webhook  # with others, one bot user
```

Docker: [`examples/music/`](../examples/music/).

## Config file

The shared `[server]` section plus `[music]` (every key: [configuration.md](configuration.md#music-the-music-bot)):

```toml
[server]
host = "mumble.example.org"
name = "Music"
channel = "Lobby"            # joined at login; the bot returns here when the queue is empty

[music]
listen = "127.0.0.1:8081"    # its own listener, loopback by default
channel = "Lobby"            # where a track plays when the request names no channel
media_dir = "/srv/music"     # what {"file": …} may play
bitrate = 96000              # Opus bitrate of the music; overrides [voice]

[[music.keys]]
key = "a-long-random-key"    # openssl rand -hex 32
label = "jukebox"            # shown in the log instead of the key
channels = ["Lobby", 7]      # channels this key may play into
```

The module refuses to start without a key, or with neither a `media_dir` nor URLs switched on.

## Play

```bash
# A file from the media folder.
curl -s -X POST http://127.0.0.1:8081/play -H "X-API-Key: $KEY" \
  -d '{"file": "jingle.mp3"}'
# {"queued":true,"id":1,"position":0,"title":"Artist – Jingle"}

# Into channel 7, before everything else, ending the current track.
curl -s -X POST http://127.0.0.1:8081/play -H "X-API-Key: $KEY" \
  -d '{"file": "raid/intro.mp3", "channel": 7, "interrupt": true}'

# An upload: options as query parameters.
curl -s -X POST "http://127.0.0.1:8081/play?channel=7" -H "X-API-Key: $KEY" \
  -H "Content-Type: audio/mpeg" --data-binary @song.mp3

# From an allowed host (needs [music.urls]).
curl -s -X POST http://127.0.0.1:8081/play -H "X-API-Key: $KEY" \
  -d '{"url": "https://files.example.com/song.mp3"}'
```

`position` 0 means the track plays next (right away when nothing plays). The title comes from the
MP3's tags, else from the file name.

What happens then: the bot moves into the track's channel, shows `Now playing: <title>` as its
comment, plays the queue track by track and, when the queue is empty, clears the comment and goes
back to `server.channel` (turn off with `return_home = false`).

## Control

```bash
curl -s http://127.0.0.1:8081/queue -H "X-API-Key: $KEY"
# {"now_playing":{"id":1,"title":"…","source":"file","channel":1,"duration_secs":184},
#  "queue":[…],"volume":60}
curl -s http://127.0.0.1:8081/now-playing -H "X-API-Key: $KEY"   # {"now_playing":{…}} or null
curl -s -X POST http://127.0.0.1:8081/skip -H "X-API-Key: $KEY"   # {"skipped":true}
curl -s -X POST http://127.0.0.1:8081/stop -H "X-API-Key: $KEY"   # {"stopped":true,"dropped":3}
curl -s -X POST http://127.0.0.1:8081/volume -H "X-API-Key: $KEY" -d '{"volume": 40}'
curl -s http://127.0.0.1:8081/health                             # no key needed
```

There is one player per bot: any music key can skip, stop or change the volume. A key can only
play into its own channels.

## Endpoints

| Endpoint | Key | Body | Answer |
| --- | --- | --- | --- |
| `POST /play` | yes | JSON `file` or `url`, optional `channel` (id), `interrupt`; or an `audio/mpeg` upload with `?channel=&interrupt=` | `queued`, `id`, `position`, `title` |
| `GET /queue` | yes | – | `now_playing`, `queue`, `volume` |
| `GET /now-playing` | yes | – | `now_playing` |
| `POST /skip` | yes | – | `skipped` (false when nothing played) |
| `POST /stop` | yes | – | `stopped`, `dropped` (waiting tracks removed) |
| `POST /volume` | yes | `{"volume": 0–100}` | `volume` |
| `GET /health` | no | – | `connected`, `uptime_seconds`, `server` |

Errors are JSON `{"error": "…"}`:

| Status | Meaning |
| --- | --- |
| 400 | Invalid JSON, both or neither of `file`/`url`, not an MP3, volume out of range |
| 401 | Missing or unknown `X-API-Key` |
| 403 | Channel not allowed for this key; path outside the media folder; URLs disabled, host not allowed or a private address |
| 404 | File or channel not found; unknown endpoint |
| 405 | Wrong method |
| 408 | The request (with its upload) did not arrive within 60 seconds |
| 409 | Queue full |
| 413 | Track larger than `max_track_mb` |
| 422 | Track longer than `max_track_secs` |
| 429 | More than `rate_limit_per_minute` requests from this key |
| 502 | Download failed: no answer within `timeout_secs`, not status 200, a redirect |
| 503 | The bot is not connected to the server |

## Limits

| Setting | Default | What it limits |
| --- | --- | --- |
| `max_track_mb` | 20 | Size of a file, an upload or a download. |
| `max_track_secs` | 900 | Length; a longer track is refused, a track without a known length is cut. |
| `max_queue` | 10 | Waiting tracks. Queued tracks are kept in memory, so the worst case is about (`max_queue` + 1) × `max_track_mb`. |
| `rate_limit_per_minute` | 30 | Requests per key. |
| `[music.urls] timeout_secs` | 20 | The whole download. |

## URL sources

Off unless switched on, and then only from the hosts you list:

```toml
[music.urls]
enabled = true
allow_hosts = ["files.example.com", "*.cdn.example.com"]   # *.domain = every subdomain
allow_private = false
timeout_secs = 20
```

- Only `http` and `https`; no user name or password in the URL.
- The bot refuses any host that resolves to a private, loopback, link-local or otherwise
  non-public address, so a URL cannot be used to reach machines inside your network. Set
  `allow_private = true` only for a media server on your own network that you list in
  `allow_hosts`.
- Redirects are not followed. HTTPS certificates are checked against Mozilla's root store.
- Direct MP3 files only: streaming services (YouTube, Spotify, radio streams) do not work.

## Sound

- MP3 only (MPEG-1/2 Layer III). Stereo is mixed to mono, every sample rate is converted to
  48 kHz, then the bot encodes Opus like a Mumble client.
- Bitrate and packet length: `[music] bitrate`/`frame_ms`, else `[voice]`, else 96000 bit/s and
  20 ms; `--bitrate` and `MUMBOT_VOICE_BITRATE` override both ([configuration.md](configuration.md#voice-bitrate-of-voice-senders-optional)).
  The server's bandwidth limit lowers it, with a warning in the log.
- Volume is linear in percent of the original level; there is no loudness normalisation yet.
- Decoding runs on its own thread with two seconds of buffer, so a slow disk or a busy machine
  does not make the voice stutter.

## Permissions on the server

The bot needs "Enter" and "Speak" in the channels it plays into. Without "Enter" the track fails
with a log line; without "Speak" the server drops the voice silently. A server that suppresses or
mutes the bot (for example in a channel where only registered users may speak) does the same.

## Mumcord's Mumbot Music plugin

Mumcord users can control the bot from inside the client with the official Mumbot Music plugin.
So that the plugin knows which user on the server is a music bot, the module announces the
plugin to Mumcord clients:

- When a Mumcord client says hello to the bot (when it joins, or when the bot joins) and the
  hello lists the feature `plugins`, the bot sends plugin data `mumcord.plugins` with
  `{"v":1,"p":["mumbot-music@1"]}` to that client only. The entry has no source suffix, which
  marks it as an official plugin (Mumcord spec 06, FR-6.35).
- At most once every 10 seconds per client; a client that greets again sooner gets the announce
  when the 10 seconds are over. Answers to several clients go out together, once a second.
- The bot never sends a hello itself and receives nothing back. Official Mumble clients and
  Mumcord versions without plugins get nothing. Nothing is needed on the server beyond plugin
  data (Mumble 1.4 or newer); there is nothing to configure.

## Security notes

- Music keys are separate from the webhook's `[[keys]]`: a webhook key cannot play audio.
- The log shows the key label, endpoint, status, channel and the kind of source (file, upload,
  url with its host only), never the key, the file contents or a full URL.
- The listener is plain HTTP on loopback by default; to serve other machines, put a TLS proxy in
  front ([security.md](security.md#the-http-listener)).
- Files are confined to `media_dir`: absolute paths, `..` and symlinks pointing outside are
  refused. Mount the folder read-only in Docker.
