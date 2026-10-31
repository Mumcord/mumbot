//! Peer module against the local 1.5 test server and a mock relay API. Start the server first
//! (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --test peer_live -- --ignored --test-threads=1`
//!
//! Two test bots play Mumcord clients: they send the Mumcord hello, get the relay announcement,
//! and one asks for a token for the other. The mock relay checks the API calls: the allowlist
//! with both certificates, and the token for exactly that pair.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mumbot::peer::PeerConfig;
use mumbot::protocol::Event;
use mumbot::webhook::{self, HttpConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, UserCheck, env};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

const SECRET: &str = "test-relay-secret";

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

/// One API call as the mock relay saw it.
#[derive(Debug, Clone)]
struct Call {
    method: String,
    path: String,
    authorized: bool,
    body: serde_json::Value,
}

/// A relay API that accepts everything and records the calls.
async fn mock_relay() -> (String, Arc<Mutex<Vec<Call>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then as many body bytes as Content-Length announces.
            let call = loop {
                let n = socket.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break None;
                }
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
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if body.len() < length {
                    continue;
                }
                let mut parts = head.split_whitespace();
                break Some(Call {
                    method: parts.next().unwrap_or("").to_owned(),
                    path: parts.next().unwrap_or("").to_owned(),
                    authorized: head.lines().any(|l| {
                        l.eq_ignore_ascii_case(&format!("authorization: Bearer {SECRET}"))
                    }),
                    body: serde_json::from_str(body).unwrap_or(serde_json::Value::Null),
                });
            };
            let Some(call) = call else { continue };
            let answer = if call.path == "/v1/status" {
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 19\r\n\
                 connection: close\r\n\r\n{\"instance\":\"live\"}"
            } else {
                "HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n"
            };
            recorded.lock().expect("lock").push(call);
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    (url, calls)
}

fn client(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
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

async fn wait_for_call(calls: &Arc<Mutex<Vec<Call>>>, pred: impl Fn(&Call) -> bool) -> Call {
    for _ in 0..100 {
        if let Some(call) = calls.lock().expect("lock").iter().find(|c| pred(c)) {
            return call.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("relay API call not seen");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn clients_learn_the_relay_and_a_pair_gets_a_registered_token() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (relay_url, calls) = mock_relay().await;
    let secret_file = dir.path().join("relay.secret");
    std::fs::write(&secret_file, format!("{SECRET}\n")).expect("secret");

    let (alice_name, bob_name, bot_name) = (unique("alice"), unique("bob"), unique("peerbot"));
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    let bob = Bot::connect(client(dir.path(), &bob_name))
        .await
        .expect("bob");
    let (mut alice_events, mut bob_events) = (alice.subscribe(), bob.subscribe());

    let config = Arc::new(WebhookConfig {
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: bot_name.clone(),
            data_dir: Some(dir.path().to_owned()),
            ..ServerConfig::default()
        },
        http: HttpConfig {
            listen: "127.0.0.1:0".to_owned(),
            ..HttpConfig::default()
        },
        keys: Vec::new(),
        tokens: Vec::new(),
        peer: Some(PeerConfig {
            udp: vec!["relay.test:3478".to_owned()],
            tcp: vec!["relay.test:443".to_owned()],
            fingerprint: "ab".repeat(32),
            api_url: relay_url,
            secret_file,
            token_ttl_secs: 120,
        }),
    });
    let running = tokio::spawn(webhook::run(config));

    alice
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in");
    let bot = session_of(&alice, &bot_name);
    let hello = br#"{"app":"mumcord","v":1,"features":[]}"#.to_vec();
    for who in [&alice, &bob] {
        who.handle()
            .send_plugin_data(&[bot], "mumcord.hello", hello.clone())
            .expect("hello");
    }
    for events in [&mut alice_events, &mut bob_events] {
        let relay = next_peer_message(events, bot).await;
        assert_eq!(relay["t"], "relay");
        assert_eq!(relay["udp"][0], "relay.test:3478");
        assert_eq!(relay["tcp"][0], "relay.test:443");
        assert_eq!(relay["fp"], "ab".repeat(32));
    }

    let (alice_cert, bob_cert) = (cert_of(&alice), cert_of(&bob));
    let allowlist = wait_for_call(&calls, |c| {
        c.path == "/v1/allowlist"
            && [&alice_cert, &bob_cert].iter().all(|cert| {
                c.body["hashes"]
                    .as_array()
                    .is_some_and(|h| h.contains(&(*cert).clone().into()))
            })
    })
    .await;
    assert_eq!(allowlist.method, "PUT");
    assert!(allowlist.authorized, "the API secret is sent");

    let bob_session = session_of(&alice, &bob_name);
    let request = format!(r#"{{"v":1,"t":"token_req","id":42,"peer":{bob_session}}}"#);
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.peer", request.into_bytes())
        .expect("token request");
    let for_alice = next_peer_message(&mut alice_events, bot).await;
    let for_bob = next_peer_message(&mut bob_events, bot).await;
    assert_eq!(for_alice["t"], "token");
    assert_eq!(for_alice["id"], 42);
    assert_eq!(for_alice["b"], bob_session);
    assert_eq!(for_alice, for_bob, "both get the same token");

    let registered = wait_for_call(&calls, |c| c.path == "/v1/tokens").await;
    assert_eq!(registered.method, "POST");
    assert!(registered.authorized);
    assert_eq!(registered.body["token"], for_alice["token"]);
    assert_eq!(registered.body["a"], alice_cert.as_str());
    assert_eq!(registered.body["b"], bob_cert.as_str());
    assert_eq!(registered.body["expires_at"], for_alice["exp"]);

    running.abort();
    alice.disconnect().await.expect("alice leaves");
    bob.disconnect().await.expect("bob leaves");
}
