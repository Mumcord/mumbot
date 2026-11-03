# History bot

Mumble servers keep no chat history: whoever was offline misses what was said. Mumcord clients
fill those gaps among each other with **history sync**: a Mumcord user who joins or follows a
channel asks the other Mumcord users on the server for the messages from before they arrived.
That only works while someone who saw the messages is online.

The history module is that someone, around the clock. It sits on the server as an ordinary
Mumble client, records the chat of the channels you choose and answers Mumcord's history sync
requests exactly like a Mumcord client with "Share my channel history" switched on. Mumcord needs
no change and nothing is installed or changed on the server. Spec:
[`specs/04-history-specification.md`](../specs/04-history-specification.md).

## Run it

```toml
[server]
host = "mumble.example.org"
name = "History"          # shown to everyone; pick a name that says what the bot does
channel = "Lobby"         # where the bot sits

[history]
channels = ["Lobby", "Raid"]
```

```bash
mumbot history --config bot.toml               # alone
mumbot run --config bot.toml --modules history,webhook   # with other modules, one bot user
```

- The server must be Mumble 1.4 or newer (plugin data and channel listeners).
- The bot sits in `server.channel` and **listens** to every other recorded channel (Mumble's
  channel listeners), so it receives their chat without moving. It needs the "Listen"
  permission there (granted to everyone by default) and, if the server limits listeners per
  user, enough of them.
- The channel list is resolved at every login. A channel created later is picked up at the next
  login (restart the bot).
- Every key is described in [configuration.md](configuration.md#history-the-history-bot).

## What is recorded

Only channel messages of the recorded channels: `channels` minus `exclude` (`"*"` in `channels`
means every channel). For each message the bot stores one row in its SQLite file:

| Field | Content |
| --- | --- |
| channel | The channel's path below the root, for example `Lobby` or `Lobby/Sub`. |
| sent_at | When the bot received the message (Unix milliseconds); Mumble has no server timestamps. |
| sender_name | The sender's name at that moment. |
| sender_cert | The SHA-1 hash of the sender's client certificate, if they have one. |
| body | The message HTML exactly as received: text, formatting, links, quotes and replies, and image references (`<img src="…">`). |

Never recorded:

- **Private messages**, whatever the channel list says.
- **Voice**: the bot stores no audio.
- Messages from the server itself, channels outside the list, and messages larger than 8 KB
  (inline images; history sync cannot carry them anyway).

The bot logs no message texts, names or certificate hashes, only counts and errors.

## How long it is kept

| Key | Default | Range |
| --- | --- | --- |
| `retention_days` | 7 | 1 to 7 |
| `max_messages_per_channel` | 5000 | 1 to 100000 |

Older messages and, per channel, all but the newest `max_messages_per_channel` are deleted at
start, every hour and right after a new message exceeds the count. Mumcord never asks for more
than 7 days back, so a longer retention is not offered.

To delete everything at once, stop the bot and delete the database file (and its `-wal` and
`-shm` files next to it). Taking a channel off the list stops recording it; its rows are deleted
by the retention within `retention_days`.

## Transparency

After every login the bot sets its comment (what everyone sees when they look at the bot) to a
fixed notice: which channels it records so Mumcord users can load messages from before they
joined, how long messages are kept, and that private messages and voice are never recorded. The
notice cannot be configured away. Choose a name that makes the purpose obvious, such as
"History".

## Who gets the history

The bot answers like a Mumcord client, with Mumcord's rules (mumcord FR-1.60):

- Only people who currently receive the channel live (they are in it or listen to it) can ask
  (for [locked channels](#locked-channels) only people inside),
  and only for messages from before the moment the bot saw them start receiving it. For people
  who were already there when the bot logged in, that moment is the bot's login.
- At most 7 days back and 500 messages per request; Mumcord asks for more with "Load earlier
  messages".
- Each person may make about one request every 5 seconds (burst 6). At most 3 answers are
  prepared at the same time, 256 KB each. An answer that is not fetched within 60 seconds is
  dropped.
- The bot sends at most 3 plugin data packets per second (burst 10), below the server's default
  limit (4 per second, burst 15), round robin between receivers.

Official Mumble clients never ask (they do not know history sync), so they see the bot as an
ordinary user with a comment.

### Locked channels

A channel is **locked** when it or one of its parent channels restricts entering: a channel
password (access token) or an ACL that denies _Enter_. On a standard Mumble server _Listen_ is a
separate permission that everyone has by default, so listening to a locked channel must not be
enough to read its history (Mumcord issue #382). The bot follows the same rules as Mumcord; the
wire contract is Mumcord's `docs/history-sync.md`.

- **Only people physically inside** a locked channel get its history. Listeners get nothing: a
  summary request (`areq`) stays unanswered, a hash request (`hreq`) is denied. This is checked
  again before messages are sent (`get`), and an open answer ends as soon as the requester
  leaves the channel. Channels the bot does not know count as locked.
- **With channel passwords** in the config (`[[token]]`, the ones the bot logs in with), the bot
  also asks for a **password proof** before it sends anything for a locked channel. It cannot
  tell a password channel from one locked by groups, so it asks for every locked channel:
  1. It sends a challenge (`chal`) with 16 fresh random bytes and asks the server for its own
     permissions in that channel (PermissionQuery).
  2. The requester answers (`proof`) with one HMAC per password it holds; the password itself
     never travels. Both sides need a client certificate.
  3. The bot answers only if the proof arrives within 10 seconds, matches one of its own
     passwords (compared in constant time), the requester is still inside, and the server
     confirmed after the challenge that the bot may still enter (its password is current). Each
     challenge counts once, valid or not. Before sending the messages it checks again that it may
     still enter.
- **Without passwords** the bot answers people inside a locked channel on their presence alone,
  like a Mumcord client that got in through groups or registration.
- **Open channels** behave as before: whoever is in the channel or listens to it is answered.

A bot that only listens to a password channel is not inside it, so Mumcord does not send it a
proof: to share a password channel's history, let the bot sit in it (`server.channel`). Older
Mumcord versions do not answer challenges and get no history from a bot with passwords.

**Proof test vector.** `K = HMAC-SHA256(key = "mumcord.history.token.v1", token)`, then
`proof = HMAC-SHA256(K, nonce ‖ channel id (u32 big endian) ‖ len ‖ requester cert hash ‖ len ‖
answerer cert hash)`, lengths as one byte. For the token `s3cret`, the nonce `00 01 … 0f`,
channel 7, requester certificate hash `a` × 40 and answerer certificate hash `b` × 40 the proof
is `89745f2f4b58fa89eb78ba68d735b8016f90b438fc15d1effcc2ab7d1cd95297`. The bot's unit test
checks this value, so any implementation can compare against it.

Mumcord itself keeps deleted history deleted: after "Clear history" a Mumcord client only asks
for messages after the clear and drops anything older, so the bot's copy cannot bring cleared
messages back to that person. Mumcord has no message editing or deletion over the network, so
there are no edits or deletions for the bot to follow.

## Where the database lives

`history.database`, by default `history.sqlite3` in the bot's data directory (`server.data_dir`,
`MUMBOT_DATA`, `/data` in the Docker image). The file is created readable only by the bot's user.
Use one database per server: the rows do not say which server they came from. Back it up like any
other personal data, or not at all: nothing is lost that the retention would not delete within a
week anyway.

## Docker

[`examples/history/`](../examples/history/) runs the bot with Docker Compose and keeps the
certificate and the database on a volume. The image's health check asks the webhook module's
`/health`, so the example disables it for the history bot alone.
