//! Where a track comes from: a file in the media directory, an upload or an allowed URL, with
//! the checks of spec 04, FR-4.12 to FR-4.17.

use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey};
use symphonia::core::probe::Hint;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::music::config::{MusicConfig, UrlConfig};

/// Room for the status line and headers of a download, on top of the size cap.
const MAX_ANSWER_HEAD: usize = 16 * 1024;

/// What a `/play` request asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(String),
    Url(String),
    Upload(Vec<u8>),
}

impl Source {
    /// For the log: never the upload, never more of a URL than its host.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Url(_) => "url",
            Self::Upload(_) => "upload",
        }
    }
}

/// Why a source cannot be played; each maps to one HTTP status (FR-4.11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    Invalid(String),
    NotFound,
    Forbidden(String),
    TooLarge,
    TooLong,
    Fetch(String),
}

impl SourceError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::NotFound => 404,
            Self::Forbidden(_) => 403,
            Self::TooLarge => 413,
            Self::TooLong => 422,
            Self::Fetch(_) => 502,
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Self::Invalid(reason) | Self::Forbidden(reason) => reason.clone(),
            Self::NotFound => "file not found".to_owned(),
            Self::TooLarge => "track too large".to_owned(),
            Self::TooLong => "track too long".to_owned(),
            Self::Fetch(reason) => format!("fetch failed: {reason}"),
        }
    }
}

/// A source read into memory and checked to be an MP3 within the limits.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub title: String,
    pub duration: Option<Duration>,
    pub bytes: Arc<[u8]>,
}

/// Reads or downloads `source`, then checks size, format and length.
pub async fn load(source: Source, config: &MusicConfig) -> Result<Loaded, SourceError> {
    let max = config.max_track_bytes();
    let (fallback, bytes) = match source {
        Source::File(name) => {
            let dir = config
                .media_dir
                .as_deref()
                .ok_or_else(|| SourceError::Forbidden("no media_dir configured".to_owned()))?;
            let path = media_path(dir, &name)?;
            (file_title(&path), read_file(&path, max).await?)
        }
        Source::Url(url) => {
            let url = SourceUrl::parse(&url, &config.urls)?;
            (url.title(), fetch(&url, &config.urls, max).await?)
        }
        Source::Upload(bytes) if bytes.len() > max => return Err(SourceError::TooLarge),
        Source::Upload(bytes) => ("upload".to_owned(), bytes),
    };
    let max_len = config.max_track_len();
    let bytes: Arc<[u8]> = bytes.into();
    tokio::task::spawn_blocking(move || {
        let (title, duration) = probe(&bytes)?;
        if duration.is_some_and(|d| d > max_len) {
            return Err(SourceError::TooLong);
        }
        Ok(Loaded {
            title: title.unwrap_or(fallback),
            duration,
            bytes,
        })
    })
    .await
    .map_err(|e| SourceError::Invalid(format!("probe failed: {e}")))?
}

/// The file `requested` names inside `media_dir`; nothing outside it, also not through a
/// symlink (FR-4.12).
pub fn media_path(media_dir: &Path, requested: &str) -> Result<PathBuf, SourceError> {
    let relative = Path::new(requested);
    let plain = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if requested.is_empty() || !plain {
        return Err(SourceError::Forbidden(
            "file must be a relative path inside the media directory".to_owned(),
        ));
    }
    let root = media_dir
        .canonicalize()
        .map_err(|_| SourceError::Forbidden("media directory is not available".to_owned()))?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| SourceError::NotFound)?;
    if !path.starts_with(&root) {
        return Err(SourceError::Forbidden(
            "file is outside the media directory".to_owned(),
        ));
    }
    if !path.is_file() {
        return Err(SourceError::NotFound);
    }
    Ok(path)
}

fn file_title(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
}

async fn read_file(path: &Path, max: usize) -> Result<Vec<u8>, SourceError> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| SourceError::NotFound)?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| SourceError::Invalid(format!("file cannot be read: {e}")))?;
    if bytes.len() > max {
        return Err(SourceError::TooLarge);
    }
    Ok(bytes)
}

/// An `http(s)` URL taken apart and checked against the allowlist (FR-4.13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUrl {
    pub tls: bool,
    /// Lowercase host name or IP address (IPv6 without brackets).
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub target: String,
}

