//! `[peer]` section of `bot.toml`: how the bot reaches the relay's API (spec 04, section 7.5).
//!
//! The section only names where the API secret comes from (a file or `MUMBOT_PEER_API_SECRET`),
//! never the secret itself, so the config file can be shared.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Environment variable that holds the API secret; it wins over `api_secret_file`.
pub const SECRET_ENV: &str = "MUMBOT_PEER_API_SECRET";
/// Shortest accepted secret (spec 04, section 7.5).
pub const MIN_SECRET_LEN: usize = 32;
/// Default token requests per certificate and minute: two devices of one user at the
/// per-session limit, short of the relay's global pair budget.
pub const CERT_REQUESTS_PER_MINUTE: usize = 10;
/// Default open pairs per certificate, as many as the relay's default sessions per certificate.
pub const MAX_PAIRS_PER_CERT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    /// Base URL of the relay's API, e.g. `http://127.0.0.1:7480`.
    pub api_url: String,
    /// File holding the API secret on its first line.
    #[serde(default)]
    pub api_secret_file: Option<PathBuf>,
    /// SHA-256 of the API's TLS certificate; required for `https`, where it replaces the usual
    /// certificate authorities (the relay's certificate is self-signed).
    #[serde(default)]
    pub api_fp: Option<String>,
    /// Allows plain `http` to a relay API on another host, which sends the secret in clear text.
    #[serde(default)]
    pub allow_insecure_http: bool,
    /// Mumble groups whose members may get relay tokens; empty means every registered user.
    #[serde(default)]
    pub allowed_groups: Vec<String>,
    /// Channel whose ACL defines `allowed_groups`; the root channel when unset.
    #[serde(default)]
    pub groups_channel: Option<String>,
    #[serde(default = "default_cert_requests_per_minute")]
    pub cert_requests_per_minute: usize,
    #[serde(default = "default_max_pairs_per_cert")]
    pub max_pairs_per_cert: usize,
}

fn default_cert_requests_per_minute() -> usize {
    CERT_REQUESTS_PER_MINUTE
}

fn default_max_pairs_per_cert() -> usize {
    MAX_PAIRS_PER_CERT
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

/// `api_url` taken apart; only what a plain HTTP/1.1 client needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiUrl {
    pub tls: bool,
    /// Host name or IP address (IPv6 without brackets).
    pub host: String,
    pub port: u16,
    /// Path prefix without a trailing slash, often empty.
    pub base: String,
}

impl ApiUrl {
    pub fn parse(url: &str) -> Result<Self> {
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            bail!("peer: api_url {url:?} must start with http:// or https://");
        };
        let (authority, base) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6
                .split_once(']')
                .with_context(|| format!("peer: api_url {url:?} has a broken IPv6 address"))?;
            (host, after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host, Some(port)),
                None => (authority, None),
            }
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .with_context(|| format!("peer: api_url {url:?} has an invalid port"))?,
            None if tls => 443,
            None => 80,
        };
        if host.is_empty() || host.contains(['@', '?', '#', ' ']) {
            bail!("peer: api_url {url:?} has no valid host");
        }
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
            base: base.trim_end_matches('/').to_owned(),
        })
    }

    /// Plain HTTP is fine on the same host; anywhere else the secret would travel in clear text.
    pub fn is_local_or_tls(&self) -> bool {
        self.tls
            || self.host == "localhost"
            || self
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

impl PeerConfig {
    /// `api_url` with every other key at its default.
    pub fn new(api_url: impl Into<String>) -> Self {
        Self {
            api_url: api_url.into(),
            api_secret_file: None,
            api_fp: None,
            allow_insecure_http: false,
            allowed_groups: Vec::new(),
            groups_channel: None,
            cert_requests_per_minute: CERT_REQUESTS_PER_MINUTE,
            max_pairs_per_cert: MAX_PAIRS_PER_CERT,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let url = ApiUrl::parse(&self.api_url)?;
        match &self.api_fp {
            Some(fp) => {
                normalize_sha256(fp).context("peer: api_fp")?;
            }
            None if url.tls => bail!("peer: an https api_url needs api_fp"),
            None => {}
        }
        if !url.is_local_or_tls() && !self.allow_insecure_http {
            bail!(
                "peer: api_url {:?} is plain http to another host, which sends the API secret in \
                 clear text; use https with api_fp, or set allow_insecure_http = true",
                self.api_url
            );
        }
        if self.cert_requests_per_minute == 0 || self.max_pairs_per_cert == 0 {
            bail!("peer: cert_requests_per_minute and max_pairs_per_cert must be at least 1");
        }
        if self
            .allowed_groups
            .iter()
            .any(|group| group.trim().is_empty())
        {
            bail!("peer: allowed_groups has an empty group name");
        }
        Ok(())
    }

    pub fn url(&self) -> Result<ApiUrl> {
        ApiUrl::parse(&self.api_url)
    }

    /// The pinned API certificate, normalized.
    pub fn api_fp(&self) -> Result<Option<String>> {
        self.api_fp.as_deref().map(normalize_sha256).transpose()
    }

    /// Reads the API secret: `MUMBOT_PEER_API_SECRET`, else `api_secret_file`.
    pub fn load_secret(&self) -> Result<Secret> {
        let from_env = std::env::var(SECRET_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let secret = match (from_env, &self.api_secret_file) {
            (Some(value), _) => Secret::new(value.trim()),
            (None, Some(path)) => read_secret(path)?,
            (None, None) => bail!("peer: no API secret: set {SECRET_ENV} or api_secret_file"),
        };
        if secret.expose().len() < MIN_SECRET_LEN {
            bail!("peer: the API secret is shorter than {MIN_SECRET_LEN} characters");
        }
        // The secret goes into an HTTP header, where CR or LF would end the header early.
        if secret.expose().chars().any(char::is_control) {
            bail!("peer: the API secret contains control characters");
        }
        Ok(secret)
    }
}

fn read_secret(path: &Path) -> Result<Secret> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("peer: cannot read api_secret_file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .with_context(|| format!("peer: cannot check api_secret_file {}", path.display()))?;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "peer: api_secret_file {} is open to other users, restrict it to the bot \
                 (chmod 600)",
                path.display()
            );
        }
    }
    Ok(Secret::new(text.lines().next().unwrap_or("").trim()))
}

