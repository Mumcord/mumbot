//! TOML config for the webhook bot: connection settings plus the API keys.
//!
//! Keys are secrets: the real config file stays git-ignored, the repository only holds an
//! example with a placeholder. The `Debug` impl of `ApiKey` hides the key value so a key can
//! never end up in a log line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::Module;
use crate::history::HistoryConfig;
use crate::music::MusicConfig;
use crate::peer::PeerConfig;
use crate::plugins::PluginsConfig;
use crate::voice_settings::VoiceConfig;
use crate::webhook::{DELIVERY_TIMEOUT, RATE_LIMIT_PER_MINUTE};

/// Default listener: loopback only, must be changed explicitly to serve a LAN.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

/// The bot's config file: the shared server settings plus one section per module. Named after
/// the webhook bot it started with.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    /// Which modules `mumbot run` starts; a command line choice replaces the list.
    #[serde(default)]
    pub modules: ModulesConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub keys: Vec<ApiKey>,
    /// Channel passwords, presented to the server at login (mumcord FR-2.78, spec addendum FR-1.22).
    /// `[[token]]` is accepted as the nicer spelling; a field that is not part of the config is
    /// an error, so a typo is reported instead of silently ignored.
    #[serde(default, alias = "token")]
    pub tokens: Vec<ChannelToken>,
    /// The peer module; ignored unless it runs.
    #[serde(default)]
    pub peer: Option<PeerConfig>,
    /// The music module; ignored unless it runs.
    #[serde(default)]
    pub music: Option<MusicConfig>,
    /// The history module; ignored unless it runs.
    #[serde(default)]
    pub history: Option<HistoryConfig>,
    /// The plugins module; ignored unless it runs.
    #[serde(default)]
    pub plugins: Option<PluginsConfig>,

    /// Bitrate and packet length of every module that sends voice (a module can override them
    /// in its own section).
    #[serde(default)]
    pub voice: VoiceConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulesConfig {
    #[serde(default)]
    pub enabled: Vec<Module>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelToken {
    pub channel: ChannelRef,
    pub password: String,
}

/// One channel password. `channel` may be a name or an ID like in the key entries.
impl std::fmt::Debug for ChannelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelToken")
            .field("channel", &self.channel)
            .field("password", &"<hidden>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    /// Standard murmur port.
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_name")]
    pub name: String,
    /// Persistent certificate name; defaults to `name`.
    #[serde(default)]
    pub cert_name: Option<String>,
    /// Channel to join at login (name or `Root/Path`).
    #[serde(default)]
    pub channel: Option<String>,
    /// Connect password, if the server needs one.
    #[serde(default)]
    pub password: Option<String>,
    /// Where the persistent certificate is kept; defaults to `cert::default_dir`.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// Log in as `SuperUser` (no certificate); only for local test servers.
    #[serde(default)]
    pub superuser: bool,
    /// SHA-256 of the server certificate (hex, colons allowed); any other certificate is
    /// refused. Required by the peer bot.
    #[serde(default)]
    pub server_cert_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    #[serde(default = "default_delivery_timeout")]
    pub delivery_timeout_secs: u64,
}

/// One API key and the channels it may write to (Q2.3).
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    pub key: String,
    /// Name used in logs; the key value itself is never logged.
    pub label: String,
    pub channels: Vec<ChannelRef>,
}

/// A channel entry in the config: name or numeric ID, mixed allowed (Q4.2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ChannelRef {
    Name(String),
    Id(u32),
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("key", &"<hidden>")
            .field("label", &self.label)
            .field("channels", &self.channels)
            .finish()
    }
}

