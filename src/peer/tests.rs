use std::sync::{Arc, Mutex};

use anyhow::bail;
use bytes::Bytes;
use mumble_client_protocol::{DisconnectReason, PluginData, User, UserChange};

use super::*;

const BOT: u32 = 1;
const ALICE: u32 = 2;
const BOB: u32 = 3;
const OFFICIAL: u32 = 4;
const NO_CERT: u32 = 5;
/// Has a certificate but is not registered.
const GUEST: u32 = 6;
/// Alice's second device: same certificate, same account.
const ALICE2: u32 = 7;
const CAROL: u32 = 8;
const ALICE_ID: u32 = 10;
const BOB_ID: u32 = 11;
const CAROL_ID: u32 = 13;
const RID: &str = "0123456789abcdef";

/// How the mock answers `POST /v1/pairs`.
#[derive(Clone, Copy, PartialEq)]
enum PairAnswer {
    Ok,
    Full,
    RateLimited,
    UseNotAllowed,
    Fail,
}

/// How the mock answers the broadcast calls; `None` creates or admits.
type BcastAnswer = Option<fn() -> BroadcastError>;

const BCAST_ID: &str = "00000000000000b1";

struct MockRelay {
    status: Mutex<Option<Status>>,
    create_answer: Mutex<BcastAnswer>,
    admit_answer: Mutex<BcastAnswer>,
    /// Broadcast calls in order, e.g. `create a1 25`, `create a1 8 plg`,
    /// `admit 00000000000000b1 b1`.
    bcast_calls: Mutex<Vec<String>>,
    next_id: Mutex<u64>,
    pair_answer: Mutex<PairAnswer>,
    pairs: Mutex<Vec<(String, String)>>,
    pair_uses: Mutex<Vec<RelayUse>>,
    removed: Mutex<Vec<String>>,
}

impl RelayApi for Arc<MockRelay> {
    async fn status(&self) -> Result<Status> {
        match self.status.lock().expect("lock").clone() {
            Some(status) => Ok(status),
            None => bail!("down"),
        }
    }

    async fn create_pair(&self, a: &str, b: &str, relay_use: RelayUse) -> Result<Pair, PairError> {
        self.pair_uses.lock().expect("lock").push(relay_use);
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
            PairAnswer::UseNotAllowed => Err(PairError::UseNotAllowed),
            PairAnswer::Fail => Err(PairError::Unavailable(anyhow::anyhow!("down"))),
        }
    }

    async fn remove_peer(&self, hash: &str) -> Result<()> {
        self.removed.lock().expect("lock").push(hash.to_owned());
        Ok(())
    }

    async fn create_broadcast(
        &self,
        publisher: &str,
        max_viewers: u16,
        relay_use: RelayUse,
        _media: bool,
    ) -> Result<NewBroadcast, BroadcastError> {
        let kind = match relay_use {
            RelayUse::ScreenShare => String::new(),
            other => format!(" {}", other.as_str()),
        };
        self.call(format!("create {} {max_viewers}{kind}", short(publisher)));
        if let Some(error) = *self.create_answer.lock().expect("lock") {
            return Err(error());
        }
        let mut next = self.next_id.lock().expect("lock");
        let id = BroadcastId(*next);
        *next += 1;
        Ok(NewBroadcast {
            id,
            tok: "P".repeat(43),
            expires_in: 120,
        })
    }

    async fn admit_viewer(
        &self,
        id: BroadcastId,
        viewer: &str,
    ) -> Result<ViewerToken, BroadcastError> {
        self.call(format!(
            "admit {} {}",
            relay::broadcast_path_id(id),
            short(viewer)
        ));
        if let Some(error) = *self.admit_answer.lock().expect("lock") {
            return Err(error());
        }
        Ok(ViewerToken {
            tok: "V".repeat(43),
            expires_in: 60,
        })
    }

    async fn remove_viewer(&self, id: BroadcastId, viewer: &str) -> Result<()> {
        self.call(format!(
            "kick {} {}",
            relay::broadcast_path_id(id),
            short(viewer)
        ));
        Ok(())
    }

    async fn delete_broadcast(&self, id: BroadcastId) -> Result<()> {
        self.call(format!("delete {}", relay::broadcast_path_id(id)));
        Ok(())
    }
}

impl MockRelay {
    fn call(&self, call: String) {
        self.bcast_calls.lock().expect("lock").push(call);
    }

    fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.bcast_calls.lock().expect("lock"))
    }
}

/// The name a test certificate was made from (`cert("a1")` → `a1`).
fn short(hash: &str) -> &str {
    hash.trim_start_matches('0')
}

#[derive(Default)]
struct MockLink {
    /// Whether the bot's own user is registered; `None` while it counts as disconnected.
    registered: Mutex<Option<bool>>,
    users: Mutex<Option<Users>>,
    sent: Mutex<Vec<(Vec<u32>, serde_json::Value)>>,
    /// What the mock answers an ACL query with, and the channels it was asked for.
    answer: Mutex<Option<Result<groups::Members, groups::QueryError>>>,
    queries: Mutex<Vec<String>>,
}

impl MockLink {
    fn take_sent(&self) -> Vec<(Vec<u32>, serde_json::Value)> {
        std::mem::take(&mut *self.sent.lock().expect("lock"))
    }

    fn take_queries(&self) -> Vec<String> {
        std::mem::take(&mut *self.queries.lock().expect("lock"))
    }

    fn answers(&self, read: Result<groups::Members, groups::QueryError>) {
        *self.answer.lock().expect("lock") = Some(read);
    }
}

fn members_of(groups: &[(&str, &[u32])]) -> groups::Members {
    groups
        .iter()
        .map(|(name, ids)| {
            (
                name.to_string(),
                ids.iter().copied().collect::<HashSet<u32>>(),
            )
        })
        .collect()
}

fn group_rules(names: &[&str]) -> Rules {
    Rules {
        allowed_groups: names.iter().map(|n| n.to_string()).collect(),
        ..Rules::default()
    }
}

impl Link for Arc<MockLink> {
    async fn registered(&self) -> Option<bool> {
        *self.registered.lock().expect("lock")
    }

