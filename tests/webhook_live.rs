//! Webhook bot against the local 1.5 test server. Start it first (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --test webhook_live -- --ignored --test-threads=1`
//!
//! Checks the acceptance criteria of `specs/01-webhook-specification.md` in the running bot:
//! AC-1.1 connection and reconnect, AC-1.2 message reaches the channel chat, AC-1.3 status codes,
//! AC-1.4 `GET /channels` and `GET /health`, AC-1.5 markup, AC-1.7 parallel requests,
//! AC-1.8 nothing beyond an unmodified standard server is needed.

use std::sync::Arc;

use anyhow::Result;
use mumbot::webhook::{Api, ApiKey, BotApi, ChannelRef, HttpConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, MessageCheck, env};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Channel ids the test server seeds on first start (`testenv/up.sh`): Root 0, Lobby 1, Raid 2.
const LOBBY: u32 = 1;
const RAID: u32 = 2;

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MUMBOT_LIVE=1 and start testenv/up.sh");
    }
    on
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{:06x}", std::process::id() ^ rand_suffix())
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() & 0xff_ffff)
        .unwrap_or(0)
}

/// Bot under test: key `ci` may write to Lobby only, listener on loopback.
fn config(dir: &std::path::Path, name: &str) -> Arc<WebhookConfig> {
    Arc::new(WebhookConfig {
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            cert_name: None,
            channel: Some("Lobby".to_owned()),
            password: None,
            data_dir: Some(dir.to_owned()),
            superuser: false,
        },
        http: HttpConfig {
            listen: "127.0.0.1:0".to_owned(),
            rate_limit_per_minute: 20,
            delivery_timeout_secs: 3,
        },
        keys: vec![
            ApiKey {
                key: "test-key-ci".to_owned(),
                label: "ci".to_owned(),
                channels: vec![ChannelRef::Name("Lobby".to_owned())],
            },
            // Its own budget, so the offline check is not confused by the rate limit.
            ApiKey {
                key: "test-key-probe".to_owned(),
                label: "probe".to_owned(),
                channels: vec![ChannelRef::Name("Lobby".to_owned())],
            },
        ],
        tokens: Vec::new(),
    })
}

