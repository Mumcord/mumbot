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
- **Decide who may use the relay** (see [Who gets tokens](#who-gets-tokens)). By default every
  registered user; with `allowed_groups` only members of those groups, which needs the "Write
  ACL" permission for the bot.

## How it works

Everything between the bot and the clients travels as plugin data (servers 1.4 and later), which
official Mumble clients ignore. Each message is compact JSON of at most 1000 bytes with data id
`mumcord.peer`; any `v` from 1 up is read and unknown fields are ignored.

1. **Announcement.** Every Mumcord client sends a hello (`mumcord.hello`) when it connects and to
   everyone who joins later. When the hello lists the feature `p2p`, the bot remembers the session
   and answers it, to that client only:

   ```json
   {"v":1,"t":"relay","host":"relay.example.org","udp":443,"tcp":443,"fp":"<64 hex>","state":"up","seq":4}
   ```

   `host`, ports and `fp` (SHA-256 of the relay certificate) come from the relay's status, so the
   operator sets the public address in one place. `seq` grows with every change. The bot checks
   the relay every 30 seconds; when the relay restarts, its address, ports or certificate change,
   or it becomes unreachable (two failed checks in a row, `state: "down"`) or reachable again,
   every remembered client gets the new announcement in one message. Before the relay answered
   once there is nothing to announce; waiting clients get it as soon as it answers. After a bot
   restart the clients greet the bot again (it is a new joiner for them) and get it that way. The
   bot never sends a Mumcord hello itself.

2. **Token.** A client that wants a relayed connection to one other person asks the bot:

   ```json
   {"v":1,"t":"token-req","rid":"<16 hex>","peer":<session of the other person>}
   ```

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
   | `denied` | The requester has no certificate or sent no `p2p` hello, or asked for itself, the bot, a peer without certificate or a second session of the same identity; or the requester or the peer may not use the relay (a guest, or not in `allowed_groups`). The bot's debug log tells these apart; the client only knows this list of reasons. |
   | `peer-gone` | The peer is not on the server. |
   | `peer-not-mumcord` | The peer sent no hello with `p2p`. |
   | `rate-limited` | More than 6 requests from this session in a minute, more than `cert_requests_per_minute` (10) from all sessions with this certificate in a minute, more than `max_pairs_per_cert` (4) open pairs with this certificate, or the relay asked the bot to slow down. |
   | `relay-down` | The relay is down or did not answer. |
   | `relay-full` | The relay has no room for another session. |

   A request without a valid `rid` (16 lowercase hex digits) is ignored.

3. **Leaving.** When the last remembered session with a certificate leaves the server or changes
   certificate, the bot tells the relay to drop that certificate hash; the relay closes its
   sessions and pairs. A second device with the same certificate keeps its relay session while
   the other one leaves. A user who loses access (removed from an allowed group) is dropped the
   same way as soon as the bot reads the groups again.

## Who gets tokens

The relay carries traffic between the two users of a pair, so the bot only hands out tokens to
people the server knows. Both users of a pair must be allowed.

- **Default: registered users.** Unregistered guests never get a token, also not as the peer of
  someone else's request; they get `denied`. Certificates are free to create, a registration is
  not.
- **`allowed_groups`: only members of these Mumble groups.** The bot reads the groups from the
  server's ACL on the root channel (or on `groups_channel`). Groups only hold registered users,
  so guests stay out here too. Reading an ACL needs the **"Write ACL"** permission on that
  channel: the Mumble server answers an ACL query only to users who may edit it. When groups are
  configured and the bot cannot read them, it refuses to start; if reading fails later, nobody
  gets a token until it works again. The server never tells clients about changed group members,
  so the bot reads the groups again for every token request and once a minute; someone removed
  from a group loses open relay sessions within a minute.

Per certificate, the bot also allows at most `cert_requests_per_minute` (default 10) token
requests per minute across all sessions, and at most `max_pairs_per_cert` (default 4, the relay's
default sessions per certificate) open pairs. A pair stays open until one of its two
certificates is dropped; a new token for the same two users replaces the old one and does not
count twice. A reconnect gets a new session but not a new budget.

### Example: a `relay-users` group

Everything happens in the normal admin UI of the Mumble client, logged in as `SuperUser` or an
admin; nothing on the server is changed beyond its usual ACL configuration.

1. Register the bot and every person who should use the relay (Self → Register).
2. Create a channel just for this, for example `Relay` under Root, so the bot's "Write ACL"
   permission covers only that channel and not the whole server.
3. Right-click `Relay` → Edit → **Groups**: add the group `relay-users` and add the registered
   people as members.
4. Same dialog, **ACL** tab: add an entry for the bot's user (type its registered name), tick
   "Applies to this channel", and grant **Write ACL**. Apply.
5. In `bot.toml`:

   ```toml
   [peer]
   allowed_groups = ["relay-users"]
   groups_channel = "Relay"
   ```

Adding members later in the same dialog takes effect with the next token request; removing them
within a minute. Protect the
bot's certificate: with "Write ACL" it can change that channel's ACL.

The bot sends nothing to official clients and stays far below the server's plugin data rate
limit: one message per new Mumcord client, one per token request, one per relay change.

## Configuration

```toml
[server]
host = "mumble.example.org"
name = "RelayBot"
server_cert_sha256 = "<SHA-256 of the Mumble server certificate>"

[peer]
api_url = "http://127.0.0.1:7480"
api_secret_file = "/run/secrets/mumbot_peer_api_secret"
# api_fp = "<SHA-256 of the relay API certificate>"   # required for https
```

A Docker Compose setup with server, relay and bot is in
[`examples/peer-with-relay/`](../examples/peer-with-relay/). Every key is described in
[configuration.md](configuration.md#peer-the-peer-bot). There is no way
to announce a relay without the bot.

## Relay API

The bot calls the relay's API (HTTP/1.1, JSON) with `Authorization: Bearer <secret>`. Every call
times out after 5 seconds.

| Call | Answer | When |
| --- | --- | --- |
| `GET /v1/status` | `200 {"boot_id","host","udp","tcp","fp",…}` | Every 30 seconds; the first check right at start. A new `boot_id` means the relay restarted. |
| `POST /v1/pairs` with `{"a":"<cert hash>","b":"<cert hash>"}` | `201 {"tok","expires_in"}`, `503` relay full, `429` rate limited | For every accepted token request. |
| `DELETE /v1/peers/{cert hash}` | `204` | When the last remembered session with that certificate leaves or changes certificate, or loses access. |

Over `https` the relay's self-signed certificate is pinned with `api_fp` instead of being checked
against certificate authorities; TLS 1.3 only. Plain `http` is meant for a relay on the same host
(loopback); any other host over plain HTTP stops the bot at start, because the secret would
travel in clear text. `allow_insecure_http = true` allows it anyway (for example inside a private
container network) and logs a warning.

## Logs

`RUST_LOG=mumbot=info` shows when the bot connected, when the relay became reachable, changed or
went down, and failed API calls. No user names, sessions, certificate hashes, tokens or the secret
are logged.