    async fn users(&self) -> Option<Users> {
        self.users.lock().expect("lock").clone()
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        assert!(data.len() <= mumble_client_protocol::PLUGIN_DATA_MAX);
        let json = serde_json::from_slice(&data).expect("json");
        self.sent
            .lock()
            .expect("lock")
            .push((receivers.to_vec(), json));
        Ok(())
    }

    async fn acl(&self, channel: &str) -> Result<groups::Members, groups::QueryError> {
        self.queries.lock().expect("lock").push(channel.to_owned());
        self.answer
            .lock()
            .expect("lock")
            .clone()
            .unwrap_or(Err(groups::QueryError::NoPermission))
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
        uses: None,
        broadcast: None,
        plugin_data: None,
        media: 0,
    }
}

struct Setup {
    peer: Peer<Arc<MockRelay>, Arc<MockLink>>,
    relay: Arc<MockRelay>,
    link: Arc<MockLink>,
}

fn setup() -> Setup {
    setup_with(Rules::default())
}

fn guests_too() -> Rules {
    Rules {
        registered_only: false,
        ..Rules::default()
    }
}

fn setup_with(rules: Rules) -> Setup {
    let relay = Arc::new(MockRelay {
        status: Mutex::new(Some(status("one"))),
        create_answer: Mutex::new(None),
        admit_answer: Mutex::new(None),
        bcast_calls: Mutex::default(),
        next_id: Mutex::new(0xb1),
        pair_answer: Mutex::new(PairAnswer::Ok),
        pairs: Mutex::default(),
        pair_uses: Mutex::default(),
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
            (GUEST, cert("g1")),
            (ALICE2, cert("a1")),
            (CAROL, cert("c2")),
        ]),
        user_ids: HashMap::from([
            (ALICE, ALICE_ID),
            (ALICE2, ALICE_ID),
            (BOB, BOB_ID),
            (OFFICIAL, 12),
            (CAROL, CAROL_ID),
        ]),
        // Everyone in the root channel, Carol elsewhere.
        channels: HashMap::from([
            (BOT, 0),
            (ALICE, 0),
            (BOB, 0),
            (OFFICIAL, 0),
            (NO_CERT, 0),
            (GUEST, 0),
            (ALICE2, 0),
            (CAROL, 5),
        ]),
    });
    let peer = Peer::new(Arc::clone(&relay), Arc::clone(&link), rules);
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

fn request_for(sender: u32, peer: u32, relay_use: &str) -> Event {
    plugin(
        sender,
        "mumcord.peer",
        format!(r#"{{"v":1,"t":"token-req","rid":"{RID}","peer":{peer},"use":"{relay_use}"}}"#)
            .as_bytes(),
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
    ready_with(Rules::default(), &[ALICE, BOB]).await
}

async fn ready_with(rules: Rules, sessions: &[u32]) -> Setup {
    let mut s = setup_with(rules);
    s.peer.check_status().await;
    for &session in sessions {
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
            "fp":"ab".repeat(32),"state":"up","seq":1,"reg":true})
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
    s.peer.on_event(hello(GUEST), Instant::now()).await;
    s.link.take_sent();
    let cases = [
        (request(GUEST, ALICE), "not-registered"),
        (request(ALICE, GUEST), "not-registered"),
        (request(ALICE, ALICE), "denied"),
        (request(ALICE, BOT), "denied"),
        (request(OFFICIAL, ALICE), "no-hello"),
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
        echoed_rights: 0,
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

fn tokens(sent: &[(Vec<u32>, serde_json::Value)]) -> usize {
    sent.iter().filter(|(_, json)| json["t"] == "token").count()
}

#[tokio::test]
async fn without_registered_only_guests_get_tokens_within_the_limits() {
    let mut s = ready_with(guests_too(), &[ALICE, GUEST]).await;
    let now = Instant::now();
    s.peer.on_event(request(GUEST, ALICE), now).await;
    s.peer.on_event(request(ALICE, GUEST), now).await;
    let sent = s.link.take_sent();
    assert_eq!(tokens(&sent), 2, "as requester and as peer");
    for _ in 1..=REQUESTS_PER_MINUTE {
        s.peer.on_event(request(GUEST, ALICE), now).await;
    }
    assert_eq!(
        s.link.take_sent().last().expect("sent").1["r"],
        "rate-limited",
        "the limits still apply"
    );
}

#[tokio::test]
async fn the_hash_is_dropped_only_when_its_last_session_leaves() {
    let mut s = ready_with(Rules::default(), &[ALICE, ALICE2, BOB]).await;
    let now = Instant::now();
    s.peer.on_event(request(ALICE, BOB), now).await;
    s.peer.on_event(removed(ALICE, cert("a1")), now).await;
    assert!(
        s.relay.removed.lock().expect("lock").is_empty(),
        "the second device keeps the relay session"
    );
    s.peer.on_event(removed(ALICE2, cert("a1")), now).await;
    assert_eq!(
        *s.relay.removed.lock().expect("lock"),
        vec![cert("a1").expect("a")]
    );
}

#[tokio::test]
async fn requests_are_limited_per_certificate_across_sessions() {
    let mut s = ready_with(Rules::default(), &[ALICE, ALICE2, BOB]).await;
    let now = Instant::now();
    for _ in 0..REQUESTS_PER_MINUTE {
        s.peer.on_event(request(ALICE, BOB), now).await;
    }
    let limit = config::CERT_REQUESTS_PER_MINUTE;
    for _ in REQUESTS_PER_MINUTE..=limit {
        s.peer.on_event(request(ALICE2, BOB), now).await;
    }
    let sent = s.link.take_sent();
    assert_eq!(tokens(&sent), limit);
    assert_eq!(sent.last().expect("sent").1["r"], "rate-limited");

    // A reconnect gets a new session but keeps the certificate's budget.
    s.peer.on_event(removed(ALICE, cert("a1")), now).await;
    s.peer.on_event(hello(ALICE), now).await;
    s.link.take_sent();
    s.peer.on_event(request(ALICE, BOB), now).await;
    assert_eq!(s.link.take_sent()[0].1["r"], "rate-limited");
    s.peer
        .on_event(request(ALICE, BOB), now + Duration::from_secs(61))
        .await;
    assert_eq!(s.link.take_sent()[0].1["t"], "token");
}

#[tokio::test]
async fn open_pairs_are_capped_per_certificate() {
    let rules = Rules {
        max_pairs_per_cert: 1,
        ..Rules::default()
    };
    let mut s = ready_with(rules, &[ALICE, BOB, CAROL]).await;
    let now = Instant::now();
    s.peer.on_event(request(ALICE, BOB), now).await;
    s.peer.on_event(request(ALICE, BOB), now).await;
    s.peer.on_event(request(ALICE, CAROL), now).await;
    let sent = s.link.take_sent();
    assert_eq!(tokens(&sent), 2, "a new token for the same pair is fine");
    assert_eq!(sent[2].1["r"], "rate-limited");
    s.peer.on_event(removed(BOB, cert("b1")), now).await;
    s.peer.on_event(request(ALICE, CAROL), now).await;
    assert_eq!(s.link.take_sent()[0].1["t"], "token", "Bob's pair ended");
}

#[tokio::test]
async fn the_relays_uses_and_broadcast_limit_go_into_the_announcement() {
    let mut s = setup();
    let mut limited = status("one");
    limited.uses = Some(vec!["screen_share".into(), "images".into()]);
    limited.broadcast = Some(BroadcastStatus { max_viewers: 25 });
    *s.relay.status.lock().expect("lock") = Some(limited);
    s.peer.on_event(hello(ALICE), Instant::now()).await;
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["u"], serde_json::json!(["img", "scr"]));
    assert_eq!(sent[0].1["bc"], 25);
    assert_eq!(sent[0].1["seq"], 1);
}

#[tokio::test]
async fn a_relay_without_uses_or_broadcasts_announces_neither() {
    let mut s = ready().await;
    s.peer.check_status().await;
    s.peer.on_event(hello(CAROL), Instant::now()).await;
    let sent = s.link.take_sent();
    assert!(sent[0].1.get("u").is_none());
    assert!(sent[0].1.get("bc").is_none());
}

#[tokio::test]
async fn a_changed_use_list_or_broadcast_limit_is_announced_to_all() {
    let mut s = ready().await;
    let mut changed = status("one");
    changed.uses = Some(vec!["files".into()]);
    *s.relay.status.lock().expect("lock") = Some(changed.clone());
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].0, vec![ALICE, BOB]);
    assert_eq!(sent[0].1["u"], serde_json::json!(["file"]));

    changed.broadcast = Some(BroadcastStatus { max_viewers: 10 });
    *s.relay.status.lock().expect("lock") = Some(changed);
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["bc"], 10);
    assert_eq!(sent[0].1["seq"], 3);
}

