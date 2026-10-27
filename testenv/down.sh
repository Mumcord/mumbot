#!/bin/bash
# Usage: ./down.sh   (stops the test server; data is kept)
set -euo pipefail
cd "$(dirname "$0")"
. ./env.sh
export PUID="$(id -u)" PGID="$(id -g)" SUPERUSER_PASSWORD="${SUPERUSER_PASSWORD:-x}"
docker compose down --remove-orphans