impl SourceUrl {
    pub fn parse(url: &str, config: &UrlConfig) -> Result<Self, SourceError> {
        if !config.enabled {
            return Err(SourceError::Forbidden(
                "URL sources are disabled".to_owned(),
            ));
        }
        let invalid = |why: &str| SourceError::Invalid(format!("invalid URL: {why}"));
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(invalid("only http and https"));
        };
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(invalid("contains spaces or control characters"));
        }
        let rest = rest.split('#').next().unwrap_or_default();
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, target) = rest.split_at(end);
        if authority.contains('@') {
            return Err(invalid("user info is not allowed"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or_else(|| invalid("broken IPv6"))?;
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
                .ok_or_else(|| invalid("bad port"))?,
            None if tls => 443,
            None => 80,
        };
        let host = host.to_ascii_lowercase();
        let host_chars = host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
        if host.is_empty() || !host_chars {
            return Err(invalid("bad host"));
        }
        if !host_allowed(&host, &config.allow_hosts) {
            return Err(SourceError::Forbidden(format!(
                "host {host} is not in allow_hosts"
            )));
        }
        let target = match target {
            "" => "/".to_owned(),
            t if t.starts_with('?') => format!("/{t}"),
            t => t.to_owned(),
        };
        Ok(Self {
            tls,
            host,
            port,
            target,
        })
    }

    /// The last path segment, as a title when the file has no tags.
    fn title(&self) -> String {
        let path = self.target.split('?').next().unwrap_or_default();
        path.rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or(&self.host)
            .to_owned()
    }

    fn host_header(&self) -> String {
        let default = if self.tls { 443 } else { 80 };
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == default {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// `host` is listed, or a subdomain of a listed `*.domain`.
pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    allow.iter().any(|entry| {
        let entry = entry.to_ascii_lowercase();
        match entry.strip_prefix("*.") {
            Some(domain) => host
                .strip_suffix(domain)
                .is_some_and(|sub| sub.len() > 1 && sub.ends_with('.')),
            None => host == entry,
        }
    })
}

/// An address on the public internet; everything a bot inside a network must not be tricked
/// into calling (FR-4.14) is not.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || first & 0xfe00 == 0xfc00 // unique local
                || first & 0xffc0 == 0xfe80 // link local
                || (first == 0x2001 && v6.segments()[1] == 0x0db8) // documentation
                || (first == 0x0064 && v6.segments()[1] == 0xff9b) // NAT64 can reach IPv4 inside
                || v6.segments()[..6] == [0; 6]) // IPv4-compatible and other reserved
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // carrier-grade NAT
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (18..20).contains(&b)) // benchmarking
        || a >= 240) // reserved
}

/// The first address of `url` the policy allows; the connection then goes to exactly this
/// address, so a second DNS answer cannot point it elsewhere.
async fn resolve(url: &SourceUrl, allow_private: bool) -> Result<SocketAddr, SourceError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((url.host.as_str(), url.port))
        .await
        .map_err(|_| SourceError::Fetch("host not found".to_owned()))?
        .collect();
    if addrs.is_empty() {
        return Err(SourceError::Fetch("host not found".to_owned()));
    }
    // Every answer must be public, so a mixed answer cannot be used to reach inside.
    if !allow_private && addrs.iter().any(|addr| !is_public(addr.ip())) {
        return Err(SourceError::Forbidden(
            "URL points to a private address".to_owned(),
        ));
    }
    Ok(addrs[0])
}

/// Downloads `url` with one plain GET: no redirects, at most `max` bytes, within the timeout
/// (FR-4.14 to FR-4.16).
pub async fn fetch(
    url: &SourceUrl,
    config: &UrlConfig,
    max: usize,
) -> Result<Vec<u8>, SourceError> {
    let timeout = Duration::from_secs(config.timeout_secs);
    tokio::time::timeout(timeout, async {
        let addr = resolve(url, config.allow_private).await?;
        let tcp = TcpStream::connect(addr)
            .await
            .map_err(|_| SourceError::Fetch("cannot connect".to_owned()))?;
        // HTTP/1.0 keeps the answer free of chunked encoding.
        let request = format!(
            "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: mumbot\r\nAccept: audio/mpeg\r\n\
             Connection: close\r\n\r\n",
            url.target,
            url.host_header()
        );
        let cap = max.saturating_add(MAX_ANSWER_HEAD);
        let raw = if url.tls {
            let name = ServerName::try_from(url.host.clone())
                .map_err(|_| SourceError::Invalid("invalid TLS server name".to_owned()))?;
            let stream = tls_connector()?
                .connect(name, tcp)
                .await
                .map_err(|e| SourceError::Fetch(format!("TLS: {e}")))?;
            exchange(stream, &request, cap).await?
        } else {
            exchange(tcp, &request, cap).await?
        };
        parse_download(&raw, max)
    })
    .await
    .map_err(|_| SourceError::Fetch(format!("no answer within {} s", timeout.as_secs())))?
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    request: &str,
    cap: usize,
) -> Result<Vec<u8>, SourceError> {
    let failed = |e: std::io::Error| SourceError::Fetch(e.to_string());
    stream.write_all(request.as_bytes()).await.map_err(failed)?;
    stream.flush().await.map_err(failed)?;
    let mut raw = Vec::new();
    (&mut stream)
        .take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut raw)
        .await
        .map_err(failed)?;
    Ok(raw)
}

