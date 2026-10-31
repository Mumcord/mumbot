//! The `[music]` section of the config file (spec 04, FR-4.2).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::voice_settings::VoiceConfig;
use crate::webhook::{ApiKey, ChannelRef};

/// Default listener: loopback only, next to the webhook's 8080.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8081";

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MusicConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Default channel of a track; without it the bot's current channel.
    #[serde(default)]
    pub channel: Option<ChannelRef>,
    /// Directory that `{"file": …}` requests may play from.
    #[serde(default)]
    pub media_dir: Option<PathBuf>,
    /// Move back to `server.channel` when the queue is empty.
    #[serde(default = "yes")]
    pub return_home: bool,
    /// Start volume in percent.
    #[serde(default = "default_volume")]
    pub volume: u8,
    /// Tracks waiting, the playing one not counted.
    #[serde(default = "default_max_queue")]
    pub max_queue: usize,
    #[serde(default = "default_max_track_mb")]
    pub max_track_mb: u64,
    #[serde(default = "default_max_track_secs")]
    pub max_track_secs: u64,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Voice settings for music, above `[voice]` (issue #7).
    #[serde(default)]
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub frame_ms: Option<u32>,
    #[serde(default)]
    pub urls: UrlConfig,
    #[serde(default)]
    pub keys: Vec<ApiKey>,
}

/// URL sources: off unless enabled, and only from the listed hosts (FR-4.13, FR-4.14).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UrlConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Exact host names, or `*.domain` for every subdomain of `domain`.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Allow loopback, private and link-local addresses (a media server on the same network).
    #[serde(default)]
    pub allow_private: bool,
    #[serde(default = "default_fetch_timeout")]
    pub timeout_secs: u64,
}

impl Default for UrlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_hosts: Vec::new(),
            allow_private: false,
            timeout_secs: default_fetch_timeout(),
        }
    }
}

impl MusicConfig {
    pub fn validate(&self) -> Result<()> {
        self.voice()
            .validate(" in [music]")
            .context("invalid [music] voice settings")?;
        self.listen_addr()?;
        if self.keys.is_empty() {
            bail!("[music] has no API key ([[music.keys]])");
        }
        for entry in &self.keys {
            if entry.key.is_empty() || entry.label.is_empty() {
                bail!("a [[music.keys]] entry has an empty key or label");
            }
            if entry.channels.is_empty() {
                bail!("music key {} has no allowed channels", entry.label);
            }
        }
        if self.media_dir.is_none() && !self.urls.enabled {
            bail!(
                "[music] needs a media_dir or [music.urls] enabled = true, else nothing can play"
            );
        }
        if let Some(dir) = &self.media_dir
            && !dir.is_dir()
        {
            bail!("[music] media_dir {} is not a directory", dir.display());
        }
        if self.urls.enabled && self.urls.allow_hosts.is_empty() {
            bail!("[music.urls] is enabled but allow_hosts is empty");
        }
        if self.volume > 100 {
            bail!("[music] volume must be 0 to 100");
        }
        if self.max_queue == 0
            || self.max_track_mb == 0
            || self.max_track_secs == 0
            || self.rate_limit_per_minute == 0
            || self.urls.timeout_secs == 0
        {
            bail!("[music] limits must be above 0");
        }
        Ok(())
    }

    /// The module's layer of the voice settings.
    pub fn voice(&self) -> VoiceConfig {
        VoiceConfig {
            bitrate: self.bitrate,
            frame_ms: self.frame_ms,
        }
    }

    pub fn listen_addr(&self) -> Result<std::net::SocketAddr> {
        crate::webhook::parse_listen(&self.listen)
            .with_context(|| format!("invalid [music] listen {:?}", self.listen))
    }

    pub fn max_track_bytes(&self) -> usize {
        usize::try_from(self.max_track_mb.saturating_mul(1_000_000)).unwrap_or(usize::MAX)
    }

    pub fn max_track_len(&self) -> Duration {
        Duration::from_secs(self.max_track_secs)
    }

    pub fn find_key(&self, presented: &str) -> Option<&ApiKey> {
        crate::webhook::find_key(&self.keys, presented)
    }
}

fn default_listen() -> String {
    DEFAULT_LISTEN.to_owned()
}

fn yes() -> bool {
    true
}

fn default_volume() -> u8 {
    60
}

fn default_max_queue() -> usize {
    10
}

fn default_max_track_mb() -> u64 {
    20
}

fn default_max_track_secs() -> u64 {
    900
}

fn default_rate_limit() -> u32 {
    30
}

fn default_fetch_timeout() -> u64 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<MusicConfig> {
        Ok(toml::from_str(text)?)
    }

    const KEY: &str = "[[keys]]\nlabel = \"ci\"\nkey = \"k\"\nchannels = [\"Music\"]\n";

    #[test]
    fn defaults_and_validation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let config = parse(&format!("media_dir = {:?}\n{KEY}", dir.path()))?;
        assert_eq!(config.listen, DEFAULT_LISTEN);
        assert_eq!((config.volume, config.max_queue), (60, 10));
        assert!(!config.urls.enabled);
        config.validate()?;

        let no_source = parse(KEY)?;
        assert!(no_source.validate().is_err(), "neither media_dir nor urls");
        let no_key = parse(&format!("media_dir = {:?}", dir.path()))?;
        assert!(no_key.validate().is_err());
        let urls_without_hosts = parse(&format!("{KEY}[urls]\nenabled = true\n"))?;
        assert!(urls_without_hosts.validate().is_err());
        let bad_bitrate = parse(&format!("bitrate = 1\nmedia_dir = {:?}\n{KEY}", dir.path()))?;
        assert!(bad_bitrate.validate().is_err());
        assert!(parse("volum = 3").is_err(), "typos are reported");
        Ok(())
    }

    #[test]
    fn the_shipped_example_parses() -> Result<()> {
        let text = include_str!("../../examples/music/bot.example.toml");
        let config: crate::webhook::WebhookConfig = toml::from_str(text)?;
        let music = config.music.context("example has [music]")?;
        assert_eq!(music.listen, "0.0.0.0:8081");
        assert_eq!(music.keys.len(), 1);
        Ok(())
    }

    #[test]
    fn keys_hide_their_value() -> Result<()> {
        let config = parse(KEY)?;
        assert!(config.find_key("k").is_some());
        assert!(config.find_key("x").is_none());
        assert!(!format!("{config:?}").contains("\"k\""));
        Ok(())
    }
}
