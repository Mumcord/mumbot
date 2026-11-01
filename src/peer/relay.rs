//! The relay's allowlist API (mumpeer, spec 04 section 6.6), behind a narrow trait so the module
//! logic can be tested against a mock.
//!
//! HTTP/1.1 with JSON and `Authorization: Bearer <secret>`:
//! - `GET /v1/status` → `{"boot_id","host","udp","tcp","fp",…}`
//! - `POST /v1/pairs` with `{"a","b"}` (two certificate hashes) → `201 {"tok","expires_in"}`,
//!   `503` when the relay is full, `429` when the bot asks too often
//! - `DELETE /v1/peers/{hash}` → `204`; the relay drops the hash from all pairs
//!
//! Three calls do not justify an HTTP library: the client below sends one request per
//! connection and reads the answer up to a size cap. Over `https` the relay's self-signed
//! certificate is pinned by its SHA-256 (`api_fp`) instead of checked against authorities.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::peer::config::{ApiUrl, PeerConfig, Secret, normalize_sha256};

/// Upper bound for one API call; the bot never waits longer on the relay.
pub const API_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest answer the bot reads; the real ones are a few hundred bytes.
const MAX_RESPONSE: u64 = 16 * 1024;

/// What `GET /v1/status` reports; everything the announcement needs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Status {
    /// Changes when the relay restarts.
    pub boot_id: String,
    /// Public address clients connect to.
    pub host: String,
    pub udp: u16,
    pub tcp: u16,
    /// SHA-256 of the relay certificate.
    pub fp: String,
}

impl Status {
    /// Checks what goes out to clients, so the bot never announces something they must reject
    /// (spec 04 section 6.4: host, ports and fingerprint rules).
    pub fn validate(mut self) -> Result<Self> {
        if !valid_host(&self.host) {
            bail!("relay API status: invalid host");
        }
        if self.udp == 0 || self.tcp == 0 {
            bail!("relay API status: invalid port");
        }
        self.fp = normalize_sha256(&self.fp).context("relay API status: invalid fp")?;
        if self.boot_id.is_empty() || self.boot_id.len() > 64 {
            bail!("relay API status: invalid boot_id");
        }
        Ok(self)
    }
}

/// A token for one pair, as the relay issued it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Pair {
    pub tok: String,
    pub expires_in: u64,
}

/// Why `POST /v1/pairs` gave no token.
#[derive(Debug)]
pub enum PairError {
    /// `503`: the relay has no room for another session.
    Full,
    /// `429`: the bot asked too often.
    RateLimited,
    /// Anything else: unreachable, wrong secret, unexpected answer.
    Unavailable(anyhow::Error),
}

pub trait RelayApi {
    fn status(&self) -> impl Future<Output = Result<Status>> + Send;
    /// Creates a token for the pair of certificate hashes `a` and `b`.
    fn create_pair(&self, a: &str, b: &str)
    -> impl Future<Output = Result<Pair, PairError>> + Send;
    /// Drops `hash` from all pairs and closes its sessions.
    fn remove_peer(&self, hash: &str) -> impl Future<Output = Result<()>> + Send;
}

/// The real API.
pub struct HttpRelay {
    url: ApiUrl,
    secret: Secret,
    /// Set for `https`.
    tls: Option<TlsConnector>,
}

impl HttpRelay {
    pub fn new(config: &PeerConfig, secret: Secret) -> Result<Self> {
        let url = config.url()?;
        let tls = match (url.tls, config.api_fp()?) {
            (true, Some(pin)) => Some(pinned_connector(pin)?),
            (true, None) => bail!("peer: an https api_url needs api_fp"),
            (false, _) => None,
        };
        Ok(Self { url, secret, tls })
    }