#[tokio::test]
async fn the_requested_use_reaches_the_relay_and_a_missing_one_means_screen_share() {
    let mut s = ready().await;
    let now = Instant::now();
    s.peer.on_event(request_for(ALICE, BOB, "file"), now).await;
    s.peer.on_event(request_for(ALICE, BOB, "img"), now).await;
    s.peer.on_event(request_for(ALICE, BOB, "scr"), now).await;
    s.peer.on_event(request(ALICE, BOB), now).await;
    assert_eq!(
        *s.relay.pair_uses.lock().expect("lock"),
        vec![
            RelayUse::Files,
            RelayUse::Images,
            RelayUse::ScreenShare,
            RelayUse::ScreenShare
        ]
    );
}

#[tokio::test]
async fn a_use_the_relay_refuses_is_answered_with_use_not_allowed() {
    let mut s = ready().await;
    *s.relay.pair_answer.lock().expect("lock") = PairAnswer::UseNotAllowed;
    s.peer
        .on_event(request_for(ALICE, BOB, "file"), Instant::now())
        .await;
    let sent = s.link.take_sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, vec![ALICE]);
    assert_eq!(
        sent[0].1,
        serde_json::json!({"v":1,"t":"token-err","rid":RID,"r":"use-not-allowed"})
    );
}

fn screen_hello(sender: u32) -> Event {
    plugin(
        sender,
        "mumcord.hello",
        br#"{"app":"mumcord","v":1,"features":["p2p","screen-share"]}"#,
    )
}

fn peer_message(sender: u32, json: &str) -> Event {
    plugin(sender, "mumcord.peer", json.as_bytes())
}

fn bcast_req(sender: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":25}}"#),
    )
}

fn admit(sender: u32, viewer: u32) -> Event {
    peer_message(
        sender,
        &format!(
            r#"{{"v":1,"t":"bcast-admit","rid":"{RID}","id":"{BCAST_ID}","viewer":{viewer}}}"#
        ),
    )
}

fn kick(sender: u32, viewer: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-kick","id":"{BCAST_ID}","viewer":{viewer}}}"#),
    )
}

fn moved(session: u32, cert_hash: Option<String>) -> Event {
    Event::UserUpdated(UserChange {
        user: user(session, cert_hash),
        joined: false,
        moved_from: Some(0),
        actor: None,
        echoed_rights: 0,
    })
}

fn set_channel(s: &Setup, session: u32, channel: u32) {
    let mut users = s.link.users.lock().expect("lock");
    users
        .as_mut()
        .expect("users")
        .channels
        .insert(session, channel);
}

/// Alice, Bob, Carol and Alice's second device can share screens; Alice runs broadcast b1.
async fn sharing() -> Setup {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    for session in [ALICE, BOB, CAROL, ALICE2] {
        s.peer.on_event(screen_hello(session), now).await;
    }
    s.link.take_sent();
    s.peer.on_event(bcast_req(ALICE), now).await;
    s.link.take_sent();
    s.relay.take_calls();
    s
}

fn bcast_req_media(sender: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":25,"mv":2}}"#),
    )
}

fn only_error(s: &Setup, to: u32) -> String {
    let sent = s.link.take_sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, vec![to]);
    assert_eq!(sent[0].1["t"], "bcast-err");
    assert_eq!(sent[0].1["rid"], RID);
    sent[0].1["r"].as_str().expect("reason").to_owned()
}

#[tokio::test]
async fn a_sharer_gets_a_broadcast_for_its_certificate() {
    let mut s = setup();
    s.peer.check_status().await;
    s.peer.on_event(screen_hello(ALICE), Instant::now()).await;
    s.link.take_sent();
    s.peer.on_event(bcast_req(ALICE), Instant::now()).await;
    assert_eq!(s.relay.take_calls(), vec!["create a1 25"]);
    assert_eq!(
        s.link.take_sent(),
        vec![(
            vec![ALICE],
            serde_json::json!({"v":1,"t":"bcast","rid":RID,"id":BCAST_ID,
                "tok":"P".repeat(43),"ttl":120})
        )]
    );
    // A new share replaces the running broadcast of the same session.
    s.peer.on_event(bcast_req(ALICE), Instant::now()).await;
    assert_eq!(
        s.relay.take_calls(),
        vec!["create a1 25", "delete 00000000000000b1"]
    );
    assert_eq!(s.link.take_sent()[0].1["id"], "00000000000000b2");
}

