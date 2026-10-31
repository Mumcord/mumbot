//! The history module's state between events: what to record, and the holder side of
//! Mumcord's history sync (spec 04, FR-4.4 to FR-4.16), ported from Mumcord's
//! `crates/engine/src/history_sync/core.rs` (the "Answering" half).
//!
//! **Recording:** a channel message is stored for every recorded channel it reaches that the
//! bot receives live (its own channel or a channel it listens to). Private messages are never
//! stored.
//!
//! **Answering:** channel messages only, and only up to the moment the requester started
//! receiving that channel live as seen here (join, move or listen; the bot's login for people
//! already there); requests reaching later are clamped. Abuse limits: a per-person request
//! budget, at most [`MAX_SERVES`] open answers, size caps on everything. Outgoing packets are
//! paced below the server's plugin message rate limit and sent round robin per receiver.
//!
//! Pure logic over Unix milliseconds: packets go out through [`Core::take_outgoing`], so all of
//! it runs in unit tests without a server. Store calls block, so the caller runs this off the
//! async runtime.

use std::collections::{HashMap, HashSet, VecDeque};

use mumcord_protocol::{Event, ServerState, TextMessage, User};

use super::config::HistoryConfig;
use super::hash::{Content, content_hash};
use super::store::{Record, Store};
use super::wire::{self, DATA_ID, Hash, MAX_MESSAGES, Packet, WireMessage};
use crate::checks::resolve_channel;
use crate::webhook::ChannelRef;

/// How far back an answer reaches (mumcord FR-1.60).
pub const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// An answer whose hash list was not followed by `get` within this time is dropped.
const SERVE_TTL_MS: i64 = 60_000;
/// Most answers prepared at the same time.
pub const MAX_SERVES: usize = 3;
/// Messages with a larger body (e.g. inline images) are neither recorded nor synced.
pub const MAX_BODY: usize = 8 * 1024;
/// Largest message stream of one answer.
pub const MAX_STREAM: usize = 256 * 1024;
/// Requests per person: refill rate per second and burst.
const PEER_RATE: f64 = 0.2;
const PEER_BURST: f64 = 6.0;
/// Outgoing packets per second and burst, below murmur's default plugin message limit
/// (4 per second, burst 15).
const SEND_RATE: f64 = 3.0;
const SEND_BURST: f64 = 10.0;

/// A token bucket over Unix milliseconds.
struct Bucket {
    tokens: f64,
    at: i64,
    rate: f64,
    burst: f64,
}

impl Bucket {
    fn new(rate: f64, burst: f64, now: i64) -> Self {
        Self {
            tokens: burst,
            at: now,
            rate,
            burst,
        }
    }

