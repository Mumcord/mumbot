# Peer bot (relay discovery)

The peer bot connects a Mumble server to a [mumpeer](https://github.com/Mumcord/mumpeer) relay
([Mumcord#62](https://github.com/Mumcord/mumcord/issues/62), spec
`specs/04-mumpeer-relay-specification.md` in Mumcord). Two Mumcord clients that cannot reach each
other directly (strict NAT, blocked UDP) can then meet on the relay, and users can hide their IP
address from the other person. The bot only tells clients where the relay is and fetches
short-lived tokens for them; it never relays anything itself, and the Mumble server stays
unmodified.

```sh
mumbot peer --config bot.toml
```

The bot is off unless you run this command. It reads `[server]` and `[peer]` from `bot.toml`;
`[http]` and `[[keys]]` belong to the webhook bot and are not needed. When the modular bot
([#5](https://github.com/Mumcord/mumbot/issues/5)) exists, this becomes the module `peer` with the
same config section.

## Before you start

- **Register the bot** on the Mumble server (a registered user). Mumcord clients only trust a
  relay announced by a registered user.
- **Pin the server certificate.** The peer bot refuses to start without
  `server.server_cert_sha256` and refuses any server with another certificate, so it never hands
  out tokens on a server it did not mean to join. Get the value on the server host with
  `openssl x509 -in <server cert> -noout -fingerprint -sha256`.
- **The API secret** of your mumpeer relay: at least 32 characters (for example
  `openssl rand -hex 32`), no control characters, in a file only the bot can read (`chmod 600`)
  or in the environment variable `MUMBOT_PEER_API_SECRET`. A secret file that group or others
  can read stops the bot at start; in Docker, where mounted secrets are often readable by
  everyone in the container, use the environment variable. Never put it into `bot.toml` or Git.
- **Only registered users get relay tokens** by default (see
  [Who gets tokens](#who-gets-tokens)), so register everyone who should use the relay. With
  `allowed_groups`, only the members of the configured groups get them.

## How it works

Everything between the bot and the clients travels as plugin data (servers 1.4 and later), which
official Mumble clients ignore. Each message is compact JSON of at most 1000 bytes with data id
`mumcord.peer`; any `v` from 1 up is read and unknown fields are ignored.

1. **Announcement.** Every Mumcord client sends a hello (`mumcord.hello`) when it connects and to
   everyone who joins later. When the hello lists the feature `p2p`, the bot remembers the session
   and answers it, to that client only:

   ```json
   {"v":1,"t":"relay","host":"relay.example.org","udp":443,"tcp":443,"fp":"<64 hex>","state":"up","seq":4,"u":["img","file","scr","plg","cam"],"bc":25,"pk":512}
   ```

   `host`, ports and `fp` (SHA-256 of the relay certificate) come from the relay's status, so the
   operator sets the public address in one place. `seq` grows with every change. The bot checks
   the relay every 30 seconds; when the relay restarts, its address, ports or certificate change,
   or it becomes unreachable (two failed checks in a row, `state: "down"`) or reachable again,
   every remembered client gets the new announcement in one message. Before the relay answered
   once there is nothing to announce; waiting clients get it as soon as it answers. After a bot
   restart the clients greet the bot again (it is a new joiner for them) and get it that way. The
   bot answers each hello with its own, which says it is a bot (see
   [How Mumcord clients see the bot](modules.md#how-mumcord-clients-see-the-bot)).

   `u` lists what the relay allows (`img` full-size images, `file` files, `scr` screen sharing,
   `plg` plugin data, `cam` cameras; it comes from `uses` in the relay's status, and a relay without `uses` leaves
   `u` out, which means all) and `bc` is the relay's maximum viewers per broadcast
   (`broadcast.max_viewers`; left out when the relay offers no broadcasts). `pk` is the most
   kilobits per second one participant may send as plugin data (`plugin_data.max_kbps_per_sender`);
   it is left out when the relay reports no such limit or does not allow plugin data. When any of
   them changes, every remembered client gets a new announcement.

2. **Token.** A client that wants a relayed connection to one other person asks the bot:

   ```json
   {"v":1,"t":"token-req","rid":"<16 hex>","peer":<session of the other person>,"use":"img|file|scr|plg"}
   ```

   `use` says what the pair is for; a request without it (an older client) counts as `scr`. `plg`
   is plugin data between two Mumcord clients (Mumcord spec 09); the bot asks the relay for a pair
   with use `plugin_data` and never learns which plugin the data belongs to.

   The requester is the session the server stamped on the message, never a field of it. Both must
   be online with a certificate, both must have sent a hello with `p2p`, both must be allowed to
   use the relay ([Who gets tokens](#who-gets-tokens)), and they must be different users. The bot then asks the relay for a token for the two certificate hashes and
   sends it to both in one message:

   ```json
   {"v":1,"t":"token","rid":"<16 hex>","a":<requester session>,"b":<peer session>,"tok":"<43 base64url>","ttl":120}
   ```

   Otherwise only the requester gets `{"v":1,"t":"token-err","rid":"…","r":"…"}` with one of:

   | `r` | Why |
   | --- | --- |
   | `denied` | The requester has no certificate or sent no `p2p` hello, or asked for itself, the bot, a peer without certificate or a second session of the same identity; or the requester or the peer may not use the relay (a guest while `registered_only` is on, or not a member of an `allowed_groups` group); or the bot cannot read the groups. The bot's debug log tells these apart; the client only knows this list of reasons. |
   | `peer-gone` | The peer is not on the server. |
   | `peer-not-mumcord` | The peer sent no hello with `p2p`. |
   | `rate-limited` | More than 6 requests from this session in a minute, more than `cert_requests_per_minute` (10) from all sessions with this certificate in a minute, more than `max_pairs_per_cert` (4) open pairs with this certificate, or the relay asked the bot to slow down. |
   | `relay-down` | The relay is down or did not answer. |
   | `relay-full` | The relay has no room for another session. |
   | `use-not-allowed` | The relay has switched off the requested use (`403` from `POST /v1/pairs`). |

   A request without a valid `rid` (16 lowercase hex digits) is ignored.

3. **Broadcasts.** A screen share can be sent to the relay once and copied there to every viewer
   (Mumcord spec 07, relay fan-out). Only sessions whose hello lists both `p2p` and
   `screen-share` take part, sharer and viewer alike, and both must have a certificate and be
   allowed to use the relay ([Who gets tokens](#who-gets-tokens)).

   The sharer asks for a broadcast for up to `max` viewers (the client uses the `bc` it was
   announced; the relay caps it at its own limit):

   ```json
   {"v":1,"t":"bcast-req","rid":"<16 hex>","max":25}
   ```

   The bot creates it at the relay for the sharer's certificate and answers the sharer with the
   broadcast id and the publisher token:

   ```json
   {"v":1,"t":"bcast","rid":"<16 hex>","id":"<16 hex>","tok":"<43 base64url>","ttl":120}
   ```

   A session runs one broadcast of each kind at a time: a new `bcast-req` ends its previous
   broadcast of the same kind.

   **Plugin data.** Plugins send data to everyone in the channel through the same broadcasts, with
   `k` set to `plg` (Mumcord spec 09); without `k`, or with `"k":"scr"`, the request is a screen
   share:

   ```json
   {"v":1,"t":"bcast-req","rid":"<16 hex>","max":8,"k":"plg"}
   ```

   For plugin data the hello of sender and receiver must list `p2p` and `plugins` instead of
   `screen-share`. The bot creates the broadcast at the relay with use `plugin_data`, and at most
   8 sessions in one channel send plugin data broadcasts at the same time; a sender renewing its
   own broadcast keeps its place. The cap is checked when a broadcast is created, so someone who
   moves into a channel keeps a running broadcast. Admission, kicks, the rate limit and the
   cleanup below are the same as for screen sharing. The bot sees only that a broadcast carries
   plugin data, never which plugin or what it sends.

   **Cameras.** A camera is a broadcast with `k` set to `cam` (Mumcord spec 08):

   ```json
   {"v":1,"t":"bcast-req","rid":"<16 hex>","max":25,"k":"cam"}
   ```

   For cameras the hello of sender and viewer must list `p2p` and `camera` instead of
   `screen-share`. The bot creates the broadcast at the relay with use `camera`. Because a session
   runs one broadcast per kind, someone can share the screen and the camera at the same time. A
   channel runs at most `max_cameras_per_channel` cameras (default 9); one more gets `room-full`
   before the relay is asked, and a sender renewing its own camera keeps its place. Unlike a screen
   share, a camera ends when its sender changes channel: the bot deletes it at the relay. Camera
   admits do not count against the 60 requests per minute; they have their own bucket of 24 at
   once (someone turning the camera on admits everyone in the channel together), refilling at 60
   per minute. Kicks and the other cleanup are the same as for screen sharing.

   When someone presses Watch, the sharer decides (not full, viewer not blocked) and admits them:

   ```json
   {"v":1,"t":"bcast-admit","rid":"<16 hex>","id":"<16 hex>","viewer":<viewer session>}
   ```

   Only the session that created the broadcast may admit. The viewer must be online, another
   user than the sharer (not a second session of the same certificate), and in the sharer's
   channel. The bot then registers the viewer's certificate at the relay and sends the subscriber
   token to the viewer only:

   ```json
   {"v":1,"t":"bcast-tok","id":"<16 hex>","tok":"<43 base64url>","ttl":120,"sharer":<sharer session>,"k":"scr"}
   ```

   `k` is the kind of the broadcast (`scr`, `plg` or `cam`, the same values as in `bcast-req`).
   A sharer can run one broadcast of each kind at once, so the viewer needs it to hand the token
   to the right one.

   The sharer removes a viewer with `{"v":1,"t":"bcast-kick","id":"<16 hex>","viewer":<session>}`;
   the bot removes that viewer's certificate at the relay, which tells the sharer the viewer is
   gone (so it can change the media key). A kick is never answered and only works for viewers the
   bot admitted to that sharer's broadcast.

   The bot also cleans up by itself:

   - An admitted viewer leaves the server, changes certificate or moves out of the sharer's
     channel: removed at the relay (unless another admitted session with the same certificate
     still watches).
   - The sharer moves to another channel: every viewer not in the new channel is removed; the
     broadcast keeps running. A camera is deleted at the relay instead.
   - The sharer leaves or changes certificate: the broadcast is deleted at the relay.
   - After a reconnect of the bot or a relay restart the bot forgets its broadcasts; the relay ends
     unused ones after 60 seconds without frames.

   A refused `bcast-req` or `bcast-admit` gets `{"v":1,"t":"bcast-err","rid":"…","r":"…"}` to the
   sender with one of:

   | `r` | Why |
   | --- | --- |
   | `denied` | The sender or viewer has no certificate, sent no hello with `p2p` and `screen-share` (`plugins` for plugin data, `camera` for cameras), may not use the relay, or the viewer is the sharer, the bot or not on the server; the sender is not the broadcast's publisher; or the broadcast is unknown or has already ended at the relay. |
   | `not-same-channel` | The viewer is not in the sharer's channel. |
   | `full` | The broadcast has as many viewers as it allows. |
   | `rate-limited` | More than 60 `bcast-req` and `bcast-admit` from this session in a minute, or camera admits beyond a burst of 24 and 60 per minute. |
   | `relay-down` | The relay is down or did not answer. |
   | `relay-full` | The relay runs as many broadcasts as it allows, or none at all (`max_broadcasts = 0`); or 8 other sessions in the sender's channel already send plugin data. |
   | `room-full` | Only for `k = cam`: the sender's channel already runs `max_cameras_per_channel` other cameras. |
   | `use-not-allowed` | The relay has switched screen sharing (or plugin data for `k = plg`, cameras for `k = cam`) off. |

   The relay itself never sees Mumble sessions, only certificate hashes, and cannot read the
   stream: the media is encrypted end to end between sharer and viewers.

4. **Leaving.** When the last remembered session with a certificate leaves the server or changes
   certificate, the bot tells the relay to drop that certificate hash; the relay closes its
   sessions and pairs. A second device with the same certificate keeps its relay session while
   the other one leaves.

## Who gets tokens

The relay carries traffic between the two users of a pair, so the bot only hands out tokens to
people the server knows. Both users of a pair must be allowed.

- **Default: registered users (`registered_only = true`).** Unregistered guests never get a
  token, also not as the peer of someone else's request; they get `denied`. Certificates are free
  to create, a registration is not: an admin decides who is registered.
- **`registered_only = false`: guests too.** Anyone with a certificate who sends a Mumcord hello
  can get tokens. The bot logs a warning at start. Risk: someone can join twice with fresh
  certificates, pair their own two sessions and use the relay as a long-lived bandwidth tunnel
  between two hosts they control, filling its sessions and bandwidth and costing the operator
  traffic. The per-certificate limits below still apply, but new certificates cost nothing. Only
  turn it off on a server where every guest is trusted, for example a private server with a
  password.
- **`allowed_groups = ["…"]`: members of these groups only** ([#31](https://github.com/Mumcord/mumbot/issues/31)).
  When at least one group is configured it replaces the rule above: only the members of one of the
  listed groups get tokens, as requester and as peer, and both users of a pair must be members.

  ```toml
  [peer]
  api_url = "http://127.0.0.1:7480"
  allowed_groups = ["Relay users"]
  group_channel = "Root/Support"   # optional, "Root" by default
  ```

  - A group's members are the users the server lists for it: those it inherited from the parent
    channel, plus those added in this channel, minus those removed in it. Group members are
    registered users, so `registered_only` is implied.
  - The bot needs the **Write** permission on `group_channel`: standard servers answer an ACL query
    only for people who may write its ACLs. Give the bot that permission in the server's channel
    ACL (an admin decision, nothing on the server side is changed). Without it the bot refuses to
    start, and so does a configured group that the channel does not define — the bot never falls
    back to allowing everyone. During running, a read that fails (permission taken away, or the
    server does not answer within 5 seconds) refuses tokens until the next successful read.
  - The server never tells clients that a group's members changed, so the bot reads them again for
    every token request (at most one query per 10 seconds, so a busy server is not flooded) and at
    least every 60 seconds. Someone who loses access is dropped at the relay within a minute and
    gets no new token.

Per certificate, the bot also allows at most `cert_requests_per_minute` (default 10) token
requests per minute across all sessions, and at most `max_pairs_per_cert` (default 4, the relay's
default sessions per certificate) open pairs. A pair stays open until one of its two
certificates is dropped; a new token for the same two users replaces the old one and does not
count twice. A reconnect gets a new session but not a new budget.

## Relay API

The bot calls the relay's API (HTTP/1.1, JSON) with `Authorization: Bearer <secret>`. Every call
times out after 5 seconds.

| Call | Answer | When |
| --- | --- | --- |
| `GET /v1/status` | `200 {"boot_id","host","udp","tcp","fp",…}` | Every 30 seconds; the first check right at start. A new `boot_id` means the relay restarted. |
| `POST /v1/pairs` with `{"a":"<cert hash>","b":"<cert hash>","use":"images|files|screen_share|plugin_data"}` | `201 {"tok","expires_in"}`, `403` use off, `503` relay full, `429` rate limited | For every accepted token request. |
| `DELETE /v1/peers/{cert hash}` | `204` | When the last remembered session with that certificate leaves or changes certificate, or loses access. |
| `POST /v1/broadcasts` with `{"publisher":"<cert hash>","max_viewers":n}`, plus `"use":"plugin_data"` for plugin data | `201 {"id","tok","expires_in"}`, `403` that use off, `503` no room for another broadcast | For every accepted `bcast-req`. |
| `POST /v1/broadcasts/{id}/viewers` with `{"viewer":"<cert hash>"}` | `201 {"tok","expires_in"}`, `409` broadcast full, `404` broadcast ended | For every accepted `bcast-admit`. |
| `DELETE /v1/broadcasts/{id}/viewers/{cert hash}` | `204` | On `bcast-kick`, and when an admitted viewer leaves or moves out of the sharer's channel. |
| `DELETE /v1/broadcasts/{id}` | `204` | When the sharer leaves or starts a new broadcast. |

Over `https` the relay's self-signed certificate is pinned with `api_fp` instead of being checked
against certificate authorities; TLS 1.3 only. Plain `http` is meant for a relay on the same host
(loopback); any other host over plain HTTP stops the bot at start, because the secret would
travel in clear text. `allow_insecure_http = true` allows it anyway (for example inside a private
container network) and logs a warning.

## Logs

`RUST_LOG=mumbot=info` shows when the bot connected, when the relay became reachable, changed or
went down, and failed API calls. No user names, broadcast ids, sessions, certificate hashes, tokens or the secret
are logged.
