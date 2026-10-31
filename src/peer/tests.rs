use std::sync::{Arc, Mutex};

use anyhow::bail;
use bytes::Bytes;
use mumcord_protocol::{DisconnectReason, PluginData, User};

use super::*;

const BOT: u32 = 1;
const ALICE: u32 = 2;
const BOB: u32 = 3;
const OFFICIAL: u32 = 4;

#[derive(Default)]
struct MockRelay {
    instance: Mutex<String>,
    down: Mutex<bool>,
    allowlists: Mutex<Vec<Vec<String>>>,
    tokens: Mutex<Vec<TokenGrant>>,
}

impl RelayApi for Arc<MockRelay> {
    async fn status(&self) -> Result<String> {
        if *self.down.lock().expect("lock") {
            bail!("down");
        }
        Ok(self.instance.lock().expect("lock").clone())
    }

    async fn put_allowlist(&self, hashes: &[String]) -> Result<()> {
        if *self.down.lock().expect("lock") {
            bail!("down");
        }
        self.allowlists.lock().expect("lock").push(hashes.to_vec());
        Ok(())
    }

    async fn register_token(&self, grant: &TokenGrant) -> Result<()> {
        if *self.down.lock().expect("lock") {
            bail!("down");
        }
        self.tokens.lock().expect("lock").push(grant.clone());
        Ok(())
    }
}

#[derive(Default)]
struct MockLink {
    users: Mutex<Option<Users>>,
    sent: Mutex<Vec<(Vec<u32>, serde_json::Value)>>,
}

impl MockLink {
    fn take_sent(&self) -> Vec<(Vec<u32>, serde_json::Value)> {
        std::mem::take(&mut *self.sent.lock().expect("lock"))
    }
}

impl Link for Arc<MockLink> {
    async fn users(&self) -> Option<Users> {
        self.users.lock().expect("lock").clone()
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        assert!(data.len() <= mumcord_protocol::PLUGIN_DATA_MAX);
        let json = serde_json::from_slice(&data).expect("json");
        self.sent
            .lock()
            .expect("lock")
            .push((receivers.to_vec(), json));
        Ok(())
    }
}

fn config() -> PeerConfig {
    PeerConfig {
        udp: vec!["relay.test:3478".into()],
        tcp: vec!["relay.test:443".into()],
        fingerprint: "ab".repeat(32),
        api_url: "http://127.0.0.1:1".into(),
        secret_file: "/nonexistent".into(),
        token_ttl_secs: 120,
    }
}

fn cert(name: &str) -> Option<String> {
    Some(format!("{name:0>40}"))
}

type Setup = (
    Peer<Arc<MockRelay>, Arc<MockLink>>,
    Arc<MockRelay>,
    Arc<MockLink>,
);

fn setup() -> Setup {
    let relay = Arc::new(MockRelay::default());
    *relay.instance.lock().expect("lock") = "one".into();
    let link = Arc::new(MockLink::default());
    *link.users.lock().expect("lock") = Some(Users {
        own: BOT,
        certs: HashMap::from([
            (BOT, cert("b0")),
            (ALICE, cert("a1")),
            (BOB, cert("b1")),
            (OFFICIAL, None),
        ]),
    });
    let peer = Peer::new(config(), Arc::clone(&relay), Arc::clone(&link)).expect("peer");
    (peer, relay, link)
}

fn plugin(sender: u32, id: &str, data: &[u8]) -> Event {
    Event::PluginData(PluginData {
        sender: Some(sender),
        data_id: id.into(),
        data: Bytes::copy_from_slice(data),
    })
}

fn hello(sender: u32) -> Event {
    plugin(
        sender,
        "mumcord.hello",
        br#"{"app":"mumcord","v":1,"features":[]}"#,
    )
}

fn request(sender: u32, id: u32, peer: u32) -> Event {
    plugin(
        sender,
        "mumcord.peer",
        format!(r#"{{"v":1,"t":"token_req","id":{id},"peer":{peer}}}"#).as_bytes(),
    )
}

#[tokio::test]
async fn a_hello_gets_the_relay_once_and_only_to_that_client() {
    let (mut peer, _, link) = setup();
    let now = Instant::now();
    peer.on_event(hello(ALICE), now).await;
    peer.on_event(hello(ALICE), now).await;
    peer.on_event(plugin(BOB, "mumcord.hello", b"{}"), now)
        .await;
    let sent = link.take_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, vec![ALICE]);
    assert_eq!(sent[0].1["t"], "relay");
    assert_eq!(sent[0].1["udp"][0], "relay.test:3478");
    assert_eq!(sent[0].1["tcp"][0], "relay.test:443");
    assert_eq!(sent[0].1["fp"], "ab".repeat(32));
    assert_eq!(peer.clients().iter().copied().collect::<Vec<_>>(), [ALICE]);
}