    fn take(&mut self, now: i64) -> bool {
        let elapsed = now.saturating_sub(self.at).max(0) as f64 / 1000.0;
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.at = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Outgoing packets, one queue per receiver list, served round robin.
#[derive(Default)]
struct Outbox {
    queues: VecDeque<(Vec<u32>, VecDeque<Vec<u8>>)>,
}

impl Outbox {
    fn push(&mut self, to: Vec<u32>, packet: &Packet) {
        let data = wire::encode(packet);
        match self.queues.iter_mut().find(|(t, _)| *t == to) {
            Some((_, queue)) => queue.push_back(data),
            None => self.queues.push_back((to, VecDeque::from([data]))),
        }
    }

    fn pop(&mut self) -> Option<(Vec<u32>, Vec<u8>)> {
        let (to, mut queue) = self.queues.pop_front()?;
        let data = queue.pop_front()?;
        if !queue.is_empty() {
            self.queues.push_back((to.clone(), queue));
        }
        Some((to, data))
    }

    fn forget(&mut self, session: u32) {
        self.queues.retain(|(to, _)| to != &[session]);
    }
}

struct Peer {
    /// Channels this person receives live (in it or listening to it) and since when.
    live: HashMap<u32, i64>,
    budget: Bucket,
}

struct Serve {
    requester: u32,
    req: u32,
    created: i64,
    messages: Vec<Record>,
}

pub struct Core {
    config: HistoryConfig,
    store: Store,
    /// Channels and users as the events describe them.
    state: ServerState,
    peers: HashMap<u32, Peer>,
    serves: Vec<Serve>,
    outbox: Outbox,
    pacing: Bucket,
}

impl Core {
    pub fn new(config: HistoryConfig, store: Store, now: i64) -> Self {
        Self {
            config,
            store,
            state: ServerState::default(),
            peers: HashMap::new(),
            serves: Vec::new(),
            outbox: Outbox::default(),
            pacing: Bucket::new(SEND_RATE, SEND_BURST, now),
        }
    }

    /// Takes over a snapshot of the connection after a login or missed events. People keep the
    /// times they were already known to receive a channel.
    pub fn resync(&mut self, state: &ServerState, now: i64) {
        self.state.channels = state.channels.clone();
        self.state.users = state.users.clone();
        self.state.session = state.session;
        self.peers
            .retain(|session, _| state.users.contains_key(session));
        for user in state.users.values() {
            self.user_seen(user, now);
        }
    }

    /// One event of the connection; text messages go through [`Core::record`].
    pub fn on_event(&mut self, event: Event, now: i64) {
        match event {
            Event::ChannelUpdated(channel) => {
                self.state.channels.insert(channel.id, channel);
            }
            Event::ChannelRemoved(channel) => {
                self.state.channels.remove(&channel.id);
            }
            Event::UserUpdated(change) => {
                self.user_seen(&change.user, now);
                self.state.users.insert(change.user.session, change.user);
            }
            Event::UserRemoved { user, .. } => {
                self.state.users.remove(&user.session);
                self.user_left(user.session);
            }
            Event::PluginData(data) if data.data_id == DATA_ID => {
                if let Some(sender) = data.sender {
                    self.on_packet(sender, &data.data, now);
                }
            }
            // Sessions are only valid per connection; the next login describes everyone again.
            Event::Disconnected(_) => {
                self.state = ServerState::default();
                self.peers.clear();
                self.serves.clear();
                self.outbox = Outbox::default();
            }
            _ => {}
        }
    }

    fn user_seen(&mut self, user: &User, now: i64) {
        let peer = self.peers.entry(user.session).or_insert_with(|| Peer {
            live: HashMap::new(),
            budget: Bucket::new(PEER_RATE, PEER_BURST, now),
        });
        let channels: HashSet<u32> = std::iter::once(user.channel_id)
            .chain(user.listening.iter().copied())
            .collect();
        peer.live.retain(|c, _| channels.contains(c));
        for channel in channels {
            peer.live.entry(channel).or_insert(now);
        }
    }

    fn user_left(&mut self, session: u32) {
        self.peers.remove(&session);
        self.serves.retain(|s| s.requester != session);
        self.outbox.forget(session);
    }

    /// Stores a text message for every recorded channel it reaches (FR-4.5, FR-4.6). `state`
    /// is the connection's current state (the own user is only known there). Returns how many
    /// records were written.
    pub fn record(&mut self, message: &TextMessage, state: &ServerState, now: i64) -> usize {
        let records = records_for(&self.config, message, state, now);
        let mut written = 0;
        for record in &records {
            let stored = self.store.insert(record).and_then(|()| {
                self.store
                    .trim_channel(&record.channel, self.config.max_messages_per_channel)
            });
            match stored {
                Ok(_) => written += 1,
                Err(error) => tracing::warn!(%error, "history: storing a message failed"),
            }
        }
        written
    }

    /// Applies the retention (FR-4.7).
    pub fn prune(&mut self, now: i64) {
        let before = now - i64::from(self.config.retention_days) * 24 * 60 * 60 * 1000;
        match self
            .store
            .prune(before, self.config.max_messages_per_channel)
        {
            Ok(0) => {}
            Ok(deleted) => tracing::info!(deleted, "history: retention removed old messages"),
            Err(error) => tracing::warn!(%error, "history: applying the retention failed"),
        }
    }

    /// Drops answers whose `get` never came.
    pub fn tick(&mut self, now: i64) {
        self.serves
            .retain(|s| now.saturating_sub(s.created) < SERVE_TTL_MS);
    }

    /// Packets due now, paced below the server's limit, as (receivers, data).
    pub fn take_outgoing(&mut self, now: i64) -> Vec<(Vec<u32>, Vec<u8>)> {
        let mut out = Vec::new();
        while !self.outbox.queues.is_empty() && self.pacing.take(now) {
            out.extend(self.outbox.pop());
        }
        out
    }

    fn on_packet(&mut self, sender: u32, data: &[u8], now: i64) {
        if Some(sender) == self.state.session || !self.peers.contains_key(&sender) {
            return;
        }
        let packet = match wire::decode(data) {
            Ok(packet) => packet,
            Err(error) => {
                tracing::debug!(%error, "history: ignoring a history sync packet");
                return;
            }
        };
        match packet {
            Packet::AvailabilityRequest {
                req,
                channel,
                from,
                to,
            } => self.answer_summary(sender, req, channel, from, to, now),
            Packet::HashRequest {
                req,
                channel,
                from,
                to,
            } => self.answer_hashes(sender, req, channel, from, to, now),
            Packet::Fetch { req, wanted } => self.answer_fetch(sender, req, &wanted, now),
            // The bot never asks, so answers to requests are not for it.
            Packet::Availability { .. }
            | Packet::Hashes { .. }
            | Packet::Part { .. }
            | Packet::Deny { .. } => {}
        }
    }

    /// The channel key and clamped range for answering `sender`, or `None` if not allowed.
    fn answer_range(
        &self,
        sender: u32,
        channel: u32,
        from: i64,
        to: i64,
        now: i64,
    ) -> Option<(String, i64, i64)> {
        // Only people who receive the channel live, and only up to when they started to.
        let since = *self.peers.get(&sender)?.live.get(&channel)?;
        let to = to.min(since).min(now);
        let from = from.max(now - MAX_AGE_MS);
        if from >= to {
            return None;
        }
        Some((channel_key(&self.state, channel)?, from, to))
    }

    fn spend(&mut self, sender: u32, now: i64) -> bool {
        let allowed = self
            .peers
            .get_mut(&sender)
            .is_some_and(|p| p.budget.take(now));
        if !allowed {
            tracing::debug!("history: request budget of a person exhausted");
        }
        allowed
    }

    fn answer_summary(
        &mut self,
        sender: u32,
        req: u32,
        channel: u32,
        from: i64,
        to: i64,
        now: i64,
    ) {
        if !self.spend(sender, now) {
            return;
        }
        // Asked everyone at once: stay silent rather than deny.
        let Some((key, from, to)) = self.answer_range(sender, channel, from, to, now) else {
            return;
        };
        let summary = match self.store.range_summary(&key, from, to) {
            Ok(summary) => summary,
            Err(error) => {
                tracing::warn!(%error, "history: reading the summary failed");
                return;
            }
        };
        let (Some(oldest), Some(newest)) = (summary.oldest, summary.newest) else {
            return;
        };
        self.outbox.push(
            vec![sender],
            &Packet::Availability {
                req,
                channel,
                count: u32::try_from(summary.count).unwrap_or(u32::MAX),
                oldest,
                newest,
            },
        );
    }

    fn answer_hashes(&mut self, sender: u32, req: u32, channel: u32, from: i64, to: i64, now: i64) {
        if !self.spend(sender, now) {
            return;
        }
        self.serves.retain(|s| s.requester != sender);
        let range = self.answer_range(sender, channel, from, to, now);
        let Some((key, from, to)) = range.filter(|_| self.serves.len() < MAX_SERVES) else {
            self.outbox.push(vec![sender], &Packet::Deny { req });
            return;
        };
        let limit = u32::try_from(MAX_MESSAGES).unwrap_or(u32::MAX);
        let messages: Vec<Record> = match self.store.messages_between(&key, from, to, limit) {
            Ok(rows) => rows
                .into_iter()
                .filter(|m| m.body.len() <= MAX_BODY)
                .collect(),
            Err(error) => {
                tracing::warn!(%error, "history: reading messages failed");
                self.outbox.push(vec![sender], &Packet::Deny { req });
                return;
            }
        };
        let hashes: Vec<Hash> = messages.iter().map(|m| content_hash(&content(m))).collect();
        tracing::debug!(count = hashes.len(), "history: sending a hash list");
        for packet in wire::chunk_hashes(req, &hashes) {
            self.outbox.push(vec![sender], &packet);
        }
        self.serves.push(Serve {
            requester: sender,
            req,
            created: now,
            messages,
        });
    }

    fn answer_fetch(&mut self, sender: u32, req: u32, wanted: &[u8], now: i64) {
        if !self.spend(sender, now) {
            return;
        }
        let Some(at) = self
            .serves
            .iter()
            .position(|s| s.requester == sender && s.req == req)
        else {
            return;
        };
        let serve = self.serves.swap_remove(at);
        let mut size = 0;
        let mut messages = Vec::new();
        for k in wire::selected(wanted, serve.messages.len()) {
            let m = &serve.messages[k];
            let message = WireMessage {
                ts: m.sent_at,
                n: m.sender_name.clone(),
                h: m.sender_cert.clone(),
                b: m.body.clone(),
            };
            size += wire::encode_messages(std::slice::from_ref(&message)).len();
            if size > MAX_STREAM {
                break;
            }
            messages.push(message);
        }
        tracing::debug!(count = messages.len(), "history: sending messages");
        for packet in wire::chunk_stream(req, &wire::encode_messages(&messages)) {
            self.outbox.push(vec![sender], &packet);
        }
    }
}

fn content(m: &Record) -> Content<'_> {
    Content {
        channel: &m.channel,
        sender_name: &m.sender_name,
        sender_cert_hash: m.sender_cert.as_deref(),
        sent_at: m.sent_at,
        body: &m.body,
    }
}

/// Mumcord's key of a channel: its path below the root joined with `/` (root: empty).
pub fn channel_key(state: &ServerState, id: u32) -> Option<String> {
    let mut path = Vec::new();
    let mut current = state.channels.get(&id)?;
    while let Some(parent) = current.parent {
        path.push(current.name.as_str());
        current = state.channels.get(&parent)?;
    }
    path.reverse();
    Some(path.join("/"))
}

/// Whether `channel` is `ancestor` or below it.
fn is_within(state: &ServerState, channel: u32, ancestor: u32) -> bool {
    let mut current = Some(channel);
    // Bounded by the channel count, so a malformed tree with a loop cannot hang.
    for _ in 0..=state.channels.len() {
        match current {
            Some(id) if id == ancestor => return true,
            Some(id) => current = state.channels.get(&id).and_then(|c| c.parent),
            None => return false,
        }
    }
    false
}

/// The configured channels that exist in `state` (allow list minus deny list, FR-4.4).
/// Entries that do not resolve are left out; [`unresolved`] names them.
pub fn recorded_channels(config: &HistoryConfig, state: &ServerState) -> Vec<u32> {
    let resolve = |entry: &ChannelRef| match entry {
        ChannelRef::Id(id) => state.channels.contains_key(id).then_some(*id),
        ChannelRef::Name(name) => resolve_channel(state, name).ok(),
    };
    let excluded: HashSet<u32> = config.exclude.iter().filter_map(resolve).collect();
    let mut ids: Vec<u32> = if config.records_all() {
        state.channels.keys().copied().collect()
    } else {
        config.channels.iter().filter_map(resolve).collect()
    };
    ids.retain(|id| !excluded.contains(id));
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Config entries that name no channel in `state`.
pub fn unresolved<'a>(config: &'a HistoryConfig, state: &ServerState) -> Vec<&'a ChannelRef> {
    config
        .channels
        .iter()
        .chain(&config.exclude)
        .filter(|entry| match entry {
            ChannelRef::Id(id) => !state.channels.contains_key(id),
            ChannelRef::Name(name) => {
                name != super::config::ALL_CHANNELS && resolve_channel(state, name).is_err()
            }
        })
        .collect()
}

