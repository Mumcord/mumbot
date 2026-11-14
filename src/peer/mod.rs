//! Peer bot: connects the Mumble server to a mumpeer relay, so two Mumcord clients that cannot
//! reach each other directly can meet on the relay. Spec: Mumcord `specs/04-mumpeer-relay-
//! specification.md` (FR-4.22 to FR-4.28, sections 6.4 and 6.6).
//!
//! Discovery runs in two stages over plugin data (see [`wire`]); nothing is added to or required
//! by the Mumble server, and the bot never relays anything itself:
//! 1. A Mumcord client whose hello lists `p2p` gets the relay announcement (address, ports,
//!    certificate fingerprint, state), to that client only. On every change (relay restart,
//!    address, ports, fingerprint, reachable or not) all of them get it again in one message.
//! 2. A client that asks for a token for one peer gets one from the relay's API, sent to both.
//! 3. A screen sharer asks for a broadcast (one upload, copied by the relay to every viewer) and
//!    admits viewers one by one; the bot checks each viewer and hands it a subscriber token
//!    (Mumcord spec 07, FR-7.40 to FR-7.50, section 6.6; see [`broadcast`]). Plugin data uses the
//!    same broadcasts with kind `plg` (Mumcord spec 09, FR-9.88): at most
//!    [`PLUGIN_DATA_SENDERS_PER_CHANNEL`] senders per channel, and the bot never sees which plugin
//!    the data belongs to. Cameras use kind `cam` (Mumcord spec 08, FR-8.85 to FR-8.90): a
//!    session runs one broadcast per kind, so a screen share and a camera can run together; at
//!    most `max_cameras_per_channel` cameras per channel (`room-full`), and a camera ends when
//!    its sender leaves the channel it started in.
//!
//! Only registered users get tokens, or the members of the configured `allowed_groups` when they
//! are set (issue #31). When the last session of a certificate leaves or changes certificate, the
//! relay drops that hash. Runs as a module (`mumbot peer --config bot.toml`, or `mumbot run` with
//! other modules) with a `[peer]` section.

pub mod broadcast;
pub mod config;
pub mod groups;
pub mod relay;
pub mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::time::Duration;

use anyhow::{Result, bail};
use mumcord_protocol::Event;
use tokio::sync::broadcast as events;
use tokio::time::Instant;

pub use config::{PeerConfig, Secret};
pub use relay::{
    BroadcastError, BroadcastStatus, HttpRelay, NewBroadcast, Pair, PairError, PluginDataStatus,
    RelayApi, Status, ViewerToken,
};

use broadcast::{Broadcasts, Cleanup};
use wire::{
    BroadcastErrorReason as BcastError, BroadcastId, BroadcastKind, MEDIA_VERSION, PeerMessage,
    RelayAnnouncement, RelayState as State, RelayToken, RelayUse, RequestId,
    TokenErrorReason as TokenError,
};

/// A `token-req`: the sender wants a relay token for itself and session `peer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TokenRequest {
    rid: RequestId,
    peer: u32,
    /// What the pair is for; a request without it comes from an older client that only shared
    /// screens through the relay.
    relay_use: RelayUse,
}

/// Why a session may not use the relay, in the words `token-err` and `bcast-err` share. A
/// refusal names what the person can change; whatever the bot cannot name stays `Denied`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// No Mumcord hello of this client, or none that lists what the request needs; a client that
    /// reconnects sends its hello again (FR-4.24).
    NoHello,
    /// A guest, while only registered users get tokens.
    NotRegistered,
    /// A registered user who is in none of the `allowed_groups`.
    GroupNotAllowed,
    /// Not on the server, or no certificate.
    Denied,
}

impl Refusal {
    fn token(self) -> TokenError {
        match self {
            Self::NoHello => TokenError::NoHello,
            Self::NotRegistered => TokenError::NotRegistered,
            Self::GroupNotAllowed => TokenError::GroupNotAllowed,
            Self::Denied => TokenError::Denied,
        }
    }

    fn broadcast(self) -> BcastError {
        match self {
            Self::NoHello => BcastError::NoHello,
            Self::NotRegistered => BcastError::NotRegistered,
            Self::GroupNotAllowed => BcastError::GroupNotAllowed,
            Self::Denied => BcastError::Denied,
        }
    }
}

/// How often the relay's status is polled (FR-4.25).
pub const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Failed polls in a row before the relay counts as down (FR-4.25).
const FAILURES_UNTIL_DOWN: u32 = 2;
/// Token requests per requesting session and minute (FR-4.26).
pub const REQUESTS_PER_MINUTE: usize = 6;
/// `bcast-req` and `bcast-admit` per session and minute: a full broadcast of 25 viewers that
/// all join at once must fit, with room for viewers who press Watch again.
pub const BROADCAST_REQUESTS_PER_MINUTE: usize = 60;
/// Most sessions in one channel that send plugin data broadcasts at the same time (FR-9.88).
pub const PLUGIN_DATA_SENDERS_PER_CHANNEL: usize = 8;
/// Camera admits a session may send at once: someone who turns the camera on admits everyone in
/// the channel together (Mumcord spec 08, FR-8.89).
pub const CAMERA_ADMIT_BURST: u32 = 24;
/// Camera admits per session and minute once the burst is used up (FR-8.89).
pub const CAMERA_ADMITS_PER_MINUTE: u32 = 60;

