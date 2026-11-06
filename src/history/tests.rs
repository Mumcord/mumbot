//! The history module's core against a scripted Mumcord requester (spec 04, AC-4.2 to AC-4.5):
//! packets are built and read with the ported wire format, as Mumcord's engine sends them.

use std::collections::BTreeSet;

use bytes::Bytes;
use mumcord_protocol::{
    Channel, DisconnectReason, Event, PluginData, ServerState, TextMessage, TextTarget, User,
    UserChange,
};

use super::config::HistoryConfig;
use super::core::{Core, MAX_AGE_MS, MAX_BODY, recorded_channels, records_for};
use super::hash::{Content, content_hash};
use super::notice;
use super::proof;
use super::store::{Record, Store};
use super::wire::{self, DATA_ID, Hash, Packet, WireMessage};
use crate::webhook::ChannelRef;

const START: i64 = 1_700_000_000_000;
const ROOT: u32 = 0;
const LOBBY: u32 = 1;
const RAID: u32 = 2;
const AFK: u32 = 3;
const SUB: u32 = 4;
const BOT: u32 = 1;
const ALICE: u32 = 2;
const GUEST: u32 = 3;
const CAROL: u32 = 4;
const ALICE_CERT: &str = "0123456789abcdef0123456789abcdef01234567";

fn config(channels: &[&str], exclude: &[&str]) -> HistoryConfig {
    HistoryConfig {
        channels: channels
            .iter()
            .map(|c| ChannelRef::Name((*c).to_owned()))
            .collect(),
        exclude: exclude
            .iter()
            .map(|c| ChannelRef::Name((*c).to_owned()))
            .collect(),
        retention_days: 7,
        max_messages_per_channel: 5000,
        database: None,
    }
}

fn channel(id: u32, parent: Option<u32>, name: &str) -> Channel {
    Channel {
        id,
        parent,
        name: name.to_owned(),
        ..Channel::default()
    }
}

fn user(session: u32, name: &str, channel_id: u32, listening: &[u32]) -> User {
    User {
        session,
        name: name.to_owned(),
        channel_id,
        listening: listening.iter().copied().collect::<BTreeSet<u32>>(),
        cert_hash: (session == ALICE).then(|| ALICE_CERT.to_owned()),
        ..User::default()
    }
}

/// Root, Lobby (with Sub), Raid, AFK; the bot sits in Lobby and listens to Raid.
fn state() -> ServerState {
    let mut state = ServerState {
        session: Some(BOT),
        ..ServerState::default()
    };
    for c in [
        channel(ROOT, None, "Root"),
        channel(LOBBY, Some(ROOT), "Lobby"),
        channel(RAID, Some(ROOT), "Raid"),
        channel(AFK, Some(ROOT), "AFK"),
        channel(SUB, Some(LOBBY), "Sub"),
    ] {
        state.channels.insert(c.id, c);
    }
    for u in [
        user(BOT, "History", LOBBY, &[RAID]),
        user(ALICE, "Alice", LOBBY, &[]),
        user(GUEST, "Guest", AFK, &[]),
    ] {
        state.users.insert(u.session, u);
    }
    state
}

fn text(actor: u32, target: TextTarget, body: &str) -> TextMessage {
    TextMessage {
        actor: Some(actor),
        sender: None,
        target,
        message: body.to_owned(),
    }
}

fn to_channels(channels: &[u32]) -> TextTarget {
    TextTarget {
        channels: channels.to_vec(),
        ..TextTarget::default()
    }
}

fn keys(records: &[Record]) -> Vec<&str> {
    records.iter().map(|r| r.channel.as_str()).collect()
}

