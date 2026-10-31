use std::sync::{Arc, Mutex};

use anyhow::bail;
use bytes::Bytes;
use mumcord_protocol::{DisconnectReason, PluginData, User, UserChange};

use super::*;

const BOT: u32 = 1;
const ALICE: u32 = 2;
const BOB: u32 = 3;
const OFFICIAL: u32 = 4;
const NO_CERT: u32 = 5;
const RID: &str = "0123456789abcdef";

/// How the mock answers `POST /v1/pairs`.
#[derive(Clone, Copy, PartialEq)]
enum PairAnswer {
    Ok,
    Full,
    RateLimited,
    Fail,
}

struct MockRelay {
    status: Mutex<Option<Status>>,
    pair_answer: Mutex<PairAnswer>,
    pairs: Mutex<Vec<(String, String)>>,
    removed: Mutex<Vec<String>>,
}

impl RelayApi for Arc<MockRelay> {
    async fn status(&self) -> Result<Status> {
        match self.status.lock().expect("lock").clone() {
            Some(status) => Ok(status),
            None => bail!("down"),
        }
    }

    async fn create_pair(&self, a: &str, b: &str) -> Result<Pair, PairError> {
        self.pairs
            .lock()
            .expect("lock")
            .push((a.to_owned(), b.to_owned()));
        match *self.pair_answer.lock().expect("lock") {
            PairAnswer::Ok => Ok(Pair {
                tok: "T".repeat(43),
                expires_in: 120,
            }),
            PairAnswer::Full => Err(PairError::Full),
            PairAnswer::RateLimited => Err(PairError::RateLimited),
            PairAnswer::Fail => Err(PairError::Unavailable(anyhow::anyhow!("down"))),
        }
    }

    async fn remove_peer(&self, hash: &str) -> Result<()> {
        self.removed.lock().expect("lock").push(hash.to_owned());
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

fn cert(name: &str) -> Option<String> {
    Some(format!("{name:0>40}"))
}

fn status(boot_id: &str) -> Status {
    Status {
        boot_id: boot_id.into(),
        host: "relay.test".into(),
        udp: 443,
        tcp: 443,
        fp: "ab".repeat(32),
    }
}

struct Setup {
    peer: Peer<Arc<MockRelay>, Arc<MockLink>>,
    relay: Arc<MockRelay>,
    link: Arc<MockLink>,
}

fn setup() -> Setup {
    let relay = Arc::new(MockRelay {
        status: Mutex::new(Some(status("one"))),
        pair_answer: Mutex::new(PairAnswer::Ok),
        pairs: Mutex::default(),
        removed: Mutex::default(),
    });
    let link = Arc::new(MockLink::default());
    *link.users.lock().expect("lock") = Some(Users {
        own: BOT,
        certs: HashMap::from([
            (BOT, cert("b0")),
            (ALICE, cert("a1")),
            (BOB, cert("b1")),
            (OFFICIAL, cert("c1")),
            (NO_CERT, None),
        ]),
    });
    let peer = Peer::new(Arc::clone(&relay), Arc::clone(&link));
    Setup { peer, relay, link }
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
        br#"{"app":"mumcord","v":1,"features":["p2p"]}"#,
    )
}

fn request(sender: u32, peer: u32) -> Event {
    plugin(
        sender,
        "mumcord.peer",
        format!(r#"{{"v":1,"t":"token-req","rid":"{RID}","peer":{peer}}}"#).as_bytes(),
    )
}

fn user(session: u32, cert_hash: Option<String>) -> User {
    User {
        session,
        cert_hash,
        ..User::default()
    }
}

fn removed(session: u32, cert_hash: Option<String>) -> Event {
    Event::UserRemoved {
        user: user(session, cert_hash),
        actor: None,
        reason: None,
        ban: false,
    }
}

/// Alice and Bob said hello, the relay is up; nothing sent so far is kept.
async fn ready() -> Setup {
    let mut s = setup();
    s.peer.check_status().await;
    for session in [ALICE, BOB] {
        s.peer.on_event(hello(session), Instant::now()).await;
    }
    s.link.take_sent();
    s
}

#[tokio::test]
async fn a_p2p_hello_gets_the_announcement_once_and_only_to_that_client() {
    let mut s = setup();
    let now = Instant::now();
    s.peer.on_event(hello(ALICE), now).await;
    assert!(
        s.link.take_sent().is_empty(),
        "nothing to announce before the relay answered"
    );
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec![ALICE]);
    assert_eq!(
        sent[0].1,
        serde_json::json!({"v":1,"t":"relay","host":"relay.test","udp":443,"tcp":443,
            "fp":"ab".repeat(32),"state":"up","seq":1})
    );
    s.peer.on_event(hello(ALICE), now).await;
    let no_p2p = br#"{"app":"mumcord","v":1,"features":[]}"#;
    s.peer
        .on_event(plugin(OFFICIAL, "mumcord.hello", no_p2p), now)
        .await;
    assert!(
        s.link.take_sent().is_empty(),
        "repeated or non-P2P hellos get nothing"
    );
    s.peer.on_event(hello(BOB), now).await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].0, vec![BOB]);
    assert_eq!(sent[0].1["seq"], 1);
    assert_eq!(s.peer.clients(), vec![ALICE, BOB]);
}

