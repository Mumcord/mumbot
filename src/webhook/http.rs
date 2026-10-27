//! Minimal HTTP/1.1 handling for the webhook API: three routes, no framework.
//!
//! The handler is separated from the connection: it gets a parsed request plus an `Api` view of
//! the bot, so the status codes of the spec (AC-1.3) can be tested without a server.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::webhook::config::{ApiKey, WebhookConfig};
use crate::webhook::{MAX_BODY_BYTES, markdown};

/// Header the API key is read from (Q2.2).
pub const KEY_HEADER: &str = "X-API-Key";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// What the caller declared; the body is only read up to `MAX_BODY_BYTES`.
    pub declared_len: usize,
}

impl Request {
    /// Header lookup, case-insensitive as HTTP requires.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub json: Option<String>,
    /// Key label and channel id of the request, for the log (FR-1.20). Never the key value.
    pub key: Option<String>,
    pub channel: Option<u32>,
}

impl Response {
    fn ok(status: u16, json: impl Into<String>) -> Self {
        Self {
            status,
            json: Some(json.into()),
            key: None,
            channel: None,
        }
    }

    /// Fills in what the request log needs (FR-1.20).
    fn audit(mut self, key: Option<&ApiKey>, channel: Option<u32>) -> Self {
        self.key = key.map(|entry| entry.label.clone());
        self.channel = channel;
        self
    }

    fn error(status: u16, reason: &str) -> Self {
        Self::ok(status, format!("{{\"error\":{}}}", quote(reason)))
    }

    /// Full HTTP/1.1 answer. One request per connection, so the answer closes it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = self.json.as_deref().unwrap_or("");
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len(),
            status = self.status
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(body.as_bytes());
        out
    }
}

/// What the caller sends: channel id plus text. The sender label is not part of the MVP (Q3.3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MessageRequest {
    pub channel: u32,
    pub message: String,
}

/// The part of the bot the handler needs. `BotApi` is the real implementation, tests use a fake.
pub trait Api {
    fn connected(&self) -> impl Future<Output = bool> + Send;
    fn uptime(&self) -> impl Future<Output = Duration> + Send;
    fn channel_exists(&self, id: u32) -> impl Future<Output = bool> + Send;
    /// Channels the key may write to, names resolved against the live tree (FR-1.4).
    fn allowed_ids(&self, key: &ApiKey) -> impl Future<Output = Vec<u32>> + Send;
    /// `GET /channels` answer: only the channels of the presented key (Round 5).
    fn allowed_channels(&self, key: &ApiKey) -> impl Future<Output = Vec<(u32, String)>> + Send;
    /// Cap for one message: the spec cap, or the smaller value the server reports (FR-1.6).
    fn message_cap(&self) -> impl Future<Output = usize> + Send;
    /// Sends the message and reports whether the server accepted it (Q2.10).
    fn deliver(
        &self,
        channel_id: u32,
        html: &str,
    ) -> impl Future<Output = Result<(), DeliveryError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryError {
    NotConnected,
    /// The server refused the message (no chat permission on that channel).
    Refused,
}

impl DeliveryError {
    fn response(&self) -> Response {
        match self {
            Self::NotConnected => Response::error(503, "not connected"),
            Self::Refused => Response::error(503, "server refused the message"),
        }
    }
}

/// Requests per key per minute (Q2.9).
#[derive(Debug)]
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    state: HashMap<String, (Instant, u32)>,
}

impl RateLimiter {
    pub fn new(limit: u32) -> Self {
        Self {
            limit,
            window: Duration::from_secs(60),
            state: HashMap::new(),
        }
    }