#[test]
fn channel_messages_are_recorded_private_ones_never() {
    let state = state();
    let cfg = config(&["Lobby", "Raid", "AFK"], &[]);
    let lobby = records_for(
        &cfg,
        &text(ALICE, to_channels(&[LOBBY]), "hi"),
        &state,
        START,
    );
    assert_eq!(
        lobby,
        [Record {
            channel: "Lobby".into(),
            sender_name: "Alice".into(),
            sender_cert: Some(ALICE_CERT.into()),
            sent_at: START,
            body: "hi".into(),
        }]
    );
    // Listened channel, and one message to two channels makes one record per channel.
    let both = records_for(
        &cfg,
        &text(GUEST, to_channels(&[RAID, LOBBY]), "x"),
        &state,
        1,
    );
    assert_eq!(keys(&both), ["Lobby", "Raid"]);
    assert_eq!(both[0].sender_cert, None);
    // Private message to the bot: never, whatever the channel list says.
    let private = records_for(
        &cfg,
        &text(ALICE, TextTarget::user(BOT), "secret"),
        &state,
        1,
    );
    assert!(private.is_empty());
    // AFK is configured, but the bot does not receive it live (not in it, not listening).
    assert!(records_for(&cfg, &text(GUEST, to_channels(&[AFK]), "x"), &state, 1).is_empty());
    // Server messages and oversized bodies (inline images) are skipped.
    let mut server = text(ALICE, to_channels(&[LOBBY]), "x");
    server.actor = None;
    assert!(records_for(&cfg, &server, &state, 1).is_empty());
    let big = "x".repeat(MAX_BODY + 1);
    assert!(records_for(&cfg, &text(ALICE, to_channels(&[LOBBY]), &big), &state, 1).is_empty());
}

#[test]
fn allow_and_deny_lists_and_tree_messages() {
    let mut state = state();
    state
        .users
        .insert(BOT, user(BOT, "History", LOBBY, &[RAID, SUB, AFK]));
    let tree = TextTarget {
        trees: vec![ROOT],
        ..TextTarget::default()
    };
    let only_lobby = config(&["Lobby"], &[]);
    let records = records_for(&only_lobby, &text(ALICE, tree.clone(), "all"), &state, 1);
    assert_eq!(
        keys(&records),
        ["Lobby"],
        "unlisted channels are not recorded"
    );
    let all = config(&["*"], &["AFK"]);
    assert_eq!(recorded_channels(&all, &state), [ROOT, LOBBY, RAID, SUB]);
    let records = records_for(&all, &text(ALICE, tree, "all"), &state, 1);
    assert_eq!(
        keys(&records),
        ["Lobby", "Raid", "Lobby/Sub"],
        "AFK excluded"
    );
    let by_path = config(&["Root/Lobby/Sub", "Raid"], &["Raid"]);
    assert_eq!(recorded_channels(&by_path, &state), [SUB]);
    let mut by_id = config(&[], &[]);
    by_id.channels = vec![ChannelRef::Id(RAID), ChannelRef::Id(99)];
    assert_eq!(recorded_channels(&by_id, &state), [RAID]);
}

/// The bot's core plus a store, fed like the module's run loop feeds it.
struct Holder {
    core: Core,
    now: i64,
}

impl Holder {
    fn new() -> Self {
        let mut holder = Self {
            core: Core::new(
                config(&["Lobby", "Raid"], &[]),
                Vec::new(),
                Store::in_memory().expect("store"),
                START,
            ),
            now: START,
        };
        holder.core.resync(&state(), START);
        holder
    }

    fn record(&mut self, at: i64, sender: u32, channel: u32, body: &str) {
        self.now = at;
        let written = self
            .core
            .record(&text(sender, to_channels(&[channel]), body), &state(), at);
        assert_eq!(written, 1, "{body}");
    }

    fn advance(&mut self, ms: i64) {
        self.now += ms;
        self.core.tick(self.now);
    }

    /// Delivers `data` from `sender` and returns everything the bot sends within 10 s.
    fn ask(&mut self, sender: u32, data: &[u8]) -> Vec<(Vec<u32>, Packet)> {
        self.core.on_event(
            Event::PluginData(PluginData {
                sender: Some(sender),
                data_id: DATA_ID.to_owned(),
                data: Bytes::copy_from_slice(data),
            }),
            self.now,
        );
        self.drain()
    }

    fn drain(&mut self) -> Vec<(Vec<u32>, Packet)> {
        self.drain_for(10_000)
    }

    fn drain_for(&mut self, ms: i64) -> Vec<(Vec<u32>, Packet)> {
        let mut out = Vec::new();
        for _ in 0..ms / 100 {
            self.now += 100;
            self.core.tick(self.now);
            for (to, data) in self.core.take_outgoing(self.now) {
                assert!(data.len() <= mumcord_protocol::PLUGIN_DATA_MAX);
                out.push((to, wire::decode(&data).expect("valid packet")));
            }
        }
        out
    }

