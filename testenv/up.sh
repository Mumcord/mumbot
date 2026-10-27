#!/bin/bash
# Usage: ./up.sh   (starts the local test server; data survives restarts)
set -euo pipefail
cd "$(dirname "$0")"
. ./env.sh
svc=v15
[ -f .env ] || { echo "missing testenv/.env - run: cp testenv/.env.example testenv/.env" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is required on the host to seed channels" >&2; exit 1; }
export PUID="$(id -u)" PGID="$(id -g)"
./gen-certs.sh
mkdir -p "$DATA_ROOT/$svc"

docker compose up -d --force-recreate "$svc"

if [ ! -f "$DATA_ROOT/$svc/.seeded" ]; then
  db="$DATA_ROOT/$svc/mumble-server.sqlite"
  # The schema appears early but is rolled back if the container is stopped mid-initialisation
  # (slow runner). The root channel row is written once the server has booted its virtual server.
  ready=0
  for _ in $(seq 120); do
    if [ -f "$db" ] && [ "$(sqlite3 "$db" "select count(*) from channels where channel_id = 0" 2>/dev/null)" = 1 ]; then
      ready=1; break
    fi
    sleep 1
  done
  [ "$ready" = 1 ] || { echo "the test server did not initialise its database within 120s" >&2; docker compose logs --tail 20 "$svc" >&2; exit 1; }
  # The server only reads channels at startup, so seed while it is stopped.
  docker compose stop "$svc" >/dev/null
  sqlite3 "$db" "insert or ignore into channels (server_id, channel_id, parent_id, name, inheritacl) values
    (1,1,0,'Lobby',1),(1,2,0,'Raid',1),(1,3,0,'AFK',1);"
  touch "$DATA_ROOT/$svc/.seeded"
  docker compose up -d "$svc"
fi
# An open port is not enough: right after a (re)start the server resets connections until its
# virtual server is up, so wait for a complete TLS handshake.
for _ in $(seq 60); do
  if openssl s_client -connect "127.0.0.1:$PORT_V15" </dev/null >/dev/null 2>&1; then
    echo "test server up on port $PORT_V15"
    exit 0
  fi
  sleep 1
done
echo "the test server did not accept TLS connections within 60s" >&2
docker compose logs --tail 20 "$svc" >&2
exit 1