    pub fn allow(&mut self, label: &str, now: Instant) -> bool {
        let entry = self.state.entry(label.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            entry.0 = now;
            entry.1 = 0;
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// Handles one parsed request and produces the answer (Q2.10, Q4.3, Q4.6).
pub async fn handle<A: Api>(
    request: &Request,
    config: &WebhookConfig,
    api: &A,
    limiter: &mut RateLimiter,
    now: Instant,
) -> Response {
    match request.path.as_str() {
        "/health" if request.method == "GET" => Response::ok(
            200,
            format!(
                "{{\"connected\":{connected},\"uptime_seconds\":{uptime},\"server\":\"{server}\"}}",
                connected = api.connected().await,
                uptime = api.uptime().await.as_secs(),
                server = config.server.host
            ),
        ),
        "/channels" if request.method == "GET" => match authenticated(request, config) {
            Some(key) => {
                let channels = api
                    .allowed_channels(key)
                    .await
                    .iter()
                    .map(|(id, name)| format!("{{\"id\":{id},\"name\":{}}}", quote(name)))
                    .collect::<Vec<_>>()
                    .join(",");
                Response::ok(200, format!("{{\"channels\":[{channels}]}}")).audit(Some(key), None)
            }
            None => Response::error(401, "unknown or missing key"),
        },
        "/message" if request.method == "POST" => message(request, config, api, limiter, now).await,
        "/message" | "/channels" | "/health" => Response::error(405, "wrong method"),
        _ => Response::error(404, "not found"),
    }
}

async fn message<A: Api>(
    request: &Request,
    config: &WebhookConfig,
    api: &A,
    limiter: &mut RateLimiter,
    now: Instant,
) -> Response {
    // The key is checked before the body is parsed (Q4.6).
    let Some(key) = authenticated(request, config) else {
        return Response::error(401, "unknown or missing key");
    };
    if !limiter.allow(&key.label, now) {
        return Response::error(429, "rate limit reached").audit(Some(key), None);
    }
    if request.declared_len > MAX_BODY_BYTES {
        return Response::error(413, "request body too large").audit(Some(key), None);
    }
    let Ok(parsed) = serde_json::from_slice::<MessageRequest>(&request.body) else {
        return Response::error(400, "invalid JSON body").audit(Some(key), None);
    };
    if parsed.message.trim().is_empty() {
        return Response::error(400, "empty message").audit(Some(key), Some(parsed.channel));
    }
    // FR-1.6: the server's own limit counts when it is smaller.
    if parsed.message.chars().count() > api.message_cap().await {
        return Response::error(400, "message too long");
    }
    let audit = |response: Response| response.audit(Some(key), Some(parsed.channel));
    if !api.channel_exists(parsed.channel).await {
        return audit(Response::error(404, "channel not found"));
    }
    if !api.allowed_ids(key).await.contains(&parsed.channel) {
        return audit(Response::error(403, "channel not allowed for this key"));
    }
    if !api.connected().await {
        return audit(Response::error(503, "not connected"));
    }
    match api
        .deliver(parsed.channel, &markdown::render_html(&parsed.message))
        .await
    {
        Ok(()) => audit(Response::ok(200, "{\"delivered\":true}")),
        Err(error) => audit(error.response()),
    }
}

fn authenticated<'a>(request: &Request, config: &'a WebhookConfig) -> Option<&'a ApiKey> {
    request
        .header(KEY_HEADER)
        .and_then(|key| config.find_key(key))
}

fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            control if control.is_control() => out.push_str(&format!("\\u{:04x}", control as u32)),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parses a request that is fully read (headers and body in one buffer).
pub fn parse_request(bytes: &[u8]) -> Option<Request> {
    let (head, body) = split_at(bytes, b"\r\n\r\n")?;
    let mut lines = head.split(|b| *b == b'\r').filter(|line| !line.is_empty());
    let first = std::str::from_utf8(lines.next()?).ok()?;
    let parts = first.split(' ').collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = std::str::from_utf8(line).ok()?.split_once(':')?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Some(Request {
        declared_len: body.len(),
        body: body.to_vec(),
        method: parts[0].to_owned(),
        path: parts[1].to_owned(),
        headers,
    })
}

fn split_at<'a>(bytes: &'a [u8], pattern: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let pos = bytes
        .windows(pattern.len())
        .position(|window| window == pattern)?;
    Some((&bytes[..pos], &bytes[pos + pattern.len()..]))
}

