//! `[plugins]` section of the bot config: the Mumcord plugins the server community announces,
//! and the announcement counter (`seq`) kept next to the bot's certificate.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::wire::{self, Suggestion};

/// Most plugins one announcement may list (FR-6.77).
pub const MAX_PLUGINS: usize = 8;
/// Longest `for` text in characters (FR-6.77).
pub const MAX_PURPOSE_CHARS: usize = 40;
/// The `source` of plugins from Mumcord's official repository.
pub const OFFICIAL: &str = "official";
/// File in the data directory that keeps `seq` across restarts.
const SEQ_FILE: &str = "plugins-seq.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    /// The announced plugins, in the order clients show them.
    #[serde(default)]
    pub announce: Vec<Announce>,
}

/// One announced plugin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announce {
    /// Plugin id as in its manifest.
    pub id: String,
    /// Lowest version the community uses (`MAJOR.MINOR.PATCH`).
    pub min: String,
    /// `official`, or the HTTPS address of a third-party repository's `index.json`.
    pub source: String,
    /// The third-party repository's minisign public key; required for third-party sources.
    #[serde(default)]
    pub key: Option<String>,
    /// What the community uses the plugin for, shown to users (≤ 40 characters).
    #[serde(rename = "for")]
    pub purpose: String,
}

impl PluginsConfig {
    /// Checks every entry and that the message fits into one plugin data message.
    pub fn validate(&self) -> Result<()> {
        if self.announce.is_empty() {
            bail!("[plugins] announces nothing: add at least one [[plugins.announce]] entry");
        }
        if self.announce.len() > MAX_PLUGINS {
            bail!(
                "[plugins] announces {} plugins, at most {MAX_PLUGINS} are allowed",
                self.announce.len()
            );
        }
        for (index, entry) in self.announce.iter().enumerate() {
            entry
                .validate()
                .with_context(|| format!("[[plugins.announce]] entry {}", index + 1))?;
            if self.announce[..index].iter().any(|e| e.id == entry.id) {
                bail!("[plugins] announces `{}` twice", entry.id);
            }
        }
        // The largest counter the message can carry, so it never grows past the limit later.
        let size = wire::encode(u32::MAX, &self.suggestions()).len();
        if size > mumcord_protocol::PLUGIN_DATA_MAX {
            bail!(
                "[plugins] announcement is {size} bytes, more than the {} a plugin data message \
                 may carry: shorten the addresses or `for` texts, or announce fewer plugins",
                mumcord_protocol::PLUGIN_DATA_MAX
            );
        }
        Ok(())
    }

    /// The entries as they go on the wire.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        self.announce
            .iter()
            .map(|entry| Suggestion {
                id: entry.id.clone(),
                min: entry.min.clone(),
                src: entry.source.clone(),
                key: entry.key.clone(),
                purpose: entry.purpose.clone(),
            })
            .collect()
    }
}

impl Announce {
    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            bail!(
                "id `{}` is not a plugin id (3 to 32 characters: lowercase letters, digits and \
                 `-`, starting with a letter)",
                self.id
            );
        }
        if !valid_version(&self.min) {
            bail!("min `{}` is not a version like 1.2.0", self.min);
        }
        if self.source == OFFICIAL {
            if self.key.is_some() {
                bail!("official plugins take no key: Mumcord has the official key built in");
            }
        } else {
            if !valid_https(&self.source) {
                bail!("source must be `official` or an https:// address of a repository index");
            }
            match &self.key {
                None => bail!("a third-party source needs the repository's minisign public key"),
                Some(key) if !valid_key(key) => {
                    bail!("key is not a minisign public key (56 characters starting with RW)")
                }
                Some(_) => {}
            }
        }
        let chars = self.purpose.chars().count();
        if self.purpose.trim().is_empty() || chars > MAX_PURPOSE_CHARS {
            bail!("`for` must say in 1 to {MAX_PURPOSE_CHARS} characters what the plugin is for");
        }
        if self.purpose.chars().any(char::is_control) {
            bail!("`for` contains control characters");
        }
        Ok(())
    }
}

/// `^[a-z][a-z0-9-]{2,31}$`, the manifest id rule of Mumcord (FR-6.11).
fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (3..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `MAJOR.MINOR.PATCH` without leading zeros.
fn valid_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 9
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}

/// An `https://` address with a host and nothing a client would have to escape.
fn valid_https(address: &str) -> bool {
    let Some(rest) = address.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.is_empty()
        && !host.contains('@')
        && address
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '<' | '>' | '\\'))
}