/// What the module sees of the Mumble connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Users {
    /// The bot's own session.
    pub own: u32,
    /// Everyone online with their certificate hash (SHA-1 hex, `None` without a certificate).
    pub certs: HashMap<u32, Option<String>>,
    /// Registration ids of the registered users online; guests are missing.
    pub user_ids: HashMap<u32, u32>,
    /// The channel each session is in.
    pub channels: HashMap<u32, u32>,
}

/// The narrow view of the bot the module needs: who is online, sending plugin data, and reading
/// the server's ACL groups.
pub trait Link {
    /// Whether the bot's own user is registered; `None` while the bot is not connected.
    fn registered(&self) -> impl Future<Output = Option<bool>> + Send;
    /// `None` while the bot is not connected.
    fn users(&self) -> impl Future<Output = Option<Users>> + Send;
    fn send(&self, receivers: &[u32], data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;
    /// The groups of `channel`, from the server's answer to an ACL query. Standard servers answer
    /// only for users with the Write permission there, which `allowed_groups` needs.
    fn acl(
        &self,
        channel: &str,
    ) -> impl Future<Output = Result<groups::Members, groups::QueryError>> + Send;
}

/// Who may get tokens and how many, from `[peer]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// Only registered users get tokens; guests too when off.
    pub registered_only: bool,
    pub cert_requests_per_minute: usize,
    pub max_pairs_per_cert: usize,
    /// Members of these groups get tokens instead of every registered user (issue #31).
    pub allowed_groups: Vec<String>,
    /// Channel whose ACL defines them.
    pub group_channel: String,
    /// Camera broadcasts per channel before `room-full` (FR-8.87).
    pub max_cameras_per_channel: usize,
}

impl From<&PeerConfig> for Rules {
    fn from(config: &PeerConfig) -> Self {
        Self {
            registered_only: config.registered_only,
            cert_requests_per_minute: config.cert_requests_per_minute,
            max_pairs_per_cert: config.max_pairs_per_cert,
            allowed_groups: config
                .allowed_groups
                .iter()
                .map(|name| name.trim().to_owned())
                .collect(),
            group_channel: config.group_channel.clone(),
            max_cameras_per_channel: config.max_cameras_per_channel,
        }
    }
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            registered_only: true,
            cert_requests_per_minute: config::CERT_REQUESTS_PER_MINUTE,
            max_pairs_per_cert: config::MAX_PAIRS_PER_CERT,
            allowed_groups: Vec::new(),
            group_channel: String::from(config::DEFAULT_GROUP_CHANNEL),
            max_cameras_per_channel: config::MAX_CAMERAS_PER_CHANNEL,
        }
    }
}

/// Sliding one-minute window of token requests per key (session or certificate hash).
struct RequestLimiter<K> {
    limit: usize,
    seen: HashMap<K, VecDeque<Instant>>,
}