#[tokio::test]
async fn the_media_version_passes_through_status_and_broadcast() {
    let mut s = setup();
    let mut relay = status("one");
    relay.media = 2;
    *s.relay.status.lock().expect("lock") = Some(relay);
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(screen_hello(ALICE), now).await;
    let announced = s.link.take_sent();
    let relay = announced
        .iter()
        .find(|(_, message)| message["t"] == "relay")
        .expect("relay announcement");
    assert_eq!(relay.1["mv"], 2);

    s.peer.on_event(bcast_req_media(ALICE), now).await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["t"], "bcast");
    assert_eq!(sent[0].1["mv"], 2);

    s.peer.on_event(screen_hello(BOB), now).await;
    s.link.take_sent();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["t"], "bcast-tok");
    assert_eq!(sent[0].1["mv"], 2);
}

#[tokio::test]
async fn a_broadcast_needs_a_screen_sharing_registered_client_and_a_working_relay() {
    let mut s = setup_with(Rules::default());
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(hello(BOB), now).await;
    s.peer.on_event(screen_hello(GUEST), now).await;
    s.peer.on_event(screen_hello(NO_CERT), now).await;
    s.peer.on_event(screen_hello(ALICE), now).await;
    s.link.take_sent();
    for (sender, reason) in [
        (BOB, "no-hello"),
        (GUEST, "not-registered"),
        (NO_CERT, "denied"),
        (OFFICIAL, "no-hello"),
    ] {
        s.peer.on_event(bcast_req(sender), now).await;
        assert_eq!(only_error(&s, sender), reason, "session {sender}");
    }
    let refusals: [(BcastAnswer, &str); 3] = [
        (Some(|| BroadcastError::RelayFull), "relay-full"),
        (Some(|| BroadcastError::UseNotAllowed), "use-not-allowed"),
        (
            Some(|| BroadcastError::Unavailable(anyhow::anyhow!("down"))),
            "relay-down",
        ),
    ];
    for (answer, reason) in refusals {
        *s.relay.create_answer.lock().expect("lock") = answer;
        s.peer.on_event(bcast_req(ALICE), now).await;
        assert_eq!(only_error(&s, ALICE), reason);
    }
    *s.relay.create_answer.lock().expect("lock") = None;
    *s.relay.status.lock().expect("lock") = None;
    s.peer.check_status().await;
    s.peer.check_status().await;
    s.link.take_sent();
    s.relay.take_calls();
    s.peer.on_event(bcast_req(ALICE), now).await;
    assert_eq!(only_error(&s, ALICE), "relay-down");
    assert!(s.relay.take_calls().is_empty(), "a down relay is not asked");
    // A later hello without screen-share takes the right back.
    *s.relay.status.lock().expect("lock") = Some(status("one"));
    s.peer.check_status().await;
    s.peer.on_event(hello(ALICE), now).await;
    s.link.take_sent();
    s.peer.on_event(bcast_req(ALICE), now).await;
    assert_eq!(only_error(&s, ALICE), "no-hello");
}

#[tokio::test]
async fn an_admitted_viewer_gets_its_token_and_nobody_else_does() {
    let mut s = sharing().await;
    s.peer.on_event(admit(ALICE, BOB), Instant::now()).await;
    assert_eq!(s.relay.take_calls(), vec!["admit 00000000000000b1 b1"]);
    assert_eq!(
        s.link.take_sent(),
        vec![(
            vec![BOB],
            serde_json::json!({"v":1,"t":"bcast-tok","id":BCAST_ID,
                "tok":"V".repeat(43),"ttl":60,"sharer":ALICE,"k":"scr"})
        )]
    );
}

fn admit_id(sender: u32, id: &str, viewer: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-admit","rid":"{RID}","id":"{id}","viewer":{viewer}}}"#),
    )
}

#[tokio::test]
async fn a_viewer_token_carries_the_kind_of_its_broadcast() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    let all = br#"{"app":"mumcord","v":1,"features":["p2p","screen-share","plugins","camera"]}"#;
    for session in [ALICE, BOB] {
        s.peer
            .on_event(plugin(session, "mumcord.hello", all), now)
            .await;
    }
    s.peer.on_event(bcast_req(ALICE), now).await;
    s.peer.on_event(plg_req(ALICE), now).await;
    s.peer.on_event(cam_req(ALICE), now).await;
    s.link.take_sent();
    for (id, kind) in [
        ("00000000000000b1", "scr"),
        ("00000000000000b2", "plg"),
        ("00000000000000b3", "cam"),
    ] {
        s.peer.on_event(admit_id(ALICE, id, BOB), now).await;
        let sent = s.link.take_sent();
        assert_eq!(sent.len(), 1, "{kind}: {sent:?}");
        assert_eq!(sent[0].0, vec![BOB]);
        assert_eq!(sent[0].1["t"], "bcast-tok");
        assert_eq!(sent[0].1["id"], id);
        assert_eq!(sent[0].1["k"], kind);
    }
}

#[tokio::test]
async fn admissions_are_checked_before_the_relay_is_asked() {
    let mut s = sharing().await;
    let now = Instant::now();
    s.peer.on_event(hello(OFFICIAL), now).await;
    s.link.take_sent();
    // Not the publisher (even with Alice's certificate on another device).
    for sender in [BOB, ALICE2] {
        s.peer.on_event(admit(sender, CAROL), now).await;
        assert_eq!(only_error(&s, sender), "denied");
    }
    // Viewers that cannot take part: the bot, the sharer's own identity, offline, no
    // certificate, no screen-share hello, a guest.
    for (viewer, reason) in [
        (BOT, "denied"),
        (ALICE, "denied"),
        (ALICE2, "denied"),
        (99, "denied"),
        (NO_CERT, "denied"),
        (OFFICIAL, "no-hello"),
        (GUEST, "no-hello"),
    ] {
        s.peer.on_event(admit(ALICE, viewer), now).await;
        assert_eq!(only_error(&s, ALICE), reason, "viewer {viewer}");
    }
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    assert_eq!(only_error(&s, ALICE), "not-same-channel");
    // An unknown broadcast.
    let other =
        r#"{"v":1,"t":"bcast-admit","rid":"0123456789abcdef","id":"00000000000000ff","viewer":3}"#;
    s.peer.on_event(peer_message(ALICE, other), now).await;
    assert_eq!(only_error(&s, ALICE), "denied");
    assert!(s.relay.take_calls().is_empty());
}

