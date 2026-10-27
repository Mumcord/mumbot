# Test bot and load generator carried over from Mumcord

Date: 2026-10-27
Source: Mumcord `origin/main` (repository `Mumcord/mumcord`), state of 2026-10-27.

## Purpose

Mumcord, the desktop Mumble client, had a scripted test bot in `crates/bot` (CLI `mumcord-bot`). Mumcord's E2E tests, live tests and cross-OS smoke tests use it as a second participant on a local Mumble server. The bot has now moved into Mumbot as the binary `mumbot`, with the same CLI. This file collects everything bot-related from the Mumcord specs, so that Mumbot owns these requirements from now on. The Mumcord spec files are unchanged and still contain the original text.

The webhook bot (Mumcord Epic 3) is not part of this file. It is already carried over in `specs/01-webhook-specification.md` and `specs/roadmap.md`.

Mumcord files used, all quoted as "mumcord" followed by the path:

- `specs/01-foundation-specification.md` (spec 01)
- `specs/01-foundation-brainstorming.md` (brainstorming 01)
- `specs/01-foundation-implementation.md` (implementation 01)
- `specs/01-foundation-tasks.md` (tasks 01)
- `specs/01-foundation-scale-report.md` (scale report)
- `specs/02-multi-server-themes-specification.md` (spec 02)
- `specs/02-multi-server-themes-brainstorming.md` (brainstorming 02)
- `specs/02-multi-server-themes-tasks.md` (tasks 02)

Where the sources use the old names, `mumcord-bot` now reads `mumbot` and "Bot" (capitalised) is the scripted client in the acceptance criteria.

Mumbot CLI subcommands referred to below: connect, join, say, say-sample, pm, rename, avatar (--sample/--clear), comment, register, listen, unlisten, mute, deaf, moderate (--mute/--deaf), move, deny-speak, allow-speak, deny-listen, allow-listen, describe, expect user|user-gone|message|denied, play (--loop --seconds --position), game-context, record, expect-audio, sleep, script. Global flags: --host --port --name --cert --superuser --timeout --hold. Library-only helpers also exist (ACL deny-text/require-token, voice `BotVoice`).

---

## 1. Test-bot requirements and decisions

Each item has an id (R-n) for this file and the exact Mumcord source.

- **R-1 Automated tests with scripted bots (decision).** Mumcord is tested with automated tests against a local Mumble server (Docker) with scripted bot users, plus UI tests. Only sound quality needs a human ear. The alternative "unit tests plus manual checks" was rejected.
  Source: mumcord spec 01 decision Q4.8 (line 469); brainstorming 01 Q4.8 (line 430).
- **R-2 Test server setup (requirement).** A reproducible local Mumble server (Docker, official `mumble-server` image, versions 1.4 and 1.5) plus scripted bot clients for multi-user tests, with a documented one-command start.
  Source: mumcord spec 01 section 10, "Test server setup" (line 353).
- **R-3 The "Bot" as a test actor (requirement).** Unless stated otherwise, acceptance criteria assume a fresh install, a local 1.5 test server and, where needed, a bot or second client called "Bot" that is connected. Every acceptance criterion must be verifiable in the running client against a local test server.
  Source: mumcord spec 01 section 11 intro (line 357); mumcord CLAUDE.md "Way of working".
- **R-4 Multi-user tests in Epic 2 (requirement).** Integration tests use two or more local test servers (Docker, 1.4 and 1.5) and several Mumcord instances or bots for multi-server, direct connection, watch-party and screen-share signalling flows.
  Source: mumcord spec 02 section on test setup, "Multi-user tests" (line 364).
- **R-5 Extra test servers (task result).** Test infrastructure for Epic 2 added a third server v15b (port 64742), multi-server E2E helpers, and a configurable port base and compose project.
  Source: mumcord tasks 02 B1.
- **R-6 Bot control library (task result, contract).** `Bot::connect(BotConfig::new(name, port))` and `BotConfig::superuser(port)`. Actions: join, send_channel, send_private, set_mute, set_deaf, listen, unlisten, set_avatar, set_comment, register, `reconnect_as` (keeps the certificate), deny_listen and allow_listen (SuperUser, server 1.4 or newer). Waits: wait_user(name, &UserCheck), wait_user_gone, wait_message(&MessageCheck), wait_denied. `samples::Sample` provides canned messages: bold, link, image, rich, script, external image. Voice hook: `bot.handle()` and `bot.subscribe()`. Raw protocol access: `Handle::send_acl(proto::Acl)`.
  Source: mumcord tasks 01 notes section, B10.
