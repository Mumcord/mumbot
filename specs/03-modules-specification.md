# Epic 3 – Modular bot and two release builds · Specification

> Status: Final · As of: 2026-10-31
> Sources: issues [#5](https://github.com/Mumcord/mumbot/issues/5) (modular bot: choose modules via config file or CLI) and [#16](https://github.com/Mumcord/mumbot/issues/16) (release two builds: the main bot without the tester, and a tester build)
> Builds on: `specs/00-test-bot-carried-over.md` (test bot), `specs/01-webhook-specification.md` (webhook bot), the peer bot (Mumcord `specs/04-mumpeer-relay-specification.md`, merged in #17)

## 1. Goal & context

Mumbot ships two production bots (webhook, peer) and the scripted test bot in one binary and one Docker image. Production users carry test code (voice analysis, samples, the Opus codec) they never use, and every bot kind has its own command with its own connection, so a server that wants both the webhook and the peer bot runs two processes and two bot users.

This epic turns the production bots into **modules** of one bot that can run alone or together on one Mumble connection, and splits every release into two **variants**: the main bot without any tester code, and a tester build with everything.

## 2. Scope

### 2.1 In scope

- Production modules `webhook` and `peer`, chosen on the command line or in the config file.
- Several modules in one process share one Mumble connection (one bot user).
- A Cargo feature `tester` (off by default) that holds all test-bot code; the main build compiles none of it.
- The release builds both variants for every target, with distinct archive names, and two Docker images.
- CI builds and tests both feature sets.

### 2.2 Out of scope / later

- The music (#3) and history (#2) modules. The module list is ready for them (one more entry plus its config section and task); a module that needs its own Mumble user (music occupying a voice channel) is a decision for its own spec.
- Regrouping the test commands under `mumbot test <area> …` (see Q4 in §5).
- One shared HTTP listener with a route prefix per module: only the webhook module has a listener today.
- An environment variable per config field.

## 3. Functional requirements

### 3.1 Modules and commands

- **FR-3.1** Modules are `webhook` (today's webhook bot) and `peer` (today's peer bot).
- **FR-3.2** `mumbot webhook --config bot.toml` and `mumbot peer --config bot.toml` run one module, exactly as before.
- **FR-3.3** `mumbot run --config bot.toml` runs the modules listed in the config file:
  ```toml
  [modules]
  enabled = ["webhook", "peer"]
  ```
  `mumbot run --modules webhook,peer` (or the environment variable `MUMBOT_MODULES=webhook,peer`) chooses them on the command line and overrides the config list. A single-module command (`webhook`, `peer`) also overrides it. A repeated module counts once. No module chosen is an error with a message that names both ways to choose.
- **FR-3.4** All modules of one process share one Mumble connection: one bot user, one certificate, one reconnect loop. The process ends when any module fails.
- **FR-3.5** The config is validated for the chosen modules only: `webhook` needs at least one API key; `peer` needs the `[peer]` section and `server.server_cert_sha256`. `server.server_cert_sha256` stays rejected when `webhook` runs without `peer`; with `peer` the pin protects the shared connection, so both modules use it.
- **FR-3.6** Existing configs keep working unchanged: `[modules]` is optional, and the Docker image's default command stays `webhook`.

### 3.2 Variants

- **FR-3.7** The Cargo feature `tester` (off by default) holds the scripted test bot (every test subcommand, `script`, `expect` and voice checks), the voice module, the chat and image samples, the ACL helpers, the user and message checks, and the load generator example. The WAV crate is an optional dependency of that feature (the Opus codec was too, until the music module needed it in the main build).
- **FR-3.8** The tester build keeps every test subcommand at the top level with unchanged flags, output and exit codes (Mumcord's E2E and smoke tests rely on them).
- **FR-3.9** In the main build, any other command than the production ones prints a clear message that test commands need the tester build (the `mumbot-tester` archive or Docker image) and exits with code 2.
- **FR-3.10** `register` stays in the main build: the peer bot's setup registers the bot once with its own identity (`mumbot --name RelayBot register`), and that is not a test.

### 3.3 Release and CI

- **FR-3.11** Every release target (linux-x86_64, linux-aarch64, windows-x86_64, macos-universal) ships `mumbot-<version>-<target>` (main) and `mumbot-tester-<version>-<target>` (tester). Both archives hold a binary named `mumbot` (`mumbot.exe` on Windows), so scripts call it the same way. `SHA256SUMS` lists all eight archives.
- **FR-3.12** Two multi-arch Docker images: `ghcr.io/mumcord/mumbot:<version>` (main) and `ghcr.io/mumcord/mumbot-tester:<version>` (tester), each with `latest` for plain versions. The Dockerfile takes a build argument `FEATURES` (empty or `tester`).
- **FR-3.13** CI runs format, clippy and the unit tests for both feature sets, the live tests with the tester feature (a superset of the main build's code), the Docker health check for both images, and the static linux-aarch64 build of the tester variant (the one Mumcord's smoke tests download).

## 4. Acceptance criteria (verifiable against `testenv/`)

- **AC-3.1** `mumbot run --modules webhook,peer --config bot.toml` logs in once; the webhook API delivers a message and a Mumcord-style client receives the relay announcement from the same bot user.
- **AC-3.2** `mumbot run --config bot.toml` with `[modules] enabled = ["webhook"]` behaves like `mumbot webhook --config bot.toml`.
- **AC-3.3** `mumbot run --config bot.toml` without `[modules]` and without `--modules` exits with an error naming `--modules` and `[modules]`.
- **AC-3.4** An existing webhook config without `[modules]` still works with `mumbot webhook`.
- **AC-3.5** A main build answers `mumbot say hi` with the tester-build message and exit code 2; `mumbot webhook`, `mumbot peer`, `mumbot run` and `mumbot register` still parse.
- **AC-3.6** A tester build accepts every test subcommand Mumcord's E2E uses, unchanged.
- **AC-3.7** `cargo tree` of the main build contains no `hound` (`mumcord-codec` joined the main build with the music module, `specs/04-music-specification.md` Q11).

## 5. Decision log

| # | Question | Decision | Why |
| --- | --- | --- | --- |
| Q1 | One binary with all modules, or slim builds per module? | One binary per variant with all production modules; only the tester is a Cargo feature. | The production modules are small; one image covers every production use. |
| Q2 | Shared Mumble connection or one user per module? | Shared, one bot user per process. | One identity to register and pin; a second process gives a second user when someone needs one. |
| Q3 | Module list in the config: top-level `modules = […]` (issue) or a section? | `[modules] enabled = […]`. | Room for later module-wide settings without another top-level key. |
| Q4 | `mumbot test <area> …` grouping (issue proposal)? | Not now: the tester build keeps the flat commands, which Mumcord's E2E calls. | Two spellings of every command would double the CLI to maintain; the variant split already separates test from production. |
| Q5 | Docker: tag suffix or a second image? | Second image `ghcr.io/mumcord/mumbot-tester`. | `latest` and version tags work the same way for both. |
| Q6 | Name of the binary inside the tester archive? | `mumbot`, as in the main archive. | Mumcord's scripts only change the archive name. |
| Q7 | Is `register` a test command? | No, it stays in the main build. | The peer bot setup needs it once. |

## 6. Impact on Mumcord

Mumcord's E2E and smoke tests (`scripts/mumbot.env`, `scripts/fetch-mumbot.sh`) must download `mumbot-tester-<version>-<target>` instead of `mumbot-<version>-<target>` from the first release with this change, with the new hashes. The binary name and every command stay the same.
