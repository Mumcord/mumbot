#!/bin/bash
# Stops the load-test server; data is kept in testenv/data/scale.
set -euo pipefail
cd "$(dirname "$0")"
export PUID="$(id -u)" PGID="$(id -g)" PORT_SCALE=$((${MUMBOT_PORT_BASE:-44738} + 112))
SUPERUSER_PASSWORD="${SUPERUSER_PASSWORD:-x}" docker compose -p mumbot-scale -f docker-compose.yml down