- **R-7 Bot CLI contract (task result).** `mumcord-bot [--port --name --cert --superuser --timeout --hold] <connect|join|say|say-sample|pm|rename|avatar|comment|register|listen|unlisten|mute on/off|deaf on/off|deny-listen|allow-listen|expect user|user-gone|message|denied|sleep|script>`. Exit code 0 means pass. The bot leaves after the command unless `--hold` (seconds) keeps it connected.
  Source: mumcord tasks 01 B10 notes; implementation 01 line 59.
- **R-8 Later CLI additions (task results).** `describe` for channel descriptions and the welcome message (pinned info); bot moderation commands (server mute/deafen, suppress) for the muted-icon display.
  Source: mumcord tasks 01 F65 and F81.
- **R-9 Bot identities (task result).** Bot certificates live in `$MUMCORD_BOT_DATA` or `~/.local/share/mumcord-bot/certs/<name>/`. A stable certificate per name makes the bot the same registered identity across runs (needed for friends and for registration tests).
  Source: mumcord tasks 01 B10 notes; implementation 01 line 121 (`MUMCORD_BOT_DATA`).
- **R-10 Rename and reconnect (known limitation and workaround).** A bot rename keeps its old voice connection, so persistence tests that need a "new" connection use `reconnect_as` (a full reconnect with the same certificate). `BotVoice` is invalid after `reconnect_as` and must be started again.
  Source: mumcord tasks 01 C3 and C3b; B11 notes.
- **R-11 E2E harness usage (task result).** Helpers `bot([...cmd], {name, port, cert, superuser, timeout, hold})` throw unless the exit code is 0; `botInBackground(...)` offers send, done and stop. `bot say` needs `hold: 1` so the message is delivered before the bot leaves. The sample name for an external image is `external-image`.
  Source: mumcord tasks 01 B27 notes and C4.
- **R-12 Test server kit (task result, environment for the bot).** `testenv/up.sh <v15|v14|v13|limit>`; ports v15 64738, v14 64739, v13 64740, limit 64741 (TCP and UDP). The `limit` server has listener limits 0. SuperUser password comes from a git-ignored env file (never in the repo). Channels Lobby, Raid and AFK are seeded. Autoban is disabled on all test servers because tests reconnect often; `reset.sh` restores a fresh state and must never run while another test is using the server.
  Source: mumcord tasks 01 B2, B2b and notes.
- **R-13 Listen refusal (note).** Listen refusal (AC-1.16) is verified only through the bot and E2E against the `limit` server; Mumble 1.3 is untested live.
  Source: mumcord implementation 01 line 109.
- **R-14 Smoke test (requirement).** The cross-OS smoke test starts Mumcord with `mumble://127.0.0.1:64738/`, waits for the log line `connected server_id=`, and the bot must see the user `SmokeTester`. Connecting works without audio devices.
  Source: mumcord tasks 01 B29 notes; spec 01 AC-1.30.
- **R-15 Limit of the test bot (note).** The AC-1.51 history sync between Mumcord clients cannot be tested with the test bot (it does not speak the Mumcord plugin-data protocol). It is covered by a live engine test instead.
  Source: mumcord tasks 01 B91.
- **R-16 Talking indicator caveat (note).** The F28 report that everyone appears to be talking could not be reproduced; probably other QA bots were talking on the shared v15 server. Tests must not share a server with outside users (flakes from an outside user on shared v15 were seen).
  Source: mumcord implementation 01 line 112; tasks 01 F28, C3b.
- **R-17 Manual-run recipe (note).** To get a second participant by hand: `cargo run -p mumcord-bot -- --port 64739 --name Bot --hold 600 join Raid`. The bot is also used in manual checks with `expect-audio`, listening or recording, and for join/leave and private-message sounds.
  Source: mumcord implementation 01 lines 59, 136, 140, 172, 174, 175.
- **R-18 Sounds with bots in AFK (task result).** The "sounds-close" E2E uses bots in AFK on server v14.
  Source: mumcord tasks 01 F95.