    fn join(&mut self, session: u32, name: &str, channel: u32, listening: &[u32]) {
        self.core.on_event(
            Event::UserUpdated(UserChange {
                user: user(session, name, channel, listening),
                joined: true,
                moved_from: None,
                actor: None,
            }),
            self.now,
        );
    }
}

fn hashes_of(packets: &[(Vec<u32>, Packet)]) -> Vec<Hash> {
    packets
        .iter()
        .flat_map(|(_, p)| match p {
            Packet::Hashes { hashes, .. } => hashes.clone(),
            _ => Vec::new(),
        })
        .collect()
}

fn stream_of(packets: &[(Vec<u32>, Packet)]) -> Vec<WireMessage> {
    let data: Vec<u8> = packets
        .iter()
        .flat_map(|(_, p)| match p {
            Packet::Part { data, .. } => data.clone(),
            _ => Vec::new(),
        })
        .collect();
    wire::decode_messages(&data).expect("message stream")
}

fn hash(channel: &str, name: &str, cert: Option<&str>, at: i64, body: &str) -> Hash {
    content_hash(&Content {
        channel,
        sender_name: name,
        sender_cert_hash: cert,
        sent_at: at,
        body,
    })
}

#[test]
fn a_late_joiner_gets_the_channel_history_like_from_a_mumcord_holder() {
    let mut bot = Holder::new();
    bot.record(START + 1_000, ALICE, LOBBY, "<b>first</b>");
    bot.record(START + 2_000, GUEST, LOBBY, "second");
    bot.record(START + 3_000, ALICE, RAID, "raid only");
    bot.advance(60_000);
    bot.join(CAROL, "Carol", LOBBY, &[]);
    let joined = bot.now;
    bot.advance(1_000);

    // 1. Summary request to everyone (Mumcord: `areq` from now − 7 days to the join time).
    let areq = wire::encode(&Packet::AvailabilityRequest {
        req: 7,
        channel: LOBBY,
        from: joined - MAX_AGE_MS,
        to: joined,
    });
    assert_eq!(
        bot.ask(CAROL, &areq),
        [(
            vec![CAROL],
            Packet::Availability {
                req: 7,
                channel: LOBBY,
                count: 2,
                oldest: START + 1_000,
                newest: START + 2_000,
            }
        )]
    );

    // 2. Hash list: equal to what Mumcord computes for the same messages, oldest first.
    let hreq = wire::encode(&Packet::HashRequest {
        req: 8,
        channel: LOBBY,
        from: joined - MAX_AGE_MS,
        to: joined,
    });
    let answer = bot.ask(CAROL, &hreq);
    assert!(answer.iter().all(|(to, _)| to == &[CAROL]));
    assert!(matches!(
        answer.last(),
        Some((_, Packet::Hashes { last: true, .. }))
    ));
    let first = hash("Lobby", "Alice", Some(ALICE_CERT), START + 1_000, "first");
    let second = hash("Lobby", "Guest", None, START + 2_000, "second");
    assert_eq!(hashes_of(&answer), [first, second]);

    // 3. Fetch only the second one (the requester already had the first).
    let get = wire::encode(&Packet::Fetch {
        req: 8,
        wanted: wire::bitmap(2, [1]),
    });
    let parts = bot.ask(CAROL, &get);
    assert_eq!(
        stream_of(&parts),
        [WireMessage {
            ts: START + 2_000,
            n: "Guest".into(),
            h: None,
            b: "second".into(),
        }]
    );
    // The answer is used up: a second `get` gets nothing.
    assert!(bot.ask(CAROL, &get).is_empty());
}

#[test]
fn mumcord_request_fixture_is_answered() {
    // The `hreq` bytes from Mumcord's history_sync tests (`holders_answer_only_from_the_requested_start`).
    let request = br#"{"v":1,"t":"hreq","r":77,"c":1,"from":1699999997000,"to":1700000001000}"#;
    let mut bot = Holder::new();
    bot.record(START - 4_000, ALICE, LOBBY, "too early");
    bot.record(START - 2_000, ALICE, LOBBY, "in range");
    bot.advance(10_000);
    bot.join(CAROL, "Carol", LOBBY, &[]);
    let answer = bot.ask(CAROL, request);
    assert_eq!(
        hashes_of(&answer),
        [hash(
            "Lobby",
            "Alice",
            Some(ALICE_CERT),
            START - 2_000,
            "in range"
        )]
    );
    assert_eq!(
        answer[0].1,
        Packet::Hashes {
            req: 77,
            index: 0,
            last: true,
            hashes: hashes_of(&answer),
        }
    );
}

