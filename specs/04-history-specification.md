# Epic 4 – History module · Specification

> Status: Final · As of: 2026-10-31
> Sources: issue [#2](https://github.com/Mumcord/mumbot/issues/2) (history bot)
> Builds on: `specs/03-modules-specification.md` (modules on one connection, main and tester builds); Mumcord's history sync, mumcord `specs/01-foundation-specification.md` FR-1.60, FR-1.60b, QA.48, QA.49, QA.69, with the wire format in mumcord `crates/engine/src/history_sync/` and `docs/engine-api.md` ("History sync")

## 1. Goal & context

Mumble servers keep no chat history. Mumcord clients fill each other's gaps with history sync (mumcord FR-1.60): a client that joins or follows a channel asks the other Mumcord users on the server, over plugin data, for the channel's messages from before it started receiving the channel live. That only works while someone who saw the messages is online.

The history module is a peer that is always there: the bot stays on the server, records the chat of the channels it is set up for and answers history sync requests exactly like a Mumcord client with "Share my channel history" switched on. Mumcord needs no change, and nothing is added on the server side.

## 2. Scope

### 2.1 In scope

- Module `history`, alone (`mumbot history --config bot.toml`) or with other modules (`mumbot run --modules history,…`), in the main build.
- Recording channel messages of a configured channel list (allow list, deny list) in a local SQLite database: channel, sender name, sender certificate hash, receive time, message HTML (which carries replies, quotes and image references).
- Retention by age (days) and by count (messages per channel).
- The holder side of Mumcord's history sync over plugin data (`mumcord.history`, packet version 1): summaries, hash lists and message transfers, with Mumcord's limits.
- Transparency: the bot's comment says what it records, for how long, and what it never records.

### 2.2 Out of scope / later

- An HTTP API for reading, searching or exporting the history (open question in #2): later, as its own decision.
- The requester side: the bot does not fill its own gaps from Mumcord clients after downtime.
- Purging one person's messages on request (not asked for in #2). Operators can delete the database file; retention removes everything after at most 7 days.
- Edited or deleted messages: Mumcord has no message editing or deletion over the network today, so there is nothing to follow. "Clear history" in Mumcord is local; Mumcord itself keeps cleared messages from coming back through the sync (it asks only after the clear and drops anything older), so the bot needs nothing for it. When Mumcord adds edits or deletions, the bot has to follow in a new spec.
- Downloading or storing image files: only what is in the message HTML is kept.
- A channel created while the bot is online is picked up at the next login, not right away.

## 3. Functional requirements

### 3.1 Module and config

- **FR-4.1** Module `history` in the module list of `specs/03-modules-specification.md`; `mumbot history --config bot.toml` runs it alone (like `webhook` and `peer`), `mumbot run` with other modules on the same connection.
- **FR-4.2** Config section `[history]`, required when the module runs:
  ```toml
  [history]
  channels = ["Lobby", "Root/Raid"]   # names, paths or IDs; "*" = every channel
  exclude = ["AFK"]                    # never recorded, wins over channels
  retention_days = 7                   # 1 to 7, default 7
  max_messages_per_channel = 5000      # 1 to 100000, default 5000
  database = "/data/history.sqlite3"   # default: history.sqlite3 in the data directory
  ```
  `channels` is required and not empty (opt-in: no channel is recorded unless listed). Unknown keys are errors, as everywhere in the config.
- **FR-4.3** `server.server_cert_sha256` is accepted with the history module (it pins the shared connection), as with `peer`.

### 3.2 Recording

- **FR-4.4** After every login the bot joins `server.channel` (as before), resolves the recorded channels against the live channel tree (allow list minus deny list; `"*"` is every channel) and listens to every recorded channel other than its own (Mumble channel listeners, servers 1.4+). A channel name that does not resolve is logged as a warning and skipped.
- **FR-4.5** A text message is recorded for every recorded channel it reaches: each channel in its channel targets, and each channel inside one of its tree targets. Messages with only session targets (private messages) are never recorded, whatever the channel list says. Messages from the server itself (no sender) are not recorded.
- **FR-4.6** A record holds the channel key (the channel's path below the root, joined with `/`, the root being the empty string: Mumcord's key), the sender's name and certificate hash (if any), the receive time in Unix milliseconds and the message HTML exactly as received. Bodies over 8 KB (inline images) are not recorded, because history sync cannot carry them (mumcord `MAX_BODY`).
- **FR-4.7** Retention: messages older than `retention_days` and, per channel, all but the newest `max_messages_per_channel` are deleted at start, hourly and after inserts that exceed the count. History sync reaches 7 days back at most, so longer retention is not offered.
- **FR-4.8** No audio is received into storage, and no message text, name or certificate hash is ever logged (counts and errors only).

### 3.3 Answering history sync (holder side, mumcord FR-1.60)

Packets, limits and rules are Mumcord's (`crates/engine/src/history_sync/core.rs`, holder side):

- **FR-4.9** Plugin data with data id `mumcord.history`, compact JSON header `{"v":1,"t":…}` plus optional `\n` and binary body, at most 1000 bytes. Packets with a version below 1, malformed packets and packets from unknown sessions are ignored.
- **FR-4.10** `areq` (who holds channel `c` in `[from, to)`): answered with `avail` (count, oldest, newest; no content) when there is something to offer, silence otherwise.
- **FR-4.11** `hreq`: the newest 500 recorded messages of the channel in the range, oldest first, as chunked 8-byte content hashes (`hashes`); `deny` when the request is not allowed or 3 answers are already open. A new `hreq` from the same person replaces their open answer.
- **FR-4.12** `get` (bitmap over the hash list): the selected messages as a chunked JSON array of `{ts, n, h, b}` (`part`), at most 256 KB per answer. An answer whose `get` does not arrive within 60 s is dropped.
- **FR-4.13** Allowed requests: only from people who currently receive the channel live (in it or listening to it), clamped to the moment the bot saw them start receiving it (join, move or listen; the bot's own login for people already there), to now and to 7 days back. Requests outside that range get silence (`areq`) or `deny` (`hreq`).
- **FR-4.14** Content hash: first 8 bytes of SHA-1 over `mumcord-history-v1`, channel key, `cert:<hash>` or `name:<name>`, send second and the normalized body, each followed by a zero byte (mumcord QA.48, `hash.rs`).
- **FR-4.15** Limits: each person may make about one request per 5 s (burst 6); further packets are ignored. Outgoing packets are paced at 3 per second (burst 10), round robin per receiver, below murmur's default plugin message limit (4 per second, burst 15).
- **FR-4.16** On a reconnect all sessions, open answers and queued packets are dropped; people are known again from the new login on.

### 3.4 Transparency and privacy

- **FR-4.17** After every login the bot sets its comment to a fixed notice: that it records the chat of the listed channels so Mumcord users can load messages from before they joined, the retention (days and messages per channel), and that private messages and voice are never recorded. Channel names in it are HTML-escaped.
- **FR-4.18** The database file is created with private permissions (owner only) on Unix. `docs/history.md` lists every stored field.

## 4. Acceptance criteria (verifiable against `testenv/`)

- **AC-4.1** `mumbot run --modules history --config bot.toml` with `channels = ["Lobby", "Raid"]` and `server.channel = "Lobby"` logs in, sits in Lobby, listens to Raid and shows the notice as its comment.
- **AC-4.2** Messages posted to Lobby and Raid are recorded; a private message to the bot and a message to an unlisted channel are not.
- **AC-4.3** A test client that joins Lobby afterwards and sends a scripted history sync request (`areq`, `hreq`, `get`, as Mumcord does) receives an `avail` with the right count, the hash list (equal to the hashes computed with Mumcord's algorithm) and exactly the requested messages.
- **AC-4.4** A request from a client that is not in or listening to the channel gets no `avail` and a `deny` for `hreq`.
- **AC-4.5** Unit tests: wire encoding and decoding with Mumcord's fixtures, content hashes against values computed by Mumcord's `hash.rs`, storage, retention by age and count, range clamping and rate limits.

## 5. Decision log

| # | Question (issue #2) | Decision | Why |
| --- | --- | --- | --- |
| Q1 | Which channels by default? | None: an explicit `channels` list is required (`"*"` for all), plus `exclude`. | Recording is a privacy decision the operator should make on purpose. |
| Q2 | Retention and storage limits? | `retention_days` 1–7 (default 7), `max_messages_per_channel` (default 5000). | Mumcord never asks for more than 7 days, so older data would only be kept, never used. |
| Q3 | Images stored or only referenced? | Only referenced: the message HTML is kept as received; bodies over 8 KB (inline images) are skipped. | History sync carries at most 8 KB per message; no file downloads. |
| Q4 | How does the bot show that it records? | A fixed comment notice on every login; the name is the operator's choice (`server.name`, e.g. "History"). | Every client shows the comment; a fixed text cannot be configured away. |
| Q5 | HTTP API for reading the history? | Not now. | Not needed for history sync; its own privacy questions. |
| Q6 | Which timestamp is stored? | The bot's receive time, like every Mumcord client. | Mumble has no server timestamps; hashes accept ±1 s between clients. |
| Q7 | Storage engine? | SQLite (`rusqlite`, bundled), one file. | Mumcord's own store uses it; no server process, works in the static builds. |

## 6. Impact on Mumcord

None. The bot speaks history sync packet version 1 exactly as a Mumcord holder does. Known limits that come from Mumcord's protocol, not from the bot: clocks that differ by more than about a second from a client's can produce duplicates (mumcord QA.48), and Mumcord asks at most the 3 largest holders per channel.