#[tokio::test]
async fn a_token_is_registered_for_the_pair_and_sent_to_both() {
    let (mut peer, relay, link) = setup();
    let now = Instant::now();
    peer.on_event(hello(ALICE), now).await;
    peer.on_event(hello(BOB), now).await;
    link.take_sent();
    peer.on_event(request(ALICE, 7, BOB), now).await;
    let grants = relay.tokens.lock().expect("lock").clone();
    assert_eq!(grants.len(), 1);
    assert_eq!(
        (grants[0].a.clone(), grants[0].b.clone()),
        (cert("a1").expect("a"), cert("b1").expect("b"))
    );
    assert_eq!(grants[0].token.len(), 64);
    let ttl = grants[0].expires_at - unix_now();
    assert!((115..=120).contains(&ttl), "{ttl}");
    let sent = link.take_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    let json = &sent[0].1;
    assert_eq!(
        (json["t"].clone(), json["id"].clone()),
        ("token".into(), 7.into())
    );
    assert_eq!(
        (json["a"].clone(), json["b"].clone()),
        (ALICE.into(), BOB.into())
    );
    assert_eq!(json["token"], grants[0].token);
    assert_eq!(json["exp"], grants[0].expires_at);
}

#[tokio::test]
async fn bad_requests_are_denied_with_a_reason() {
    let (mut peer, relay, link) = setup();
    let now = Instant::now();
    for session in [ALICE, BOB] {
        peer.on_event(hello(session), now).await;
    }
    link.take_sent();
    let cases = [
        (request(ALICE, 1, ALICE), "unknown_peer"),
        (request(ALICE, 2, BOT), "unknown_peer"),
        (request(ALICE, 3, 99), "unknown_peer"),
        // Online, but never sent a hello: not a Mumcord client.
        (request(ALICE, 4, OFFICIAL), "unknown_peer"),
        (request(OFFICIAL, 5, ALICE), "no_cert"),
    ];
    for (event, reason) in cases {
        peer.on_event(event, now).await;
        let sent = link.take_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1["t"], "deny");
        assert_eq!(sent[0].1["reason"], reason);
    }
    *relay.down.lock().expect("lock") = true;
    peer.on_event(request(BOB, 6, ALICE), now).await;
    let sent = link.take_sent();
    assert_eq!(
        (sent[0].0.clone(), sent[0].1["reason"].clone()),
        (vec![BOB], "relay_unavailable".into())
    );
    assert!(relay.tokens.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn requests_are_limited_per_certificate_and_minute() {
    let (mut peer, _, link) = setup();
    let now = Instant::now();
    for session in [ALICE, BOB] {
        peer.on_event(hello(session), now).await;
    }
    link.take_sent();
    for id in 0..REQUESTS_PER_MINUTE as u32 + 1 {
        peer.on_event(request(ALICE, id, BOB), now).await;
    }
    let sent = link.take_sent();
    assert_eq!(sent.last().expect("sent").1["reason"], "rate_limited");
    assert_eq!(
        sent.iter().filter(|(_, j)| j["t"] == "token").count(),
        REQUESTS_PER_MINUTE
    );
    peer.on_event(request(ALICE, 99, BOB), now + Duration::from_secs(61))
        .await;
    assert_eq!(link.take_sent()[0].1["t"], "token");
}

#[tokio::test]
async fn the_allowlist_follows_joins_and_leaves_and_is_retried() {
    let (mut peer, relay, link) = setup();
    let now = Instant::now();
    peer.tick(now).await;
    let expected = vec![cert("a1").expect("a"), cert("b1").expect("b")];
    assert_eq!(
        *relay.allowlists.lock().expect("lock"),
        vec![expected.clone()]
    );
    // Unchanged users: nothing is pushed again.
    peer.on_event(Event::Disconnected(DisconnectReason::Closed), now)
        .await;
    peer.tick(now + Duration::from_secs(2)).await;
    assert_eq!(relay.allowlists.lock().expect("lock").len(), 1);

    peer.on_event(hello(BOB), now).await;
    link.users
        .lock()
        .expect("lock")
        .as_mut()
        .expect("users")
        .certs
        .remove(&BOB);
    let user = User {
        session: BOB,
        ..User::default()
    };
    peer.on_event(
        Event::UserRemoved {
            user,
            actor: None,
            reason: None,
            ban: false,
        },
        now,
    )
    .await;
    assert!(peer.clients().is_empty());
    *relay.down.lock().expect("lock") = true;
    peer.tick(now + Duration::from_secs(1)).await;
    assert_eq!(relay.allowlists.lock().expect("lock").len(), 1);
    *relay.down.lock().expect("lock") = false;
    peer.tick(now + Duration::from_secs(5)).await;
    assert_eq!(
        relay.allowlists.lock().expect("lock").len(),
        1,
        "waits for the retry"
    );
    peer.tick(now + Duration::from_secs(11)).await;
    assert_eq!(
        relay.allowlists.lock().expect("lock").last(),
        Some(&vec![cert("a1").expect("a")])
    );
}

#[tokio::test]
async fn a_restarted_relay_gets_the_allowlist_and_all_clients_a_new_epoch() {
    let (mut peer, relay, link) = setup();
    let now = Instant::now();
    peer.check_status(now).await;
    peer.tick(now).await;
    for session in [ALICE, BOB] {
        peer.on_event(hello(session), now).await;
    }
    let first_epoch = link.take_sent()[0].1["epoch"].clone();
    peer.check_status(now).await;
    assert!(
        link.take_sent().is_empty(),
        "same instance: no announcement"
    );

    *relay.instance.lock().expect("lock") = "two".into();
    peer.check_status(now).await;
    let sent = link.take_sent();
    assert_eq!(sent.len(), 1, "one message for everyone");
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    assert_ne!(sent[0].1["epoch"], first_epoch);
    peer.tick(now).await;
    assert_eq!(relay.allowlists.lock().expect("lock").len(), 2);
}