#[tokio::test]
async fn every_relay_change_is_announced_to_all_in_one_message() {
    let mut s = ready().await;
    s.peer.check_status().await;
    assert!(s.link.take_sent().is_empty(), "unchanged status");

    *s.relay.status.lock().expect("lock") = Some(status("two"));
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    assert_eq!(sent[0].1["state"], "up");
    assert_eq!(sent[0].1["seq"], 2);

    let mut moved = status("two");
    moved.host = "relay2.test".into();
    *s.relay.status.lock().expect("lock") = Some(moved);
    s.peer.check_status().await;
    assert_eq!(s.link.take_sent()[0].1["host"], "relay2.test");

    *s.relay.status.lock().expect("lock") = None;
    s.peer.check_status().await;
    assert!(
        s.link.take_sent().is_empty(),
        "one failed poll is not down yet"
    );
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    assert_eq!(sent[0].1["state"], "down");
    assert_eq!(sent[0].1["seq"], 4);
    s.peer.check_status().await;
    assert!(s.link.take_sent().is_empty(), "down is announced once");

    *s.relay.status.lock().expect("lock") = Some(status("three"));
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["state"], "up");
    assert_eq!(sent[0].1["seq"], 5);
}

#[tokio::test]
async fn a_token_from_the_relay_goes_to_both_of_the_pair() {
    let mut s = ready().await;
    s.peer.on_event(request(ALICE, BOB), Instant::now()).await;
    assert_eq!(
        *s.relay.pairs.lock().expect("lock"),
        vec![(cert("a1").expect("a"), cert("b1").expect("b"))]
    );
    let sent = s.link.take_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    assert_eq!(
        sent[0].1,
        serde_json::json!({"v":1,"t":"token","rid":RID,"a":ALICE,"b":BOB,
            "tok":"T".repeat(43),"ttl":120})
    );
}

#[tokio::test]
async fn refused_requests_get_the_reason_and_reach_only_the_requester() {
    let mut s = ready().await;
    s.peer.on_event(hello(NO_CERT), Instant::now()).await;
    s.link.take_sent();
    let cases = [
        (request(ALICE, ALICE), "denied"),
        (request(ALICE, BOT), "denied"),
        (request(OFFICIAL, ALICE), "denied"),
        (request(NO_CERT, ALICE), "denied"),
        (request(ALICE, NO_CERT), "denied"),
        (request(ALICE, 99), "peer-gone"),
        (request(BOB, OFFICIAL), "peer-not-mumcord"),
    ];
    for (event, reason) in cases {
        s.peer.on_event(event, Instant::now()).await;
        let sent = s.link.take_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].1,
            serde_json::json!({"v":1,"t":"token-err","rid":RID,"r":reason})
        );
    }
    assert!(s.relay.pairs.lock().expect("lock").is_empty());

    for (answer, reason) in [
        (PairAnswer::Full, "relay-full"),
        (PairAnswer::RateLimited, "rate-limited"),
        (PairAnswer::Fail, "relay-down"),
    ] {
        *s.relay.pair_answer.lock().expect("lock") = answer;
        s.peer.on_event(request(BOB, ALICE), Instant::now()).await;
        let sent = s.link.take_sent();
        assert_eq!(sent[0].0, vec![BOB]);
        assert_eq!(sent[0].1["r"], reason);
    }
}

#[tokio::test]
async fn a_down_relay_is_not_asked_for_tokens() {
    let mut s = ready().await;
    *s.relay.status.lock().expect("lock") = None;
    s.peer.check_status().await;
    s.peer.check_status().await;
    s.link.take_sent();
    s.peer.on_event(request(ALICE, BOB), Instant::now()).await;
    assert_eq!(s.link.take_sent()[0].1["r"], "relay-down");
    assert!(s.relay.pairs.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn requests_are_limited_per_session_and_minute() {
    let mut s = ready().await;
    let now = Instant::now();
    for _ in 0..=REQUESTS_PER_MINUTE {
        s.peer.on_event(request(ALICE, BOB), now).await;
    }
    let sent = s.link.take_sent();
    let tokens = sent.iter().filter(|(_, json)| json["t"] == "token").count();
    assert_eq!(tokens, REQUESTS_PER_MINUTE);
    assert_eq!(sent.last().expect("sent").1["r"], "rate-limited");
    s.peer
        .on_event(request(ALICE, BOB), now + Duration::from_secs(61))
        .await;
    assert_eq!(s.link.take_sent()[0].1["t"], "token");
}

#[tokio::test]
async fn leaving_or_changing_certificate_drops_the_hash_at_the_relay() {
    let mut s = ready().await;
    let now = Instant::now();
    let change = UserChange {
        user: user(BOB, cert("b2")),
        joined: false,
        moved_from: None,
        actor: None,
    };
    s.peer.on_event(Event::UserUpdated(change), now).await;
    s.peer.on_event(removed(ALICE, cert("a1")), now).await;
    s.peer.on_event(removed(OFFICIAL, cert("c1")), now).await;
    assert_eq!(
        *s.relay.removed.lock().expect("lock"),
        vec![cert("b1").expect("b"), cert("a1").expect("a")],
        "only Mumcord P2P clients, the old hash on a change"
    );
    assert_eq!(s.peer.clients(), vec![BOB]);
    s.peer
        .on_event(Event::Disconnected(DisconnectReason::Closed), now)
        .await;
    assert!(s.peer.clients().is_empty());
}