#[tokio::test]
async fn relay_refusals_of_an_admission_are_mapped() {
    let mut s = sharing().await;
    let now = Instant::now();
    *s.relay.admit_answer.lock().expect("lock") = Some(|| BroadcastError::Full);
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "full");
    *s.relay.admit_answer.lock().expect("lock") =
        Some(|| BroadcastError::Unavailable(anyhow::anyhow!("down")));
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "relay-down");
    // Ended at the relay: the bot forgets it, so the next admission is refused right away.
    *s.relay.admit_answer.lock().expect("lock") = Some(|| BroadcastError::Gone);
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "denied");
    s.relay.take_calls();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "denied");
    assert!(s.relay.take_calls().is_empty());
}

#[tokio::test]
async fn broadcast_requests_are_limited_per_session_and_minute() {
    let mut s = sharing().await;
    let now = Instant::now();
    for _ in 1..BROADCAST_REQUESTS_PER_MINUTE {
        s.peer.on_event(admit(ALICE, BOB), now).await;
    }
    s.link.take_sent();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "rate-limited");
    s.peer.on_event(bcast_req(ALICE), now).await;
    assert_eq!(only_error(&s, ALICE), "rate-limited");
    s.peer
        .on_event(admit(ALICE, BOB), now + Duration::from_secs(61))
        .await;
    assert_eq!(s.link.take_sent()[0].1["t"], "bcast-tok");
}

#[tokio::test]
async fn only_the_publisher_kicks_and_only_admitted_viewers() {
    let mut s = sharing().await;
    let now = Instant::now();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    s.relay.take_calls();
    s.link.take_sent();
    s.peer.on_event(kick(BOB, BOB), now).await;
    s.peer.on_event(kick(ALICE, CAROL), now).await;
    assert!(s.relay.take_calls().is_empty());
    s.peer.on_event(kick(ALICE, BOB), now).await;
    assert_eq!(s.relay.take_calls(), vec!["kick 00000000000000b1 b1"]);
    s.peer.on_event(kick(ALICE, BOB), now).await;
    assert!(s.relay.take_calls().is_empty(), "already removed");
    assert!(s.link.take_sent().is_empty(), "a kick is never answered");
}

