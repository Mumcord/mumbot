//! The one avatar download: https only, within the timeout and below the size cap (issue #460).
//! Redirects are followed, but only to https, so an avatar never arrives over a plain connection.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Room for the status line and the headers of an answer, on top of the size cap.
const MAX_HEAD: usize = 16 * 1024;
/// Redirects followed before the download is given up on.
const MAX_REDIRECTS: usize = 5;

/// Host, port and path of an https URL.
#[derive(Debug, PartialEq)]
struct Target {
    host: String,
    port: u16,
    /// Path and query, starting with `/`.
    path: String,
}

impl Target {
    fn host_header(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// What one answer carries: the image, or the target of a redirect.
#[derive(Debug, PartialEq)]
enum Answer {
    Body(Vec<u8>),
    Location(String),
}

/// Downloads `url` once at startup: at most `cap` bytes, `timeout` for all hops together.
pub async fn get(url: &str, cap: usize, timeout: Duration) -> Result<Vec<u8>> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut target = parse(url)?;
    for _ in 0..=MAX_REDIRECTS {
        match fetch(&target, cap, deadline).await {
            Ok(Answer::Body(body)) => return Ok(body),
            Ok(Answer::Location(next)) => target = parse(&next)?,
            Err(error) => return Err(error),
        }
    }
    Err(anyhow!("too many redirects"))
}

/// Parses an https URL; plain http and a redirect to it are refused.
fn parse(url: &str) -> Result<Target> {
    let invalid = |why: &str| anyhow!("invalid avatar URL: {why}");
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| invalid("must be https"))?;
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("contains spaces or control characters"));
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.contains('@') {
        return Err(invalid("user info is not allowed"));
    }
    if authority.is_empty() {
        return Err(invalid("no host"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host,
            Some(
                port.parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| invalid("bad port"))?,
            ),
        ),
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
    {
        return Err(invalid("bad host"));
    }
    let path = match path {
        "" => "/".to_owned(),
        p if p.starts_with('?') => format!("/{p}"),
        p => p.to_owned(),
    };
    Ok(Target {
        host,
        port: port.unwrap_or(443),
        path,
    })
}

async fn fetch(target: &Target, cap: usize, deadline: tokio::time::Instant) -> Result<Answer> {
    let passed = |_| anyhow!("no answer within the download timeout");
    let addr = tokio::time::timeout_at(deadline, resolve(target))
        .await
        .map_err(passed)?
        .with_context(|| format!("avatar host {:?}", target.host))?;
    let tcp = tokio::time::timeout_at(deadline, TcpStream::connect(addr))
        .await
        .map_err(passed)?
        .with_context(|| format!("connecting to {}", addr))?;
    let name = ServerName::try_from(target.host.clone()).context("invalid TLS server name")?;
    let mut stream = tokio::time::timeout_at(deadline, tls()?.connect(name, tcp))
        .await
        .map_err(passed)?
        .context("TLS to the avatar host failed")?;
    // HTTP/1.0 keeps the answer free of chunked encoding.
    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: mumbot\r\nAccept: image/*\r\n\
         Connection: close\r\n\r\n",
        target.path,
        target.host_header()
    );
    let raw = tokio::time::timeout_at(deadline, read(&mut stream, &request, cap))
        .await
        .map_err(passed)?
        .context("reading the answer failed")?;
    parse_answer(&raw, cap)
}

async fn resolve(target: &Target) -> Result<SocketAddr> {
    tokio::net::lookup_host((target.host.as_str(), target.port))
        .await
        .ok()
        .and_then(|mut addrs| addrs.next())
        .ok_or_else(|| anyhow!("avatar host {:?} not found", target.host))
}

async fn read<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    request: &str,
    cap: usize,
) -> Result<Vec<u8>> {
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut raw = Vec::new();
    (&mut *stream)
        .take(
            u64::try_from(cap + MAX_HEAD)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut raw)
        .await?;
    Ok(raw)
}

/// Status 200 gives the body, 3xx the Location; everything else is refused. Chunked answers are
/// not supported, and the body must stay below `cap`.
fn parse_answer(raw: &[u8], cap: usize) -> Result<Answer> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("no end of the answer header"))?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| anyhow!("malformed status line"))?;
    if (300..=399).contains(&status) {
        let location = header(&head, "location")
            .ok_or_else(|| anyhow!("redirect without a Location"))?
            .to_owned();
        return Ok(Answer::Location(location));
    }
    if status != 200 {
        return Err(anyhow!("the avatar server answered {status}"));
    }
    let mut body = raw[split + 4..].to_vec();
    for line in head.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(anyhow!("chunked answers are not supported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length: usize = value
                .trim()
                .parse()
                .map_err(|_| anyhow!("malformed Content-Length"))?;
            if length > cap {
                return Err(anyhow!("image is larger than the {} byte cap", cap));
            }
            body.truncate(length);
        }
    }
    if body.is_empty() {
        return Err(anyhow!("the answer has no image"));
    }
    if body.len() > cap {
        return Err(anyhow!("image is larger than the {} byte cap", cap));
    }
    Ok(Answer::Body(body))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

/// Certificates checked against Mozilla's root store, as for the music module's URLs.
fn tls() -> Result<TlsConnector> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("TLS setup")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(TlsConnector::from(std::sync::Arc::new(config)))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_urls_are_accepted() {
        let target = |host: &str, port: u16, path: &str| Target {
            host: host.to_owned(),
            port,
            path: path.to_owned(),
        };
        assert_eq!(
            parse("https://example.org/a.png").expect("https"),
            target("example.org", 443, "/a.png")
        );
        assert_eq!(
            parse("https://Example.org:8443/x?y=1#top").expect("https"),
            target("example.org", 8443, "/x?y=1")
        );
        assert_eq!(
            parse("https://example.org").expect("https"),
            target("example.org", 443, "/")
        );
        for url in [
            "http://example.org/a.png",
            "ftp://example.org/a.png",
            "https://user@example.org/a.png",
            "https://",
            "https://example.org/a b.png",
            "https://exam ple.org/a.png",
            "https://example.org:0/a.png",
        ] {
            assert!(parse(url).is_err(), "{url}");
        }
    }

    #[test]
    fn answers_are_parsed_within_the_cap() {
        let answer =
            parse_answer(b"HTTP/1.0 200 OK\r\nContent-Length: 5\r\n\r\nimage", 100).expect("ok");
        assert_eq!(answer, Answer::Body(b"image".to_vec()));
        let redirect = parse_answer(
            b"HTTP/1.0 302 Found\r\nLocation: https://cdn.example.org/a.png\r\n\r\n",
            100,
        )
        .expect("redirect");
        assert_eq!(
            redirect,
            Answer::Location("https://cdn.example.org/a.png".to_owned())
        );
        assert!(parse("/a.png").is_err(), "a relative redirect is not https");
        assert!(parse_answer(b"HTTP/1.0 404 Not Found\r\n\r\n", 100).is_err());
        assert!(
            parse_answer(
                b"HTTP/1.0 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nx",
                100
            )
            .is_err()
        );
        assert!(parse_answer(b"HTTP/1.0 200 OK\r\nContent-Length: 500\r\n\r\nx", 100).is_err());
        assert!(parse_answer(b"garbage", 100).is_err());
    }
}
