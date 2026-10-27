# Sourced by up.sh, down.sh and reset.sh. Derives compose variables from two optional overrides:
#   MUMBOT_PORT_BASE   host port of the test server (default 44738, away from a real server's
#                      64738 and from Mumcord's own test servers)
#   MUMBOT_PROJECT     compose project name (default mumbot-testenv); a non-default name also gets
#                      its own data dir (data/_<name>, git-ignored) so parallel instances never
#                      share databases
# The Rust live tests read the same MUMBOT_PORT_BASE.
MUMBOT_PORT_BASE="${MUMBOT_PORT_BASE:-44738}"
MUMBOT_PROJECT="${MUMBOT_PROJECT:-mumbot-testenv}"
export COMPOSE_PROJECT_NAME="$MUMBOT_PROJECT"
export PORT_V15=$((MUMBOT_PORT_BASE))
if [ "$MUMBOT_PROJECT" = mumbot-testenv ]; then DATA_ROOT=data; else DATA_ROOT="data/_$MUMBOT_PROJECT"; fi
export DATA_ROOT
