//! `[peer]` section of `bot.toml`: where the relay is and how the bot reaches its API.
//!
//! The section only holds the path to the API secret, never the secret itself, so the config
//! file can be shared; the secret file stays local and git-ignored.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::peer::wire::{self, Relay};
use crate::peer::{DEFAULT_TOKEN_TTL, MAX_TOKEN_TTL};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    /// Relay addresses for UDP (`host:port`), tried first by clients.
    pub udp: Vec<String>,
    /// TCP fallback addresses, usually port 443 for networks that block UDP.
    #[serde(default)]
    pub tcp: Vec<String>,
    /// SHA-256 of the relay's TLS certificate (hex, colons allowed).
    pub fingerprint: String,
    /// Base URL of the relay's management API, e.g. `http://127.0.0.1:8090`.
    pub api_url: String,
    /// File holding the API secret (first line, surrounding whitespace ignored).
    pub secret_file: PathBuf,
    #[serde(default = "default_token_ttl")]
    pub token_ttl_secs: u64,
}

/// The API secret. `Debug` hides it so it can never end up in a log line.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

impl PeerConfig {
    pub fn validate(&self) -> Result<()> {
        if self.udp.is_empty() {
            bail!("peer: no relay address in udp");
        }
        for address in self.udp.iter().chain(&self.tcp) {
            if !valid_address(address) {
                bail!("peer: relay address {address:?} is not host:port");
            }
        }
        normalize_fingerprint(&self.fingerprint)?;
        let url = reqwest::Url::parse(&self.api_url)
            .with_context(|| format!("peer: api_url {:?} is not a URL", self.api_url))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            bail!("peer: api_url must be an http(s) URL");
        }
        if self.token_ttl_secs == 0 || self.token_ttl() > MAX_TOKEN_TTL {
            bail!(
                "peer: token_ttl_secs must be between 1 and {}",
                MAX_TOKEN_TTL.as_secs()
            );
        }
        // The announcement must fit one plugin data message, even with the longest epoch.
        let size = wire::encode_relay(&self.relay("0".repeat(wire::EPOCH_LEN))?).len();
        if size > mumcord_protocol::PLUGIN_DATA_MAX {
            bail!("peer: too many or too long relay addresses ({size} bytes, max 1000)");
        }
        Ok(())
    }

    /// The announcement for the current epoch.
    pub fn relay(&self, epoch: String) -> Result<Relay> {
        Ok(Relay {
            udp: self.udp.clone(),
            tcp: self.tcp.clone(),
            fp: normalize_fingerprint(&self.fingerprint)?,
            epoch,
        })
    }

    pub fn token_ttl(&self) -> Duration {
        Duration::from_secs(self.token_ttl_secs)
    }

    /// Plain HTTP is fine on the same host; anywhere else the secret would travel in clear text.
    pub fn api_is_local_or_tls(&self) -> bool {
        let Ok(url) = reqwest::Url::parse(&self.api_url) else {
            return false;
        };
        let host = url.host_str().unwrap_or("");
        url.scheme() == "https"
            || host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }

    /// Reads the API secret from `secret_file`.
    pub fn load_secret(&self) -> Result<Secret> {
        read_secret(&self.secret_file)
    }
}

fn read_secret(path: &Path) -> Result<Secret> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("peer: cannot read secret_file {}", path.display()))?;
    let secret = text.lines().next().unwrap_or("").trim();
    if secret.is_empty() {
        bail!("peer: secret_file {} is empty", path.display());
    }
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(
                path = %path.display(),
                "peer: secret_file is readable by other users, restrict it to the bot (chmod 600)"
            );
        }
    }
    Ok(Secret::new(secret))
}

/// `host:port` with a non-empty host and a port from 1 to 65535.
fn valid_address(address: &str) -> bool {
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    !host.is_empty()
        && !host.chars().any(|c| c.is_whitespace() || c == '/')
        && port.parse::<u16>().is_ok_and(|p| p != 0)
}

/// Lowercase hex without separators, exactly 32 bytes.
fn normalize_fingerprint(value: &str) -> Result<String> {
    let hex: String = value
        .chars()
        .filter(|c| *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("peer: fingerprint must be the SHA-256 of the relay certificate (64 hex digits)");
    }
    Ok(hex)
}

fn default_token_ttl() -> u64 {
    DEFAULT_TOKEN_TTL.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PeerConfig {
        PeerConfig {
            udp: vec!["relay.example.org:3478".into()],
            tcp: vec!["relay.example.org:443".into()],
            fingerprint: "AB:".repeat(31) + "AB",
            api_url: "http://127.0.0.1:8090".into(),
            secret_file: "/nonexistent".into(),
            token_ttl_secs: 120,
        }
    }

    #[test]
    fn a_valid_section_passes_and_the_fingerprint_is_normalized() {
        let config = config();
        config.validate().expect("valid");
        assert_eq!(config.relay("e".into()).expect("relay").fp, "ab".repeat(32));
        assert!(config.api_is_local_or_tls());
    }

    #[test]
    fn rejects_bad_addresses_fingerprints_urls_and_ttls() {
        let cases: [fn(&mut PeerConfig); 7] = [
            |c| c.udp.clear(),
            |c| c.udp = vec!["relay.example.org".into()],
            |c| c.tcp = vec!["relay.example.org:0".into()],
            |c| c.fingerprint = "abcd".into(),
            |c| c.api_url = "ftp://relay".into(),
            |c| c.token_ttl_secs = 0,
            |c| c.udp = vec![format!("{}:1", "h".repeat(1000))],
        ];
        for change in cases {
            let mut config = config();
            change(&mut config);
            assert!(config.validate().is_err(), "{config:?}");
        }
    }

    #[test]
    fn plain_http_to_another_host_is_not_local() {
        let mut config = config();
        config.api_url = "http://relay.example.org:8090".into();
        assert!(!config.api_is_local_or_tls());
        config.api_url = "https://relay.example.org".into();
        assert!(config.api_is_local_or_tls());
    }

    #[test]
    fn the_secret_is_read_trimmed_and_hidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secret");
        std::fs::write(&path, "  s3cret \nignored\n").expect("write");
        let secret = read_secret(&path).expect("secret");
        assert_eq!(secret.expose(), "s3cret");
        assert!(!format!("{secret:?}").contains("s3cret"));
        std::fs::write(&path, "\n").expect("write");
        assert!(read_secret(&path).is_err());
    }
}
