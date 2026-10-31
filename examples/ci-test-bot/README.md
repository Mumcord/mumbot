# Example: the test bot in a CI pipeline

A throwaway standard Mumble server plus mumbot's scripted test bot, for automated tests of a
Mumble client, a bot or a server setup. Each test step is one command; **exit code 0 means the
step or check passed**, so any CI system can use it.

```bash
docker compose run --rm -T bot --name Observer expect user Alice --in Root --muted false
```

| File | What it is |
| --- | --- |
| `docker-compose.yml` | `mumble-server` (unmodified `mumblevoip/mumble-server:v1.5.735`) and `bot` (`ghcr.io/mumcord/mumbot-tester`, the tester build with the test bot, one run per step) |
| `.env.example` | the SuperUser password for admin steps and the server port |
| `smoke-test.sh` | a complete run: two bots chat, an observer checks a user, an admin step as SuperUser |
| `github-actions.yml` | the same setup as a GitHub Actions workflow to copy into your project |

## Prerequisites

- Docker with the Compose plugin (`docker compose version` prints 2.20 or newer, for `--wait`).
- `openssl` to generate the password. Nothing else: the bot comes as an image.
- Only point the bot at throwaway or local test servers, never at public or third-party ones.

## Steps

```bash
cd examples/ci-test-bot
printf 'SUPERUSER_PASSWORD=%s\n' "$(openssl rand -hex 16)" > .env

./smoke-test.sh            # starts the server, runs the steps, removes everything
# <b>hello</b> from CI
# all test-bot steps passed
```

By hand, step by step:

```bash
docker compose up -d --wait mumble-server        # server on 127.0.0.1:64738

# A single step. The bot connects, checks, disconnects and sets the exit code.
docker compose run --rm -T bot --name Observer --timeout 20 expect user MyClient --in Root

# Several steps on one connection: one subcommand per line on stdin.
printf '%s\n' 'say "<b>hello</b>"' 'expect message --from MyClient --contains hi' 'sleep 1' \
  | docker compose run --rm -T bot --name Bob script

# Admin steps log in as SuperUser with the password from .env.
printf '%s\n' 'deny-listen Root' 'sleep 1' | docker compose run --rm -T bot --superuser script

docker compose --profile bot down -v              # remove server, bot runs and volumes
```

- `--host` and `--port` are set by the compose file; give the other connection options
  (`--name`, `--cert`, `--timeout`, `--superuser`) before the subcommand.
- The client under test connects from the CI machine to `127.0.0.1:64738` (port in `.env`).
- A step that waits (`expect …`) must start before the action it waits for: start it in the
  background (`&`), trigger the action, then `wait` for it.
- End a script with `sleep 1`: the bot disconnects as soon as its steps are done, and the pause
  lets the server take the last step first.
- Every subcommand and check: [docs/cli.md](../../docs/cli.md).

## Check that it works

```bash
docker compose ps mumble-server                  # "healthy" = accepts TLS connections
docker compose run --rm -T bot --name Probe connect
# session=1 name=Probe cert_hash=…   (exit code 0)
docker compose logs mumble-server | tail         # "Authenticated" and "Connection closed" per step
```

## Identity and cleanup

- The `bot-data` volume keeps one client certificate per `--name` (or `--cert`), so a name keeps
  the same identity in every step of a run. That matters for `register` and certificate checks.
- `docker compose --profile bot down -v` deletes the server and the volume: the next run starts
  from a fresh server. Nothing restarts by itself (`restart: "no"`); the run is throwaway.
- The bot has no HTTP port here, so there is no `/health`: the server's health check gates
  every step, and each step's exit code is its result.

## Common problems

| Symptom | Cause and fix |
| --- | --- |
| `SUPERUSER_PASSWORD` error | `.env` is missing; create it as shown above. |
| `timed out after 10s waiting for …` | The condition never held: check names and channels, raise `--timeout`, or start the `expect` before the action. |
| A message or change from the last step never arrives | End the script with `sleep 1` (see above). |
| `port is already allocated` | Another server uses 64738: set `MUMBLE_PORT` in `.env`. |
