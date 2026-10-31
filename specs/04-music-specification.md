# Epic 4 – Music module · Specification

> Status: Final · As of: 2026-10-31
> Source: issue [#3](https://github.com/Mumcord/mumbot/issues/3) (music bot: play MP3 sent over HTTP)
> Builds on: `specs/03-modules-specification.md` (modules on one connection), `specs/01-webhook-specification.md` (API keys, HTTP handler), issue [#7](https://github.com/Mumcord/mumbot/issues/7) / PR #24 (voice bitrate settings)

## 1. Goal & context

Outside systems and people want to play audio into a voice channel: a jingle from a CI system, a song for a raid, an announcement. Every client must hear it as ordinary voice, Mumcord or not.

The music module is a production module like `webhook` and `peer`: it runs alone (`mumbot music --config bot.toml`) or next to the others on the same connection (`mumbot run --modules music,webhook`). It has its own authenticated HTTP API, plays MP3 files from a media directory, uploaded MP3s and (opt-in) MP3s from allowed URLs, keeps a queue, and sends the audio as Opus voice over the standard Mumble protocol.

## 2. Scope

### 2.1 In scope

- Module `music` in the main build, its `[music]` config section and the `mumbot music` command.
- HTTP API with `X-API-Key` keys per system: play, queue, now playing, skip, stop, volume.
- Sources: a file inside the configured media directory, an uploaded MP3, an MP3 from a URL (off by default, host allowlist).
- MP3 only. Decoding, mixing to mono and resampling to 48 kHz on a decode thread; Opus encoding at the configured voice bitrate (`[voice]`, overridable in `[music]`, from #7).
- Limits: size per track, length per track, queue length, fetch timeout, rate limit per key.
- The bot's comment shows the current track; the bot moves to the track's channel and back afterwards (configurable).

### 2.2 Out of scope / later

- Formats beyond MP3 (OGG, WAV, AAC, FLAC) and streaming services (YouTube, Spotify, radio streams). Only direct files.
- Chat commands in Mumble (`!skip`).
- Ducking, loudness normalisation, behaviour when someone talks.
- Mumcord's plugin-data "Now playing" card (mumcord FR-1.58): needs the message format from Mumcord and a compatibility decision.
- A second Mumble user for music: one process is one bot user (spec 03, Q2); an operator who wants a separate music user runs a second process.
- HTTPS on the bot's own listener (same as the webhook: a TLS proxy in front).

## 3. Functional requirements

### 3.1 Module and config

- **FR-4.1** `music` is a module (`Module::Music`). `mumbot music --config bot.toml` runs it alone; `mumbot run --modules music,…` or `[modules] enabled = ["music", …]` runs it with others on one connection.
- **FR-4.2** Its config is the `[music]` section; the module refuses to start without it, without at least one `[[music.keys]]` entry, or with neither a `media_dir` nor URLs enabled.

  ```toml
  [music]
  listen = "127.0.0.1:8081"   # own listener; loopback by default
  channel = "Music"           # default channel of a track (name or ID); else the bot's channel
  media_dir = "/media"        # files that `{"file": …}` may play
  return_home = true          # back to server.channel when the queue is empty
  volume = 60                 # start volume in percent (0–100)
  max_queue = 10              # tracks waiting, the playing one not counted
  max_track_mb = 20           # size cap per file, upload and download
  max_track_secs = 900        # length cap per track
  rate_limit_per_minute = 30  # requests per key
  bitrate = 96000             # overrides [voice] for music (#7)
  frame_ms = 20

  [music.urls]
  enabled = false             # URL sources are off unless switched on
  allow_hosts = ["files.example.com", "*.cdn.example.com"]
  allow_private = false       # private, loopback and link-local addresses
  timeout_secs = 20

  [[music.keys]]
  label = "ci"
  key = "…"
  channels = ["Music", 7]
  ```

- **FR-4.3** Voice settings follow the precedence of #7: command line, environment, `[music]`, `[voice]`, default; the server's bandwidth limit lowers the bitrate at send time and is logged.
- **FR-4.4** The bot user is shared with the other modules (spec 03, FR-3.4); the music module must not change how they behave apart from where the bot sits.

### 3.2 HTTP API

All endpoints except `/health` need a valid `X-API-Key` of `[[music.keys]]` (constant-time comparison, never logged). Answers are JSON. Unknown key: 401, rate limit: 429.

- **FR-4.5** `POST /play` with a JSON body `{"file": "jingle.mp3"}` or `{"url": "https://…"}`, optional `"channel": <id>` and `"interrupt": true`; or an upload with `Content-Type: audio/mpeg` and the options as query parameters (`/play?channel=3&interrupt=true`). Answer 200 `{"queued":true,"id":…,"position":…,"title":…}` (`position` 0 means playing now).
- **FR-4.6** The channel defaults to `[music] channel`, else the bot's current channel. The key must be allowed for the channel (403), the channel must exist (404).
- **FR-4.7** `interrupt` puts the track at the front of the queue and skips the current one.
- **FR-4.8** `GET /queue` returns `now_playing` (or null), the waiting tracks and the volume. `GET /now-playing` returns only the current track (or null).
- **FR-4.9** `POST /skip` ends the current track; `POST /stop` ends it and empties the queue; `POST /volume` with `{"volume": 0–100}` changes the volume of the current and later tracks.
- **FR-4.10** `GET /health` (no key) as in the webhook: `connected`, `uptime_seconds`, `server`.
- **FR-4.11** Errors: invalid body or not an MP3: 400; file not found: 404; path outside the media directory: 403; URLs disabled or host not allowed or private address: 403; too large: 413; too long: 422; queue full: 409; fetch failed or timed out: 502; not connected: 503.

### 3.3 Sources and security

- **FR-4.12** File paths are relative to `media_dir`. Absolute paths, `..`, and symlinks that resolve outside the media directory are refused (the canonical path must stay inside the canonical media directory).
- **FR-4.13** URLs are refused unless `[music.urls] enabled = true`. Only `http` and `https`, the host must match `allow_hosts` (exact, or `*.domain` for subdomains), no user info in the URL.
- **FR-4.14** No SSRF: every address the host resolves to must be public unless `allow_private = true` (loopback, private, link-local, unique-local, CGNAT, multicast, unspecified and documentation ranges are refused). The bot connects to the address it checked, not to a second lookup.
- **FR-4.15** Redirects are not followed (a 3xx answer is a fetch error). HTTPS verifies the certificate against the Mozilla root store.
- **FR-4.16** Size: a file, upload or download above `max_track_mb` is refused (413) without reading more than the cap. The whole fetch is bounded by `timeout_secs`; uploads by a request timeout.
- **FR-4.17** Length: a track whose MP3 header gives a length above `max_track_secs` is refused (422); a track without a known length is cut at the cap.
- **FR-4.18** Logs carry the key label, the endpoint, the status and the source kind; never the key, never the upload contents. URLs are logged by host only.

### 3.4 Playback

- **FR-4.19** Decoding never blocks the voice sender: a decode thread decodes, mixes to mono and resamples to 48 kHz into a lock-free ring buffer. The sender takes one packet per tick from the ring buffer, applies the volume, encodes Opus and hands the packet to the voice transport. The sender's tick path does no locking, no I/O, no logging and no heap allocation of its own.
- **FR-4.20** Before a track the bot moves to its channel; when the queue is empty it moves back to `server.channel` (if `return_home` and that is set). The comment shows `Now playing: <title>` while a track plays and is cleared afterwards; titles are escaped.
- **FR-4.21** A track ends with a voice terminator packet. A dropped connection ends the current track; the queue continues after the reconnect.

## 4. Acceptance criteria (verifiable against `testenv/`)

- **AC-4.1** `mumbot music --config bot.toml` with a media directory holding the bundled test sample: `POST /play {"file": "speech.mp3"}` answers 200 and a second (tester) bot in the channel hears audio above -45 dBFS from the music bot.
- **AC-4.2** `GET /queue` shows the playing track; `POST /skip` stops the audio within one second; `POST /stop` empties the queue.
- **AC-4.3** `{"file": "../bot.toml"}` answers 403; without a key 401; a URL with URLs disabled 403.
- **AC-4.4** `mumbot run --modules music,webhook` logs in once and serves both APIs.
- **AC-4.5** The sent voice uses the configured `[music] bitrate`, capped by the server limit (as in #7).

## 5. Decision log

| # | Question (issue) | Decision | Why |
| --- | --- | --- | --- |
| Q1 | Formats beyond MP3, streaming services? | MP3 only; direct files only, no streaming services. | The issue's MVP; every other format is one more decoder feature later. |
| Q2 | Limits? | Size 20 MB, length 15 min, queue 10, fetch timeout 20 s, 30 requests per minute per key; all configurable. | Bounded memory (queued tracks live in memory) and no endless tracks. |
| Q3 | Who may control it? | Any valid music key may play into its allowed channels; skip, stop and volume act on the one shared player for every music key. | One player per bot; per-key ownership of tracks adds rules nobody asked for yet. |
| Q4 | Chat commands? | Later. | HTTP covers the issue's use cases. |
| Q5 | Ducking, loudness? | Later; a fixed volume in percent. | Needs audio analysis of other speakers. |
| Q6 | Own listener or the webhook's? | Own listener (`[music] listen`, default `127.0.0.1:8081`), own keys. | Spec 03 keeps one listener per module; a webhook key should not be able to play audio. |
| Q7 | Decoder? | `symphonia` (pure Rust, MPL-2.0, only the MP3 feature). | Pure Rust builds statically everywhere (musl, Windows, macOS); MPL-2.0 is compatible with AGPL-3.0. `minimp3` needs a C build. |
| Q8 | Ring buffer? | `rtrb` (MIT/Apache-2.0), already in the tree through `mumcord-codec`. | Real-time safe single-producer single-consumer queue without `unsafe` in our code. |
| Q9 | URL TLS roots? | `webpki-roots` (Mozilla's root store compiled in). | The Alpine image and static binaries have no reliable system store. |
| Q10 | Resampling? | Linear interpolation to 48 kHz on the decode thread. | Most MP3s are 44.1 or 48 kHz; the Opus voice path hides the difference. A better resampler can replace it later. |
| Q11 | The main build carried no `mumcord-codec` (spec 03, AC-3.7). | The music module needs Opus in the main build, so `mumcord-codec` (and `bytes`) become main dependencies; `hound` and all test code stay tester-only. AC-3.7 now reads: `cargo tree` of the main build contains no `hound`. | A production module that sends voice cannot work without the encoder. |
| Q12 | Own Mumble user for music? | No, the shared user (spec 03, Q2). | One identity; a second process gives a second user. |