#[test]
fn only_people_receiving_the_channel_get_messages_from_before_they_did() {
    let mut bot = Holder::new();
    bot.record(START + 1_000, ALICE, LOBBY, "before");
    bot.advance(10_000);
    bot.join(CAROL, "Carol", AFK, &[]);
    let hreq = |req, to| {
        wire::encode(&Packet::HashRequest {
            req,
            channel: LOBBY,
            from: START,
            to,
        })
    };
    // Not in Lobby, not listening: no summary, `deny` for the hash list.
    let areq = wire::encode(&Packet::AvailabilityRequest {
        req: 1,
        channel: LOBBY,
        from: START,
        to: i64::MAX,
    });
    assert!(bot.ask(CAROL, &areq).is_empty());
    assert_eq!(
        bot.ask(CAROL, &hreq(2, i64::MAX)),
        [(vec![CAROL], Packet::Deny { req: 2 })]
    );
    // Listening to Lobby from now on: asking up to "now" is clamped to that moment.
    bot.join(CAROL, "Carol", AFK, &[LOBBY]);
    bot.advance(5_000);
    bot.record(bot.now, ALICE, LOBBY, "seen live by Carol");
    bot.advance(5_000);
    let answer = bot.ask(CAROL, &hreq(3, i64::MAX));
    assert_eq!(
        hashes_of(&answer),
        [hash(
            "Lobby",
            "Alice",
            Some(ALICE_CERT),
            START + 1_000,
            "before"
        )]
    );
    // Someone the bot never saw is ignored.
    assert!(bot.ask(42, &hreq(4, i64::MAX)).is_empty());
}

#[test]
fn requests_are_rate_limited_and_answers_expire() {
    let mut bot = Holder::new();
    bot.record(START + 1_000, ALICE, LOBBY, "hi");
    bot.advance(10_000);
    bot.join(CAROL, "Carol", LOBBY, &[]);
    let areq = wire::encode(&Packet::AvailabilityRequest {
        req: 1,
        channel: LOBBY,
        from: START,
        to: bot.now,
    });
    // Burst of 6 at one moment; the rest is ignored.
    for _ in 0..20 {
        bot.core.on_event(
            Event::PluginData(PluginData {
                sender: Some(CAROL),
                data_id: DATA_ID.to_owned(),
                data: Bytes::copy_from_slice(&areq),
            }),
            bot.now,
        );
    }
    assert_eq!(bot.drain().len(), 6);

    // A hash list whose `get` does not follow within 60 s is dropped.
    let hreq = wire::encode(&Packet::HashRequest {
        req: 9,
        channel: LOBBY,
        from: START,
        to: START + 10_000,
    });
    bot.advance(60_000);
    assert_eq!(hashes_of(&bot.ask(CAROL, &hreq)).len(), 1);
    bot.advance(60_000);
    let get = wire::encode(&Packet::Fetch {
        req: 9,
        wanted: wire::bitmap(1, [0]),
    });
    assert!(bot.ask(CAROL, &get).is_empty());
}

#[test]
fn at_most_three_answers_are_open_and_a_reconnect_forgets_everyone() {
    let mut bot = Holder::new();
    bot.record(START + 1_000, ALICE, LOBBY, "hi");
    bot.advance(10_000);
    for session in 10..14 {
        bot.join(session, &format!("P{session}"), LOBBY, &[]);
    }
    bot.advance(1_000);
    let hreq = wire::encode(&Packet::HashRequest {
        req: 5,
        channel: LOBBY,
        from: START,
        to: START + 10_000,
    });
    for session in 10..13 {
        assert_eq!(hashes_of(&bot.ask(session, &hreq)).len(), 1);
    }
    assert_eq!(bot.ask(13, &hreq), [(vec![13], Packet::Deny { req: 5 })]);

    bot.core
        .on_event(Event::Disconnected(DisconnectReason::Requested), bot.now);
    assert!(
        bot.ask(10, &hreq).is_empty(),
        "sessions are gone after a reconnect"
    );
}