impl<K: Eq + Hash> RequestLimiter<K> {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            seen: HashMap::new(),
        }
    }

    fn allow(&mut self, key: K, now: Instant) -> bool {
        let window = Duration::from_secs(60);
        // Forget idle requesters so the map cannot grow with every session ever seen.
        self.seen.retain(|_, times| {
            while times.front().is_some_and(|t| now - *t >= window) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = self.seen.entry(key).or_default();
        if times.len() >= self.limit {
            return false;
        }
        times.push_back(now);
        true
    }
}

/// Token bucket per key that allows `burst` calls at once and refills at `per_minute`. Kept as
/// the time the bucket is full again (GCRA), so there is no timer and no float.
struct BurstLimiter<K> {
    interval: Duration,
    tolerance: Duration,
    full_at: HashMap<K, Instant>,
}

impl<K: Eq + Hash> BurstLimiter<K> {
    fn new(burst: u32, per_minute: u32) -> Self {
        let interval = Duration::from_secs(60) / per_minute;
        Self {
            interval,
            tolerance: interval * burst.saturating_sub(1),
            full_at: HashMap::new(),
        }
    }

    fn allow(&mut self, key: K, now: Instant) -> bool {
        // Forget full buckets so the map cannot grow with every session ever seen.
        self.full_at.retain(|_, full| *full > now);
        let full = self.full_at.get(&key).copied().unwrap_or(now).max(now);
        if full - now > self.tolerance {
            return false;
        }
        self.full_at.insert(key, full + self.interval);
        true
    }
}

/// State of the module between events.
pub struct Peer<R, L> {
    relay: R,
    link: L,
    /// Last good status; `None` until the relay answered once.
    status: Option<Status>,
    state: State,
    failures: u32,
    seq: u32,
    /// Sessions whose hello listed `p2p`, with their certificate hash.
    clients: HashMap<u32, Option<String>>,
    limiter: RequestLimiter<u32>,
    /// Per certificate, so a reconnect or a second session does not get a fresh budget.
    cert_limiter: RequestLimiter<String>,
    rules: Rules,
    /// Pairs handed out, as sorted hash pairs, until one of the two certificates is dropped.
    pairs: HashSet<(String, String)>,
    /// Sessions of `clients` whose hello also listed `screen-share`.
    screen: HashSet<u32>,
    /// Sessions of `clients` whose hello also listed `plugins`.
    plugins: HashSet<u32>,
    /// Sessions of `clients` whose hello also listed `camera`.
    camera: HashSet<u32>,
    broadcasts: Broadcasts,
    bcast_limiter: RequestLimiter<u32>,
    /// Camera admits, apart from `bcast_limiter` (FR-8.89).
    camera_admit_limiter: BurstLimiter<u32>,
    /// Members of `rules.allowed_groups`, as the server listed them for the last query.
    groups: groups::Members,
    /// When the groups were last read, because the server never reports a membership change.
    read_at: Option<Instant>,
}

impl<R: RelayApi, L: Link> Peer<R, L> {
    pub fn new(relay: R, link: L, rules: Rules) -> Self {
        Self {
            relay,
            link,
            status: None,
            state: State::Down,
            failures: 0,
            seq: 0,
            clients: HashMap::new(),
            limiter: RequestLimiter::new(REQUESTS_PER_MINUTE),
            cert_limiter: RequestLimiter::new(rules.cert_requests_per_minute),
            rules,
            pairs: HashSet::new(),
            screen: HashSet::new(),
            plugins: HashSet::new(),
            camera: HashSet::new(),
            broadcasts: Broadcasts::default(),
            bcast_limiter: RequestLimiter::new(BROADCAST_REQUESTS_PER_MINUTE),
            camera_admit_limiter: BurstLimiter::new(CAMERA_ADMIT_BURST, CAMERA_ADMITS_PER_MINUTE),
            groups: groups::Members::new(),
            read_at: None,
        }
    }

    /// Sessions known as Mumcord P2P clients (tests).
    pub fn clients(&self) -> Vec<u32> {
        let mut sessions: Vec<u32> = self.clients.keys().copied().collect();
        sessions.sort_unstable();
        sessions
    }

    /// Reacts to one event of the Mumble connection.
    pub async fn on_event(&mut self, event: Event, now: Instant) {
        match event {
            Event::PluginData(data) => {
                let Some(sender) = data.sender else { return };
                if data.data_id == wire::HELLO_ID && wire::is_p2p_hello(&data.data) {
                    let screen = wire::hello_lists(&data.data, wire::SCREEN_SHARE_FEATURE);
                    let plugins = wire::hello_lists(&data.data, wire::PLUGINS_FEATURE);
                    let camera = wire::hello_lists(&data.data, wire::CAMERA_FEATURE);
                    self.on_hello(sender, screen, plugins, camera).await;
                } else if data.data_id == wire::DATA_ID
                    && let Ok(message) = PeerMessage::decode(&data.data)
                {
                    self.on_message(sender, message, now).await;
                }
            }
            Event::UserUpdated(change) => {
                let moved = change.moved_from.is_some();
                let user = change.user;
                if let Some(known) = self.clients.get_mut(&user.session)
                    && *known != user.cert_hash
                {
                    let old = std::mem::replace(known, user.cert_hash);
                    // The relay knows broadcast members by certificate, so a new certificate is
                    // a new person to it: the session's broadcasts end and it stops watching.
                    let cleanup = self.broadcasts.left(user.session);
                    self.clean_up(cleanup).await;
                    if let Some(hash) = old {
                        self.release(&hash).await;
                    }
                }
                if moved && self.broadcasts.involves(user.session) {
                    self.check_channels().await;
                }
            }
            Event::UserRemoved { user, .. } => {
                self.screen.remove(&user.session);
                self.plugins.remove(&user.session);
                self.camera.remove(&user.session);
                let cleanup = self.broadcasts.left(user.session);
                self.clean_up(cleanup).await;
                if let Some(Some(hash)) = self.clients.remove(&user.session) {
                    self.release(&hash).await;
                }
            }
            // Sessions are only valid per connection; after a reconnect the clients greet the
            // bot again because it is a new joiner for them (FR-4.24). Broadcasts nobody
            // publishes to any more end at the relay's idle timeout.
            Event::Disconnected(_) => {
                self.clients.clear();
                self.pairs.clear();
                self.screen.clear();
                self.plugins.clear();
                self.camera.clear();
                self.broadcasts.clear();
                // Group membership belongs to the connection too: the bot reads it again.
                self.groups.clear();
                self.read_at = None;
            }
            _ => {}
        }
    }

    /// Checks at start that the bot may read the group channel and that every configured group
    /// exists there. The bot must refuse to run rather than fail open (issue #31).
    pub async fn check_access(&mut self) -> Result<()> {
        if self.rules.allowed_groups.is_empty() {
            return Ok(());
        }
        self.read_groups(Instant::now(), true).await?;
        for name in &self.rules.allowed_groups {
            if !self.groups.contains_key(name) {
                bail!(
                    "peer: group {name:?} is not defined in channel {:?}; the bot cannot check its \
                     members",
                    self.rules.group_channel
                );
            }
        }
        tracing::info!(
            channel = %self.rules.group_channel,
            groups = ?self.rules.allowed_groups,
            "peer: relay tokens restricted to the members of these groups"
        );
        Ok(())
    }

    /// Reads the configured groups, unless the last answer is still fresh (issue #31).
    async fn read_groups(&mut self, now: Instant, force: bool) -> Result<(), groups::QueryError> {
        if self.rules.allowed_groups.is_empty() {
            return Ok(());
        }
        if !force
            && self
                .read_at
                .is_some_and(|seen| now - seen < groups::MIN_READ_INTERVAL)
        {
            return Ok(());
        }
        match self.link.acl(&self.rules.group_channel).await {
            Ok(read) => self.groups = read,
            Err(error) => {
                // A failed read must not keep the members of the previous one, because that would
                // let people use the relay after the server took their access or the bot's
                // permission away.
                self.groups.clear();
                tracing::warn!(%error, "peer: group members not read, tokens refused");
                return Err(error);
            }
        }
        self.read_at = Some(now);
        Ok(())
    }

    /// Reads the groups again and ends at the relay everything a lost member had there — its open
    /// pair and its broadcast memberships — because the server never reports a change (issue #31).
    pub async fn check_groups(&mut self) {
        if self.rules.allowed_groups.is_empty() {
            return;
        }
        let Some(users) = self.link.users().await else {
            return;
        };
        if self.read_groups(Instant::now(), true).await.is_err() {
            return;
        }
        let known: Vec<(u32, String)> = self
            .clients
            .iter()
            .filter_map(|(session, hash)| Some((*session, hash.clone()?)))
            .collect();
        for (session, hash) in known {
            if self.refusal(&users, session).is_none() {
                continue;
            }
            let pair = self.pairs.iter().any(|(a, b)| a == &hash || b == &hash);
            let broadcast = self.broadcasts.left(session);
            if !pair && broadcast.is_empty() {
                continue;
            }
            tracing::info!("peer: a user lost group access, its relay sessions ended");
            if pair {
                self.pairs.retain(|(a, b)| a != &hash && b != &hash);
                self.remove_peer(&hash).await;
            }
            self.clean_up(broadcast).await;
        }
    }

    /// Polls the relay's status and announces any change to every known client (FR-4.24,
    /// FR-4.25).
    pub async fn check_status(&mut self) {
        match self.relay.status().await {
            Ok(status) => {
                self.failures = 0;
                if self.state == State::Up && self.status.as_ref() == Some(&status) {
                    return;
                }
                if self.status.is_none() {
                    tracing::info!("peer: relay reachable");
                } else {
                    tracing::info!("peer: relay changed or back, announcing it again");
                }
                // A restarted relay has forgotten every broadcast.
                if self
                    .status
                    .as_ref()
                    .is_some_and(|old| old.boot_id != status.boot_id)
                {
                    self.broadcasts.clear();
                }
                self.status = Some(status);
                self.state = State::Up;
                self.announce_change().await;
            }
            Err(error) => {
                self.failures += 1;
                tracing::warn!(%error, failures = self.failures, "peer: relay status failed");
                if self.failures >= FAILURES_UNTIL_DOWN && self.state == State::Up {
                    tracing::warn!("peer: relay down, announcing it");
                    self.state = State::Down;
                    self.announce_change().await;
                }
            }
        }
    }

    async fn announce_change(&mut self) {
        self.seq += 1;
        let receivers = self.clients();
        if !receivers.is_empty() {
            self.announce(&receivers).await;
        }
    }

    /// The announcement as of now; `None` while the relay never answered.
    fn announcement(&self) -> Option<PeerMessage> {
        let status = self.status.as_ref()?;
        Some(PeerMessage::Relay(RelayAnnouncement {
            host: status.host.clone(),
            udp: status.udp,
            tcp: status.tcp,
            fingerprint: fingerprint(&status.fp)?,
            state: self.state,
            seq: self.seq,
            uses: status.announced_uses(),
            broadcast: status.broadcast.map(|b| b.max_viewers),
            plugin_data_kbps: status.plugin_data_kbps(),
            // A guest gets no token when groups are configured either: group members are
            // registered users.
            registered_only: self.rules.registered_only || !self.rules.allowed_groups.is_empty(),
            media: status.media >= MEDIA_VERSION,
        }))
    }

    async fn announce(&self, receivers: &[u32]) {
        if let Some(relay) = self.announcement() {
            self.send(receivers, &relay).await;
        }
    }

    async fn on_hello(&mut self, sender: u32, screen: bool, plugins: bool, camera: bool) {
        // The feature list may change with a later hello (screen sharing, plugins or the camera
        // turned on or off).
        for (set, listed) in [
            (&mut self.screen, screen),
            (&mut self.plugins, plugins),
            (&mut self.camera, camera),
        ] {
            if listed {
                set.insert(sender);
            } else {
                set.remove(&sender);
            }
        }
        // Only the first hello per session is answered, so a client repeating its hello cannot
        // make the bot run into the server's plugin data limit.
        if self.clients.contains_key(&sender) {
            return;
        }
        let Some(users) = self.link.users().await else {
            return;
        };
        let Some(cert) = users.certs.get(&sender) else {
            return;
        };
        self.clients.insert(sender, cert.clone());
        self.announce(&[sender]).await;
    }

    async fn send(&self, receivers: &[u32], message: &PeerMessage) {
        let data = match message.encode() {
            Ok(data) => data,
            Err(error) => {
                tracing::warn!(%error, "peer: message not encoded");
                return;
            }
        };
        if let Err(error) = self.link.send(receivers, data).await {
            tracing::warn!(%error, "peer: plugin data not sent");
        }
    }

    async fn on_message(&mut self, sender: u32, message: PeerMessage, now: Instant) {
        match message {
            PeerMessage::TokenRequest {
                rid,
                peer,
                relay_use,
            } => {
                let relay_use = relay_use.unwrap_or(RelayUse::ScreenShare);
                let request = TokenRequest {
                    rid,
                    peer,
                    relay_use,
                };
                self.on_request(sender, request, now).await;
            }
            PeerMessage::BroadcastRequest {
                rid,
                max,
                kind,
                media,
            } => {
                // A request without a kind comes from a client older than plugin data.
                let kind = kind.unwrap_or(RelayUse::ScreenShare);
                let result = self
                    .create_broadcast(sender, rid, max, kind, media, now)
                    .await;
                self.answer(sender, rid, result).await;
            }
            PeerMessage::BroadcastAdmit { rid, id, viewer } => {
                let result = self.admit(sender, id, viewer, now).await;
                self.answer(sender, rid, result).await;
            }
            PeerMessage::BroadcastKick { id, viewer } => self.kick(sender, id, viewer).await,
            // Messages for clients, sent by another client: nothing for the bot to do.
            _ => {}
        }
    }

    /// Sends `bcast` or a token to the right receiver, or `bcast-err` to the requester.
    async fn answer(
        &self,
        sender: u32,
        rid: RequestId,
        result: Result<(u32, PeerMessage), BcastError>,
    ) {
        match result {
            Ok((receiver, message)) => self.send(&[receiver], &message).await,
            Err(reason) => {
                tracing::debug!(?reason, "peer: broadcast request refused");
                let error = PeerMessage::BroadcastError { rid, reason };
                self.send(&[sender], &error).await;
            }
        }
    }

    /// `bcast-req`: a new broadcast of `kind` published by `sender` (FR-7.40, FR-9.88).
    async fn create_broadcast(
        &mut self,
        sender: u32,
        rid: RequestId,
        max: u16,
        kind: RelayUse,
        media: bool,
        now: Instant,
    ) -> Result<(u32, PeerMessage), BcastError> {
        if !self.bcast_limiter.allow(sender, now) {
            return Err(BcastError::RateLimited);
        }
        let users = self.link.users().await.ok_or(BcastError::RelayDown)?;
        let cert = self
            .member_cert(&users, sender, kind)
            .map_err(Refusal::broadcast)?;
        // Plugin data has no reason of its own: nothing is closer than "the relay runs all it
        // allows". A full camera room has `room-full` (FR-8.87).
        let per_channel = match kind {
            RelayUse::PluginData => Some((PLUGIN_DATA_SENDERS_PER_CHANNEL, BcastError::RelayFull)),
            RelayUse::Camera => Some((self.rules.max_cameras_per_channel, BcastError::RoomFull)),
            _ => None,
        };
        let channel = users.channels.get(&sender).copied();
        if let Some((limit, reason)) = per_channel {
            let channel = channel.ok_or(BcastError::NotInChannel)?;
            // The sender's own running broadcast is replaced below, so it does not count.
            let senders = self
                .broadcasts
                .publishers_in(kind, channel, sender, |session| {
                    users.channels.get(&session).copied()
                });
            if senders >= limit {
                return Err(reason);
            }
        }
        if self.state != State::Up {
            return Err(BcastError::RelayDown);
        }
        let created = match self.relay.create_broadcast(&cert, max, kind, media).await {
            Ok(created) => created,
            Err(BroadcastError::RelayFull) => return Err(BcastError::RelayFull),
            Err(BroadcastError::UseNotAllowed) => return Err(BcastError::UseNotAllowed),
            Err(BroadcastError::Full | BroadcastError::Gone) => return Err(BcastError::RelayDown),
            Err(BroadcastError::Unavailable(error)) => {
                tracing::warn!(%error, "peer: no broadcast from the relay");
                return Err(BcastError::RelayDown);
            }
        };
        let Ok(token) = RelayToken::parse(&created.tok) else {
            tracing::warn!("peer: the relay sent a malformed broadcast token");
            self.delete_broadcast(created.id).await;
            return Err(BcastError::RelayDown);
        };
        if let Some(old) = self
            .broadcasts
            .insert(created.id, sender, cert, kind, channel, media)
        {
            self.delete_broadcast(old).await;
        }
        let message = PeerMessage::Broadcast {
            rid,
            id: created.id,
            token,
            ttl: ttl(created.expires_in),
            media,
        };
        Ok((sender, message))
    }

    /// `bcast-admit`: the publisher of `id` lets session `viewer` watch (FR-7.42).
    async fn admit(
        &mut self,
        sender: u32,
        id: BroadcastId,
        viewer: u32,
        now: Instant,
    ) -> Result<(u32, PeerMessage), BcastError> {
        // Camera admits come in bursts and have their own bucket (FR-8.89); everything else,
        // unknown broadcasts too, counts against the shared one.
        let allowed = match self.broadcasts.get(id) {
            Some(broadcast) if broadcast.kind == RelayUse::Camera => {
                self.camera_admit_limiter.allow(sender, now)
            }
            _ => self.bcast_limiter.allow(sender, now),
        };
        if !allowed {
            return Err(BcastError::RateLimited);
        }
        let users = self.link.users().await.ok_or(BcastError::RelayDown)?;
        let (publisher_cert, kind, media) = match self.broadcasts.get(id) {
            Some(broadcast) if broadcast.publisher == sender => (
                broadcast.publisher_cert.clone(),
                broadcast.kind,
                broadcast.media,
            ),
            _ => return Err(BcastError::Denied),
        };
        if self
            .member_cert(&users, sender, kind)
            .map_err(Refusal::broadcast)?
            != publisher_cert
            || viewer == users.own
        {
            return Err(BcastError::Denied);
        }
        let viewer_cert = self
            .member_cert(&users, viewer, kind)
            .map_err(Refusal::broadcast)?;
        let broadcast_kind = broadcast_kind(kind).ok_or(BcastError::Denied)?;
        // Two sessions of one identity are one user; the relay refuses it anyway.
        if viewer_cert == publisher_cert {
            return Err(BcastError::Denied);
        }
        // A sharer whose channel the bot cannot see keeps no room; a viewer without one has
        // left already.
        let Some(sharer_channel) = users.channels.get(&sender) else {
            return Err(BcastError::NotInChannel);
        };
        let Some(viewer_channel) = users.channels.get(&viewer) else {
            return Err(BcastError::Denied);
        };
        if sharer_channel != viewer_channel {
            return Err(BcastError::NotSameChannel);
        }
        if self.state != State::Up {
            return Err(BcastError::RelayDown);
        }
        let granted = match self.relay.admit_viewer(id, &viewer_cert).await {
            Ok(granted) => granted,
            Err(BroadcastError::Full) => return Err(BcastError::Full),
            Err(BroadcastError::Gone) => {
                // Ended at the relay (idle, restart): the sharer asks for a new one.
                tracing::debug!("peer: admission to a broadcast the relay already ended");
                self.broadcasts.remove(id);
                return Err(BcastError::Denied);
            }
            Err(BroadcastError::RelayFull) => return Err(BcastError::RelayFull),
            Err(BroadcastError::UseNotAllowed) => return Err(BcastError::UseNotAllowed),
            Err(BroadcastError::Unavailable(error)) => {
                tracing::warn!(%error, "peer: viewer not admitted by the relay");
                return Err(BcastError::RelayDown);
            }
        };
        let Ok(token) = RelayToken::parse(&granted.tok) else {
            tracing::warn!("peer: the relay sent a malformed viewer token");
            self.remove_viewer(id, &viewer_cert).await;
            return Err(BcastError::RelayDown);
        };
        self.broadcasts.admit(id, viewer, viewer_cert);
        let message = PeerMessage::BroadcastToken {
            id,
            token,
            ttl: ttl(granted.expires_in),
            sharer: sender,
            kind: broadcast_kind,
            media,
        };
        Ok((viewer, message))
    }

    /// `bcast-kick`: only the publisher may remove, and only viewers the bot admitted, so a
    /// kick needs no rate limit of its own.
    async fn kick(&mut self, sender: u32, id: BroadcastId, viewer: u32) {
        if self
            .broadcasts
            .get(id)
            .is_none_or(|b| b.publisher != sender)
        {
            return;
        }
        if let Some(cert) = self.broadcasts.kick(id, viewer) {
            self.remove_viewer(id, &cert).await;
        }
    }

    /// The certificate of a session that may take part in a broadcast of `kind`: online with a
    /// certificate, hello with `p2p` and `screen-share` (screen sharing), `plugins` (plugin
    /// data) or `camera` (cameras), and allowed to use the relay.
    fn member_cert(&self, users: &Users, session: u32, kind: RelayUse) -> Result<String, Refusal> {
        let feature = match kind {
            RelayUse::PluginData => &self.plugins,
            RelayUse::Camera => &self.camera,
            _ => &self.screen,
        };
        let Some(Some(cert)) = users.certs.get(&session) else {
            return Err(Refusal::Denied);
        };
        // Without a hello the bot cannot tell whether the client can do this at all; a client
        // answers a new hello when it comes back (FR-4.24).
        if !self.clients.contains_key(&session) || !feature.contains(&session) {
            return Err(Refusal::NoHello);
        }
        if let Some(refusal) = self.refusal(users, session) {
            return Err(refusal);
        }
        Ok(cert.clone())
    }

    /// Ends cameras whose sender changed channel and removes viewers who are no longer in their
    /// sharer's channel.
    async fn check_channels(&mut self) {
        let Some(users) = self.link.users().await else {
            return;
        };
        let cleanup = self
            .broadcasts
            .outside_channel(|session| users.channels.get(&session).copied());
        self.clean_up(cleanup).await;
    }

    async fn clean_up(&self, cleanup: Vec<Cleanup>) {
        for step in cleanup {
            match step {
                Cleanup::Delete(id) => self.delete_broadcast(id).await,
                Cleanup::RemoveViewer(id, cert) => self.remove_viewer(id, &cert).await,
            }
        }
    }

    async fn delete_broadcast(&self, id: BroadcastId) {
        if let Err(error) = self.relay.delete_broadcast(id).await {
            tracing::warn!(%error, "peer: the relay did not end a broadcast");
        }
    }

    async fn remove_viewer(&self, id: BroadcastId, cert: &str) {
        if let Err(error) = self.relay.remove_viewer(id, cert).await {
            tracing::warn!(%error, "peer: the relay did not remove a viewer");
        }
    }

    async fn on_request(&mut self, sender: u32, request: TokenRequest, now: Instant) {
        let result = match self.check(sender, &request, now).await {
            Ok((a, b)) => {
                let pair = self.pair(&a, &b, request.relay_use).await;
                if pair.is_ok() {
                    self.pairs.insert(pair_key(&a, &b));
                }
                pair
            }
            Err(error) => Err(error),
        };
        match result {
            Ok((token, ttl)) => {
                let token = PeerMessage::Token {
                    rid: request.rid,
                    a: sender,
                    b: request.peer,
                    token,
                    ttl,
                };
                self.send(&[sender, request.peer], &token).await;
            }
            Err(reason) => {
                tracing::debug!(?reason, "peer: token request refused");
                let error = PeerMessage::TokenError {
                    rid: request.rid,
                    reason,
                };
                self.send(&[sender], &error).await;
            }
        }
    }

    /// Checks a request (FR-4.26) and returns the two certificate hashes.
    async fn check(
        &mut self,
        sender: u32,
        request: &TokenRequest,
        now: Instant,
    ) -> Result<(String, String), TokenError> {
        if !self.limiter.allow(sender, now) {
            return Err(TokenError::RateLimited);
        }
        let users = self.link.users().await.ok_or(TokenError::RelayDown)?;
        let Some(Some(own_cert)) = users.certs.get(&sender) else {
            return Err(TokenError::Denied);
        };
        let own_cert = own_cert.clone();
        if !self.clients.contains_key(&sender) {
            return Err(TokenError::NoHello);
        }
        if !self.cert_limiter.allow(own_cert.clone(), now) {
            return Err(TokenError::RateLimited);
        }
        // Groups are read again per request, so a stale answer never grants a token (issue #31).
        if !self.rules.allowed_groups.is_empty() {
            self.read_groups(now, false)
                .await
                .map_err(|_| TokenError::Denied)?;
        }
        if let Some(refusal) = self.refusal(&users, sender) {
            return Err(refusal.token());
        }
        if request.peer == sender || request.peer == users.own {
            return Err(TokenError::Denied);
        }
        let peer_cert = match users.certs.get(&request.peer) {
            None => return Err(TokenError::PeerGone),
            Some(_) if !self.clients.contains_key(&request.peer) => {
                return Err(TokenError::PeerNotMumcord);
            }
            Some(None) => return Err(TokenError::Denied),
            Some(Some(cert)) => cert.clone(),
        };
        // Two sessions of one identity are still one user.
        if peer_cert == own_cert {
            return Err(TokenError::Denied);
        }
        if let Some(refusal) = self.refusal(&users, request.peer) {
            return Err(refusal.token());
        }
        // A new token for an open pair replaces the old one at the relay (FR-4.26).
        if !self.pairs.contains(&pair_key(&own_cert, &peer_cert))
            && self.open_pairs(&own_cert) >= self.rules.max_pairs_per_cert
        {
            return Err(TokenError::RateLimited);
        }
        if self.state != State::Up {
            return Err(TokenError::RelayDown);
        }
        Ok((own_cert, peer_cert))
    }

    /// A token for the pair from the relay, with its validity in seconds.
    async fn pair(
        &self,
        a: &str,
        b: &str,
        relay_use: RelayUse,
    ) -> Result<(RelayToken, u16), TokenError> {
        match self.relay.create_pair(a, b, relay_use).await {
            Ok(pair) => match RelayToken::parse(&pair.tok) {
                Ok(token) => Ok((token, ttl(pair.expires_in))),
                Err(_) => {
                    tracing::warn!("peer: the relay sent a malformed token");
                    Err(TokenError::RelayDown)
                }
            },
            Err(PairError::Full) => Err(TokenError::RelayFull),
            Err(PairError::RateLimited) => Err(TokenError::RateLimited),
            Err(PairError::UseNotAllowed) => Err(TokenError::UseNotAllowed),
            Err(PairError::Unavailable(error)) => {
                tracing::warn!(%error, "peer: no token from the relay");
                Err(TokenError::RelayDown)
            }
        }
    }

    /// Why `session` may not use the relay, or `None` when it may: a member of one of the
    /// configured groups, or registered when no groups are configured.
    fn refusal(&self, users: &Users, session: u32) -> Option<Refusal> {
        if self.rules.allowed_groups.is_empty() {
            return match self.rules.registered_only && !users.user_ids.contains_key(&session) {
                true => Some(Refusal::NotRegistered),
                false => None,
            };
        }
        // Groups need a registered user, so a guest gets the reason that names the first step.
        let Some(id) = users.user_ids.get(&session) else {
            return Some(Refusal::NotRegistered);
        };
        let member = self.rules.allowed_groups.iter().any(|name| {
            self.groups
                .get(name)
                .is_some_and(|members| members.contains(id))
        });
        match member {
            true => None,
            false => Some(Refusal::GroupNotAllowed),
        }
    }

    fn open_pairs(&self, hash: &str) -> usize {
        self.pairs
            .iter()
            .filter(|(a, b)| a == hash || b == hash)
            .count()
    }

    /// A session gave up `hash`: once no other session holds it, the relay drops it with all its
    /// pairs. Two devices of one user share a certificate, and one leaving must not cut off the
    /// other.
    async fn release(&mut self, hash: &str) {
        if self
            .clients
            .values()
            .any(|other| other.as_deref() == Some(hash))
        {
            return;
        }
        self.pairs.retain(|(a, b)| a != hash && b != hash);
        self.remove_peer(hash).await;
    }

    async fn remove_peer(&self, hash: &str) {
        if let Err(error) = self.relay.remove_peer(hash).await {
            tracing::warn!(%error, "peer: the relay did not drop a leaving user");
        }
    }
}

/// A token's validity in seconds as it fits the wire.
/// The kind a viewer needs to hand a token to the right one of a sharer's broadcasts. Only
/// broadcast uses can be created, so images and files have none.
fn broadcast_kind(kind: RelayUse) -> Option<BroadcastKind> {
    match kind {
        RelayUse::ScreenShare => Some(BroadcastKind::ScreenShare),
        RelayUse::PluginData => Some(BroadcastKind::PluginData),
        RelayUse::Camera => Some(BroadcastKind::Camera),
        RelayUse::Images | RelayUse::Files => None,
    }
}

fn ttl(expires_in: u64) -> u16 {
    u16::try_from(expires_in).unwrap_or(u16::MAX)
}

fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

/// 43 base64url characters: 32 random bytes from the relay.
/// The relay certificate fingerprint from the lowercase hex the status check normalized it to.
fn fingerprint(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Fails unless the bot's user is registered: Mumcord clients accept a relay announcement only
/// from a registered user with a certificate, so an unregistered bot announces a relay nobody
/// uses (FR-4.7, FR-4.8).
async fn check_registered<L: Link>(link: &L) -> Result<()> {
    let deadline = Instant::now() + crate::DEFAULT_TIMEOUT;
    loop {
        match link.registered().await {
            Some(true) => return Ok(()),
            Some(false) => bail!(
                "the peer module needs a registered bot user, Mumcord clients ignore relay \
                 announcements of anyone else: register the bot on the server (an admin, or \
                 `mumbot register`) and start it again"
            ),
            None if Instant::now() >= deadline => bail!("peer: the bot did not connect in time"),
            None => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Runs the module until the bot's event stream ends.
pub async fn run<R: RelayApi, L: Link>(
    relay: R,
    link: L,
    rules: Rules,
    mut events: events::Receiver<Event>,
) -> Result<()> {
    check_registered(&link).await?;
    if !rules.registered_only {
        tracing::warn!(
            "peer: registered_only is off, guests get relay tokens too: anyone can join with \
             fresh certificates and use the relay as a bandwidth tunnel between their own hosts"
        );
    }
    let mut peer = Peer::new(relay, link, rules);
    // Without the Write permission on the group channel the bot cannot know its members, so it
    // refuses to run instead of handing tokens to everyone (issue #31).
    peer.check_access().await?;
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut group_check = tokio::time::interval(groups::READ_INTERVAL);
    group_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => peer.on_event(event, Instant::now()).await,
                // A missed hello or request is repeated by the client; a missed leave only
                // keeps a pair until its token expires.
                Err(events::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "peer: fell behind the event stream");
                }
                Err(events::error::RecvError::Closed) => return Ok(()),
            },
            _ = status.tick() => peer.check_status().await,
            _ = group_check.tick() => peer.check_groups().await,
        }
    }
}

#[cfg(test)]
mod tests;