/// Listens and answers one request per connection; requests run concurrently (Q3.6).
pub async fn serve<A: Api + Send + Sync + 'static>(
    listener: TcpListener,
    config: Arc<WebhookConfig>,
    api: Arc<A>,
) -> Result<()> {
    let limiter = Arc::new(tokio::sync::Mutex::new(RateLimiter::new(
        config.rate_limit(),
    )));
    while let Ok((mut socket, peer)) = listener.accept().await {
        let (config, api, limiter) = (Arc::clone(&config), Arc::clone(&api), Arc::clone(&limiter));
        tokio::spawn(async move {
            let request = match read_request(&mut socket).await {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(peer = %peer, %error, "request could not be read");
                    return;
                }
            };
            let response = handle(
                &request,
                &config,
                api.as_ref(),
                &mut *limiter.lock().await,
                Instant::now(),
            )
            .await;
            // Only request metadata is logged: peer, key label, channel, status. Never the key
            // value, never the message text (FR-1.20, Q3.7).
            tracing::info!(
                peer = %peer,
                path = %request.path,
                key = response.key.as_deref().unwrap_or("-"),
                // 0 is a real channel (Root), so an empty value is shown as "-".
                channel = response
                    .channel
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                status = response.status,
                "request"
            );
            if let Err(error) = socket.write_all(&response.to_bytes()).await {
                tracing::warn!(peer = %peer, %error, "answer could not be sent");
            }
            let mut scratch = [0u8; 512];
            // Drain what the client already sent (a rejected body is not read), so closing the
            // connection does not reset the answer before the client can read it.
            let timeout = Duration::from_millis(200);
            let _ = tokio::time::timeout(timeout, socket.read(&mut scratch)).await;
        });
    }
    Ok(())
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    while split_at(&buffer, b"\r\n\r\n").is_none() {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            anyhow::bail!("connection closed before the headers ended");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_BODY_BYTES + 4096 {
            anyhow::bail!("request header too large");
        }
    }
    let request = parse_request(&buffer).ok_or_else(|| anyhow::anyhow!("malformed request"))?;
    let length: usize = request
        .header("Content-Length")
        .unwrap_or("0")
        .parse()
        .unwrap_or(0);
    // Read the body, but never more than the cap.
    let mut body = request.body;
    while body.len() < length.min(MAX_BODY_BYTES) {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(Request {
        declared_len: length,
        body,
        ..request
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::MAX_MESSAGE_CHARS;
    use crate::webhook::config::ChannelRef;

    struct FakeApi {
        connected: bool,
        channels: Vec<(u32, String)>,
        deliver_result: Result<(), DeliveryError>,
        /// 0 means "the server reports no limit", so the spec cap applies.
        server_cap: usize,
    }

    impl Api for FakeApi {
        async fn connected(&self) -> bool {
            self.connected
        }
        async fn uptime(&self) -> Duration {
            Duration::from_secs(1234)
        }
        async fn channel_exists(&self, id: u32) -> bool {
            self.channels.iter().any(|(channel, _)| *channel == id)
        }
        async fn allowed_ids(&self, key: &ApiKey) -> Vec<u32> {
            let mut ids: Vec<u32> = key
                .channels
                .iter()
                .filter_map(|entry| match entry {
                    ChannelRef::Id(id) => Some(*id),
                    ChannelRef::Name(name) => self
                        .channels
                        .iter()
                        .find(|(_, channel)| channel == name)
                        .map(|(id, _)| *id),
                })
                .collect();
            ids.sort_unstable();
            ids.dedup();
            ids
        }
        async fn allowed_channels(&self, key: &ApiKey) -> Vec<(u32, String)> {
            self.allowed_ids(key)
                .await
                .into_iter()
                .filter_map(|id| {
                    self.channels
                        .iter()
                        .find(|(channel, _)| *channel == id)
                        .cloned()
                })
                .collect()
        }
        async fn message_cap(&self) -> usize {
            if self.server_cap == 0 {
                MAX_MESSAGE_CHARS
            } else {
                self.server_cap.min(MAX_MESSAGE_CHARS)
            }
        }
        async fn deliver(&self, _channel: u32, _html: &str) -> Result<(), DeliveryError> {
            self.deliver_result.clone()
        }
    }

    fn config() -> WebhookConfig {
        let text = r#"
[[keys]]
key = "test-key-1"
label = "ci"
channels = ["Lobby", 42]
"#;
        toml::from_str(text).expect("config")
    }

    fn request(method: &str, path: &str, key: Option<&str>, body: &str) -> Request {
        let mut headers = Vec::new();
        if let Some(value) = key {
            headers.push((KEY_HEADER.to_owned(), value.to_owned()));
        }
        Request {
            method: method.to_owned(),
            path: path.to_owned(),
            headers,
            body: body.as_bytes().to_vec(),
            declared_len: body.len(),
        }
    }

    fn fake(connected: bool, deliver_result: Result<(), DeliveryError>) -> FakeApi {
        FakeApi {
            connected,
            channels: vec![(42, "Lobby".to_owned()), (7, "Raid".to_owned())],
            deliver_result,
            server_cap: 0,
        }
    }

    #[tokio::test]
    async fn the_server_reported_cap_wins_when_it_is_smaller() {
        let api = FakeApi {
            connected: true,
            channels: vec![(42, "Lobby".to_owned())],
            deliver_result: Ok(()),
            server_cap: 1000,
        };
        let config = config();
        let mut limiter = RateLimiter::new(config.rate_limit());
        let now = Instant::now();
        let over = "x".repeat(1001);
        let body = format!(r#"{{"channel":42,"message":"{over}"}}"#);
        assert_eq!(
            handle(
                &request("POST", "/message", Some("test-key-1"), &body),
                &config,
                &api,
                &mut limiter,
                now
            )
            .await
            .status,
            400,
            "the server only accepts 1000 characters"
        );
        let fits = "x".repeat(1000);
        let body = format!(r#"{{"channel":42,"message":"{fits}"}}"#);
        assert_eq!(
            handle(
                &request("POST", "/message", Some("test-key-1"), &body),
                &config,
                &api,
                &mut limiter,
                now
            )
            .await
            .status,
            200
        );
    }

    #[tokio::test]
    async fn the_request_log_carries_the_key_label_and_the_channel() {
        let api = fake(true, Ok(()));
        let config = config();
        let mut limiter = RateLimiter::new(config.rate_limit());
        let body = r#"{"channel":42,"message":"hello"}"#;
        let response = handle(
            &request("POST", "/message", Some("test-key-1"), body),
            &config,
            &api,
            &mut limiter,
            Instant::now(),
        )
        .await;
        assert_eq!(
            response.key.as_deref(),
            Some("ci"),
            "the label, not the key value"
        );
        assert_eq!(response.channel, Some(42));
    }

    #[tokio::test]
    async fn delivers_with_a_known_key() {
        let api = fake(true, Ok(()));
        let config = config();
        let mut limiter = RateLimiter::new(20);
        let response = handle(
            &request(
                "POST",
                "/message",
                Some("test-key-1"),
                r#"{"channel":42,"message":"hi"}"#,
            ),
            &config,
            &api,
            &mut limiter,
            Instant::now(),
        )
        .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.json.as_deref(), Some("{\"delivered\":true}"));
    }

    #[tokio::test]
    async fn status_codes() {
        let api = fake(true, Ok(()));
        let config = config();
        let mut limiter = RateLimiter::new(20);
        let long = format!(
            r#"{{"channel":42,"message":"{}"}}"#,
            "x".repeat(MAX_MESSAGE_CHARS + 1)
        );
        let cases = [
            (None, r#"{"channel":42,"message":"x"}"#, 401u16),
            (Some("wrong"), r#"{"channel":42,"message":"x"}"#, 401),
            (Some("test-key-1"), "not json", 400),
            (Some("test-key-1"), r#"{"channel":42,"message":"  "}"#, 400),
            (Some("test-key-1"), r#"{"channel":7,"message":"x"}"#, 403),
            (Some("test-key-1"), r#"{"channel":99,"message":"x"}"#, 404),
            (Some("test-key-1"), long.as_str(), 400),
        ];
        for (key, body, want) in cases {
            let response = handle(
                &request("POST", "/message", key, body),
                &config,
                &api,
                &mut limiter,
                Instant::now(),
            )
            .await;
            assert_eq!(response.status, want, "{body}");
        }
        let big = Request {
            method: "POST".to_owned(),
            path: "/message".to_owned(),
            headers: vec![(KEY_HEADER.to_owned(), "test-key-1".to_owned())],
            body: vec![0u8; MAX_BODY_BYTES],
            declared_len: MAX_BODY_BYTES + 1,
        };
        let too_big = handle(&big, &config, &api, &mut limiter, Instant::now()).await;
        assert_eq!(
            (too_big.status, too_big.json.as_deref()),
            (413, Some(r#"{"error":"request body too large"}"#)),
            "the answer is the status plus a short reason, not a channel list"
        );
        assert_eq!(
            handle(
                &request("GET", "/message", None, ""),
                &config,
                &api,
                &mut limiter,
                Instant::now(),
            )
            .await
            .status,
            405
        );
        assert_eq!(
            handle(
                &request("GET", "/nope", None, ""),
                &config,
                &api,
                &mut limiter,
                Instant::now(),
            )
            .await
            .status,
            404
        );
    }

    #[tokio::test]
    async fn offline_and_refused_are_503() {
        let config = config();
        let mut limiter = RateLimiter::new(20);
        let offline = fake(false, Ok(()));
        let refused = fake(true, Err(DeliveryError::Refused));
        for api in [&offline, &refused] {
            let response = handle(
                &request(
                    "POST",
                    "/message",
                    Some("test-key-1"),
                    r#"{"channel":42,"message":"x"}"#,
                ),
                &config,
                api,
                &mut limiter,
                Instant::now(),
            )
            .await;
            assert_eq!(response.status, 503);
        }
    }

    #[tokio::test]
    async fn rate_limit_rejects_the_21st_request() {
        let api = fake(true, Ok(()));
        let config = config();
        let mut limiter = RateLimiter::new(20);
        let now = Instant::now();
        let req = request(
            "POST",
            "/message",
            Some("test-key-1"),
            r#"{"channel":42,"message":"x"}"#,
        );
        for _ in 0..20 {
            assert_eq!(
                handle(&req, &config, &api, &mut limiter, now).await.status,
                200
            );
        }
        assert_eq!(
            handle(&req, &config, &api, &mut limiter, now).await.status,
            429
        );
        assert_eq!(
            handle(
                &req,
                &config,
                &api,
                &mut limiter,
                now + Duration::from_secs(61)
            )
            .await
            .status,
            200
        );
    }

    #[tokio::test]
    async fn channels_are_limited_to_the_key_and_health_needs_no_key() {
        let api = fake(true, Ok(()));
        let config = config();
        let mut limiter = RateLimiter::new(20);
        let channels = handle(
            &request("GET", "/channels", Some("test-key-1"), ""),
            &config,
            &api,
            &mut limiter,
            Instant::now(),
        )
        .await;
        assert_eq!(
            channels.json.as_deref(),
            Some("{\"channels\":[{\"id\":42,\"name\":\"Lobby\"}]}")
        );
        let health = handle(
            &request("GET", "/health", None, ""),
            &config,
            &api,
            &mut limiter,
            Instant::now(),
        )
        .await;
        assert_eq!(
            health.json.as_deref(),
            Some("{\"connected\":true,\"uptime_seconds\":1234,\"server\":\"127.0.0.1\"}")
        );
    }

    #[test]
    fn markup_of_the_subset_only_reaches_the_chat() {
        assert_eq!(
            markdown::render_html("**bold** <b>literal</b>"),
            "<b>bold</b> &lt;b&gt;literal&lt;/b&gt;"
        );
    }

    #[test]
    fn parses_wire_requests() {
        let bytes = b"POST /message HTTP/1.1\r\nX-API-Key: abc\r\nContent-Length: 5\r\n\r\nhello";
        let request = parse_request(bytes).expect("request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/message");
        assert_eq!(request.header("x-api-key"), Some("abc"));
        assert_eq!(request.body, b"hello");
        assert!(parse_request(b"GET /").is_none());
    }

    #[test]
    fn answer_is_a_complete_http_response() {
        let text =
            String::from_utf8(Response::ok(200, "{\"delivered\":true}").to_bytes()).expect("utf8");
        assert!(text.starts_with("HTTP/1.1 200\r\n"));
        assert!(text.ends_with("{\"delivered\":true}"));
        assert!(text.contains("Content-Length: 18"));
    }
}