#[test]
fn outgoing_packets_are_paced_below_the_server_limit() {
    let mut bot = Holder::new();
    for i in 0..300 {
        bot.record(
            START + i,
            ALICE,
            LOBBY,
            &format!("message {i} {}", "x".repeat(200)),
        );
    }
    bot.advance(10_000);
    bot.join(CAROL, "Carol", LOBBY, &[]);
    let hreq = wire::encode(&Packet::HashRequest {
        req: 1,
        channel: LOBBY,
        from: START,
        to: bot.now,
    });
    assert_eq!(hashes_of(&bot.ask(CAROL, &hreq)).len(), 300);
    let get = wire::encode(&Packet::Fetch {
        req: 1,
        wanted: wire::bitmap(300, 0..300),
    });
    bot.core.on_event(
        Event::PluginData(PluginData {
            sender: Some(CAROL),
            data_id: DATA_ID.to_owned(),
            data: Bytes::copy_from_slice(&get),
        }),
        bot.now,
    );
    // Burst of 10, then 3 per second.
    let mut first_second = Vec::new();
    for _ in 0..10 {
        bot.now += 100;
        for (to, data) in bot.core.take_outgoing(bot.now) {
            first_second.push((to, wire::decode(&data).expect("valid packet")));
        }
    }
    assert!(
        (10..=13).contains(&first_second.len()),
        "{} packets in the first second",
        first_second.len()
    );
    first_second.extend(bot.drain_for(60_000));
    assert_eq!(stream_of(&first_second).len(), 300);
}

#[test]
fn the_notice_names_channels_retention_and_what_is_never_recorded() {
    let cfg = config(&["Lobby"], &[]);
    let text = notice(&cfg, &["Lobby", "<Raid>"]);
    assert!(text.contains("Lobby, &lt;Raid&gt;"));
    assert!(text.contains("7 days"));
    assert!(text.contains("5000 per channel"));
    assert!(text.contains("Private messages and voice are never recorded"));
    let mut all = config(&["*"], &[]);
    all.retention_days = 1;
    assert!(notice(&all, &[]).contains("all channels"));
    assert!(notice(&all, &[]).contains("1 day "));
}

impl Holder {
    /// Restricts entering `channel` (its own ACL denies Enter), as the server reports it.
    fn lock(&mut self, id: u32) {
        let mut locked = state().channels[&id].clone();
        locked.is_enter_restricted = true;
        self.core.on_event(Event::ChannelUpdated(locked), self.now);
    }

    fn areq(&mut self, sender: u32, req: u32, channel: u32) -> Vec<(Vec<u32>, Packet)> {
        let packet = wire::encode(&Packet::AvailabilityRequest {
            req,
            channel,
            from: self.now - MAX_AGE_MS,
            to: self.now,
        });
        self.ask(sender, &packet)
    }

    fn hreq(&mut self, sender: u32, req: u32, channel: u32) -> Vec<(Vec<u32>, Packet)> {
        let packet = wire::encode(&Packet::HashRequest {
            req,
            channel,
            from: self.now - MAX_AGE_MS,
            to: self.now,
        });
        self.ask(sender, &packet)
    }
}

#[test]
fn listeners_of_a_locked_channel_get_no_history() {
    // Issue Mumcord#382: Listen is granted to everyone, Enter is not.
    let mut bot = Holder::new();
    bot.lock(RAID);
    bot.record(START + 1_000, ALICE, RAID, "locked secret");
    bot.advance(60_000);
    bot.join(CAROL, "Carol", AFK, &[RAID]);
    bot.advance(1_000);
    assert!(bot.areq(CAROL, 1, RAID).is_empty(), "areq: silence");
    assert_eq!(
        bot.hreq(CAROL, 2, RAID),
        [(vec![CAROL], Packet::Deny { req: 2 })]
    );
}