/// One raw request, one raw answer.
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
#[ignore = "needs testenv v15 and MUMBOT_LIVE=1"]
async fn webhook_messages_reach_the_channel_chat() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("webhook-bot");
    let observer_name = unique("observer");

    let api = Arc::new(BotApi::new(config(dir.path(), &bot_name)));
    api.connect().await.expect("bot connects");
    assert!(api.connected().await, "AC-1.1 connected as a normal client");

    let observer = Bot::connect(BotConfig {
        data_dir: Some(dir.path().to_owned()),
        ..BotConfig::new(&observer_name, env::test_port())
    })
    .await
    .expect("observer client");
    observer.join("Lobby").await.expect("observer joins Lobby");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let port = listener.local_addr().expect("port").port();
    let serve = tokio::spawn({
        let api = Arc::clone(&api);
        let config = Arc::clone(api.config());
        async move { mumbot::webhook::serve(listener, config, api).await }
    });

    // AC-1.2: the posted message shows in the chat of the second client, from the bot user.
    let (status, answer) = call(
        port,
        Some("test-key-ci"),
        "POST",
        "/message",
        &format!(r#"{{"channel":{LOBBY},"message":"build failed"}}"#),
    )
    .await
    .expect("post");
    assert_eq!(status, 200, "answer: {answer}");
    let seen = observer
        .wait_message(&MessageCheck {
            from: Some(bot_name.clone()),
            contains: Some("build failed".to_owned()),
            channel: Some("Lobby".to_owned()),
            private: Some(false),
        })
        .await
        .expect("AC-1.2 the chat shows the message");
    println!("AC-1.2 ✅ chat: {seen:?}");

    // AC-1.3: each case answers its own code, error bodies hold no channel list.
    let long = format!(r#"{{"channel":{LOBBY},"message":"{}"}}"#, "x".repeat(5001));
    for (key, body, want) in [
        (None, "{}", 401u16),
        (Some("wrong"), "{}", 401),
        (Some("test-key-ci"), "not json", 400),
        (
            Some("test-key-ci"),
            &format!(r#"{{"channel":{LOBBY},"message":" "}}"#),
            400,
        ),
        (
            Some("test-key-ci"),
            &format!(r#"{{"channel":{LOBBY},"message":"x"}}"#),
            200,
        ),
        (
            Some("test-key-ci"),
            &format!(r#"{{"channel":{RAID},"message":"x"}}"#),
            403,
        ),
        (Some("test-key-ci"), r#"{"channel":99,"message":"x"}"#, 404),
        (Some("test-key-ci"), &long, 400),
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

    // AC-1.4: the key sees only its own channels; health needs no key.
    let (status, body) = call(port, Some("test-key-ci"), "GET", "/channels", "")
        .await
        .expect("channels");
    assert_eq!(status, 200);
    assert!(
        body.contains("\"name\":\"Lobby\"") && !body.contains("Raid"),
        "{body}"
    );
    let (status, body) = call(port, None, "GET", "/health", "")
        .await
        .expect("health");
    assert_eq!(status, 200);
    assert!(body.contains("\"connected\":true"), "{body}");

    // AC-1.5: the Markdown subset renders as markup, caller HTML stays literal text.
    let markup =
        format!(r#"{{"channel":{LOBBY},"message":"**bold** <b>literal</b> https://example.com"}}"#);
    let (status, _) = call(port, Some("test-key-ci"), "POST", "/message", &markup)
        .await
        .expect("markup");
    assert_eq!(status, 200);
    let rendered = observer
        .wait_message(&MessageCheck {
            contains: Some("<b>bold</b>".to_owned()),
            ..Default::default()
        })
        .await
        .expect("AC-1.5 markup reaches the chat");
    assert!(
        rendered.message.contains("&lt;b&gt;literal&lt;/b&gt;"),
        "{rendered:?}"
    );

    // AC-1.7: parallel requests, nothing lost or doubled. The key's limit is 20 per minute, so
    // the requests that remain inside the limit must all be answered.
    let mut running = Vec::new();
    for number in 0..5 {
        let body = format!(r#"{{"channel":{LOBBY},"message":"parallel {number}"}}"#);
        running.push(tokio::spawn(async move {
            call(port, Some("test-key-ci"), "POST", "/message", &body).await
        }));
    }
    let mut accepted = 0;
    for task in running {
        let (status, _) = task.await.expect("join").expect("request");
        if status == 200 {
            accepted += 1;
        }
    }
    for number in 0..accepted {
        observer
            .wait_message(&MessageCheck {
                contains: Some(format!("parallel {number}")),
                ..Default::default()
            })
            .await
            .expect("AC-1.7 every accepted message arrived");
    }
    println!("AC-1.7 ✅ {accepted} parallel messages accepted");

    // AC-1.3: the rate limit refuses above 20 requests per minute for the key.
    let probe = format!(r#"{{"channel":{LOBBY},"message":"limit probe"}}"#);
    let mut used = 0;
    loop {
        let (status, _) = call(port, Some("test-key-ci"), "POST", "/message", &probe)
            .await
            .expect("probe");
        if status == 429 {
            break;
        }
        used += 1;
        observer
            .wait_message(&MessageCheck {
                contains: Some("limit probe".to_owned()),
                ..Default::default()
            })
            .await
            .expect("the accepted probe reached the chat");
        assert!(
            used <= 20,
            "the limit must refuse the 21st request, saw {used}"
        );
    }
    println!("AC-1.3 ✅ rate limit refused after {used} requests for key ci");

    // AC-1.3 / FR-1.10: an offline bot answers 503 and queues nothing.
    api.handle().await.disconnect();
    let offline = format!(r#"{{"channel":{LOBBY},"message":"offline probe"}}"#);
    let (status, answer) = call(port, Some("test-key-probe"), "POST", "/message", &offline)
        .await
        .expect("offline request");
    assert_eq!(status, 503, "offline answer: {answer}");
    let queued = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        observer.wait_message(&MessageCheck {
            contains: Some("offline probe".to_owned()),
            ..Default::default()
        }),
    )
    .await;
    assert!(queued.is_err(), "FR-1.10: nothing is queued while offline");

    // AC-1.1: reconnects after the link is dropped and can deliver again.
    api.reconnect().await.expect("reconnect");
    assert!(api.connected().await, "AC-1.1 reconnects");
    let after = format!(r#"{{"channel":{LOBBY},"message":"after reconnect"}}"#);
    let (status, _) = call(port, Some("test-key-probe"), "POST", "/message", &after)
        .await
        .expect("request after reconnect");
    assert_eq!(status, 200);
    observer
        .wait_message(&MessageCheck {
            contains: Some("after reconnect".to_owned()),
            ..Default::default()
        })
        .await
        .expect("AC-1.1 delivery works after reconnect");
    println!("AC-1.1 ✅ reconnected and delivered again");

    serve.abort();
    drop(observer);
}