impl WebhookConfig {
    /// Reads a TOML config file, replaces its module list with `chosen` unless that is empty,
    /// and validates it for the modules that will run.
    pub fn load(path: &Path, chosen: &[Module]) -> Result<Self> {
        let mut config = Self::read(path)?;
        if !chosen.is_empty() {
            config.modules.enabled = chosen.to_vec();
        }
        config.voice.validate(" in [voice]")?;
        config.validate_modules()?;
        Ok(config)
    }

    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    /// Validates only the sections of the enabled modules; a repeated module counts once.
    fn validate_modules(&mut self) -> Result<()> {
        let mut seen = Vec::new();
        self.modules.enabled.retain(|module| {
            let first = !seen.contains(module);
            seen.push(*module);
            first
        });
        let enabled = &self.modules.enabled;
        if enabled.is_empty() {
            bail!(
                "no module to run: choose them with --modules (or MUMBOT_MODULES) or in the \
                 config's [modules] enabled list"
            );
        }
        if enabled.contains(&Module::Webhook) {
            self.validate()?;
            // Pinning is the peer module's requirement (mumcord spec 04, FR-4.28); for the
            // webhook alone it is a later decision (roadmap, "Server certificate check"), so a
            // pin here would only look like protection. With the peer module it guards the
            // shared connection, and so it does with the history module (spec 04, FR-4.3) and
            // the plugins module (spec 06, FR-6.77).
            let pins = [Module::Peer, Module::History, Module::Plugins]
                .iter()
                .any(|module| enabled.contains(module));
            if !pins && self.server.server_cert_sha256.is_some() {
                bail!(
                    "server.server_cert_sha256 is only used with the peer, history or plugins \
                     module; remove it for the webhook bot"
                );
            }
        }
        if enabled.contains(&Module::Peer) {
            self.validate_peer()?;
        }
        if enabled.contains(&Module::Music) {
            let Some(music) = &self.music else {
                bail!("config has no [music] section");
            };
            music.validate()?;
        }
        if enabled.contains(&Module::History) {
            let Some(history) = &self.history else {
                bail!("config has no [history] section");
            };
            history.validate()?;
            self.server_cert_pin()?;
        }
        if enabled.contains(&Module::Plugins) {
            self.validate_plugins()?;
        }
        Ok(())
    }

    /// `[plugins]` and a pinned server certificate are required (mumcord spec 06, FR-6.77 and
    /// mumbot#20): clients only trust suggestions from a bot that cannot be sent elsewhere.
    fn validate_plugins(&self) -> Result<()> {
        let Some(plugins) = &self.plugins else {
            bail!("config has no [plugins] section");
        };
        plugins.validate()?;
        if self.server.server_cert_sha256.is_none() {
            bail!(
                "the plugins module needs server.server_cert_sha256, the SHA-256 of the Mumble \
                 server certificate, so it never announces plugins on a server it did not mean \
                 to join"
            );
        }
        self.server_cert_pin()?;
        Ok(())
    }

    /// `[peer]` and a pinned server certificate are required, API keys are not (mumcord spec
    /// 04, FR-4.22 and FR-4.28).
    fn validate_peer(&self) -> Result<()> {
        let Some(peer) = &self.peer else {
            bail!("config has no [peer] section");
        };
        peer.validate()?;
        if self.server.server_cert_sha256.is_none() {
            bail!(
                "the peer bot needs server.server_cert_sha256, the SHA-256 of the Mumble server \
                 certificate, so it never hands out tokens on a server it did not mean to join"
            );
        }
        self.server_cert_pin()?;
        Ok(())
    }

    /// The pinned server certificate, normalized.
    fn server_cert_pin(&self) -> Result<Option<String>> {
        self.server
            .server_cert_sha256
            .as_deref()
            .map(|value| {
                crate::peer::config::normalize_sha256(value).context("server.server_cert_sha256")
            })
            .transpose()
    }

    fn validate(&self) -> Result<()> {
        if self.keys.is_empty() {
            bail!("config has no API key");
        }
        for entry in &self.keys {
            if entry.key.is_empty() {
                bail!("a key entry is empty");
            }
            if entry.label.is_empty() {
                bail!("a key entry has no label");
            }
            if entry.channels.is_empty() {
                bail!("key {} has no allowed channels", entry.label);
            }
            for channel in &entry.channels {
                if let ChannelRef::Name(name) = channel
                    && name.is_empty()
                {
                    bail!("key {} has an empty channel name", entry.label);
                }
            }
        }
        if self.http.rate_limit_per_minute == 0 {
            bail!("rate_limit_per_minute must be above 0");
        }
        if self.http.delivery_timeout_secs == 0 {
            bail!("delivery_timeout_secs must be above 0");
        }
        Ok(())
    }