#[test]
fn a_lock_on_a_parent_channel_locks_its_sub_channels() {
    let mut bot = Holder::new();
    bot.core = Core::new(
        config(&["Lobby/Sub"], &[]),
        Vec::new(),
        Store::in_memory().expect("store"),
        START,
    );
    let mut state = state();
    state.users.insert(BOT, user(BOT, "History", LOBBY, &[SUB]));
    bot.core.resync(&state, START);
    bot.lock(LOBBY);
    let written = bot.core.record(
        &text(ALICE, to_channels(&[SUB]), "sub"),
        &state,
        START + 1_000,
    );
    assert_eq!(written, 1);
    bot.advance(60_000);
    bot.join(CAROL, "Carol", AFK, &[SUB]);
    bot.advance(1_000);
    assert_eq!(
        bot.hreq(CAROL, 3, SUB),
        [(vec![CAROL], Packet::Deny { req: 3 })]
    );
}

#[test]
fn people_inside_a_locked_channel_are_answered_by_a_bot_without_tokens() {
    let mut bot = Holder::new();
    bot.lock(RAID);
    bot.record(START + 1_000, ALICE, RAID, "inside");
    bot.advance(60_000);
    bot.join(CAROL, "Carol", RAID, &[]);
    bot.advance(1_000);
    assert_eq!(bot.areq(CAROL, 1, RAID).len(), 1);
    let answer = bot.hreq(CAROL, 2, RAID);
    assert_eq!(hashes_of(&answer).len(), 1);
    // Leaving ends the open answer: the `get` brings no messages.
    bot.join(CAROL, "Carol", AFK, &[RAID]);
    let get = wire::encode(&Packet::Fetch {
        req: 2,
        wanted: wire::bitmap(1, [0]),
    });
    assert!(bot.ask(CAROL, &get).is_empty());
}

const BOT_CERT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CAROL_CERT: &str = "cccccccccccccccccccccccccccccccccccccccc";
const TOKEN: &str = "s3cret";

/// A bot with the password of the locked Raid, one message there, and Carol (with a
/// certificate) inside Raid.
fn token_holder() -> Holder {
    let mut bot = Holder::new();
    bot.core = Core::new(
        config(&["Lobby", "Raid"], &[]),
        vec![TOKEN.to_owned()],
        Store::in_memory().expect("store"),
        START,
    );
    let mut state = state();
    if let Some(own) = state.users.get_mut(&BOT) {
        own.cert_hash = Some(BOT_CERT.to_owned());
    }
    bot.core.resync(&state, START);
    bot.lock(RAID);
    bot.record(START + 1_000, ALICE, RAID, "locked secret");
    bot.advance(60_000);
    bot.carol(RAID, &[]);
    bot.advance(1_000);
    bot
}

impl Holder {
    fn carol(&mut self, channel: u32, listening: &[u32]) {
        let mut carol = user(CAROL, "Carol", channel, listening);
        carol.cert_hash = Some(CAROL_CERT.to_owned());
        self.core.on_event(
            Event::UserUpdated(UserChange {
                user: carol,
                joined: false,
                moved_from: None,
                actor: None,
            }),
            self.now,
        );
    }

    /// Sends `hreq` and returns the challenge's nonce, checking that a PermissionQuery is due.
    fn challenge(&mut self, req: u32) -> proof::Nonce {
        let packet = wire::encode(&Packet::HashRequest {
            req,
            channel: RAID,
            from: self.now - MAX_AGE_MS,
            to: self.now,
        });
        self.core.on_event(
            Event::PluginData(PluginData {
                sender: Some(CAROL),
                data_id: DATA_ID.to_owned(),
                data: Bytes::copy_from_slice(&packet),
            }),
            self.now,
        );
        assert_eq!(self.core.take_permission_queries(), [RAID]);
        match self.drain_for(100).as_slice() {
            [
                (
                    to,
                    Packet::Challenge {
                        req: r,
                        channel,
                        nonce,
                    },
                ),
            ] => {
                assert_eq!((to.as_slice(), *r, *channel), (&[CAROL][..], req, RAID));
                *nonce
            }
            other => panic!("expected a challenge, got {other:?}"),
        }
    }

    fn prove(
        &mut self,
        req: u32,
        tokens: &[&str],
        nonce: &proof::Nonce,
    ) -> Vec<(Vec<u32>, Packet)> {
        let tokens: Vec<String> = tokens.iter().map(|t| (*t).to_owned()).collect();
        let macs = proof::prove(&tokens, nonce, RAID, CAROL_CERT, BOT_CERT);
        self.ask(CAROL, &wire::encode(&Packet::Proof { req, macs }))
    }
}

