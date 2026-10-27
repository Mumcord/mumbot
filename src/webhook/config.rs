//! TOML config for the webhook bot: connection settings plus the API keys.
//!
//! Keys are secrets: the real config file stays git-ignored, the repository only holds an
//! example with a placeholder. The `Debug` impl of `ApiKey` hides the key value so a key can
//! never end up in a log line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::webhook::{DELIVERY_TIMEOUT, RATE_LIMIT_PER_MINUTE};

/// Default listener: loopback only, must be changed explicitly to serve a LAN.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
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
    /// Reads and validates a TOML config file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
        config.validate()?;
        Ok(config)
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
        self.keys
            .iter()
            .find(|entry| constant_time_eq(presented.as_bytes(), entry.key.as_bytes()))
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

fn parse_listen(value: &str) -> Option<std::net::SocketAddr> {
    value.parse().ok()
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