/// Status 200 and a body of at most `max` bytes; anything else is refused.
pub fn parse_download(raw: &[u8], max: usize) -> Result<Vec<u8>, SourceError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| {
            if raw.len() > MAX_ANSWER_HEAD {
                SourceError::Fetch("answer header too large".to_owned())
            } else {
                SourceError::Fetch("malformed answer".to_owned())
            }
        })?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| SourceError::Fetch("malformed status line".to_owned()))?;
    match status {
        200 => {}
        300..=399 => return Err(SourceError::Fetch("redirects are not followed".to_owned())),
        other => return Err(SourceError::Fetch(format!("server answered {other}"))),
    }
    let mut body = raw[split + 4..].to_vec();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(SourceError::Fetch(
                "chunked answers are not supported".to_owned(),
            ));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length: usize = value
                .trim()
                .parse()
                .map_err(|_| SourceError::Fetch("malformed Content-Length".to_owned()))?;
            if length > max {
                return Err(SourceError::TooLarge);
            }
            if body.len() < length {
                return Err(SourceError::Fetch("answer cut short".to_owned()));
            }
            body.truncate(length);
        }
    }
    if body.len() > max {
        return Err(SourceError::TooLarge);
    }
    Ok(body)
}

/// Certificates checked against Mozilla's root store (FR-4.15).
fn tls_connector() -> Result<TlsConnector, SourceError> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| SourceError::Fetch(format!("TLS setup: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Checks that `bytes` is an MP3 and reads its title and length from the headers and tags.
pub fn probe(bytes: &Arc<[u8]>) -> Result<(Option<String>, Option<Duration>), SourceError> {
    let not_mp3 = || SourceError::Invalid("not an MP3 file".to_owned());
    let stream =
        MediaSourceStream::new(Box::new(Cursor::new(Arc::clone(bytes))), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|_| not_mp3())?;
    let track = probed.format.default_track().ok_or_else(not_mp3)?;
    let params = &track.codec_params;
    let duration = match (params.n_frames, params.sample_rate) {
        (Some(frames), Some(rate)) if rate > 0 => {
            Some(Duration::from_secs_f64(frames as f64 / f64::from(rate)))
        }
        _ => None,
    };
    let mut tags = Vec::new();
    if let Some(metadata) = probed.metadata.get()
        && let Some(revision) = metadata.current()
    {
        tags.extend(revision.tags().iter().cloned());
    }
    if let Some(revision) = probed.format.metadata().current() {
        tags.extend(revision.tags().iter().cloned());
    }
    let tag = |key: StandardTagKey| {
        tags.iter()
            .find(|tag| tag.std_key == Some(key))
            .map(|tag| tag.value.to_string())
            .filter(|value| !value.trim().is_empty())
    };
    let title = match (tag(StandardTagKey::Artist), tag(StandardTagKey::TrackTitle)) {
        (Some(artist), Some(title)) => Some(format!("{artist} – {title}")),
        (None, Some(title)) => Some(title),
        _ => None,
    };
    Ok((title, duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(hosts: &[&str]) -> UrlConfig {
        UrlConfig {
            enabled: true,
            allow_hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            allow_private: false,
            timeout_secs: 5,
        }
    }

    #[test]
    fn media_paths_stay_inside_the_directory() -> anyhow::Result<()> {
        let outside = tempfile::tempdir()?;
        let media = outside.path().join("media");
        std::fs::create_dir_all(media.join("sub"))?;
        std::fs::write(media.join("a.mp3"), b"x")?;
        std::fs::write(media.join("sub/b.mp3"), b"x")?;
        std::fs::write(outside.path().join("secret.toml"), b"x")?;

        assert!(media_path(&media, "a.mp3").is_ok());
        assert!(media_path(&media, "./sub/b.mp3").is_ok());
        assert_eq!(media_path(&media, "nope.mp3"), Err(SourceError::NotFound));
        assert_eq!(media_path(&media, "sub"), Err(SourceError::NotFound));
        for bad in ["../secret.toml", "sub/../../secret.toml", "/etc/passwd", ""] {
            assert!(
                matches!(media_path(&media, bad), Err(SourceError::Forbidden(_))),
                "{bad:?} must be refused"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("secret.toml"), media.join("link.mp3"))?;
            assert!(matches!(
                media_path(&media, "link.mp3"),
                Err(SourceError::Forbidden(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn urls_need_the_switch_and_an_allowed_host() {
        let disabled = UrlConfig::default();
        assert!(matches!(
            SourceUrl::parse("https://files.example.com/a.mp3", &disabled),
            Err(SourceError::Forbidden(_))
        ));
        let config = urls(&["files.example.com", "*.cdn.example.com"]);
        let url = SourceUrl::parse("https://Files.Example.com/music/a.mp3?x=1#top", &config);
        assert_eq!(
            url,
            Ok(SourceUrl {
                tls: true,
                host: "files.example.com".to_owned(),
                port: 443,
                target: "/music/a.mp3?x=1".to_owned(),
            })
        );
        assert!(SourceUrl::parse("http://eu.cdn.example.com:8000/a.mp3", &config).is_ok());
        for forbidden in [
            "https://cdn.example.com/a.mp3",
            "https://evil.com/a.mp3",
            "https://files.example.com.evil.com/a.mp3",
            "https://xcdn.example.com/a.mp3",
        ] {
            assert!(
                matches!(
                    SourceUrl::parse(forbidden, &config),
                    Err(SourceError::Forbidden(_))
                ),
                "{forbidden}"
            );
        }
        for invalid in [
            "ftp://files.example.com/a.mp3",
            "file:///etc/passwd",
            "https://user@files.example.com/a.mp3",
            "https://files.example.com:0/a.mp3",
            "https://files.example.com/a b.mp3",
            "https://files.example.com\r\nX: y/",
        ] {
            assert!(
                matches!(
                    SourceUrl::parse(invalid, &config),
                    Err(SourceError::Invalid(_))
                ),
                "{invalid}"
            );
        }
    }

    #[test]
    fn private_addresses_are_not_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
        ] {
            let ip: IpAddr = private.parse().expect("test address");
            assert!(!is_public(ip), "{private} must not be public");
        }
        for public in ["93.184.216.34", "1.1.1.1", "2606:4700::1111"] {
            let ip: IpAddr = public.parse().expect("test address");
            assert!(is_public(ip), "{public} is public");
        }
    }

    #[tokio::test]
    async fn loopback_hosts_are_refused_before_connecting() {
        let config = urls(&["localhost"]);
        let url = SourceUrl::parse("http://localhost:9/a.mp3", &config).expect("allowed host");
        assert!(matches!(
            fetch(&url, &config, 1000).await,
            Err(SourceError::Forbidden(_))
        ));
    }

    #[test]
    fn downloads_are_checked() {
        let ok = b"HTTP/1.0 200 OK\r\nContent-Length: 3\r\n\r\nabcdef";
        assert_eq!(parse_download(ok, 10), Ok(b"abc".to_vec()));
        let big = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\n";
        assert_eq!(parse_download(big, 10), Err(SourceError::TooLarge));
        let unannounced = b"HTTP/1.0 200 OK\r\n\r\n0123456789AB";
        assert_eq!(parse_download(unannounced, 10), Err(SourceError::TooLarge));
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/\r\n\r\n";
        assert!(matches!(
            parse_download(redirect, 10),
            Err(SourceError::Fetch(_))
        ));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc";
        assert!(matches!(
            parse_download(chunked, 10),
            Err(SourceError::Fetch(_))
        ));
    }

    #[test]
    fn non_mp3_data_is_refused() {
        assert!(matches!(
            probe(&Arc::from(&b"definitely not audio"[..])),
            Err(SourceError::Invalid(_))
        ));
    }
}
