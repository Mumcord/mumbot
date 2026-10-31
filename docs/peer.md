# Peer module (relay discovery)

The peer module connects a Mumble server to a [mumpeer](https://github.com/Mumcord/mumpeer)
relay ([Mumcord#62](https://github.com/Mumcord/mumcord/issues/62)). Two Mumcord clients that
cannot reach each other directly (both behind strict NAT, or UDP blocked) can then meet on the
relay instead. The bot only tells clients where the relay is and hands out short-lived tokens; it
never relays voice or data itself, and the Mumble server stays unmodified.

The module is off by default. It runs inside the webhook bot (`mumbot webhook --config bot.toml`)
when `bot.toml` has a `[peer]` section. A bot that only runs the peer module needs no
`[[keys]]`; the HTTP listener then only answers `GET /health`.

## How it works

Everything between the bot and the clients travels as plugin data (servers 1.4 and later), which
official Mumble clients ignore. Each message is JSON of at most 1000 bytes with data id
`mumcord.peer`.

1. **Announcement.** Every Mumcord client sends a hello (`mumcord.hello`) when it connects and to
   everyone who joins later, so also to the bot. The bot answers the first hello of each session,
   to that client only, with the relay addresses and the relay's certificate fingerprint:

   ```json
   {"v":1,"t":"relay","udp":["relay.example.org:3478"],"tcp":["relay.example.org:443"],
    "fp":"<SHA-256 of the relay certificate, 64 lowercase hex digits>","epoch":"<16 hex digits>"}
   ```

   The `epoch` changes when the bot starts and when the relay restarts; a client drops what it
   knew about the relay on a new epoch. After a relay restart every known Mumcord client gets the
   new announcement in one message. After a bot restart the clients greet the bot again (it is a
   new joiner for them) and get it that way.

2. **Token.** A client that wants a relayed connection to one other person asks the bot:

   ```json
   {"v":1,"t":"token_req","id":42,"peer":<session of the other person>}
   ```

   The bot registers a random token (32 bytes, hex) for exactly these two client certificates with
   the relay, then sends it to both people in one message:

   ```json
   {"v":1,"t":"token","id":42,"a":<requester session>,"b":<peer session>,"token":"…","exp":<Unix seconds>}
   ```

   Or it refuses, to the requester only: `{"v":1,"t":"deny","id":42,"reason":"…"}` with
   `unknown_peer` (not online, not a Mumcord client, or oneself), `no_cert` (one of the two has no
   client certificate), `rate_limited` (more than 6 requests per minute from one certificate) or
   `relay_unavailable` (the relay API did not take the token).

The requester is always the session the server stamped on the message, never a field of it.
Unknown message types, newer versions' extra fields and oversized messages are ignored. The bot
sends nothing to official clients and stays far below the server's plugin data rate limit: one
message per new Mumcord client, one per token request, one per relay restart.

**Allowlist.** The certificate hashes (SHA-1, as Mumble shows them) of everyone on the server form
the relay's allowlist. The bot pushes the full list after joins and leaves (collected for one
second), again after a relay restart, and retries every 10 seconds when the relay does not answer.
While the bot reconnects to the Mumble server, the relay keeps the last list.

## Configuration

```toml
[peer]
udp = ["relay.example.org:3478"]
tcp = ["relay.example.org:443"]
fingerprint = "<SHA-256 of the relay certificate>"
api_url = "http://127.0.0.1:8090"
secret_file = "/config/relay.secret"
token_ttl_secs = 120
```

Every key is described in [configuration.md](configuration.md#peer-relay-discovery-optional).
There is no way to announce a relay without the bot.

## Relay API

The bot calls the relay's management API with `Authorization: Bearer <secret>` and JSON bodies.
Every call times out after 5 seconds. Until mumpeer exists, the module is tested against a mock of
this API.

| Call | Body | Answer | When |
| --- | --- | --- | --- |
| `GET /v1/status` | | `200 {"instance":"…"}` | Every 30 seconds. A new `instance` means the relay restarted. |
| `PUT /v1/allowlist` | `{"hashes":["<cert hash>", …]}` | any `2xx` | After joins and leaves, and after a relay restart. Replaces the whole list. |
| `POST /v1/tokens` | `{"token":"…","a":"<cert hash>","b":"<cert hash>","expires_at":<Unix seconds>}` | any `2xx` | For every granted token request, before the token is sent to the clients. |

## Security

- The API secret is read from `secret_file` at startup. It is never written to the config, the
  log or the repository. The bot warns when the file is readable by other users.
- `api_url` over plain HTTP is meant for a relay on the same host. Any other host over plain HTTP
  logs a warning at startup, because the secret then travels in clear text; use `https`.
- Tokens are bound to the two certificates, expire after `token_ttl_secs` (2 minutes by default,
  at most 10) and are only sent to the two people they are for.
- No names, certificate hashes or tokens are logged; the log shows counts and failures only.

## Health and logs

`RUST_LOG=mumbot=info` shows when the relay API becomes reachable, each allowlist update (with
the number of users) and relay restarts. Failed API calls are warnings.