    /// Maps the config onto the existing bot client settings.
    pub fn bot_config(&self) -> crate::BotConfig {
        let mut bot = crate::BotConfig::new(&self.server.name, self.server.port);
        bot.host = self.server.host.clone();
        bot.cert_name = self.server.cert_name.clone().or_else(|| {
            // The display name may contain spaces; the certificate name may not.
            Some(
                self.server
                    .name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect(),
            )
        });
        bot.data_dir = self.server.data_dir.clone();
        bot.superuser = self.server.superuser;
        // `load` validates the pin; an invalid one never matches a certificate.
        bot.server_cert_sha256 = self.server.server_cert_sha256.as_ref().map(|value| {
            self.server_cert_pin()
                .ok()
                .flatten()
                .unwrap_or_else(|| value.clone())
        });
        bot.password = self
            .server
            .password
            .clone()
            .or_else(|| crate::env::testenv_var("SERVER_PASSWORD"));
        bot.tokens = self
            .tokens
            .iter()
            .map(|token| token.password.clone())
            .collect();
        bot
    }

    pub fn rate_limit(&self) -> u32 {
        self.http.rate_limit_per_minute
    }

    pub fn delivery_timeout(&self) -> Duration {
        Duration::from_secs(self.http.delivery_timeout_secs)
    }

    /// Finds the entry a request key matches, compared in constant time (Q4.6).
    pub fn find_key(&self, presented: &str) -> Option<&ApiKey> {
        find_key(&self.keys, presented)
    }

    /// True when the listener only serves the local machine.
    pub fn listens_locally(&self) -> bool {
        parse_listen(&self.http.listen).is_some_and(|addr| addr.ip().is_loopback())
    }

    /// Parsed `host:port` of the listener.
    pub fn listen_addr(&self) -> Result<std::net::SocketAddr> {
        parse_listen(&self.http.listen)
            .with_context(|| format!("invalid http.listen {:?}", self.http.listen))
    }
}

pub(crate) fn parse_listen(value: &str) -> Option<std::net::SocketAddr> {
    value.parse().ok()
}

/// The entry of `keys` a request key matches, compared in constant time (Q4.6).
pub(crate) fn find_key<'a>(keys: &'a [ApiKey], presented: &str) -> Option<&'a ApiKey> {
    keys.iter()
        .find(|entry| constant_time_eq(presented.as_bytes(), entry.key.as_bytes()))
}

/// Length-insensitive comparison, so a wrong key cannot be read out through timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn default_host() -> String {
    crate::env::HOST.to_owned()
}

fn default_port() -> u16 {
    crate::env::DEFAULT_PORT
}

fn default_name() -> String {
    "Mumbot".to_owned()
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_owned()
}

fn default_rate_limit() -> u32 {
    RATE_LIMIT_PER_MINUTE
}

