//! Wire test of the webhook API without a Mumble server: the real listener, a stub bot view.
//!
//! Covers the HTTP layer (parsing, status codes, `GET /channels`, `GET /health`) in the running
//! program. The connection to a real server is checked in `tests/webhook_live.rs`.

use std::sync::Arc;

use anyhow::Result;
use mumbot::webhook::{Api, ApiKey, DeliveryError, MAX_MESSAGE_CHARS, WebhookConfig, serve};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Stub {
    connected: bool,
    channels: Vec<(u32, String)>,
    allowed: Vec<u32>,
}

impl Api for Stub {
    async fn connected(&self) -> bool {
        self.connected
    }
    async fn uptime(&self) -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }
    async fn channel_exists(&self, id: u32) -> bool {
        self.channels.iter().any(|(channel, _)| *channel == id)
    }
    async fn allowed_ids(&self, _key: &ApiKey) -> Vec<u32> {
        self.allowed.clone()
    }
    async fn allowed_channels(&self, _key: &ApiKey) -> Vec<(u32, String)> {
        self.channels
            .iter()
            .filter(|(id, _)| self.allowed.contains(id))
            .cloned()
            .collect()
    }
    async fn message_cap(&self) -> usize {
        MAX_MESSAGE_CHARS
    }
    async fn deliver(&self, _channel: u32, _html: &str) -> Result<(), DeliveryError> {
        Ok(())
    }
}

/// Sends one raw request and reads one raw answer.
async fn call(
    port: u16,
    key: Option<&str>,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, String)> {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let key_line = key
        .map(|value| format!("X-API-Key: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{key_line}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await?;
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await?;
    let answer = String::from_utf8(answer)?;
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .unwrap_or((answer.as_str(), ""));
    let status = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()?;
    Ok((status, body.to_owned()))
}

#[tokio::test]
async fn listener_answers_the_specified_codes() {
    let config = Arc::new(
        toml::from_str::<WebhookConfig>(
            r#"
[[keys]]
key = "wire-key"
label = "ci"
channels = ["Lobby"]
"#,
        )
        .expect("config"),
    );
    let stub = Arc::new(Stub {
        connected: true,
        channels: vec![(2, "Lobby".to_owned()), (3, "Raid".to_owned())],
        allowed: vec![2],
    });

    // Port 0: the OS picks a free port, so parallel test runs never collide.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let port = listener.local_addr().expect("port").port();
    let server = tokio::spawn({
        let (config, stub) = (Arc::clone(&config), Arc::clone(&stub));
        async move { serve(listener, config, stub).await }
    });

    for (key, body, want) in [
        (Some("wire-key"), r#"{"channel":2,"message":"hi"}"#, 200u16),
        (None, "{}", 401),
        (Some("nope"), "{}", 401),
        (Some("wire-key"), "not json", 400),
        (Some("wire-key"), r#"{"channel":2,"message":" "}"#, 400),
        (Some("wire-key"), r#"{"channel":3,"message":"x"}"#, 403),
        (Some("wire-key"), r#"{"channel":9,"message":"x"}"#, 404),
    ] {
        let (status, answer) = call(port, key, "POST", "/message", body)
            .await
            .expect("request");
        assert_eq!(status, want, "{body} → {answer}");
        assert!(
            !answer.contains("channels"),
            "no channel list in an error: {answer}"
        );
    }

    assert_eq!(
        call(port, Some("wire-key"), "GET", "/channels", "")
            .await
            .expect("channels")
            .1,
        "{\"channels\":[{\"id\":2,\"name\":\"Lobby\"}]}"
    );
    assert_eq!(
        call(port, None, "GET", "/health", "")
            .await
            .expect("health")
            .1,
        "{\"connected\":true,\"uptime_seconds\":60,\"server\":\"127.0.0.1\"}"
    );
    assert_eq!(
        call(port, Some("wire-key"), "GET", "/message", "")
            .await
            .expect("method")
            .0,
        405
    );
    // The body cap is checked before the message content (AC-1.3).
    let big = format!(
        "POST /message HTTP/1.1\r\nHost: 127.0.0.1\r\nX-API-Key: wire-key\r\n\
         Content-Length: {}\r\n\r\n{}",
        20_001,
        "x".repeat(20_001)
    );
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    socket.write_all(big.as_bytes()).await.expect("write");
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await.expect("read");
    assert!(
        String::from_utf8(answer)
            .expect("utf8")
            .starts_with("HTTP/1.1 413"),
        "body cap"
    );

    // AC-1.7: several requests at the same time, each gets its own answer.
    let mut running = Vec::new();
    for number in 0..5 {
        running.push(tokio::spawn(async move {
            call(
                port,
                Some("wire-key"),
                "POST",
                "/message",
                &format!(r#"{{"channel":2,"message":"parallel {number}"}}"#),
            )
            .await
        }));
    }
    for task in running {
        let (status, _) = task.await.expect("join").expect("request");
        assert!(matches!(status, 200 | 429), "unexpected answer {status}");
    }

    server.abort();
}
