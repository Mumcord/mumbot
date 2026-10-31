//! Peer bot against the local 1.5 test server and a mock relay API. Start the server first
//! (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test peer_live -- --ignored --test-threads=1`
//!
//! Two test bots play Mumcord clients: they send a Mumcord hello with `p2p`, get the relay
//! announcement, and one asks for a token for the other. The mock relay checks the API calls
//! (mumcord spec 04, sections 6.4 and 6.6, FR-4.22 to FR-4.28).

#![cfg(feature = "tester")]

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mumbot::peer::PeerConfig;
use mumbot::protocol::Event;
use mumbot::webhook::{ApiKey, ChannelRef, HttpConfig, ModulesConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, MessageCheck, Module, UserCheck, env, modules};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

const SECRET: &str = "test-relay-secret-0123456789abcdef";
const TOKEN: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MUMBOT_LIVE=1 and start testenv/up.sh");
    }
    on
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() & 0xff_ffff)
        .unwrap_or(0);
    format!("{prefix}-{:06x}", std::process::id() ^ nanos)
}

/// SHA-256 of the test server's certificate (`testenv/gen-certs.sh`).
fn server_pin() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testenv/certs/cert.pem");
    let der = CertificateDer::from_pem_file(path).expect("testenv/certs/cert.pem");
    ring::digest::digest(&ring::digest::SHA256, der.as_ref())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One API call as the mock relay saw it.
#[derive(Debug, Clone)]
struct Call {
    method: String,
    path: String,
    authorized: bool,
    body: serde_json::Value,
}

/// A relay API that answers like mumpeer and records the calls.
async fn mock_relay() -> (String, Arc<Mutex<Vec<Call>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let Some(call) = read_call(&mut socket).await else {
                continue;
            };
            let (status, body) = match (call.method.as_str(), call.path.as_str()) {
                ("GET", "/v1/status") => (
                    "200 OK",
                    format!(
                        r#"{{"v":1,"boot_id":"0123456789abcdef","host":"127.0.0.1","udp":443,"tcp":443,"fp":"{}","sessions":0,"max_sessions":50}}"#,
                        "cd".repeat(32)
                    ),
                ),
                ("POST", "/v1/pairs") => (
                    "201 Created",
                    format!(r#"{{"tok":"{TOKEN}","expires_in":120}}"#),
                ),
                _ => ("204 No Content", String::new()),
            };
            recorded.lock().expect("lock").push(call);
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    (url, calls)
}

/// Headers, then as many body bytes as Content-Length announces.
async fn read_call(socket: &mut tokio::net::TcpStream) -> Option<Call> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = socket.read(&mut buf).await.ok().filter(|n| *n > 0)?;
        raw.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&raw).to_string();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let length = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if body.len() < length {
            continue;
        }
        let mut parts = head.split_whitespace();
        return Some(Call {
            method: parts.next().unwrap_or("").to_owned(),
            path: parts.next().unwrap_or("").to_owned(),
            authorized: head
                .lines()
                .any(|l| l == format!("Authorization: Bearer {SECRET}")),
            body: serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
        });
    }
}

fn client(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

fn peer_config(dir: &Path, name: &str, relay_url: &str, pin: String) -> Arc<WebhookConfig> {
    let secret_file = dir.join("relay.secret");
    std::fs::write(&secret_file, format!("{SECRET}\n")).expect("secret");
    Arc::new(WebhookConfig {
        modules: ModulesConfig {
            enabled: vec![Module::Peer],
        },
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            data_dir: Some(dir.to_owned()),
            server_cert_sha256: Some(pin),
            ..ServerConfig::default()
        },
        http: Default::default(),
        keys: Vec::new(),
        tokens: Vec::new(),
        peer: Some(PeerConfig {
            api_url: relay_url.to_owned(),
            api_secret_file: Some(secret_file),
            api_fp: None,
        }),
        history: None,
    })
}

fn session_of(bot: &Bot, name: &str) -> u32 {
    bot.state()
        .users
        .values()
        .find(|u| u.name == name)
        .map(|u| u.session)
        .expect("user is online")
}

fn cert_of(bot: &Bot) -> String {
    bot.own_user()
        .expect("own user")
        .cert_hash
        .expect("test bots have a certificate")
}

/// The next `mumcord.peer` message from `from`, as JSON.
async fn next_peer_message(
    events: &mut broadcast::Receiver<Event>,
    from: u32,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(Event::PluginData(data)) = events.recv().await
                && data.sender == Some(from)
                && data.data_id == "mumcord.peer"
            {
                return serde_json::from_slice(&data.data).expect("json");
            }
        }
    })
    .await
    .expect("peer message within 10 s")
}