/// Lowercase hex without separators, exactly 32 bytes (a SHA-256 fingerprint).
pub fn normalize_sha256(value: &str) -> Result<String> {
    let hex: String = value
        .chars()
        .filter(|c| *c != ':')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a SHA-256 fingerprint (64 hex digits, colons allowed)");
    }
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(url: &str, fp: Option<&str>) -> PeerConfig {
        PeerConfig {
            api_fp: fp.map(Into::into),
            ..PeerConfig::new(url)
        }
    }

    #[test]
    fn api_urls_are_taken_apart() {
        let url = ApiUrl::parse("http://127.0.0.1:7480").expect("url");
        assert_eq!(
            (url.tls, url.host.as_str(), url.port, url.base.as_str()),
            (false, "127.0.0.1", 7480, "")
        );
        assert!(url.is_local_or_tls());
        let url = ApiUrl::parse("https://relay.example.org/api/").expect("url");
        assert_eq!((url.tls, url.port, url.base.as_str()), (true, 443, "/api"));
        let url = ApiUrl::parse("http://[::1]:7480/").expect("url");
        assert_eq!((url.host.as_str(), url.port), ("::1", 7480));
        assert!(url.is_local_or_tls());
        assert!(
            !ApiUrl::parse("http://relay.example.org")
                .expect("url")
                .is_local_or_tls()
        );
        for bad in [
            "ftp://x",
            "http://",
            "http://x:0",
            "http://x:99999",
            "http://u@x",
            "x:80",
        ] {
            assert!(ApiUrl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn https_needs_a_pinned_fingerprint() {
        config("http://127.0.0.1:7480", None)
            .validate()
            .expect("http needs no pin");
        assert!(
            config("https://relay.example.org", None)
                .validate()
                .is_err()
        );
        assert!(
            config("https://relay.example.org", Some("abcd"))
                .validate()
                .is_err()
        );
        let pinned = config(
            "https://relay.example.org",
            Some(&format!("{}AB", "AB:".repeat(31))),
        );
        pinned.validate().expect("pinned");
        assert_eq!(pinned.api_fp().expect("fp"), Some("ab".repeat(32)));
    }

    #[test]
    fn the_secret_is_read_trimmed_long_enough_and_hidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secret");
        let long = "s".repeat(MIN_SECRET_LEN);
        std::fs::write(&path, format!("  {long} \nignored\n")).expect("write");
        restrict(&path);
        let mut config = config("http://127.0.0.1:7480", None);
        config.api_secret_file = Some(path.clone());
        let secret = config.load_secret().expect("secret");
        assert_eq!(secret.expose(), long);
        assert!(!format!("{secret:?}").contains(&long));
        std::fs::write(&path, "short\n").expect("write");
        assert!(config.load_secret().is_err(), "too short");
        std::fs::write(&path, format!("{long}\u{1b}x\n")).expect("write");
        assert!(config.load_secret().is_err(), "control character");
        config.api_secret_file = None;
        assert!(config.load_secret().is_err(), "no source");
    }

    #[cfg(unix)]
    fn restrict(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn restrict(_: &Path) {}

    #[cfg(unix)]
    #[test]
    fn a_secret_file_open_to_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secret");
        std::fs::write(&path, "s".repeat(MIN_SECRET_LEN)).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let mut config = config("http://127.0.0.1:7480", None);
        config.api_secret_file = Some(path);
        let error = config.load_secret().expect_err("group-readable");
        assert!(error.to_string().contains("chmod 600"), "{error}");
    }

    #[test]
    fn plain_http_to_another_host_needs_an_explicit_opt_in() {
        let mut remote = config("http://relay.example.org:7480", None);
        let error = remote.validate().expect_err("refused");
        assert!(error.to_string().contains("allow_insecure_http"), "{error}");
        remote.allow_insecure_http = true;
        remote.validate().expect("opted in");
        config("http://localhost:7480", None)
            .validate()
            .expect("loopback is fine");
    }

    #[test]
    fn limits_and_groups_are_checked() {
        let mut bad = config("http://127.0.0.1:7480", None);
        bad.max_pairs_per_cert = 0;
        assert!(bad.validate().is_err());
        let mut bad = config("http://127.0.0.1:7480", None);
        bad.allowed_groups = vec![" ".into()];
        assert!(bad.validate().is_err());
        let parsed: PeerConfig = toml::from_str(
            "api_url = \"http://127.0.0.1:7480\"\nallowed_groups = [\"relay-users\"]\n",
        )
        .expect("toml");
        assert_eq!(parsed.allowed_groups, vec!["relay-users".to_owned()]);
        assert_eq!(parsed.cert_requests_per_minute, CERT_REQUESTS_PER_MINUTE);
        assert_eq!(parsed.max_pairs_per_cert, MAX_PAIRS_PER_CERT);
    }
}
