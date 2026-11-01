# CLI reference

Everything ships in one binary, `mumbot` (`mumbot.exe` on Windows; in Docker it is the
entrypoint). The test-bot subcommands and the load test are only in the **tester build**
(`mumbot-tester` archive or image, Cargo feature `tester`); the main build answers them with a
pointer to it and exit code 2 ([modules.md](modules.md#the-two-builds)). `mumbot --help` and `mumbot <subcommand> --help` print the same information.

```text
mumbot [OPTIONS] <COMMAND>
```

- `mumbot webhook` runs the long-lived webhook bot ([webhook.md](webhook.md)).
- `mumbot peer` runs the long-lived peer bot that announces a mumpeer relay ([peer.md](peer.md)).
- `mumbot run` runs several modules on one connection ([modules.md](modules.md)).
- Every other subcommand drives the scripted **test bot**: it connects, runs the subcommand (or a
  whole script), stays connected for `--hold` seconds and exits. Before it disconnects it waits
  until the server has received everything it sent, so a `say` or `pm` is never lost.
- **Exit code 0 means the step or check passed.** An error or a timeout prints the reason on
  stderr and exits with 1. What a subcommand prints on stdout is listed below.
- Only point the test bot at local or throwaway test servers, never at public or third-party
  servers.

## Global options

Given before the subcommand. `webhook`, `peer`, `music` and `run` ignore the connection options (their config
file holds the connection); only `--bitrate` and `--frame-ms` apply to them.

| Option | Default | Meaning |
| --- | --- | --- |
| `--host <HOST>` | `127.0.0.1` | Server address. |
| `--port <PORT>` | `64738` | Server port. This repository's test server listens on `44738`. |
| `--name <NAME>` | `Bot` | User name. |
| `--cert <CERT>` | the `--name` | Persistent certificate name; reuse it to keep the identity. |
| `--superuser` | off | Log in as `SuperUser`. Password: `SUPERUSER_PASSWORD` from the environment, else from `.env` in `MUMBOT_TESTENV_DIR`, `MUMCORD_TESTENV_DIR` or `./testenv`. |
| `--timeout <TIMEOUT>` | `10` | Seconds for the login and for each wait. |
| `--bitrate <BPS>` | `96000` | Opus bitrate of voice the bot sends (`play`), 6000 to 510000; also `MUMBOT_VOICE_BITRATE`. The server's bandwidth limit can lower it (a warning is logged). `webhook`, `peer`, `music` and `run` take it too and use it above their `[music]` and `[voice]` sections. |
| `--frame-ms <MS>` | `20` | Voice packet length, `10` or `20`; also `MUMBOT_VOICE_FRAME_MS`. |
| `--hold <HOLD>` | `0` | Seconds to stay connected after the command(s) before exiting. Not after a failed command. |
| `-h`, `--help` | | Print help. |
| `-V`, `--version` | | Print the version (`mumbot 0.4.1`). |

- Certificates are generated once per name and kept in `MUMBOT_DATA`, else `MUMCORD_BOT_DATA`,
  else `$XDG_DATA_HOME/mumbot` (usually `~/.local/share/mumbot`).
- The connect password, if the server has one, is `SERVER_PASSWORD` (same lookup as above).
- Channels are given by name (`Raid`) or path (`Root/Raid`). The root channel is `Root`.
- `on`/`off` and `true`/`false` values are literal words.

## `webhook`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot webhook --config <CONFIG>` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings and the API keys ([configuration.md](configuration.md)). |

Runs until stopped, reconnects after a dropped link and exits non-zero only when the config is
invalid, the listener cannot bind or the very first login fails.

## `peer`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot peer --config <CONFIG>` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings, `server.server_cert_sha256` and the `[peer]` section ([configuration.md](configuration.md#peer-the-peer-bot)). |

Runs until stopped, reconnects after a dropped link and exits non-zero only when the config or the
API secret is invalid or the very first login fails (also when the server certificate does not
match the pin).

## `music`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot music --config <CONFIG>` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings and the `[music]` section ([configuration.md](configuration.md#music-the-music-bot)). |

Runs the music API ([music.md](music.md)) until stopped, reconnects after a dropped link and exits
non-zero only when the config is invalid, the listener cannot bind or the very first login fails.
## `history`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot history --config <CONFIG>` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings and the `[history]` section ([configuration.md](configuration.md#history-the-history-bot)). |

Runs until stopped, reconnects after a dropped link and exits non-zero only when the config is
invalid, the database cannot be opened or the very first login fails. See [history.md](history.md).

## `plugins`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot plugins --config <CONFIG>` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings and the `[plugins]` section ([configuration.md](configuration.md#plugins-the-plugins-bot)). |

Runs until stopped and reconnects after a dropped link. Exits non-zero when the config is
invalid, the bot user is not registered or the very first login fails. See [plugins.md](plugins.md).

## `run`

| Usage | Option | Meaning |
| --- | --- | --- |
| `mumbot run --config <CONFIG> [--modules <MODULES>]` | `--config <CONFIG>` (env `MUMBOT_CONFIG`) | TOML config file with the server settings and the sections of the chosen modules. |
| | `--modules <MODULES>` (env `MUMBOT_MODULES`) | Comma-separated modules (`webhook`, `peer`, `history`, `music`, `plugins`); replaces `[modules] enabled` of the config. |

Logs in once and runs every chosen module on that connection. Runs until stopped, reconnects
after a dropped link and exits non-zero when no module is chosen, the config is invalid for the
chosen modules, the very first login fails or a module fails.

## Connection and identity

| Subcommand | Does | Prints |
| --- | --- | --- |
| `connect` | Connect only. | `session=<n> name=<name> cert_hash=<hash>` |
| `rename <NEW_NAME>` | Reconnect under a new name with the same certificate. | like `connect` |
| `register` | Self-register on the server under the current name. Also in the main build (the peer bot's setup needs it). | |
| `avatar [FILE]` | Set the avatar from an image file. | |
| `avatar --sample` | Set the built-in sample PNG as avatar. | |
| `avatar --clear` | Remove the avatar. | |
| `comment <HTML>` | Set the comment; `""` removes it. | |

## Channels and own state

| Subcommand | Does |
| --- | --- |
| `join <CHANNEL>` | Move into a channel. |
| `listen [CHANNELS]...` | Listen to (follow) channels without joining them. |
| `unlisten [CHANNELS]...` | Stop listening to channels. |
| `mute <on\|off>` | Self mute. |
| `deaf <on\|off>` | Self deafen. |

## Chat

| Subcommand | Does |
| --- | --- |
| `say [--channel <CHANNEL>] <HTML>` | Send HTML to a channel (default: the current one). |
| `say-sample [--channel <CHANNEL>] <SAMPLE>` | Send a canned message for formatting and sanitizer tests (samples below). |
| `pm <USER> <HTML>` | Send HTML privately to a user. |

| `<SAMPLE>` | Content |
| --- | --- |
| `bold` | Bold text. |
| `link` | A link. |
| `image` | An inline `data:` image. |
| `rich` | Bold text, a link and an inline image in one message. |
| `script` | A `<script>`; a client must show it without executing it. |
| `external-image` | A remote image; a client must show the message without loading it. |

## Admin steps (need `--superuser`)

| Subcommand | Does |
| --- | --- |
| `moderate <USER> [--mute <on\|off>] [--deaf <on\|off>]` | Server-mute or -deafen another user. |
| `move <USER> <CHANNEL>` | Move another user into a channel. |
| `deny-speak <CHANNEL>` | Deny "Speak" for everyone on a channel, which suppresses them. |
| `allow-speak <CHANNEL>` | Undo `deny-speak`. |
| `deny-listen <CHANNEL>` | Deny "Listen" for everyone on a channel. |
| `allow-listen <CHANNEL>` | Undo `deny-listen`. |
| `describe <CHANNEL> <HTML>` | Set a channel's description; `""` removes it. |

## Checks (`expect`)

Each check waits up to `--timeout` seconds and exits non-zero when the condition never holds.

| Subcommand | Waits until | Prints |
| --- | --- | --- |
| `expect user <NAME> [checks]` | another user matches all given checks | |
| `expect user-gone <NAME>` | no user with this name is connected | |
| `expect message [checks]` | a text message arrives (since this run connected) | the message HTML |
| `expect denied [--permission <PERMISSION>]` | a request of this bot is denied (since this run connected) | the denial kind |

Checks of `expect user` (all optional, all must hold):

| Option | Holds when |
| --- | --- |
| `--in <CHANNEL>` | the user is in this channel (name or `Root/Path/Name`) |
| `--muted <true\|false>` | the user is muted (self or by an admin), or not |
| `--deaf <true\|false>` | the user is deafened (self or by an admin), or not |
| `--listening <CHANNEL>` | the user listens to this channel (ear icon) |
| `--not-listening <CHANNEL>` | the user does not listen to this channel |
| `--registered <true\|false>` | the user is registered on the server, or not |
| `--comment-contains <TEXT>` | the user's comment contains this text |
| `--has-avatar <true\|false>` | the user has an avatar, or not |
| `--avatar-file <FILE>` | the avatar equals this file's bytes |
| `--cert-hash <HASH>` | the certificate hash (SHA-1 hex) matches |

Checks of `expect message`:

| Option | Holds when |
| --- | --- |
| `--from <USER>` | the sender has this user name |
| `--contains <TEXT>` | the message HTML contains this text |
| `--private <true\|false>` | it was sent privately to this bot (`true`) or to a channel (`false`) |
| `--channel <CHANNEL>` | it was sent to this channel |

`expect denied --permission` takes the missing permission: `write`, `traverse`, `enter`,
`speak`, `whisper`, `text`, `listen`, `register` or `self-register`.

## Voice

Real Opus voice (`mumcord-codec`, the codec settings of the Mumcord client), over UDP with the
TCP tunnel as fallback.

| Subcommand | Does | Prints |
| --- | --- | --- |
| `play <FILE> [--loop] [--seconds <S>] [--position <x,y,z>]` | Play a WAV file (any rate or channel count, converted to 48 kHz mono) as voice into the current channel in 20 ms packets (see `--frame-ms`) at `--bitrate`, then a terminator packet. `--loop` repeats until `--seconds` is over (forever without it); `--position` sends positional-audio coordinates with every packet. | `played <n> packets in <ms> ms` |
| `record <OUT> --seconds <S> [--from <USER>]` | Record received voice into a 48 kHz mono WAV: the sum of all speakers, or only `--from`, silence included. | `recorded <s> s to <path>` |
| `expect-audio [--from <USER>] [--min-rms <DBFS>] [--timeout <S>]` | Wait until voice reaches `--min-rms` (default `-40` dBFS) in a 100 ms window. Only audio after this command counts, so start it before the sender plays. `--timeout` defaults to the global one. | `<level dB> <latency ms>` |
| `game-context <CONTEXT> [--identity <ID>]` | Announce a Mumble Link game context, so people in the same game hear this bot's positional voice placed. `""` means no game; `--identity` defaults to `""`. | |

[`testenv/audio/speech.wav`](../testenv/audio/speech.wav) is a self-made, speech-like 6 s
reference clip (about -23 dBFS).

## Scripts

| Subcommand | Does | Prints |
| --- | --- | --- |
| `script` | Run subcommands from stdin, one per line, on one connection. Shell-like quoting; `#` starts a comment. | `ok <line>` after each line |
| `sleep <SECONDS>` | Wait (pacing in scripts). | |

```bash
# Is the client under test in "Raid" and unmuted?
mumbot --port 44738 --name Observer expect user Alice --in Raid --muted false

# One connection, several steps.
printf '%s\n' 'join Lobby' 'say "<b>hello</b>"' 'expect message --from Alice --contains hi' \
  | mumbot --port 44738 --name Bob script

# Voice: Ear waits for Mouth's voice; exit 0 when it arrived loud enough.
printf '%s\n' 'join Lobby' 'expect-audio --from Mouth --min-rms -50 --timeout 12' \
  | mumbot --port 44738 --name Ear script &
sleep 3
printf '%s\n' 'join Lobby' 'play testenv/audio/speech.wav' | mumbot --port 44738 --name Mouth script
wait $!
```

The bot disconnects as soon as its last step is done. When the last step sends something (a
message, a state change), end the script with `sleep 1` so the server receives it first.

## Load test

`examples/loadtest.rs` is a separate program, not a `mumbot` subcommand, and needs the tester
feature. It connects many users spread over the channels of the local load-test server (`testenv/scale/`), then adds churn
(moves, leaves and joins, chat) and talkers while the files given by `--churn-file` and
`--talk-file` exist.

```bash
testenv/scale/up.sh
mkdir -p /tmp/loadtest
cargo run --release --features tester --example loadtest -- --users 1000 \
  --churn-file /tmp/loadtest/churn --talk-file /tmp/loadtest/talk --cert-dir /tmp/loadtest/certs
touch /tmp/loadtest/churn          # switch churn on; rm switches it off
testenv/scale/down.sh
```

| Option | Default | Meaning |
| --- | --- | --- |
| `--port <PORT>` | `44850` | Port of the load-test server. |
| `--users <N>` | `1000` | Users to connect. |
| `--talkers <N>` | `6` | Users that speak in bursts (they join `--talk-channel`). |
| `--talk-channel <ID>` | `1` | Channel id of the talkers. |
| `--moves <N>` | `10.0` | Moves per second while churning. |
| `--joins <N>` | `2.0` | Leave and join pairs per second while churning. |
| `--chat <N>` | `1.0` | Chat messages per second while churning (half of them to `--talk-channel`). |
| `--connect-parallel <N>` | `50` | Logins in flight at the same time. |
| `--churn-file <PATH>` | required | Churn runs while this file exists. |
| `--talk-file <PATH>` | required | Talkers speak while this file exists. |
| `--cert-dir <PATH>` | required | Folder for the users' certificates. |

Background and measured numbers: [specs/00-test-bot-carried-over.md](../specs/00-test-bot-carried-over.md).