    async fn call(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<Answer> {
        tokio::time::timeout(API_TIMEOUT, self.exchange(method, path, body))
            .await
            .map_err(|_| anyhow!("no answer within {} s", API_TIMEOUT.as_secs()))?
    }

    async fn exchange(&self, method: &str, path: &str, body: Option<Vec<u8>>) -> Result<Answer> {
        let tcp = TcpStream::connect((self.url.host.as_str(), self.url.port))
            .await
            .context("cannot connect")?;
        let host = if self.url.host.contains(':') {
            format!("[{}]:{}", self.url.host, self.url.port)
        } else {
            format!("{}:{}", self.url.host, self.url.port)
        };
        let body = body.unwrap_or_default();
        let mut request = format!(
            "{method} {base}{path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {secret}\r\n\
             Accept: application/json\r\nConnection: close\r\nContent-Length: {length}\r\n",
            base = self.url.base,
            secret = self.secret.expose(),
            length = body.len(),
        )
        .into_bytes();
        if !body.is_empty() {
            request.extend_from_slice(b"Content-Type: application/json\r\n");
        }
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(&body);
        match &self.tls {
            Some(connector) => {
                let name = ServerName::try_from(self.url.host.clone())
                    .context("invalid TLS server name")?;
                let stream = connector
                    .connect(name, tcp)
                    .await
                    .context("TLS handshake failed (check api_fp)")?;
                send(stream, &request).await
            }
            None => send(tcp, &request).await,
        }
    }
}

/// Status code and body of an answer.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

async fn send<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, request: &[u8]) -> Result<Answer> {
    stream.write_all(request).await.context("sending failed")?;
    stream.flush().await.context("sending failed")?;
    let mut raw = Vec::new();
    (&mut stream)
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut raw)
        .await
        .context("reading the answer failed")?;
    if raw.len() as u64 > MAX_RESPONSE {
        bail!("answer too large");
    }
    parse_answer(&raw)
}

/// Reads status line, headers and a `Content-Length` (or read-to-close) body.
fn parse_answer(raw: &[u8]) -> Result<Answer> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed answer")?;
    let head = std::str::from_utf8(&raw[..split]).context("malformed answer")?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1."))
        .and_then(|rest| rest.get(2..5))
        .and_then(|code| code.parse::<u16>().ok())
        .context("malformed status line")?;
    let mut body = raw[split + 4..].to_vec();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            bail!("chunked answers are not supported");
        }
        if name.eq_ignore_ascii_case("content-length") {
            let length: usize = value.trim().parse().context("malformed Content-Length")?;
            if body.len() < length {
                bail!("answer cut short");
            }
            body.truncate(length);
        }
    }
    Ok(Answer { status, body })
}

#[derive(Serialize)]
struct PairRequest<'a> {
    a: &'a str,
    b: &'a str,
}

impl RelayApi for HttpRelay {
    async fn status(&self) -> Result<Status> {
        let answer = self
            .call("GET", "/v1/status", None)
            .await
            .context("relay API status")?;
        if answer.status != 200 {
            bail!("relay API status: HTTP {}", answer.status);
        }
        let status: Status =
            serde_json::from_slice(&answer.body).context("relay API status: invalid answer")?;
        status.validate()
    }

    async fn create_pair(&self, a: &str, b: &str) -> Result<Pair, PairError> {
        let body = serde_json::to_vec(&PairRequest { a, b })
            .map_err(|e| PairError::Unavailable(e.into()))?;
        let answer = self
            .call("POST", "/v1/pairs", Some(body))
            .await
            .map_err(|e| PairError::Unavailable(e.context("relay API pairs")))?;
        match answer.status {
            200 | 201 => serde_json::from_slice(&answer.body).map_err(|e| {
                PairError::Unavailable(anyhow!("relay API pairs: invalid answer: {e}"))
            }),
            503 => Err(PairError::Full),
            429 => Err(PairError::RateLimited),
            status => Err(PairError::Unavailable(anyhow!(
                "relay API pairs: HTTP {status}"
            ))),
        }
    }

    async fn remove_peer(&self, hash: &str) -> Result<()> {
        // Certificate hashes are hex; anything else must not reach the URL path.
        if hash.is_empty() || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("relay API peers: not a certificate hash");
        }
        let answer = self
            .call("DELETE", &format!("/v1/peers/{hash}"), None)
            .await
            .context("relay API peers")?;
        if !(200..300).contains(&answer.status) {
            bail!("relay API peers: HTTP {}", answer.status);
        }
        Ok(())
    }
}