/// A minisign public key: base64 of `Ed`, the key id and the 32-byte key.
fn valid_key(key: &str) -> bool {
    key.len() == 56
        && key.starts_with("RW")
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// What the bot remembers of its last announcement.
#[derive(Debug, Serialize, Deserialize)]
struct SeqRecord {
    seq: u32,
    /// SHA-256 hex of the announced list.
    list: String,
}

/// Where `seq` is kept: next to the bot's certificate.
pub fn seq_path(data_dir: Option<&Path>) -> PathBuf {
    data_dir
        .map_or_else(crate::cert::default_dir, Path::to_path_buf)
        .join(SEQ_FILE)
}

/// The `seq` for `plugins`: the stored one while the list is unchanged, one higher after a
/// change (clients show a dismissed suggestion again only when `seq` changes, FR-6.79). Blocking
/// file I/O; call it outside the async runtime.
pub fn current_seq(path: &Path, plugins: &[Suggestion]) -> Result<u32> {
    let list = list_hash(plugins);
    let stored = match std::fs::read(path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<SeqRecord>(&bytes).with_context(|| {
                format!("{} is damaged; delete it to start over", path.display())
            })?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };
    let seq = match stored {
        Some(record) if record.list == list => return Ok(record.seq),
        Some(record) => record
            .seq
            .checked_add(1)
            .context("plugins seq ran out; delete the seq file to start over")?,
        None => 1,
    };
    let record = serde_json::to_vec(&SeqRecord { seq, list })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    // Written aside and renamed, so a crash never leaves half a file that stops the next start.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, record)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(seq)
}

fn list_hash(plugins: &[Suggestion]) -> String {
    let encoded = wire::encode(0, plugins);
    ring::digest::digest(&ring::digest::SHA256, &encoded)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";

    fn official(id: &str) -> Announce {
        Announce {
            id: id.into(),
            min: "1.0.0".into(),
            source: OFFICIAL.into(),
            key: None,
            purpose: "game nights".into(),
        }
    }

    fn config(announce: Vec<Announce>) -> PluginsConfig {
        PluginsConfig { announce }
    }

    #[test]
    fn the_issue_example_parses_without_level() {
        let text = format!(
            r#"
            [[announce]]
            id = "echolings"
            min = "1.2.0"
            source = "official"
            for = "pets in the lobby"

            [[announce]]
            id = "guild-calendar"
            min = "0.9.0"
            source = "https://plugins.nightowls.example/v1/index.json"
            key = "{KEY}"
            for = "raid sign-ups"
            "#
        );
        let parsed: PluginsConfig = toml::from_str(&text).expect("config");
        parsed.validate().expect("valid");
        assert_eq!(parsed.suggestions()[1].key.as_deref(), Some(KEY));

        let with_level = text.replace("min = \"1.2.0\"", "min = \"1.2.0\"\nlevel = \"required\"");
        assert!(
            toml::from_str::<PluginsConfig>(&with_level).is_err(),
            "`level` was dropped from the message (FR-6.78)"
        );
    }

    #[test]
    fn entries_are_checked() {
        let cases: Vec<(&str, Announce)> = vec![
            (
                "bad id",
                Announce {
                    id: "Dice".into(),
                    ..official("dice")
                },
            ),
            ("short id", official("ab")),
            (
                "bad version",
                Announce {
                    min: "1.0".into(),
                    ..official("dice")
                },
            ),
            (
                "leading zero",
                Announce {
                    min: "1.02.0".into(),
                    ..official("dice")
                },
            ),
            (
                "key for official",
                Announce {
                    key: Some(KEY.into()),
                    ..official("dice")
                },
            ),
            (
                "plain http",
                Announce {
                    source: "http://plugins.example.org/index.json".into(),
                    key: Some(KEY.into()),
                    ..official("dice")
                },
            ),
            (
                "third party without key",
                Announce {
                    source: "https://plugins.example.org/index.json".into(),
                    ..official("dice")
                },
            ),
            (
                "bad key",
                Announce {
                    source: "https://plugins.example.org/index.json".into(),
                    key: Some("RWQ123".into()),
                    ..official("dice")
                },
            ),
            (
                "empty for",
                Announce {
                    purpose: " ".into(),
                    ..official("dice")
                },
            ),
            (
                "long for",
                Announce {
                    purpose: "x".repeat(41),
                    ..official("dice")
                },
            ),
            (
                "control in for",
                Announce {
                    purpose: "a\nb".into(),
                    ..official("dice")
                },
            ),
        ];
        for (name, entry) in cases {
            assert!(config(vec![entry]).validate().is_err(), "{name}");
        }
        let forty = Announce {
            purpose: "ü".repeat(40),
            ..official("dice")
        };
        config(vec![forty])
            .validate()
            .expect("40 characters, not bytes");
    }

    #[test]
    fn the_list_is_limited() {
        assert!(
            config(Vec::new()).validate().is_err(),
            "nothing to announce"
        );
        let eight: Vec<Announce> = (0..8).map(|i| official(&format!("plugin-{i}"))).collect();
        config(eight.clone()).validate().expect("eight fit");
        let mut nine = eight;
        nine.push(official("plugin-9"));
        assert!(config(nine).validate().is_err(), "nine are too many");
        assert!(
            config(vec![official("dice"), official("dice")])
                .validate()
                .is_err(),
            "duplicates"
        );
    }

    #[test]
    fn an_oversized_announcement_is_refused() {
        let long: Vec<Announce> = (0..8)
            .map(|i| Announce {
                id: format!("plugin-{i}"),
                source: format!("https://plugins.example.org/{}/index.json", "a".repeat(60)),
                key: Some(KEY.into()),
                purpose: "x".repeat(40),
                ..official("dice")
            })
            .collect();
        let error = config(long).validate().expect_err("too large");
        assert!(error.to_string().contains("bytes"), "{error}");
    }

    #[test]
    fn seq_grows_only_when_the_list_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = seq_path(Some(dir.path()));
        let first = config(vec![official("dice")]).suggestions();
        assert_eq!(current_seq(&path, &first).expect("seq"), 1);
        assert_eq!(
            current_seq(&path, &first).expect("seq"),
            1,
            "same list, same seq"
        );
        let second = config(vec![official("dice"), official("poll")]).suggestions();
        assert_eq!(current_seq(&path, &second).expect("seq"), 2);
        assert_eq!(
            current_seq(&path, &first).expect("seq"),
            3,
            "back is a change too"
        );

        std::fs::write(&path, b"garbage").expect("write");
        assert!(
            current_seq(&path, &first).is_err(),
            "a damaged file is reported"
        );
    }
}
