# Plugins bot

Mumcord clients can run plugins (dice rollers, polls, pets …). A server community usually settles
on a few of them, and newcomers should not have to ask which ones. The plugins module tells them:
it sits on the server as an ordinary Mumble client and announces up to 8 plugins to every
Mumcord client that joins. Mumcord shows them as a suggestion in its notification centre
(Install, Details, Not now, Don't ask again for this server). Spec: Mumcord
`specs/06-plugin-system-specification.md` (FR-6.77 to FR-6.79), issue mumbot#20.

The bot installs nothing, downloads nothing and never learns what users do with the suggestion:
clients send nothing back. Nothing is installed or changed on the server.

## Run it

```toml
[server]
host = "mumble.example.org"
name = "Plugins"
server_cert_sha256 = "…"   # required, see below

[[plugins.announce]]
id = "dice"
min = "1.0.0"
source = "official"
for = "board game nights"

[[plugins.announce]]
id = "guild-calendar"
min = "0.9.0"
source = "https://plugins.nightowls.example/v1/index.json"
key = "RWQ…"               # the repository's minisign public key
for = "raid sign-ups"
```

```bash
mumbot plugins --config bot.toml                        # alone
mumbot run --config bot.toml --modules plugins,history  # with other modules, one bot user
```

- **Register the bot** on the Mumble server (a registered user; an admin does it once, or
  `mumbot register`). Mumcord only accepts suggestions from a registered user and pins that user
  per server on first use; the module refuses to start for an unregistered bot.
- **Pin the server certificate** with `server.server_cert_sha256`, so the bot never announces
  anything on a server it did not mean to join. The module refuses to start without it.
- The server must be Mumble 1.4 or newer (plugin data).
- Every key is described in [configuration.md](configuration.md#plugins-the-plugins-bot).

## What is announced

Each `[[plugins.announce]]` entry becomes one suggestion:

| Key | Meaning | Rules |
|---|---|---|
| `id` | The plugin id from its manifest. | 3 to 32 characters: lowercase letters, digits, `-`, starting with a letter. No id twice. |
| `min` | The lowest version the community uses. | `MAJOR.MINOR.PATCH`, e.g. `1.2.0`. |
| `source` | Where Mumcord finds the plugin. | `official` (Mumcord's own repository) or the `https://` address of a third-party repository's `index.json`. |
| `key` | The third-party repository's minisign public key. Mumcord fills it in as "Expected key" when the user adds the repository. | Required for a third-party source, not allowed for `official`. |
| `for` | What the community uses the plugin for, shown with the suggestion. | 1 to 40 characters. |

Every suggestion is "recommended"; there are no required plugins. 1 to 8 entries; the whole
message must fit into one plugin data message (1000 bytes), so very long addresses or `for`
texts can make a list of 8 too large. All of this is checked at start: an invalid list stops the
bot with a message that names the entry.

## How clients get it

- A Mumcord client says hello when it joins (or when the bot joins). If that hello lists the
  feature `server-plugins`, the bot sends the list as plugin data `mumcord.srvplugins` to that
  client only: `{"v":1,"seq":…,"plugins":[{"id","min","src","key"?,"for"}]}`. Official Mumble
  clients and older Mumcord versions get nothing.
- At most once a minute per client. Answers to several clients go out together in one message
  per second, which leaves the server's plugin data limit (4 per second) to the other modules.
- **Changing the list:** edit the config and restart the bot. `seq` grows by one whenever the
  list differs from the last one the bot announced (kept in `plugins-seq.json` in the data
  directory, next to the bot's certificate), and every client greets the restarted bot again and
  gets the new list. Mumcord shows a suggestion the user put off with "Not now" again once `seq`
  changes.

## Privacy

The bot logs only counts and errors: no user names, no certificate hashes, no message contents.
It keeps nothing about clients beyond the current connection.