- **R-19 Network emulation (adjacent test tool, not part of the bot).** A packet-loss proxy `netsim` sits between the client and the server for AC-1.21 to AC-1.23; control commands: loss, latency, jitter, block-udp, kill-tcp, reset, status. AC-2.19 asks for "network emulation in the test setup" for a bot. Default ports collide with the v14 and v13 servers, so always pass explicit ports. Mumbot should decide whether bot connections can be routed through such a proxy or whether it stays in Mumcord.
  Source: mumcord tasks 01 B12 and notes; implementation 01 line 122; spec 02 AC-2.19.
- **R-20 Test-only audio backend (note).** `MUMCORD_AUDIO_BACKEND=null|file:<in.wav>:<out.wav>` exists on the client side (test builds only) so the bot's audio can be fed into and read from the client. Other test variables: `MUMCORD_LIVE=1` for live tests.
  Source: mumcord implementation 01 line 121.

## 2. Voice-check decisions

- **V-1 Voice bot API (task result).** `BotVoice::start(&Bot)`; `play(&Clip, PlayOptions{repeat, forever, max_duration})` and `play_file`; `session_of`, `speakers`, `is_talking`, `wait_talking`, `talked_between(session, from, to, min_db)`, `latency_since`, `wait_audio`, `record_to_wav`; `rms_db`, `levels_db`, `active_duration`, `Clip::load/save`.
  Source: mumcord tasks 01 B11 notes.
- **V-2 Voice CLI (task result).** `play <wav> [--loop] [--seconds N]`; `record <out.wav> --seconds N [--from name]`; `expect-audio [--from name] [--min-rms dBFS, default -40] [--timeout S]`, which prints `<dB> <latency ms>`. Record and expect must be started before the sender plays.
  Source: mumcord tasks 01 B11 notes.
- **V-3 Test clips (task result).** `testenv/audio/speech.wav` and `speech_noisy.wav`, 6 s, CC0, synthetic. The noisy clip contains keyboard and fan noise and is the "reference recording" of AC-1.11.
  Source: mumcord tasks 01 B11 notes; spec 01 AC-1.11.
- **V-4 Talking latency (requirement).** When the Bot talks, the talking indicator lights up for the Bot within 200 ms (measured result: 74 to 85 ms). The bot needs a way to measure and report latency (`latency_since`, the latency value of `expect-audio`).
  Source: mumcord spec 01 AC-1.7; tasks 01 C3.
- **V-5 Level check of noise suppression (requirement).** After an explicit Join (also via a `mumble://` link with a channel path), the Bot plays the noisy reference recording to the client's input; the audio the client transmits is recorded by another bot and must have the noise strongly reduced. This is an automated level check plus a manual listening check.
  Result: speech-to-noise ratio +21 dB, noise floor -18 dB in pauses (an earlier run on a truncated recording gave +9.1 dB and was judged unreliable). The recording `e2e/artifacts/voice/noise-rec.wav` is kept for manual listening; manual pass criteria: speech clear, noise between words quiet, speech not cut off.
  Source: mumcord spec 01 AC-1.11; tasks 01 C3 and C3b; implementation 01 lines 171 and 172.
- **V-6 Silence and activation checks (requirement).** Voice activation: speaking makes the Bot receive audio, silence sends nothing. Push-to-talk (with another app focused, mouse side button, X11 compatibility mode): the Bot hears the user only while the key or button is held. Self mute and deafen: the Bot receives no audio. After a fresh connect the user is muted and deafened and the Bot hears nothing. The bot needs "expect audio" and "expect no audio within a time" checks.
  Source: mumcord spec 01 AC-1.9, AC-1.10, AC-1.35, AC-1.37, AC-1.38; implementation 01 lines 140 and 153.
- **V-7 File audio backend and thresholds (requirement).** With the file audio backend, a quiet test tone and a loud test tone are fed to the client; with a manual input threshold between the two levels, the Bot hears only the loud tone; with Automatic the previous behaviour returns. The bot must be able to measure the received level per tone (RMS in dBFS).
  Source: mumcord spec 01 AC-1.47; implementation 01 line 121.
- **V-8 Positional audio via Mumble Link (requirement).** Two Bots feed Mumble Link positions (left and right of the listener); the client's output must come from the matching side and get quieter with distance (automated channel-level check; manual listening check too). A Bot sending positional voice packets must show the "3D" icon on its entry, and the icon disappears a few seconds after positional data stops. A fake Mumble Link game also delivers positions for the own entry.
  Source: mumcord spec 01 AC-1.31, AC-1.55 (QA.70); tasks 01 C9 (done, automated; manual listening open).