#[tokio::test]
async fn leaving_ends_the_broadcast_or_the_watching() {
    let mut s = sharing().await;
    let now = Instant::now();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    s.relay.take_calls();
    s.link.take_sent();
    s.peer.on_event(removed(BOB, cert("b1")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["kick 00000000000000b1 b1"]);
    // A new certificate is a new person to the relay.
    let change = UserChange {
        user: user(ALICE, cert("a9")),
        joined: false,
        moved_from: None,
        actor: None,
        echoed_rights: 0,
    };
    s.peer.on_event(Event::UserUpdated(change), now).await;
    assert_eq!(s.relay.take_calls(), vec!["delete 00000000000000b1"]);
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    assert_eq!(only_error(&s, ALICE), "denied");
}

#[tokio::test]
async fn the_publisher_leaving_deletes_its_broadcast() {
    let mut s = sharing().await;
    s.peer
        .on_event(removed(ALICE, cert("a1")), Instant::now())
        .await;
    assert_eq!(s.relay.take_calls(), vec!["delete 00000000000000b1"]);
}

#[tokio::test]
async fn moving_out_of_the_sharers_channel_ends_the_watching() {
    let mut s = sharing().await;
    let now = Instant::now();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    s.relay.take_calls();
    // Carol moves into the channel and is admitted too.
    set_channel(&s, CAROL, 0);
    s.peer.on_event(moved(CAROL, cert("c2")), now).await;
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    s.relay.take_calls();
    // Bob moves away.
    set_channel(&s, BOB, 5);
    s.peer.on_event(moved(BOB, cert("b1")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["kick 00000000000000b1 b1"]);
    // The sharer moves to Bob: Carol stays behind and is removed.
    set_channel(&s, ALICE, 5);
    s.peer.on_event(moved(ALICE, cert("a1")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["kick 00000000000000b1 c2"]);
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(s.link.take_sent().last().expect("sent").1["t"], "bcast-tok");
}

#[tokio::test]
async fn a_relay_restart_or_reconnect_forgets_the_broadcasts() {
    let mut s = sharing().await;
    let now = Instant::now();
    *s.relay.status.lock().expect("lock") = Some(status("two"));
    s.peer.check_status().await;
    s.link.take_sent();
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "denied");

    let mut s = sharing().await;
    s.peer
        .on_event(Event::Disconnected(DisconnectReason::Closed), now)
        .await;
    s.peer.on_event(removed(ALICE, cert("a1")), now).await;
    assert!(s.relay.take_calls().is_empty());
}

#[tokio::test]
async fn only_group_members_get_tokens_when_groups_are_configured() {
    let mut s = ready_with(group_rules(&["Relay"]), &[ALICE, BOB, GUEST]).await;
    s.link.answers(Ok(members_of(&[("Relay", &[ALICE_ID])])));
    let now = Instant::now();
    s.peer.on_event(request(ALICE, BOB), now).await;
    assert_eq!(
        s.link.take_sent()[0].1["r"],
        "group-not-allowed",
        "both users of the pair must be members"
    );
    assert_eq!(
        s.link.take_queries(),
        vec!["Root".to_owned()],
        "read per request"
    );

    s.link
        .answers(Ok(members_of(&[("Relay", &[ALICE_ID, BOB_ID])])));
    s.peer
        .on_event(request(ALICE, BOB), now + groups::MIN_READ_INTERVAL)
        .await;
}

#[tokio::test]
async fn members_are_read_again_per_request_but_not_more_than_once_per_interval() {
    let mut s = ready_with(group_rules(&["Relay"]), &[ALICE, BOB]).await;
    s.link
        .answers(Ok(members_of(&[("Relay", &[ALICE_ID, BOB_ID])])));
    let now = Instant::now();
    for _ in 0..2 {
        s.peer.on_event(request(ALICE, BOB), now).await;
    }
    assert_eq!(
        s.link.take_queries().len(),
        1,
        "the second request uses the answer"
    );
    s.peer
        .on_event(request(ALICE, BOB), now + groups::MIN_READ_INTERVAL)
        .await;
    assert_eq!(s.link.take_queries(), vec!["Root".to_owned()]);
}

#[tokio::test]
async fn a_failed_group_read_never_keeps_the_previous_membership() {
    let mut s = ready_with(group_rules(&["Relay"]), &[ALICE, BOB]).await;
    s.link
        .answers(Ok(members_of(&[("Relay", &[ALICE_ID, BOB_ID])])));
    let now = Instant::now();
    s.peer.on_event(request(ALICE, BOB), now).await;
    assert_eq!(s.link.take_sent()[0].1["t"], "token");

    s.link.answers(Err(groups::QueryError::NoPermission));
    s.peer
        .on_event(request(ALICE, BOB), now + groups::MIN_READ_INTERVAL)
        .await;
    assert_eq!(
        s.link.take_sent()[0].1["r"],
        "denied",
        "the bot fails closed, not open"
    );
    // Once the server answers again, tokens work again.
    s.link
        .answers(Ok(members_of(&[("Relay", &[ALICE_ID, BOB_ID])])));
    s.peer
        .on_event(request(ALICE, BOB), now + groups::MIN_READ_INTERVAL * 2)
        .await;
    assert_eq!(s.link.take_sent()[0].1["t"], "token");
}

#[tokio::test]
async fn the_bot_refuses_to_start_without_the_write_permission_or_an_unknown_group() {
    let mut s = setup_with(group_rules(&["Relay", "Mods"]));
    s.link.answers(Err(groups::QueryError::NoPermission));
    assert!(s.peer.check_access().await.is_err(), "no permission");

    s.link.answers(Ok(members_of(&[("Relay", &[ALICE_ID])])));
    assert!(
        s.peer.check_access().await.is_err(),
        "a configured group that the channel does not define"
    );

    s.link.answers(Ok(members_of(&[
        ("Relay", &[ALICE_ID]),
        ("Mods", &[BOB_ID]),
    ])));
    s.peer.check_access().await.expect("groups readable");
}

#[tokio::test]
async fn the_bot_refuses_to_run_until_its_own_user_is_registered() {
    let link = Arc::new(MockLink::default());
    *link.registered.lock().expect("lock") = Some(false);
    let error = check_registered(&link)
        .await
        .expect_err("Mumcord ignores announcements of unregistered users");
    assert!(error.to_string().contains("registered"), "{error}");

    *link.registered.lock().expect("lock") = Some(true);
    check_registered(&link)
        .await
        .expect("registered bots may run");
}

#[tokio::test]
async fn a_lost_group_member_is_dropped_at_the_relay_with_its_pair_and_broadcast() {
    let mut s = setup_with(group_rules(&["Relay"]));
    let now = Instant::now();
    s.peer.check_status().await;
    for session in [ALICE, BOB] {
        s.peer.on_event(screen_hello(session), now).await;
    }
    s.link
        .answers(Ok(members_of(&[("Relay", &[ALICE_ID, BOB_ID])])));
    s.peer.check_groups().await;
    s.peer.on_event(request(ALICE, BOB), now).await;
    assert_eq!(tokens(&s.link.take_sent()), 1);
    s.peer.on_event(bcast_req(ALICE), now).await;
    s.peer.on_event(admit(ALICE, BOB), now).await;
    s.link.take_sent();
    s.relay.take_calls();

    s.link.answers(Ok(members_of(&[("Relay", &[ALICE_ID])])));
    s.peer.check_groups().await;
    assert_eq!(
        s.relay.take_calls(),
        vec![format!("kick {BCAST_ID} b1")],
        "Bob is removed as a viewer"
    );
    assert_eq!(
        *s.relay.removed.lock().expect("lock"),
        vec![cert("b1").expect("b")],
        "and his open pair ended"
    );

    // A guest that never had a pair or a broadcast is not sent to the relay again.
    s.peer.on_event(hello(GUEST), now).await;
    s.peer.check_groups().await;
    assert!(s.relay.take_calls().is_empty());
    assert!(
        s.relay.removed.lock().expect("lock").len() == 1,
        "still only Bob"
    );
}

fn plugins_hello(sender: u32) -> Event {
    plugin(
        sender,
        "mumcord.hello",
        br#"{"app":"mumcord","v":1,"features":["p2p","plugins"]}"#,
    )
}

fn plg_req(sender: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":8,"k":"plg"}}"#),
    )
}

/// Adds a registered user with its own certificate in `channel`.
fn add_user(s: &Setup, session: u32, channel: u32) {
    let mut users = s.link.users.lock().expect("lock");
    let users = users.as_mut().expect("users");
    users.certs.insert(session, cert(&format!("u{session}")));
    users.user_ids.insert(session, 100 + session);
    users.channels.insert(session, channel);
}

#[tokio::test]
async fn plugin_data_and_its_limit_go_into_the_announcement() {
    let mut s = setup();
    let mut relay = status("one");
    relay.uses = Some(vec!["plugin_data".into(), "screen_share".into()]);
    relay.plugin_data = Some(PluginDataStatus {
        max_kbps_per_sender: 512,
    });
    *s.relay.status.lock().expect("lock") = Some(relay.clone());
    s.peer.check_status().await;
    s.peer.on_event(hello(ALICE), Instant::now()).await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["u"], serde_json::json!(["scr", "plg"]));
    assert_eq!(sent[0].1["pk"], 512);

    // Switched off: neither the use nor its limit is announced.
    relay.uses = Some(vec!["screen_share".into()]);
    *s.relay.status.lock().expect("lock") = Some(relay);
    s.peer.check_status().await;
    let sent = s.link.take_sent();
    assert_eq!(sent[0].1["u"], serde_json::json!(["scr"]));
    assert!(sent[0].1.get("pk").is_none());
}

