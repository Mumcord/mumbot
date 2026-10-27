#!/bin/bash
# Usage: ./reset.sh   (deletes the test server's data and starts it fresh)
set -euo pipefail
cd "$(dirname "$0")"
. ./env.sh
./down.sh
rm -rf "${DATA_ROOT:?}/v15"
./up.sh
