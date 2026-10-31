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

- Only people who currently receive the channel live (they are in it or listen to it) can ask,
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