#[test]
fn a_bot_with_tokens_answers_a_locked_channel_after_the_password_proof() {
    let mut bot = token_holder();
    let nonce = bot.challenge(5);
    bot.core.own_permissions(RAID, true, bot.now);
    let answer = bot.prove(5, &["wrong", TOKEN], &nonce);
    assert_eq!(hashes_of(&answer).len(), 1);
    let get = wire::encode(&Packet::Fetch {
        req: 5,
        wanted: wire::bitmap(1, [0]),
    });
    assert_eq!(stream_of(&bot.ask(CAROL, &get))[0].b, "locked secret");
    // The nonce is used up: the same proof again is ignored.
    assert!(bot.prove(5, &[TOKEN], &nonce).is_empty());
}

#[test]
fn wrong_late_or_unconfirmed_proofs_are_denied() {
    let deny = |req| [(vec![CAROL], Packet::Deny { req })];
    // Wrong password.
    let mut bot = token_holder();
    let nonce = bot.challenge(1);
    bot.core.own_permissions(RAID, true, bot.now);
    assert_eq!(bot.prove(1, &["wrong"], &nonce), deny(1));
    // The server never confirmed the bot's own Enter after the challenge (outdated token).
    let mut bot = token_holder();
    bot.core.own_permissions(RAID, true, bot.now - 1);
    let nonce = bot.challenge(2);
    assert_eq!(bot.prove(2, &[TOKEN], &nonce), deny(2));
    let mut bot = token_holder();
    let nonce = bot.challenge(3);
    bot.core.own_permissions(RAID, false, bot.now);
    assert_eq!(bot.prove(3, &[TOKEN], &nonce), deny(3));
    // Nobody answers the challenge: denied after 10 s, a late proof is ignored.
    let mut bot = token_holder();
    let nonce = bot.challenge(4);
    bot.core.own_permissions(RAID, true, bot.now);
    assert_eq!(bot.drain_for(10_200), deny(4));
    assert!(bot.prove(4, &[TOKEN], &nonce).is_empty());
    // The requester left the channel before proving: the challenge is gone.
    let mut bot = token_holder();
    let nonce = bot.challenge(6);
    bot.core.own_permissions(RAID, true, bot.now);
    bot.carol(AFK, &[RAID]);
    assert!(bot.prove(6, &[TOKEN], &nonce).is_empty());
}

#[test]
fn a_bot_with_tokens_denies_without_certificates_or_to_listeners() {
    // Carol has no certificate: no proof is possible.
    let mut bot = token_holder();
    bot.join(CAROL, "Carol", RAID, &[]);
    assert_eq!(
        bot.hreq(CAROL, 1, RAID),
        [(vec![CAROL], Packet::Deny { req: 1 })]
    );
    assert!(bot.core.take_permission_queries().is_empty());
    // A listener gets no challenge at all.
    let mut bot = token_holder();
    bot.carol(AFK, &[RAID]);
    assert_eq!(
        bot.hreq(CAROL, 2, RAID),
        [(vec![CAROL], Packet::Deny { req: 2 })]
    );
}

#[test]
fn the_get_is_denied_once_the_bot_lost_its_enter_permission() {
    let mut bot = token_holder();
    let nonce = bot.challenge(5);
    bot.core.own_permissions(RAID, true, bot.now);
    assert_eq!(hashes_of(&bot.prove(5, &[TOKEN], &nonce)).len(), 1);
    // The password was changed on the server.
    bot.core.own_permissions(RAID, false, bot.now);
    let get = wire::encode(&Packet::Fetch {
        req: 5,
        wanted: wire::bitmap(1, [0]),
    });
    assert_eq!(
        bot.ask(CAROL, &get),
        [(vec![CAROL], Packet::Deny { req: 5 })]
    );
}

#[test]
fn open_channels_need_no_proof_even_with_tokens() {
    let mut bot = token_holder();
    bot.record(bot.now, ALICE, LOBBY, "open");
    bot.advance(1_000);
    bot.carol(LOBBY, &[]);
    bot.advance(1_000);
    bot.carol(AFK, &[LOBBY]);
    bot.advance(1_000);
    let answer = bot.hreq(CAROL, 7, LOBBY);
    assert!(bot.core.take_permission_queries().is_empty());
    assert_eq!(
        hashes_of(&answer).len(),
        1,
        "listeners of open channels are answered"
    );
}
