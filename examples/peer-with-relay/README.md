# Example: peer bot with a mumpeer relay and its own Mumble server

Starts a standard Mumble server, a [mumpeer](https://github.com/Mumcord/mumpeer) relay and
mumbot's peer bot side by side. Mumcord clients that join the server learn about the relay from
the bot and can then use it for P2P features (screen sharing, full-size images) when a direct
connection fails. How the bot works: [docs/peer.md](../../docs/peer.md).

> **Status:** mumpeer is not released yet. `MUMPEER_IMAGE`, the relay's `command` and
> `mumpeer.example.toml` are placeholders until it is; check them against mumpeer's README then.

| Service | Image | Reachable at |
| --- | --- | --- |
| `mumble-server` | `mumblevoip/mumble-server:v1.5.735` (unmodified upstream image) | `127.0.0.1:64738` (Mumble client) |
| `mumpeer` | `ghcr.io/mumcord/mumpeer:0.1.0` (placeholder) | `443/udp` and `443/tcp` on `RELAY_BIND` (Mumcord clients); API on loopback inside the pair |
| `mumbot` | `ghcr.io/mumcord/mumbot:0.1.0`, command `peer` | Shares the relay's network namespace; no ports of its own |

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- `openssl` for the steps below.
- Port 64738 free on 127.0.0.1 and port 443 (UDP and TCP) free on `RELAY_BIND`.
- Two Mumcord clients to try a relayed connection.

## Steps

```bash
cd examples/peer-with-relay

# 1. Settings: random SuperUser password and relay API secret (never committed: .env is ignored).
cp .env.example .env
sed -i "s/^SUPERUSER_PASSWORD=.*/SUPERUSER_PASSWORD=$(openssl rand -hex 16)/" .env
sed -i "s/^PEER_API_SECRET=.*/PEER_API_SECRET=$(openssl rand -hex 32)/" .env

# 2. Configs. The container user of the bot (uid 10001) must be able to read bot.toml.
cp mumpeer.example.toml mumpeer.toml
cp bot.example.toml bot.toml
chmod 644 bot.toml

# 3. Start the Mumble server and the relay.
docker compose up -d --wait mumble-server
docker compose up -d mumpeer

# 4. Pin the Mumble server's certificate in bot.toml (the peer bot refuses to start without it).
PIN=$(docker compose exec mumble-server sh -c \
  'openssl s_client -connect 127.0.0.1:64738 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256' \
  | cut -d= -f2 | tr -d ':\r' | tr 'A-F' 'a-f')
sed -i "s/REPLACE-WITH-THE-SERVER-CERTIFICATE-SHA256/$PIN/" bot.toml

# 5. Register the bot once under its name and certificate: Mumcord only trusts a relay that a
#    registered user announces. The scripted test bot does this with the same identity.
docker compose run --rm mumbot --host mumble-server --name RelayBot register

# 6. Start the peer bot.
docker compose up -d mumbot
docker compose logs mumbot
```

## Checks

- `docker compose logs mumbot` shows `peer bot connected` and `peer: relay reachable`.
- Connect a Mumcord client to `127.0.0.1:64738`: its server info panel shows "Mumbot: Yes
  (RelayBot)" and "Mumpeer relay: Available". The packet inspector shows one `mumcord.peer`
  `relay` message from the bot.
- An official Mumble client on the same server receives nothing from the bot.

## Clean up

```bash
docker compose down -v   # also deletes the server database, the relay key and the bot identity
```

## Security notes

- The API secret lives only in `.env` and reaches both containers as an environment variable.
- The relay API listens on loopback inside the relay's network namespace, which the bot shares,
  so the secret never crosses a network.
- Changing `public_host` or deleting the relay's volume (a new key) makes clients ask whether to
  trust the changed relay.