#[tokio::test]
async fn a_plugin_data_token_request_asks_the_relay_for_plugin_data() {
    let mut s = ready().await;
    s.peer
        .on_event(request_for(ALICE, BOB, "plg"), Instant::now())
        .await;
    assert_eq!(
        *s.relay.pair_uses.lock().expect("lock"),
        vec![RelayUse::PluginData]
    );
    assert_eq!(s.link.take_sent()[0].1["t"], "token");
}

#[tokio::test]
async fn a_plugin_data_broadcast_needs_the_plugins_hello_not_screen_sharing() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(screen_hello(ALICE), now).await;
    s.peer.on_event(plugins_hello(BOB), now).await;
    s.link.take_sent();
    s.peer.on_event(plg_req(ALICE), now).await;
    assert_eq!(only_error(&s, ALICE), "no-hello");
    s.peer.on_event(bcast_req(BOB), now).await;
    assert_eq!(only_error(&s, BOB), "no-hello");
    assert_eq!(s.relay.take_calls(), Vec::<String>::new());

    s.peer.on_event(plg_req(BOB), now).await;
    assert_eq!(s.relay.take_calls(), vec!["create b1 8 plg"]);
    assert_eq!(
        s.link.take_sent(),
        vec![(
            vec![BOB],
            serde_json::json!({"v":1,"t":"bcast","rid":RID,"id":BCAST_ID,
                "tok":"P".repeat(43),"ttl":120})
        )]
    );
}

#[tokio::test]
async fn a_plugin_data_broadcast_admits_only_plugin_clients_in_the_channel() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(plugins_hello(ALICE), now).await;
    s.peer.on_event(screen_hello(BOB), now).await;
    s.peer.on_event(plugins_hello(CAROL), now).await;
    s.peer.on_event(plg_req(ALICE), now).await;
    s.link.take_sent();
    s.relay.take_calls();

    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "no-hello");
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    assert_eq!(only_error(&s, ALICE), "not-same-channel");
    assert_eq!(s.relay.take_calls(), Vec::<String>::new());

    set_channel(&s, CAROL, 0);
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    assert_eq!(s.relay.take_calls(), vec!["admit 00000000000000b1 c2"]);
    assert_eq!(s.link.take_sent()[0].0, vec![CAROL]);

    // Moving out of the sender's channel ends the receiving, as with screen sharing.
    set_channel(&s, CAROL, 5);
    s.peer.on_event(moved(CAROL, cert("c2")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["kick 00000000000000b1 c2"]);
}

#[tokio::test]
async fn a_session_may_share_its_screen_and_send_plugin_data_at_once() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer
        .on_event(
            plugin(
                ALICE,
                "mumcord.hello",
                br#"{"app":"mumcord","v":1,"features":["p2p","screen-share","plugins"]}"#,
            ),
            now,
        )
        .await;
    s.peer.on_event(bcast_req(ALICE), now).await;
    s.peer.on_event(plg_req(ALICE), now).await;
    // A second plugin data request replaces only the plugin data broadcast.
    s.peer.on_event(plg_req(ALICE), now).await;
    assert_eq!(
        s.relay.take_calls(),
        vec![
            "create a1 25",
            "create a1 8 plg",
            "create a1 8 plg",
            "delete 00000000000000b2"
        ]
    );
    // Leaving ends both.
    s.peer.on_event(removed(ALICE, cert("a1")), now).await;
    let mut calls = s.relay.take_calls();
    calls.sort();
    assert_eq!(
        calls,
        vec!["delete 00000000000000b1", "delete 00000000000000b3"]
    );
}

#[tokio::test]
async fn at_most_eight_plugin_data_senders_per_channel() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    let senders: Vec<u32> = (20..29).collect();
    for &session in &senders {
        add_user(&s, session, 7);
        s.peer.on_event(plugins_hello(session), now).await;
    }
    add_user(&s, 40, 9);
    s.peer.on_event(plugins_hello(40), now).await;
    s.link.take_sent();
    for &session in &senders[..8] {
        s.peer.on_event(plg_req(session), now).await;
    }
    assert_eq!(s.relay.take_calls().len(), 8);
    s.link.take_sent();

    // The ninth sender in the channel is refused before the relay is asked.
    s.peer.on_event(plg_req(senders[8]), now).await;
    assert_eq!(only_error(&s, senders[8]), "relay-full");
    assert_eq!(s.relay.take_calls(), Vec::<String>::new());
    // A running sender may renew its broadcast, and other channels are not affected.
    s.peer.on_event(plg_req(senders[0]), now).await;
    s.peer.on_event(plg_req(40), now).await;
    assert_eq!(s.relay.take_calls().len(), 3);

    // Once a sender leaves, the next one gets its place.
    s.peer.on_event(removed(senders[1], cert("u21")), now).await;
    s.relay.take_calls();
    s.link.take_sent();
    s.peer.on_event(plg_req(senders[8]), now).await;
    assert_eq!(s.relay.take_calls(), vec!["create u28 8 plg"]);
}

fn camera_hello(sender: u32) -> Event {
    plugin(
        sender,
        "mumcord.hello",
        br#"{"app":"mumcord","v":1,"features":["p2p","screen-share","camera"]}"#,
    )
}

fn cam_req(sender: u32) -> Event {
    peer_message(
        sender,
        &format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":25,"k":"cam"}}"#),
    )
}

#[tokio::test]
async fn the_camera_use_goes_into_the_announcement() {
    let mut s = setup();
    let mut relay = status("one");
    relay.uses = Some(vec!["camera".into(), "screen_share".into()]);
    *s.relay.status.lock().expect("lock") = Some(relay);
    s.peer.check_status().await;
    s.peer.on_event(hello(ALICE), Instant::now()).await;
    assert_eq!(
        s.link.take_sent()[0].1["u"],
        serde_json::json!(["scr", "cam"])
    );
}

