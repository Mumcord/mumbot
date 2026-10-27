#!/bin/bash
# Starts the load-test server on port MUMBOT_PORT_BASE + 112 (default 44850) and, on first start,
# seeds ~300 nested channels.
set -euo pipefail
cd "$(dirname "$0")"
[ -f ../.env ] || { echo "missing testenv/.env - run: cp testenv/.env.example testenv/.env" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is required on the host to seed channels" >&2; exit 1; }
set -a; . ../.env; set +a
export PUID="$(id -u)" PGID="$(id -g)" PORT_SCALE=$((${MUMBOT_PORT_BASE:-44738} + 112))
../gen-certs.sh
mkdir -p ../data/scale
compose=(docker compose -p mumbot-scale -f docker-compose.yml)
"${compose[@]}" up -d
db=../data/scale/mumble-server.sqlite
if [ ! -f ../data/scale/.seeded ]; then
  for _ in $(seq 120); do
    [ -f "$db" ] && [ "$(sqlite3 "$db" "select count(*) from channels where channel_id = 0" 2>/dev/null)" = 1 ] && break
    sleep 1
  done
  "${compose[@]}" stop >/dev/null
  python3 seed_channels.py | sqlite3 "$db"
  touch ../data/scale/.seeded
  "${compose[@]}" up -d
fi
echo "scale server up on port $PORT_SCALE ($(sqlite3 "$db" 'select count(*) from channels') channels)"
