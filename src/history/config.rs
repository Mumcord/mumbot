//! The `[history]` section of the bot's config file (spec 04, FR-4.2).

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::webhook::ChannelRef;

/// History sync never reaches further back (mumcord FR-1.60).
pub const MAX_RETENTION_DAYS: u32 = 7;
const DEFAULT_MAX_MESSAGES: u32 = 5000;
const MAX_MAX_MESSAGES: u32 = 100_000;
/// The `channels` entry that stands for every channel.
pub const ALL_CHANNELS: &str = "*";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    /// Channels to record (names, paths or IDs); `"*"` records every channel.
    pub channels: Vec<ChannelRef>,
    /// Channels never recorded; wins over `channels`.
    #[serde(default)]
    pub exclude: Vec<ChannelRef>,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_max_messages")]
    pub max_messages_per_channel: u32,
    /// SQLite file; defaults to `history.sqlite3` in the bot's data directory.
    #[serde(default)]
    pub database: Option<PathBuf>,
}

fn default_retention_days() -> u32 {
    MAX_RETENTION_DAYS
}

fn default_max_messages() -> u32 {
    DEFAULT_MAX_MESSAGES
}

impl HistoryConfig {
    pub fn validate(&self) -> Result<()> {
        if self.channels.is_empty() {
            bail!(
                "[history] channels is empty: list the channels to record (or \"*\" for all); \
                 nothing is recorded without it"
            );
        }
        for entry in self.channels.iter().chain(&self.exclude) {
            if let ChannelRef::Name(name) = entry
                && name.is_empty()
            {
                bail!("[history] has an empty channel name");
            }
        }
        if !(1..=MAX_RETENTION_DAYS).contains(&self.retention_days) {
            bail!(
                "[history] retention_days must be 1 to {MAX_RETENTION_DAYS}: history sync never \
                 asks for older messages"
            );
        }
        if !(1..=MAX_MAX_MESSAGES).contains(&self.max_messages_per_channel) {
            bail!("[history] max_messages_per_channel must be 1 to {MAX_MAX_MESSAGES}");
        }
        Ok(())
    }

    /// Whether `channels` holds the `"*"` entry.
    pub fn records_all(&self) -> bool {
        self.channels
            .iter()
            .any(|c| matches!(c, ChannelRef::Name(n) if n == ALL_CHANNELS))
    }

    /// The database path, in `data_dir` unless configured.
    pub fn database_path(&self, data_dir: Option<&std::path::Path>) -> PathBuf {
        match &self.database {
            Some(path) => path.clone(),
            None => data_dir
                .map_or_else(crate::cert::default_dir, std::path::Path::to_path_buf)
                .join("history.sqlite3"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<HistoryConfig> {
        let config: HistoryConfig = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn defaults_and_limits() {
        let config = parse(r#"channels = ["Lobby", 2]"#).expect("minimal");
        assert_eq!(config.retention_days, 7);
        assert_eq!(config.max_messages_per_channel, 5000);
        assert!(!config.records_all());
        assert_eq!(
            config.database_path(Some(std::path::Path::new("/data"))),
            PathBuf::from("/data/history.sqlite3")
        );
        assert!(parse(r#"channels = ["*"]"#).expect("all").records_all());
        for bad in [
            "channels = []",
            r#"channels = [""]"#,
            r#"channels = ["Lobby"]
               retention_days = 8"#,
            r#"channels = ["Lobby"]
               retention_days = 0"#,
            r#"channels = ["Lobby"]
               max_messages_per_channel = 0"#,
            r#"channels = ["Lobby"]
               typo = 1"#,
            "retention_days = 3",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