#[tokio::test]
async fn a_camera_needs_the_camera_hello_and_runs_beside_a_screen_share() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(screen_hello(BOB), now).await;
    s.peer.on_event(camera_hello(ALICE), now).await;
    s.link.take_sent();
    s.peer.on_event(cam_req(BOB), now).await;
    assert_eq!(only_error(&s, BOB), "no-hello");
    assert_eq!(s.relay.take_calls(), Vec::<String>::new());

    s.peer.on_event(bcast_req(ALICE), now).await;
    s.peer.on_event(cam_req(ALICE), now).await;
    // A second camera request replaces only the camera.
    s.peer.on_event(cam_req(ALICE), now).await;
    assert_eq!(
        s.relay.take_calls(),
        vec![
            "create a1 25",
            "create a1 25 cam",
            "create a1 25 cam",
            "delete 00000000000000b2"
        ]
    );
    let sent = s.link.take_sent();
    assert!(
        sent.iter()
            .all(|(to, m)| *to == vec![ALICE] && m["t"] == "bcast")
    );

    // Changing channel ends the camera at the relay; the screen share keeps running (spec 07).
    set_channel(&s, ALICE, 5);
    s.peer.on_event(moved(ALICE, cert("a1")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["delete 00000000000000b3"]);
    set_channel(&s, CAROL, 5);
    s.peer.on_event(camera_hello(CAROL), now).await;
    s.peer.on_event(admit(ALICE, CAROL), now).await;
    assert_eq!(s.relay.take_calls(), vec!["admit 00000000000000b1 c2"]);
}

#[tokio::test]
async fn a_full_camera_room_answers_room_full() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    let senders: Vec<u32> = (20..30).collect();
    for &session in &senders {
        add_user(&s, session, 7);
        s.peer.on_event(camera_hello(session), now).await;
    }
    add_user(&s, 40, 9);
    s.peer.on_event(camera_hello(40), now).await;
    s.link.take_sent();
    for &session in &senders[..config::MAX_CAMERAS_PER_CHANNEL] {
        s.peer.on_event(cam_req(session), now).await;
    }
    assert_eq!(s.relay.take_calls().len(), config::MAX_CAMERAS_PER_CHANNEL);
    s.link.take_sent();

    // The tenth camera in the channel is refused before the relay is asked.
    s.peer.on_event(cam_req(senders[9]), now).await;
    assert_eq!(only_error(&s, senders[9]), "room-full");
    assert_eq!(s.relay.take_calls(), Vec::<String>::new());
    // Screen sharing in that channel and cameras elsewhere are not affected.
    s.peer.on_event(bcast_req(senders[9]), now).await;
    s.peer.on_event(cam_req(40), now).await;
    assert_eq!(s.relay.take_calls().len(), 2);
    s.link.take_sent();

    // A camera sender who moves away frees its place.
    set_channel(&s, senders[0], 9);
    s.peer.on_event(moved(senders[0], cert("u20")), now).await;
    assert_eq!(s.relay.take_calls(), vec!["delete 00000000000000b1"]);
    s.peer.on_event(cam_req(senders[9]), now).await;
    assert_eq!(s.relay.take_calls(), vec!["create u29 25 cam"]);
}

#[tokio::test]
async fn the_camera_room_size_comes_from_the_config() {
    let mut s = setup_with(Rules {
        max_cameras_per_channel: 1,
        ..Rules::default()
    });
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(camera_hello(ALICE), now).await;
    s.peer.on_event(camera_hello(BOB), now).await;
    s.peer.on_event(cam_req(ALICE), now).await;
    s.link.take_sent();
    s.peer.on_event(cam_req(BOB), now).await;
    assert_eq!(only_error(&s, BOB), "room-full");
}

#[tokio::test]
async fn camera_admits_have_their_own_burst_bucket() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    s.peer.on_event(camera_hello(ALICE), now).await;
    s.peer.on_event(camera_hello(BOB), now).await;
    s.peer.on_event(cam_req(ALICE), now).await;
    s.link.take_sent();
    // A sender admits the whole channel at once: 24 go through, the 25th waits.
    for _ in 0..CAMERA_ADMIT_BURST {
        s.peer.on_event(admit(ALICE, BOB), now).await;
    }
    let sent = s.link.take_sent();
    assert!(sent.iter().all(|(_, m)| m["t"] == "bcast-tok"), "{sent:?}");
    assert_eq!(sent.len(), CAMERA_ADMIT_BURST as usize);
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "rate-limited");
    // The bucket refills by one per second (60 per minute).
    let later = now + Duration::from_secs(1);
    s.peer.on_event(admit(ALICE, BOB), later).await;
    assert_eq!(s.link.take_sent()[0].1["t"], "bcast-tok");
    s.peer.on_event(admit(ALICE, BOB), later).await;
    assert_eq!(only_error(&s, ALICE), "rate-limited");
    // Camera admits do not use up the shared budget of requests.
    s.peer.on_event(bcast_req(ALICE), later).await;
    assert_eq!(s.link.take_sent()[0].1["t"], "bcast");
}

/// Forgets where the server says `session` is, as after a reconnect.
fn forget_channel(s: &Setup, session: u32) {
    let mut users = s.link.users.lock().expect("lock");
    users.as_mut().expect("users").channels.remove(&session);
}

#[tokio::test]
async fn a_camera_sender_without_a_known_channel_is_told_so() {
    let mut s = setup();
    s.peer.check_status().await;
    let now = Instant::now();
    for session in [ALICE, BOB] {
        s.peer.on_event(camera_hello(session), now).await;
    }
    s.peer.on_event(cam_req(ALICE), now).await;
    s.link.take_sent();
    s.relay.take_calls();

    forget_channel(&s, ALICE);
    // A camera needs the sender's channel for its room; an admission is about both channels.
    s.peer.on_event(cam_req(ALICE), now).await;
    assert_eq!(only_error(&s, ALICE), "not-in-channel");
    s.peer.on_event(admit(ALICE, BOB), now).await;
    assert_eq!(only_error(&s, ALICE), "not-in-channel");
    assert!(s.relay.take_calls().is_empty());
}

#[tokio::test]
async fn the_announcement_says_when_guests_get_no_token() {
    let mut s = setup_with(guests_too());
    s.peer.check_status().await;
    s.peer.on_event(hello(ALICE), Instant::now()).await;
    assert_eq!(
        s.link.take_sent()[0].1.get("reg"),
        None,
        "guests get tokens, so there is nothing to warn about"
    );

    // Group members are registered users, so `registered_only = false` changes nothing.
    let rules = Rules {
        registered_only: false,
        ..group_rules(&["Relay"])
    };
    let mut s = setup_with(rules);
    s.peer.check_status().await;
    s.peer.on_event(hello(ALICE), Instant::now()).await;
    assert_eq!(s.link.take_sent()[0].1["reg"], true);
}
