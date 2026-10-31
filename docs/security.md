# Security notes

What mumbot protects, what it does not (yet), and what an operator has to take care of.

## Summary

| Area | State |
| --- | --- |
| Connection to the Mumble server | Always TLS, with the bot's own client certificate. |
| Checking the server's certificate | Only with `server.server_cert_sha256` (required by the peer bot); without it the bot accepts any server certificate (see below). |
| HTTP API | Plain HTTP, loopback by default; API keys in a header, compared in constant time. |
| Secrets in files | `bot.toml`, `.env` and certificates are git-ignored; only placeholders in the repository. |
| Logs | Request metadata only: no keys, no passwords, no message texts. |
| Docker | Non-root user (uid 10001); the examples add a read-only root filesystem, no capabilities and `no-new-privileges`. |

## API keys

- A key is a password for posting into channels. Generate long random keys
  (`openssl rand -hex 32`), one per outside system, each with only the channels it needs.
- Keys live in `bot.toml`. Keep that file private: readable only by you and the bot (in Docker
  the container user, uid 10001, must be able to read the mounted file). Never commit it; the
  repository's `.gitignore` excludes every `bot.toml`.
- Store keys on the calling side as CI secrets or in a secret manager, never in a script in a
  repository.
- Keys are compared in constant time, so the answer time does not reveal them. The `Debug`
  output of the config hides them, and the log shows only the key's `label`.
- Rotating a key: add the new `[[keys]]` block, restart the bot, switch the caller over, then
  remove the old block and restart again. Keys are only read at startup.
- The rate limit (`rate_limit_per_minute`, default 20 per key) limits the damage of a leaked key,
  and the size limits (20 KB body, 5000 characters) cap every request.

## The HTTP listener

- The API speaks plain HTTP. Anyone who can watch the traffic sees the key.
- It binds to `127.0.0.1:8080` by default. Any other address must be set explicitly, and the bot
  then logs a warning at startup: `listening outside loopback over plain HTTP`.
- In Docker the bot has to listen on `0.0.0.0:8080` inside the container; the examples publish
  the port to `127.0.0.1` only, which keeps it off the network. The warning is expected there.
- To reach the bot from other machines, put a TLS reverse proxy (Caddy, nginx, Traefik) in front
  and keep the bot's own port on loopback. Built-in TLS and IP allowlists are candidate features
  ([roadmap](../specs/roadmap.md)).
- `GET /health` needs no key and reveals only the connection state, the uptime and the
  configured host name.

## Message content

- Text from callers is escaped before it reaches the chat; only a small Markdown subset becomes
  formatting, and only `http`/`https` links become links ([http-api.md](http-api.md#message-format)).
  A caller cannot inject HTML, scripts or remote images into the chat.
- Error answers carry a short reason only, never a channel list.

## The bot's client certificate (its identity)

- On first start the bot creates its own client certificate in its data folder
  (`server.data_dir`, else `MUMBOT_DATA`, else `$XDG_DATA_HOME/mumbot`; in Docker `/data`).
- The certificate is the bot's identity on the server. Registrations, groups and channel
  permissions are tied to it: lose it and the server sees a new, unknown user; leak it and
  someone else can log in as the bot.
- Keep the folder on a persistent volume (all examples do), back it up like a password, and never
  commit or share it. The repository ignores `*.pem`, `*.key`, `*.p12` and `*.pfx`.
- The certificate is never logged.

## TLS to the Mumble server

- The connection to the server is always TLS; there is no plaintext mode. Voice goes over UDP
  with the server's encryption, or through the TLS tunnel as fallback.
- **The server's certificate is only checked when you pin it** with `server.server_cert_sha256`
  (the SHA-256 of the certificate); then any other certificate is refused. The peer bot requires
  the pin. Without it the bot core, which comes from Mumcord's test bot and only talked to
  self-signed local test servers, accepts any server certificate without a warning. Someone who can intercept the network path between the bot and
  the server could impersonate the server and read what the bot sends.
- Without a pin, run the bot where that path is trusted: on the same host or private
  network as the server (as in [`examples/webhook-with-server`](../examples/webhook-with-server/)),
  or over a VPN. Pinning on first use, so no fingerprint has to be copied by hand, is
  planned ([roadmap](../specs/roadmap.md), "Server certificate check").

## Passwords

- The server's connect password should come from `SERVER_PASSWORD` in the environment (in the
  examples: `.env`, git-ignored) rather than from `bot.toml`.
- Channel passwords (`[[token]]`) and the SuperUser password are secrets as well: never logged,
  only in git-ignored files or CI variables.
- `superuser = true` and `--superuser` are for local test servers only. A production bot uses its
  certificate and the permissions an admin grants it.

## Test bot

- The test bot is for local or throwaway test servers. Never point it at public or third-party
  servers.
- In CI, generate the SuperUser password per run (`openssl rand -hex 16`) and remove the server
  with its volumes afterwards ([`examples/ci-test-bot`](../examples/ci-test-bot/)).