- **V-9 Talking overlay (requirement).** The overlay shows the talking Bot's avatar and name while it talks; a second Bot talking in another channel never appears (QA.60).
  Source: mumcord spec 01 AC-1.33.
- **V-10 Notification sounds (requirement).** Bot joining or leaving the channel plays a subtle sound (measured: 240 ms chime, -18.4 dBFS peak; private message sound -30.5 dBFS). No text-to-speech at any time. Manual check of subtlety with `e2e/artifacts/c7-audio/speaker-out.wav`.
  Source: mumcord spec 01 AC-1.24; tasks 01 C4, C7; implementation 01 line 174.
- **V-11 Packet loss (requirement).** With the Bot talking and 10 % simulated loss, audio continues with at most brief dropouts (below about a third of a second); the bitrate shown drops and returns after the loss is removed. Needs a continuous talker (`play --loop`).
  Source: mumcord spec 01 AC-1.23; implementation 01 line 173; tasks 01 F20.
- **V-12 Talking-first and auto-ducking (requirement).** A talking bot moves to the top of its channel; a talking bot ducks watch-party or screen-share video to about 30 % and the volume returns about 1 s after it stops.
  Source: mumcord spec 02 AC-2.18, AC-2.57.
- **V-13 Device switching (manual).** The bot talks continuously while the user changes the output or input device; no lasting breaks and no regular clicks every 2 seconds.
  Source: mumcord implementation 01 line 175.
- **V-14 Mumble Link game context (requirement).** A fake Link context containing a server address for a Steam game makes "Join game" appear and open `steam://connect/<address>`; a second Mumcord instance sees the game icon and profile data.
  Source: mumcord spec 01 AC-1.50 (maps to `game-context`).

## 3. Load-test and scale notes

Source for all items: mumcord `specs/01-foundation-scale-report.md` (sections as named), unless stated otherwise.