/// DNS name (at most 253 characters of letter-digit-hyphen labels) or an IP literal that is not
/// unspecified, multicast or broadcast.
fn valid_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let broadcast = matches!(ip, std::net::IpAddr::V4(v4) if v4.is_broadcast());
        return !ip.is_unspecified() && !ip.is_multicast() && !broadcast;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// TLS 1.3 client that accepts exactly the certificate with SHA-256 `pin`.
fn pinned_connector(pin: String) -> Result<TlsConnector> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS setup")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

#[derive(Debug)]
struct Pinned {
    pin: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        if hex == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "relay API certificate does not match api_fp".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_parsed_with_and_without_length() {
        let answer =
            parse_answer(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}junk").expect("ok");
        assert_eq!((answer.status, answer.body.as_slice()), (201, &b"{}"[..]));
        let answer = parse_answer(b"HTTP/1.0 204 No Content\r\n\r\n").expect("ok");
        assert_eq!((answer.status, answer.body.len()), (204, 0));
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n{}").is_err());
        assert!(parse_answer(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").is_err());
        assert!(parse_answer(b"garbage").is_err());
    }

    #[test]
    fn status_is_checked_before_it_is_announced() {
        let good = Status {
            boot_id: "0123456789abcdef".into(),
            host: "relay.example.org".into(),
            udp: 443,
            tcp: 443,
            fp: "AB".repeat(32),
        };
        assert_eq!(good.clone().validate().expect("valid").fp, "ab".repeat(32));
        let bad: [fn(&mut Status); 7] = [
            |s| s.host = "0.0.0.0".into(),
            |s| s.host = "224.0.0.1".into(),
            |s| s.host = "255.255.255.255".into(),
            |s| s.host = "bad_host.example".into(),
            |s| s.udp = 0,
            |s| s.fp = "abc".into(),
            |s| s.boot_id = String::new(),
        ];
        for change in bad {
            let mut status = good.clone();
            change(&mut status);
            assert!(status.validate().is_err());
        }
        for host in ["127.0.0.1", "::1", "192.168.1.2", "relay-1.local"] {
            assert!(valid_host(host), "{host}");
        }
    }

    /// A one-shot HTTPS relay with a fresh self-signed certificate; returns its URL and pin.
    async fn tls_relay() -> (String, String) {
        let generated =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
        let der = generated.cert.der().clone();
        let pin = ring::digest::digest(&ring::digest::SHA256, der.as_ref())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(generated.signing_key.serialize_der().into());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .expect("server config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!(
            "https://localhost:{}",
            listener.local_addr().expect("addr").port()
        );
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    continue;
                };
                let mut buf = [0u8; 4096];
                let _ = tls.read(&mut buf).await;
                let body = format!(
                    r#"{{"boot_id":"b","host":"relay.test","udp":443,"tcp":443,"fp":"{}"}}"#,
                    "ab".repeat(32)
                );
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = tls.write_all(answer.as_bytes()).await;
                let _ = tls.shutdown().await;
            }
        });
        (url, pin)
    }

    fn relay(url: &str, pin: &str) -> HttpRelay {
        let config = PeerConfig {
            api_fp: Some(pin.into()),
            ..PeerConfig::new(url)
        };
        HttpRelay::new(&config, Secret::new("s".repeat(32))).expect("relay")
    }

    #[tokio::test]
    async fn https_accepts_only_the_pinned_certificate() {
        let (url, pin) = tls_relay().await;
        let status = relay(&url, &pin)
            .status()
            .await
            .expect("pinned certificate");
        assert_eq!(status.host, "relay.test");
        let error = relay(&url, &"00".repeat(32))
            .status()
            .await
            .expect_err("another certificate");
        assert!(format!("{error:#}").contains("api_fp"), "{error:#}");
    }
}
