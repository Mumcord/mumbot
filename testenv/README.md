# Mumbot test server

One local, unmodified Mumble 1.5 server (`mumblevoip/mumble-server`) for the live tests and manual checks. Server configuration only; nothing here changes Mumble itself, and the bot needs nothing beyond a standard server.
Requires: Docker (with compose), `openssl`, `sqlite3` on the host.

## Setup (once)

    cp testenv/.env.example testenv/.env   # then edit the passwords; .env is git-ignored

## Start / stop / reset

    testenv/up.sh      # starts the server on port 44738 (TCP+UDP)
    testenv/down.sh    # stops it; data is kept
    testenv/reset.sh   # deletes its data and starts it fresh

Superuser name is `SuperUser`, password is `SUPERUSER_PASSWORD` from `.env`. `SERVER_PASSWORD` (optional) is the connect password; leave it empty for an open server.

On the first start `up.sh` writes the channels Lobby (id 1), Raid (id 2) and AFK (id 3) under Root directly into the server's SQLite database while the server is stopped, then restarts it. State lives in `testenv/data/` (git-ignored). Autoban is disabled, so tests can reconnect as often as they like. The self-signed server certificate is generated once into `testenv/certs/` (git-ignored, never committed).

## Live tests

    testenv/up.sh
    MUMBOT_LIVE=1 cargo test -- --ignored --test-threads=1

## Parallel instances

    MUMBOT_PORT_BASE=44838 MUMBOT_PROJECT=qa1 testenv/up.sh
    MUMBOT_PORT_BASE=44838 MUMBOT_LIVE=1 cargo test -- --ignored --test-threads=1
    MUMBOT_PROJECT=qa1 testenv/down.sh

- `MUMBOT_PORT_BASE` (default 44738): host port of the server; the live tests read the same variable.
- `MUMBOT_PROJECT` (default `mumbot-testenv`): compose project name; a non-default project keeps its data in `testenv/data/_<project>/`.

Never point the tests at a public or third-party Mumble server.
