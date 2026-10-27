#!/usr/bin/env bash
# A small end-to-end check with the test bot: two bots chat through a throwaway server, and an
# admin step runs as SuperUser. Exits 0 when every step passed. Needs .env (see .env.example).
set -euo pipefail
cd "$(dirname "$0")"

run() { docker compose run --rm -T bot "$@"; }
# Runs several steps on one connection. The trailing sleep keeps the connection open until the
# server has taken the last step, because the bot disconnects as soon as its steps are done.
steps() { local name=$1; shift; printf '%s\n' "$@" 'sleep 1' | run --name "$name" script >/dev/null; }
cleanup() { docker compose --profile bot down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker compose up -d --wait mumble-server

# Alice stays online for a while; Observer waits until it sees her, unmuted, in the root channel.
steps Alice 'sleep 20' &
alice=$!
run --name Observer --timeout 15 expect user Alice --in Root --muted false

# Ear waits for a message from Mouth; it must be listening before Mouth speaks.
run --name Ear --timeout 20 expect message --from Mouth --contains hello &
ear=$!
sleep 3
steps Mouth 'say "<b>hello</b> from CI"'
wait "$ear"

# An admin step as SuperUser: set the root channel's description, then remove it again.
printf '%s\n' 'describe Root "<b>CI run</b>"' 'describe Root ""' 'sleep 1' \
  | run --superuser script >/dev/null

wait "$alice"
echo "all test-bot steps passed"