/// The records a text message makes (FR-4.5, FR-4.6): one per recorded channel it reaches
/// that the bot receives live. Private messages and server messages make none.
pub fn records_for(
    config: &HistoryConfig,
    message: &TextMessage,
    state: &ServerState,
    now: i64,
) -> Vec<Record> {
    let target = &message.target;
    if target.channels.is_empty() && target.trees.is_empty() {
        return Vec::new();
    }
    let (Some(actor), Some(own)) = (message.actor, state.own_user()) else {
        return Vec::new();
    };
    let Some(sender) = state.users.get(&actor) else {
        return Vec::new();
    };
    if message.message.len() > MAX_BODY {
        return Vec::new();
    }
    let live: HashSet<u32> = std::iter::once(own.channel_id)
        .chain(own.listening.iter().copied())
        .collect();
    recorded_channels(config, state)
        .into_iter()
        .filter(|c| live.contains(c))
        .filter(|&c| {
            target.channels.contains(&c) || target.trees.iter().any(|&t| is_within(state, c, t))
        })
        .filter_map(|c| channel_key(state, c))
        .map(|channel| Record {
            channel,
            sender_name: sender.name.clone(),
            sender_cert: sender.cert_hash.clone(),
            sent_at: now,
            body: message.message.clone(),
        })
        .collect()
}