async fn wait_for_call(calls: &Arc<Mutex<Vec<Call>>>, method: &str, path: &str) -> Call {
    for _ in 0..100 {
        let found = calls
            .lock()
            .expect("lock")
            .iter()
            .find(|c| c.method == method && c.path == path)
            .cloned();
        if let Some(call) = found {
            return call;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("relay API call {method} {path} not seen");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn clients_learn_the_relay_and_a_pair_gets_a_token() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (relay_url, calls) = mock_relay().await;
    let (alice_name, bob_name, bot_name) = (unique("alice"), unique("bob"), unique("peerbot"));
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    let bob = Bot::connect(client(dir.path(), &bob_name))
        .await
        .expect("bob");
    let (mut alice_events, mut bob_events) = (alice.subscribe(), bob.subscribe());
    let running = tokio::spawn(modules::run(peer_config(
        dir.path(),
        &bot_name,
        &relay_url,
        server_pin(),
    )));

    alice
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in with the pinned server certificate");
    let bot = session_of(&alice, &bot_name);
    let hello = br#"{"app":"mumcord","v":1,"features":["p2p"]}"#.to_vec();
    for who in [&alice, &bob] {
        who.handle()
            .send_plugin_data(&[bot], "mumcord.hello", hello.clone())
            .expect("hello");
    }
    for events in [&mut alice_events, &mut bob_events] {
        assert_eq!(
            next_peer_message(events, bot).await,
            serde_json::json!({"v":1,"t":"relay","host":"127.0.0.1","udp":443,"tcp":443,
                "fp":"cd".repeat(32),"state":"up","seq":1})
        );
    }
    assert!(wait_for_call(&calls, "GET", "/v1/status").await.authorized);

    let bob_session = session_of(&alice, &bob_name);
    let request =
        format!(r#"{{"v":1,"t":"token-req","rid":"0123456789abcdef","peer":{bob_session}}}"#);
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.peer", request.into_bytes())
        .expect("token request");
    let for_alice = next_peer_message(&mut alice_events, bot).await;
    let for_bob = next_peer_message(&mut bob_events, bot).await;
    let alice_session = session_of(&bob, &alice_name);
    assert_eq!(
        for_alice,
        serde_json::json!({"v":1,"t":"token","rid":"0123456789abcdef","a":alice_session,
            "b":bob_session,"tok":TOKEN,"ttl":120})
    );
    assert_eq!(for_alice, for_bob, "both get the same token");
    let pair = wait_for_call(&calls, "POST", "/v1/pairs").await;
    assert!(pair.authorized, "the API secret is sent");
    let (alice_cert, bob_cert) = (cert_of(&alice), cert_of(&bob));
    assert_eq!(
        pair.body,
        serde_json::json!({"a": alice_cert, "b": bob_cert})
    );

    bob.disconnect().await.expect("bob leaves");
    let removal = wait_for_call(&calls, "DELETE", &format!("/v1/peers/{bob_cert}")).await;
    assert!(removal.authorized);

    running.abort();
    alice.disconnect().await.expect("alice leaves");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn a_wrong_server_pin_refuses_the_server() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (relay_url, _) = mock_relay().await;
    let config = peer_config(dir.path(), &unique("peerbot"), &relay_url, "00".repeat(32));
    let result = tokio::time::timeout(Duration::from_secs(15), modules::run(config))
        .await
        .expect("fails fast");
    assert!(
        result.is_err(),
        "a server with another certificate is refused"
    );
}

/// AC-3.1 of spec 03: the webhook and the peer module share one bot user.
#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn webhook_and_peer_modules_share_one_connection() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (relay_url, _) = mock_relay().await;
    let (alice_name, bot_name) = (unique("alice"), unique("modbot"));
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    let mut alice_events = alice.subscribe();
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("free port")
        .local_addr()
        .expect("addr")
        .port();
    let key = "a-long-random-key-for-the-modules-test";
    let mut config =
        Arc::unwrap_or_clone(peer_config(dir.path(), &bot_name, &relay_url, server_pin()));
    config.modules.enabled = vec![Module::Webhook, Module::Peer];
    config.http = HttpConfig {
        listen: format!("127.0.0.1:{port}"),
        ..HttpConfig::default()
    };
    config.keys = vec![ApiKey {
        key: key.to_owned(),
        label: "modules".to_owned(),
        channels: vec![ChannelRef::Id(0)],
    }];
    let running = tokio::spawn(modules::run(Arc::new(config)));

    alice
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the bot logs in");
    let bot = session_of(&alice, &bot_name);
    let hello = br#"{"app":"mumcord","v":1,"features":["p2p"]}"#.to_vec();
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello)
        .expect("hello");
    assert_eq!(
        next_peer_message(&mut alice_events, bot).await["t"],
        "relay",
        "the peer module announces the relay"
    );

    let body = r#"{"channel": 0, "message": "from the modules test"}"#;
    let request = format!(
        "POST /message HTTP/1.1\r\nHost: 127.0.0.1\r\nX-API-Key: {key}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("webhook listener");
    socket.write_all(request.as_bytes()).await.expect("send");
    let mut response = String::new();
    socket
        .read_to_string(&mut response)
        .await
        .expect("response");
    assert!(response.contains("\"delivered\":true"), "{response}");
    let message = alice
        .wait_message(&MessageCheck {
            contains: Some("from the modules test".to_owned()),
            ..MessageCheck::default()
        })
        .await
        .expect("the webhook message arrives");
    assert_eq!(message.actor, Some(bot), "sent by the same bot user");
    assert_eq!(
        alice
            .state()
            .users
            .values()
            .filter(|u| u.name == bot_name)
            .count(),
        1,
        "one bot user for both modules"
    );

    running.abort();
    alice.disconnect().await.expect("alice leaves");
}