fn default_delivery_timeout() -> u64 {
    DELIVERY_TIMEOUT.as_secs()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            name: default_name(),
            cert_name: None,
            channel: None,
            password: None,
            data_dir: None,
            superuser: false,
            server_cert_sha256: None,
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            rate_limit_per_minute: default_rate_limit(),
            delivery_timeout_secs: default_delivery_timeout(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[server]
name = "Release Bot"

[http]
listen = "127.0.0.1:8080"

[[keys]]
key = "a-long-random-key-1234567890"
label = "ci"
channels = ["Lobby", 42]
"#;

    #[test]
    fn parses_names_and_ids_and_applies_defaults() {
        let config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        assert_eq!(config.keys.len(), 1);
        assert_eq!(config.keys[0].label, "ci");
        assert_eq!(
            config.keys[0].channels,
            [ChannelRef::Name("Lobby".to_owned()), ChannelRef::Id(42)]
        );
        assert_eq!(config.server.name, "Release Bot");
        assert_eq!(config.server.port, crate::env::DEFAULT_PORT);
        assert_eq!(config.http.listen, DEFAULT_LISTEN);
        assert_eq!(config.rate_limit(), RATE_LIMIT_PER_MINUTE);
        assert_eq!(config.delivery_timeout(), DELIVERY_TIMEOUT);
        assert!(config.listens_locally());
    }

    #[test]
    fn rejects_empty_keys_and_channels() {
        let mut config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        config.keys[0].channels.clear();
        assert!(config.validate().is_err());
        config.keys[0].channels.push(ChannelRef::Id(1));
        config.keys[0].key.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn matches_key_and_hides_it_in_debug() {
        let config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        assert!(config.find_key("a-long-random-key-1234567890").is_some());
        assert!(config.find_key("a-long-random-key-1234567891").is_none());
        assert!(!format!("{:?}", config.keys[0]).contains("1234567890"));
    }

    #[test]
    fn the_shipped_example_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/bot.example.toml");
        let config: WebhookConfig =
            toml::from_str(&std::fs::read_to_string(path).expect("example file"))
                .expect("example config");
        assert_eq!(config.keys.len(), 1);
        assert!(
            config.tokens.is_empty(),
            "passwords stay commented in the example"
        );
        assert!(config.listens_locally(), "the example binds to loopback");
        assert!(config.peer.is_none(), "the peer module is off by default");
    }

    #[test]
    fn the_peer_bot_needs_its_section_and_a_server_pin_but_no_key() {
        let text = r#"
[server]
server_cert_sha256 = "PIN"

[peer]
api_url = "http://127.0.0.1:7480"
api_secret_file = "relay.secret"
"#;
        let pin = format!("{}AB", "AB:".repeat(31));
        let config: WebhookConfig = toml::from_str(&text.replace("PIN", &pin)).expect("config");
        config.validate_peer().expect("no key needed");
        assert_eq!(
            config.bot_config().server_cert_sha256,
            Some("ab".repeat(32)),
            "the pin is normalized"
        );
        let config: WebhookConfig = toml::from_str(&text.replace("PIN", "abc")).expect("config");
        assert!(config.validate_peer().is_err(), "the pin is checked");
        let config: WebhookConfig =
            toml::from_str(&text.replace("server_cert_sha256 = \"PIN\"", "")).expect("config");
        assert!(config.validate_peer().is_err(), "the pin is required");
        let config: WebhookConfig = toml::from_str("").expect("config");
        assert!(config.validate_peer().is_err(), "[peer] is required");
    }

    const BOTH: &str = r#"
[modules]
enabled = ["webhook", "peer", "webhook"]

[server]
server_cert_sha256 = "PIN"

[peer]
api_url = "http://127.0.0.1:7480"
api_secret_file = "relay.secret"

[[keys]]
key = "a-long-random-key-1234567890"
label = "ci"
channels = ["Lobby"]
"#;

    fn both() -> WebhookConfig {
        toml::from_str(&BOTH.replace("PIN", &"ab".repeat(32))).expect("config")
    }

    #[test]
    fn the_voice_section_is_read_and_checked_at_load() {
        let mut config = both();
        assert_eq!(config.voice, VoiceConfig::default());
        let text = format!(
            "[voice]\nbitrate = 64000\nframe_ms = 10\n{}",
            BOTH.replace("PIN", &"ab".repeat(32))
        );
        config = toml::from_str(&text).expect("config");
        assert_eq!(
            (config.voice.bitrate, config.voice.frame_ms),
            (Some(64_000), Some(10))
        );

        let path = std::env::temp_dir().join(format!("mumbot-voice-{}.toml", std::process::id()));
        std::fs::write(&path, text.replace("64000", "5")).expect("write");
        let error = WebhookConfig::load(&path, &[Module::Webhook]).expect_err("out of range");
        std::fs::remove_file(&path).ok();
        assert!(format!("{error:#}").contains("out of range"), "{error:#}");
    }

    #[test]
    fn only_the_enabled_modules_are_validated() {
        let mut config = both();
        config.validate_modules().expect("webhook and peer");
        assert_eq!(
            config.modules.enabled,
            [Module::Webhook, Module::Peer],
            "a repeated module counts once"
        );

        let mut config = both();
        config.modules.enabled = vec![Module::Webhook];
        assert!(
            config.validate_modules().is_err(),
            "the webhook alone does not pin"
        );

        let mut config = both();
        config.keys.clear();
        config.modules.enabled = vec![Module::Peer];
        config
            .validate_modules()
            .expect("the peer module needs no key");

        let mut config = both();
        config.modules.enabled.clear();
        let error = config.validate_modules().expect_err("nothing to run");
        assert!(error.to_string().contains("--modules"), "{error}");

        let mut config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        config.modules.enabled = vec![Module::Webhook];
        config
            .validate_modules()
            .expect("a webhook config without [modules] still works");

        assert!(
            toml::from_str::<WebhookConfig>("[modules]\nenabled = [\"radio\"]").is_err(),
            "an unknown module is reported"
        );
    }

    #[test]
    fn the_history_module_needs_its_section_and_may_pin() {
        let mut config = both();
        config.modules.enabled = vec![Module::History];
        let error = config.validate_modules().expect_err("no [history]");
        assert!(error.to_string().contains("[history]"), "{error}");

        let text = format!(
            "{}\n[history]\nchannels = [\"Lobby\"]\n",
            BOTH.replace("PIN", &"ab".repeat(32))
        );
        let mut config: WebhookConfig = toml::from_str(&text).expect("config");
        config.modules.enabled = vec![Module::Webhook, Module::History];
        config
            .validate_modules()
            .expect("the pin guards the connection the history module shares");
        config.server.server_cert_sha256 = Some("not hex".to_owned());
        assert!(config.validate_modules().is_err(), "a bad pin is reported");
    }

    #[test]
    fn the_plugins_module_needs_its_section_and_a_pin() {
        let mut config = both();
        config.modules.enabled = vec![Module::Plugins];
        let error = config.validate_modules().expect_err("no [plugins]");
        assert!(error.to_string().contains("[plugins]"), "{error}");

        let text = format!(
            "{}\n[[plugins.announce]]\nid = \"dice\"\nmin = \"1.0.0\"\nsource = \"official\"\n\
             for = \"board game nights\"\n",
            BOTH.replace("PIN", &"ab".repeat(32))
        );
        let mut config: WebhookConfig = toml::from_str(&text).expect("config");
        config.modules.enabled = vec![Module::Webhook, Module::Plugins];
        config
            .validate_modules()
            .expect("the pin guards the connection the plugins module shares");
        config.server.server_cert_sha256 = None;
        let error = config.validate_modules().expect_err("the pin is required");
        assert!(error.to_string().contains("server_cert_sha256"), "{error}");

        let mut config: WebhookConfig = toml::from_str(&text).expect("config");
        config.modules.enabled = vec![Module::Plugins];
        config.plugins.as_mut().expect("plugins").announce[0].min = "one".to_owned();
        assert!(config.validate_modules().is_err(), "entries are checked");
    }

    #[test]
    fn a_command_line_choice_replaces_the_module_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bot.toml");
        std::fs::write(&path, BOTH.replace("PIN", &"ab".repeat(32))).expect("write");
        let config = WebhookConfig::load(&path, &[Module::Peer]).expect("load");
        assert_eq!(config.modules.enabled, [Module::Peer]);
        let config = WebhookConfig::load(&path, &[]).expect("load");
        assert_eq!(config.modules.enabled, [Module::Webhook, Module::Peer]);
    }

    #[test]
    fn the_simpler_token_spelling_works_and_typos_are_reported() {
        let good: WebhookConfig = toml::from_str(
            r#"
[[token]]
channel = "Secret"
password = "hunter2"
"#,
        )
        .expect("config");
        assert_eq!(good.tokens.len(), 1);
        let typo = toml::from_str::<WebhookConfig>(
            r#"
[[token]]
channel = "Secret"
passwort = "hunter2"
"#,
        );
        assert!(typo.is_err(), "a wrong field name is not silently ignored");
    }

    #[test]
    fn channel_passwords_become_login_tokens_and_stay_hidden() {
        let text = r#"
[[keys]]
key = "test-key-1"
label = "ci"
channels = ["Lobby"]

[[tokens]]
channel = "Secret"
password = "hunter2"

[[tokens]]
channel = 7
password = "other"
"#;
        let config: WebhookConfig = toml::from_str(text).expect("toml");
        assert_eq!(
            config.bot_config().tokens,
            vec!["hunter2".to_owned(), "other".to_owned()]
        );
        let shown = format!("{:?}", config.tokens[0]);
        assert!(
            !shown.contains("hunter2"),
            "secrets stay out of the log: {shown}"
        );
    }

    #[test]
    fn certificate_name_stays_within_the_allowed_characters() {
        let config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        assert_eq!(
            config.bot_config().cert_name.as_deref(),
            Some("Release_Bot")
        );
    }

    #[test]
    fn lan_listen_is_not_local() {
        let mut config: WebhookConfig = toml::from_str(SAMPLE).expect("toml");
        config.http.listen = "0.0.0.0:8080".to_owned();
        assert!(!config.listens_locally());
        assert!(config.listen_addr().is_ok());
    }
}
