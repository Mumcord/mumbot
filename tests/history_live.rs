//! History module against the local 1.5 test server. Start the server first
//! (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test history_live -- --ignored --test-threads=1`
//!
//! A test bot posts channel and private messages, then a second test bot plays a Mumcord client
//! that joined later: it sends the history sync requests Mumcord sends (`areq`, `hreq`, `get`)
//! and checks the answers (spec 04, AC-4.1 to AC-4.4).

#![cfg(feature = "tester")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mumbot::history::hash::{Content, content_hash};
use mumbot::history::store::Store;
use mumbot::history::wire::{self, DATA_ID, Hash, Packet};
use mumbot::history::{HistoryConfig, now_millis};
use mumbot::protocol::Event;
use mumbot::webhook::{ChannelRef, ModulesConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, Module, UserCheck, env, modules};
use tokio::sync::broadcast;

const WEEK_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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

fn client(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

fn history_config(dir: &Path, name: &str) -> Arc<WebhookConfig> {
    Arc::new(WebhookConfig {
        modules: ModulesConfig {
            enabled: vec![Module::History],
        },
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            channel: Some("Lobby".to_owned()),
            data_dir: Some(dir.to_owned()),
            ..ServerConfig::default()
        },
        http: Default::default(),
        keys: Vec::new(),
        tokens: Vec::new(),
        peer: None,
        history: Some(HistoryConfig {
            channels: vec![
                ChannelRef::Name("Lobby".to_owned()),
                ChannelRef::Name("Raid".to_owned()),
            ],
            exclude: Vec::new(),
            retention_days: 7,
            max_messages_per_channel: 100,
            database: Some(dir.join("history.sqlite3")),
        }),
        voice: Default::default(),
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

/// History sync packets from `from` until `done` says the answer is complete.
async fn answer(
    events: &mut broadcast::Receiver<Event>,
    from: u32,
    done: impl Fn(&Packet) -> bool,
) -> Vec<Packet> {
    let mut packets = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Event::PluginData(data)) = events.recv().await
                && data.sender == Some(from)
                && data.data_id == DATA_ID
            {
                let packet = wire::decode(&data.data).expect("valid packet");
                let finished = done(&packet);
                packets.push(packet);
                if finished {
                    return;
                }
            }
        }
    })
    .await
    .expect("answer within 15 s");
    packets
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn a_late_mumcord_client_loads_the_recorded_channel_history() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (alice_name, carol_name, bot_name) = (unique("alice"), unique("carol"), unique("history"));
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    alice.join("Lobby").await.expect("alice joins Lobby");
    let running = tokio::spawn(modules::run(history_config(dir.path(), &bot_name)));

    // AC-4.1: in Lobby, listening to Raid, with the notice as its comment.
    alice
        .wait_user(
            &bot_name,
            &UserCheck {
                in_channel: Some("Lobby".to_owned()),
                listening: Some("Raid".to_owned()),
                comment_contains: Some("never recorded".to_owned()),
                ..UserCheck::default()
            },
        )
        .await
        .expect("the history bot sits in Lobby, listens to Raid and shows its notice");

    // AC-4.2: channel messages are recorded, a private message and an unlisted channel not.
    let before = now_millis();
    alice
        .send_channel(Some("Lobby"), "first <b>one</b>")
        .expect("Lobby");
    alice.send_channel(Some("Raid"), "raid news").expect("Raid");
    alice
        .send_channel(Some("AFK"), "not recorded")
        .expect("AFK");
    alice
        .send_private(&bot_name, "private, never recorded")
        .expect("private");
    alice.flush().await.expect("flush");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after = now_millis();
    let store = Store::open(&dir.path().join("history.sqlite3")).expect("database");
    for (channel, count) in [("Lobby", 1), ("Raid", 1), ("AFK", 0), ("", 0)] {
        let summary = store.range_summary(channel, 0, i64::MAX).expect("summary");
        assert_eq!(summary.count, count, "{channel:?}");
    }

    // A Mumcord client joins Lobby afterwards and asks for what it missed (AC-4.3).
    let carol = Bot::connect(client(dir.path(), &carol_name))
        .await
        .expect("carol");
    let mut events = carol.subscribe();
    carol.join("Lobby").await.expect("carol joins Lobby");
    let joined = now_millis();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bot = session_of(&carol, &bot_name);
    let lobby = carol.channel_id("Lobby").expect("Lobby");
    let send = |packet: &Packet| {
        carol
            .handle()
            .send_plugin_data(&[bot], DATA_ID, wire::encode(packet))
            .expect("plugin data");
    };

    send(&Packet::AvailabilityRequest {
        req: 1,
        channel: lobby,
        from: joined - WEEK_MS,
        to: joined,
    });
    let summary = answer(&mut events, bot, |_| true).await;
    let [
        Packet::Availability {
            req: 1,
            count: 1,
            oldest,
            ..
        },
    ] = summary.as_slice()
    else {
        panic!("one message on offer: {summary:?}");
    };
    assert!((before..=after).contains(oldest), "stamped at receipt");

    send(&Packet::HashRequest {
        req: 2,
        channel: lobby,
        from: joined - WEEK_MS,
        to: joined,
    });
    let list = answer(&mut events, bot, |p| {
        matches!(p, Packet::Hashes { last: true, .. })
    })
    .await;
    let hashes: Vec<Hash> = list
        .iter()
        .flat_map(|p| match p {
            Packet::Hashes { req: 2, hashes, .. } => hashes.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(hashes.len(), 1);

    send(&Packet::Fetch {
        req: 2,
        wanted: wire::bitmap(1, [0]),
    });
    let parts = answer(&mut events, bot, |p| {
        matches!(p, Packet::Part { last: true, .. })
    })
    .await;
    let stream: Vec<u8> = parts
        .iter()
        .flat_map(|p| match p {
            Packet::Part { req: 2, data, .. } => data.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let messages = wire::decode_messages(&stream).expect("messages");
    let [message] = messages.as_slice() else {
        panic!("one message: {messages:?}");
    };
    let alice_cert = alice.own_user().expect("alice").cert_hash;
    assert_eq!(message.n, alice_name);
    assert_eq!(message.h, alice_cert);
    assert_eq!(message.b, "first <b>one</b>");
    // The requester's check: the content hash recomputed from the message is the listed one.
    let recomputed = content_hash(&Content {
        channel: "Lobby",
        sender_name: &message.n,
        sender_cert_hash: message.h.as_deref(),
        sent_at: message.ts,
        body: &message.b,
    });
    assert_eq!(hashes, [recomputed]);

    // AC-4.4: Raid is recorded, but Carol neither is in it nor listens to it.
    send(&Packet::HashRequest {
        req: 3,
        channel: carol.channel_id("Raid").expect("Raid"),
        from: joined - WEEK_MS,
        to: joined,
    });
    assert_eq!(
        answer(&mut events, bot, |_| true).await,
        [Packet::Deny { req: 3 }]
    );

    running.abort();
    carol.disconnect().await.expect("carol leaves");
    alice.disconnect().await.expect("alice leaves");
}
