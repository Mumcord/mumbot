#!/usr/bin/env bash
# Installs apt packages on a GitHub Ubuntu runner without letting a stalled mirror hold the job.
# Usage: scripts/ci/apt-install.sh [--cache-dir DIR] [--purge PATTERN]... PACKAGE...
#
# The runners' default Azure mirror regularly stalls for tens of minutes. Every connection gets a
# short timeout, every attempt a hard deadline, and each failed attempt switches the sources to
# the next mirror before trying again.
set -uo pipefail

cache_dir=""
purge=()
while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --cache-dir) cache_dir="$2"; shift 2 ;;
    --purge) purge+=("$2"); shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ $# -gt 0 ]] || { echo "no packages given" >&2; exit 2; }

if [[ "$(dpkg --print-architecture)" == amd64 ]]; then
  mirrors=(http://archive.ubuntu.com/ubuntu http://mirrors.edge.kernel.org/ubuntu)
else
  mirrors=(http://ports.ubuntu.com/ubuntu-ports http://mirrors.edge.kernel.org/ubuntu-ports)
fi

sudo tee /etc/apt/apt.conf.d/99ci-fast >/dev/null <<'CONF'
Acquire::Retries "3";
Acquire::http::Timeout "15";
Acquire::https::Timeout "15";
Acquire::ForceIPv4 "true";
CONF

# Points every Ubuntu archive entry (deb822 ubuntu.sources on 24.04, sources.list before that)
# at the given mirror. The security suites keep their own host.
use_mirror() {
  echo "apt: using mirror $1"
  sudo sed -i -E "s#(https?://)([a-z0-9.-]*\.)?(archive|ports)\.ubuntu\.com/ubuntu(-ports)?/?#$1/#; s#(https?://)mirrors\.edge\.kernel\.org/ubuntu(-ports)?/?#$1/#" \
    /etc/apt/sources.list.d/ubuntu.sources /etc/apt/sources.list 2>/dev/null || true
}

install_args=(-y --no-install-recommends)
if [[ -n "$cache_dir" ]]; then
  mkdir -p "$cache_dir/partial"
  install_args+=(-o "Dir::Cache::Archives=$cache_dir")
fi

attempt() {
  sudo timeout 180 apt-get update &&
    sudo timeout 480 apt-get install "${install_args[@]}" "$@"
}

try=0
until attempt "$@"; do
  if (( try >= ${#mirrors[@]} )); then
    echo "apt: all mirrors failed" >&2
    exit 1
  fi
  echo "apt: attempt $((try + 1)) failed or timed out, switching mirror" >&2
  sudo pkill -x apt-get 2>/dev/null || true
  sudo rm -f /var/lib/apt/lists/lock /var/cache/apt/archives/lock
  sudo dpkg --configure -a >/dev/null 2>&1 || true
  use_mirror "${mirrors[$try]}"
  try=$((try + 1))
done

if (( ${#purge[@]} )); then
  sudo apt-get purge -y "${purge[@]}" || true
fi