- **S-1 Question and result.** How does the client behave with 1000 connected users, about 300 channels nested up to 5 levels, rooms with 100+ people, a few simultaneous talkers, constant joins, leaves and moves, and chat? Result: the Rust engine is fine (about 3 % of one core, 18 MiB); the UI was the bottleneck (fixed in Mumcord, see S-12).
- **S-2 Load server (section 1).** Unmodified `mumblevoip/mumble-server:v1.5.735` in its own compose project (`mumcord-scale`), port 64850, data in `testenv/data/scale/`, user limit raised to 1500 (test infrastructure only). Start `testenv/scale/up.sh`, stop `testenv/scale/down.sh`. Test machine: 16 cores, 31 GiB RAM, Linux.
- **S-3 Channel tree (section 1).** 304 channels plus root, seeded by `testenv/scale/seed_channels.py`: Welcome Lobby, AFK, Event Stage, Gaming (12 games, each with Casual, Ranked, Squads, Squad 1 to 4, Team Red and Blue at level 5), Clans (15 clans, each Lounge and Ops with Alpha to Charlie), Community, Music, Study Hall, Support.
- **S-4 Load generator (section 1, 6).** `crates/bot/examples/loadtest.rs` (example `loadtest` of the bot crate). Starts 1000 users without certificates (connected in about 41 to 59 s, 50 at a time) plus 6 talkers with certificates. Distribution: Welcome Lobby 140, AFK 110, Event Stage 120, one game room 60, the rest spread over about 110 channels with a long tail; about 190 channels empty. Options used: `--users 1000 --churn-file <path> --talk-file <path> --cert-dir <path>`.
- **S-5 Churn and talk phases (section 1, 6).** Both are switched on and off by the presence of a file (`touch` to start, `rm` to stop). Churn: 10 moves per second, 2 leaves plus 2 joins per second, 1 chat message per second (half of them in the probe's channel). Talking: 6 speakers in the probe's channel, bursts of 1 to 4 s with pauses of 0.5 to 3 s.
- **S-6 Engine probe (section 1).** `crates/engine/examples/scale_probe.rs` (client engine with null audio backend): connects, joins Welcome Lobby and logs CPU, memory and events per second every 5 s. This stays a Mumcord tool; it is listed because it is the measuring side of the load generator.
- **S-7 Engine numbers (section 2.1).** Connect to synced: 91 to 182 ms on a quiet server, 1.5 to 7.1 s while the server is busy. Memory 10 MiB before connect, 17 to 18 MiB steady. CPU 2 to 4 % idle, 3 to 5 % talking or churn. Events to the UI about 38 to 40 per second under churn plus talk. Peak burst about 314 events within 100 ms after sync (one per permission answer). Talking events arrive only for the own and followed channels, so talking load does not depend on server size.
- **S-8 Server limits (section 2.1, 4).** Murmur used 5 to 10 % CPU idle and 60 to 80 % of one core under churn. In the first churn run the server's control thread stalled for about 30 s and dropped about 430 clients with "Timeout". This is a limit of the server and test machine, not of the client; a big server can be laggy itself, so the client must show a "syncing" state and tolerate a slow sync. A load generator must therefore connect in batches (50 at a time) and keep churn rates moderate.
- **S-9 UI measurements setup (section 1, 2.2).** Production UI build with the mock engine and the opt-in seed `stress` (`VITE_MUMCORD_MOCK=1 VITE_MUMCORD_MOCK_SEED=stress`), 1000 people, headless Chromium 127 at 1440x900, 1x and 4x CPU throttling. Not done: the real Tauri app against the load server, and the IPC hop. These stay Mumcord measurements.
- **S-10 Regression-check recommendation (section 5, recommendation 3).** Keep `testenv/scale/up.sh`, the `loadtest` and `scale_probe` examples and the `stress` seed as a regression check. Targets after fixes: at least 50 fps under the measured churn at 1x and at least 30 fps at 4x, and no long task above 200 ms after connect.
- **S-11 Reproduction recipe (section 6).** Start the load server (`up.sh`), run `loadtest --release` with `--users 1000` and churn/talk/cert directories, `touch` the talk and churn files, run the engine probe against port 64850, stop with `down.sh`. Cargo builds of this are large; use a separate target directory and delete it when done.
- **S-12 Results after fixes (section 7, tasks 01 F77).** After batched UI updates, one permission event, memoised rows and lazy avatars: 60 fps under churn at 1x (was 17 to 19), 34 to 41 fps at 4x (was 3 to 6); the 13 s freeze after connect is gone; avatar requests on connect 2 (was 98 to 307). Remaining: first render at 4x about 617 to 817 ms. The load setup was reused to prove this.
- **S-13 Epic 2 requirement tied to the load server.** FR-2.24: first drawing of a big server does not freeze the window longer than 200 ms at the 4x scale of the Epic 1 scale test. AC-2.17: connect to the 4x scale test server, "Syncing big server..." shows, no freeze over 200 ms (measured as in the scale report). A 4x scale server means about 4000 users and about 1200 channels, so the generator must support a user count and channel seed that large.
  Source: mumcord spec 02 FR-2.24, AC-2.17; tasks 02 B12 (done: switch shipped 50 to 73 ms, dev 137 to 193 ms, first connect 103 to 138 ms) and F8.
- **S-14 Decision on big-server improvements.** Performance fixes need no product decision (QA.27); the PM chose: calmer talking-ring updates, "Syncing big server..." state, hide or dim empty channels, talking people on top of their channel.
  Source: mumcord brainstorming 01 QA.27 (line 870); brainstorming 02 Q1.14 (line 184).
- **S-15 Scale check task.** F62, status "done": engine fine (0.1 to 7 s sync, 18 MiB, 3 to 5 % CPU), UI bottleneck found; files named: testenv load profile, tools/loadtest or bot, UI mock stress data.
  Source: mumcord tasks 01 F62.

## 4. Bot-related tasks (status as given in the Mumcord task files)

Tasks 01 = mumcord `specs/01-foundation-tasks.md`; tasks 02 = mumcord `specs/02-multi-server-themes-tasks.md`.

| Task | What | Status as given | Source |
|---|---|---|---|
| T-B10 | Bot harness (control): `crates/bot` except voice | done | tasks 01 B10 |
| T-B11 | Bot voice: `crates/bot/src/voice/`, `testenv/audio/` | done | tasks 01 B11 |
| T-B12 | Netsim proxy (adjacent test tool, AC-1.21 to 1.23 infra) | done | tasks 01 B12 |
| T-B2 | Test server kit (`testenv/`) | done | tasks 01 B2 |
| T-B2b | Autoban off in all testenv servers, reset helper for test users | done | tasks 01 B2b |
| T-B27 | E2E harness using the bot (Q4.8, all ACs infra) | done | tasks 01 B27 |
| T-B29 | Packaging and cross-OS smoke (bot sees `SmokeTester`) | done | tasks 01 B29 |
| T-C3 | QA of AC-1.7 to 1.12 with the bot (AC-1.9 PTT and AC-1.12 persistence were unverified at that time; AC-1.11 SNR +9.1 dB on a truncated recording) | done, partly open then | tasks 01 C3 |
| T-C3b | Re-verify AC-1.8, 1.9 PTT, 1.11 (full clip), 1.12 persistence (bot reconnect workaround) | done (all passing) | tasks 01 C3b |
| T-C4 | QA of chat ACs 1.13 to 1.18 with bot messages (AC-1.18 only the misclick failed) | done | tasks 01 C4 |
| T-C7 | Sounds and app behaviour incl. AC-1.24 chime with bot join/leave | done | tasks 01 C7 |
| T-C9 | Verify positional audio (AC-1.31, 1.32, 1.55 automated; listening manual) | done | tasks 01 C9 |
| T-F28 | Everyone shows as talking (likely QA bots talking on v15) | not reproduced, regression tests added | tasks 01 F28 |
| T-F62 | Scale check: 1000 users, 300 channels | done | tasks 01 F62 |
| T-F65 | Pinned info; bot `describe` command | done | tasks 01 F65 |
| T-F77 | Scale performance fixes | done | tasks 01 F77 |
| T-F81 | Muted-icon display; bot moderation commands | done | tasks 01 F81 |
| T-F95 | Sounds-close E2E with bots in AFK | done | tasks 01 F95 |
| T-B91 | History sync; AC-1.51 e2e not possible with the test bot | done, covered by live engine test | tasks 01 B91 |
| T-Q1 | Finalise E2E suite (18/18 after harness fixes) | done | tasks 01 Q1 |
| T-E2-B1 | Test infrastructure: 3rd server v15b (64742), multi-server E2E helpers | done (smoke E2E after F1) | tasks 02 B1 |
| T-E2-B12 | Big servers (batched talking, syncing state, hide empty, talking first, 4x scale) | done | tasks 02 B12 |
| T-E2-F8 | "Syncing big server..." visible during the real sync | done (stress-mock check pending E2E) | tasks 02 F8 |
| T-E2-SS13 | Screen share: full AC-2.29 live test with bot viewers and CPU report | done (AC-2.29 pass; sharer 28 to 29 % of a core) | tasks 02 SS13 |

## 5. Acceptance criteria where the Bot is a test actor

Capability column uses the Mumbot subcommands. "Library/other" means no subcommand fits today. Negative audio checks ("hears nothing") need `expect-audio` to be usable as an expected-absence check; Mumbot should settle that semantics.

| AC id | What the bot does | Bot capability / subcommand needed |
|---|---|---|
| AC-1.6 | Waits for the user joining "Raid" via a `mumble://` link and checks the user is not deafened | `expect user` |
| AC-1.7 | Sits in the channel and talks; sees all channels and users; talking indicator within 200 ms | `join`, `play`, `expect user` |
| AC-1.8 | Sees the user appear in "Raid" after a double-click | `expect user` |
| AC-1.9 | Receives audio when the user speaks, none in silence or with push-to-talk released | `expect-audio` (also negative), `record` |
| AC-1.10 | Sees the user's mute and deafen state, receives no audio | `expect user`, `expect-audio` |
| AC-1.11 | Plays the noisy reference recording into the client's input; a second bot records what the client sends | `play`, `record`, `expect-audio` |
| AC-1.12 | Renames itself and reconnects (same certificate) so volume and local mute persist | `rename`, `connect --cert`, `join` |
| AC-1.13 | Sends a channel message with bold text, a link, a small image; also a `<script>` message and an external image | `say-sample`, `say` |
| AC-1.14 | Receives the user's Markdown (as official-client HTML) and pasted image | `expect message` |
| AC-1.15 | Writes in "Raid"; sees the ear icon (listener) for the user; checks the user hears nothing from the followed channel | `join`, `say`, `expect user`, `expect-audio` |
| AC-1.16 | With listening denied or the listener limit reached, refuses the listen request; also an older-server check | `deny-listen`, `allow-listen`, `--superuser`, `expect denied` |
| AC-1.18 | Sends a private message; receives the reply | `pm`, `expect message` |
| AC-1.19 | Sees the user as registered after registering | `expect user` |
| AC-1.20 | Sees the user's avatar and comment; sets its own avatar and comment | `avatar`, `comment`, `expect user` |
| AC-1.24 | Joins and leaves the channel to trigger the sound | `join`, `sleep`, `script` |
| AC-1.30 | In the cross-OS smoke test, sees user `SmokeTester` | `expect user` |
| AC-1.31 | Two bots feed positions (left and right of the listener); client output side and level are checked | `play --position` (two bots, record at client side) |
| AC-1.33 | One bot talks; a second bot talks in another channel | `play`, `join` |
| AC-1.35 | Sees the user present and deafened after a fresh connect, hears nothing; later sees the restored state and hears the user | `expect user`, `expect-audio` |
| AC-1.36 | Sees the user's default avatar, or none after removal or with the toggle off | `expect user` |
| AC-1.37 | Hears the user only while the mouse side button is held | `expect-audio`, `record` |
| AC-1.38 | Hears the user only while the key or button is held (X11 compatibility mode) | `expect-audio`, `record` |
| AC-1.39 | Posts a web link | `say` |
| AC-1.40 | Posts a YouTube and a Twitch link | `say` |
| AC-1.41 | Mentions the user with `@Name` | `say` |
| AC-1.42 | Posts a GIF link; receives the GIF link the user sends | `say`, `expect message` |
| AC-1.43 | Receives the code block as plain text | `expect message` |
| AC-1.45 | Sends an external image and a direct .png link | `say-sample external-image`, `say` |
| AC-1.46 | Sends two images | `say-sample` / `say` |
| AC-1.47 | Hears only the loud test tone above a manual threshold (file audio backend) | `expect-audio --min-rms`, `record` |
| AC-1.48 | Posts a Spotify track link | `say` |
| AC-1.49 | A second Mumcord instance or bot sees the music icon and the track in the profile (plugin data) | library/other |
| AC-1.50 | A fake Link context with a server address for a Steam game; a second instance sees the game icon | `game-context` |
| AC-1.51 (b, c, d) | Another Mumcord client takes part in history sync; the test bot cannot (covered by a live engine test) | library/other |
| AC-1.53 | Is the target of double-click, pinned card, profile, volume and note | `join` |
| AC-1.54 | Is added as a friend; disconnects and reconnects with another name but the same certificate | `join`, `rename`, `connect --cert`, `expect user-gone` |
| AC-1.55 | Sends positional voice packets so the "3D" icon appears, then stops | `play --position` |
| AC-2.17 | Fills the 4x scale server with users and channels | library/other (load generator, see section 3) |
| AC-2.18 | A talking bot moves to the top of its channel | `play`, `join` |
| AC-2.19 | A Mumcord bot, as seen by a SuperUser or admin viewer, shows "Mumcord <version>" and its OS; a bad network connection is simulated for the bot | library/other (client version and OS reporting, network emulation) |
| AC-2.29 | Up to 8 bot viewers watch a screen share; the 9th gets "This share is full" | library/other (screen-share watcher; done in SS13) |
| AC-2.39 | A test bot listens to a channel without joining it while a second bot is in it; stops listening | `listen`, `unlisten`, `join` |
| AC-2.41 | Receives the reply message and sees the quote as readable text | `expect message` |
| AC-2.48 | Replies in Mumble quote format to a message of the user, or to someone else's message | `say` (quote HTML), `expect message` |
| AC-2.57 | A bot talks during a watch party or screen share (ducking) | `play` |
| AC-2.58 | A bot starts a watch party, a second bot joins and leaves, a bot starts and stops a screen share or watches the user's share (activity sounds) | library/other (plugin-data signalling) |

Not bot-driven but related: AC-1.21, AC-1.22 and AC-1.23 use the netsim proxy and a server restart (AC-1.23 also lets the Bot talk, see V-11).

---

## Counts

Carried over: 20 requirements/decisions (R-1 to R-20), 14 voice-check decisions (V-1 to V-14), 15 scale and load-test notes (S-1 to S-15), 24 bot-related tasks, and 46 acceptance-criteria rows (of which 6 marked library/other).
