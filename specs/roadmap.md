# Mumbot roadmap

Mumbot builds headless bots for Mumble. Every bot is an ordinary Mumble client: it logs in with its own certificate and works against any unmodified standard Mumble server (murmur / mumble-server). Nothing is built, required or changed on the server side; a bot's HTTP listener or other interface runs on the bot's side. No private protocol extensions.

Specs: `specs/{nr}-{name}-specification.md` per epic, created with the `/spec` skill and built with `/dev`.

## Epics

| Nr | Epic | Status | Spec |
| --- | --- | --- | --- |
| 0 | Test bot: the scripted `mumbot` CLI (chat, state, ACL helpers, `expect` checks, voice play/record/level checks) and the load generator, used by Mumcord's E2E and smoke tests | Built (carried over from Mumcord's `crates/bot`) | `specs/00-test-bot-carried-over.md` |
| 1 | Webhook bot: messages from outside systems into a channel | Built (carried over from Mumcord, PR #51); packaging added in the move addendum | `specs/01-webhook-specification.md` |
| 2 | Peer bot (`mumbot peer`, #8): announces a mumpeer relay to Mumcord clients and fetches pair tokens from its API | Built (PR #17) | Mumcord `specs/04-mumpeer-relay-specification.md` (FR-4.22 to FR-4.28, sections 6.4 and 6.6) |
| 3 | Modular bot and two release builds (#5, #16): `mumbot run` with several modules on one connection; main build without the test bot, `mumbot-tester` with it | In progress | `specs/03-modules-specification.md` |
| 4 | History module (#2): records the chat of chosen channels and answers Mumcord's history sync (mumcord FR-1.60) for clients that were offline | Built | `specs/04-history-specification.md` |

## Candidate epics (not specified yet)

Taken from the open and deferred points of Epic 1 (`specs/01-webhook-specification.md` §2.2 and §14, `specs/01-webhook-brainstorming.md`):

- **Interactive bot** that reads chat and answers, e.g. commands (Q1.1).
- **Text-to-speech or alert sounds** in a channel (Q1.4). The audio path (Opus encoding, voice transmission) now exists in the test bot (`src/voice/`); the webhook bot deliberately leaves it out.
- **Multi-server bot**: one process serving several servers, or several bot identities (Q1.6).
- **Richer delivery**: whisper/private messages, a message to a whole channel tree (Q1.3); per-request sender label as a message prefix (Q3.3, dropped from the MVP).
- **Operations**: runtime key rotation without restart (Q2.5), a key generation command (Q2.4), built-in TLS for the HTTP side (Q2.8), IP allowlists (Q2.9), request queueing (Q3.5), file logging and a verbose mode (Q3.7), config formats other than TOML (Q3.4), a systemd unit (Q3.8; the Docker image now exists).
- **Server certificate check**: the bot core still accepts any server certificate (it came from Mumcord's test bot, which only talked to self-signed local test servers). A product bot should pin the server certificate on first use and refuse a changed one, in line with the TLS rule in `CLAUDE.md`. Needs a spec decision (pin storage, how an operator accepts a new certificate).

## Open technical to-dos

- Done: `mumcord-codec` comes from crates.io (`"0.4"`, first published version 0.4.2) instead of a git dependency pinned to a commit of Mumcord PR #61.
- Done: `mumcord-protocol` comes from crates.io (`"0.4"`, first published version 0.4.2) instead of a git dependency.

## Carried over from Mumcord

Bot-related requirements, decisions and ideas found in the Mumcord client specs (repository `Mumcord/mumcord`, state of `main` on 2026-10-27). Neither Mumcord spec plans bots as a product; these are the constraints and hooks a Mumbot epic has to respect or could use.

### Constraints

- **Client only, standard servers.** Protocol weaknesses that need server changes are out of scope forever (mumcord `specs/01-foundation-specification.md:583`). Mumcord-only features use plugin data only, never protocol extensions (mumcord `specs/02-multi-server-themes-specification.md:277`). Both apply to every Mumbot bot.
- **Channel passwords** are Mumble access tokens presented at login (mumcord FR-2.78); the webhook bot uses the same mechanism (FR-1.22).
- **Chat HTML**: Mumcord renders official-client-compatible HTML (bold, links, images; mumcord AC-1.13) and sanitizes all server HTML. Bot messages must stay within that subset to look the same in Mumcord and the official client.

### Ideas a bot could build on

- **Notifications**: Mumcord shows toasts for new messages in all received channels and PMs (mumcord QA.16), highlights and notifies on `@Name` mentions (mumcord AC-1.41) and on quote replies (mumcord AC-2.48), and keeps several servers connected with notifications from all (mumcord `specs/02-multi-server-themes-brainstorming.md` Q1.4). A bot that mentions people reaches them even in background servers.
- **Link cards**: link previews and YouTube/Twitch/Spotify cards render from plain chat links (mumcord AC-1.39 to AC-1.48), so a CI or monitoring bot can post rich links without any extension.
- **Plugin-data features** (Mumcord-to-Mumcord, carried by any standard server): history sync, where Mumcord asks other clients for channel history (mumcord FR-1.60), suggesting a history/archive bot; full-size image fetch from the sender (mumcord FR-1.60c); "Now playing" with title, artist, cover and progress (mumcord FR-1.58, QA.40), suggesting a music bot that publishes its track; "Now gaming" status (mumcord FR-1.59); watch-party commands, shared playlist and host announce (mumcord FR-2.39, FR-2.40, FR-2.69, FR-2.75), suggesting a playlist host or announcer bot; the P2P layer with plugin-data signalling and certificate-hash authentication (mumcord FR-2.35, FR-2.36). The list of plugin-data uses per server is in mumcord `specs/02-multi-server-themes-specification.md:257`. Using any of these needs the message formats from Mumcord's code and a spec decision on compatibility when Mumcord changes them.
- **No TTS in Mumcord**: Mumcord itself never uses text-to-speech (mumcord Q3.8 of Epic 1, AC-1.24), so TTS stays a bot-only idea (see Q1.4 above).

### Test bot

Mumcord's scripted test bot (`crates/bot` without the webhook part: `expect`, voice checks, ACL helpers, samples, the load test) moved into Mumbot as well; Mumcord's E2E and smoke tests will download the `mumbot` release binary. Its requirements, voice-check decisions, scale notes, tasks and acceptance criteria are in `specs/00-test-bot-carried-over.md`.
