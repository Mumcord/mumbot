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
//!
//! Only registered users get tokens, or with `allowed_groups` only members of those groups (read
//! from the server's ACL, which needs the "Write ACL" permission). When the last session of a
//! certificate leaves or changes certificate, the relay drops that hash. Runs as a module
//! (`mumbot peer --config bot.toml`, or `mumbot run` with other modules) with a `[peer]` section.

pub mod config;
pub mod groups;
pub mod relay;
pub mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::time::Duration;

use anyhow::{Context, Result};
use mumcord_protocol::Event;
use tokio::sync::broadcast;
use tokio::time::Instant;

pub use config::{PeerConfig, Secret};
pub use relay::{HttpRelay, Pair, PairError, RelayApi, Status};

use wire::{Relay, State, Token, TokenError, TokenRequest};

/// How often the relay's status is polled (FR-4.25).
pub const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Failed polls in a row before the relay counts as down (FR-4.25).
const FAILURES_UNTIL_DOWN: u32 = 2;
/// Token requests per requesting session and minute (FR-4.26).
pub const REQUESTS_PER_MINUTE: usize = 6;
/// How often group membership is read again to drop users who lost access. The server tells a
/// client about ACL changes only when its own permissions change, never about group members.
pub const MEMBERS_INTERVAL: Duration = Duration::from_secs(60);

/// What the module sees of the Mumble connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Users {
    /// The bot's own session.
    pub own: u32,
    /// Everyone online with their certificate hash (SHA-1 hex, `None` without a certificate).
    pub certs: HashMap<u32, Option<String>>,
    /// Registration ids of the registered users online; guests are missing.
    pub user_ids: HashMap<u32, u32>,
}

/// The narrow view of the bot the module needs: who is online, group members, and sending
/// plugin data.
pub trait Link {
    /// `None` while the bot is not connected.
    fn users(&self) -> impl Future<Output = Option<Users>> + Send;
    /// Registration ids of the members of `groups` on the channel `channel` (root when `None`),
    /// read from the server's ACL.
    fn group_members(
        &self,
        channel: Option<&str>,
        groups: &[String],
    ) -> impl Future<Output = Result<HashSet<u32>>> + Send;
    fn send(&self, receivers: &[u32], data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;
}

/// Who may get tokens and how many, from `[peer]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// Empty: every registered user.
    pub allowed_groups: Vec<String>,
    pub groups_channel: Option<String>,
    pub cert_requests_per_minute: usize,
    pub max_pairs_per_cert: usize,
}

impl From<&PeerConfig> for Rules {
    fn from(config: &PeerConfig) -> Self {
        Self {
            allowed_groups: config.allowed_groups.clone(),
            groups_channel: config.groups_channel.clone(),
            cert_requests_per_minute: config.cert_requests_per_minute,
            max_pairs_per_cert: config.max_pairs_per_cert,
        }
    }
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            allowed_groups: Vec::new(),
            groups_channel: None,
            cert_requests_per_minute: config::CERT_REQUESTS_PER_MINUTE,
            max_pairs_per_cert: config::MAX_PAIRS_PER_CERT,
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
    /// Registration ids allowed by `allowed_groups`; `None` while unknown, which allows nobody.
    members: Option<HashSet<u32>>,
    /// Pairs handed out, as sorted hash pairs, until one of the two certificates is dropped.
    pairs: HashSet<(String, String)>,
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
            members: None,
            pairs: HashSet::new(),
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
                    self.on_hello(sender).await;
                } else if data.data_id == wire::DATA_ID
                    && let Some(request) = wire::decode_request(&data.data)
                {
                    self.on_request(sender, request, now).await;
                }
            }
            Event::UserUpdated(change) => {
                let user = change.user;
                if let Some(known) = self.clients.get_mut(&user.session)
                    && *known != user.cert_hash
                {
                    let old = std::mem::replace(known, user.cert_hash);
                    if let Some(hash) = old {
                        self.release(&hash).await;
                    }
                }
            }
            Event::UserRemoved { user, .. } => {
                if let Some(Some(hash)) = self.clients.remove(&user.session) {
                    self.release(&hash).await;
                }
            }
            // Sessions are only valid per connection; after a reconnect the clients greet the
            // bot again because it is a new joiner for them (FR-4.24).
            Event::Disconnected(_) => {
                self.clients.clear();
                self.pairs.clear();
            }
            _ => {}
        }
    }

    /// Reads the members of `allowed_groups` again and drops everyone at the relay who lost
    /// access. Without configured groups there is nothing to read. On an error nobody is
    /// allowed until the next good read.
    pub async fn refresh_members(&mut self) -> Result<()> {
        if self.rules.allowed_groups.is_empty() {
            return Ok(());
        }
        let members = self
            .link
            .group_members(
                self.rules.groups_channel.as_deref(),
                &self.rules.allowed_groups,
            )
            .await;
        self.members = members.as_ref().ok().cloned();
        if let Some(users) = self.link.users().await {
            let lost: HashSet<String> = self
                .clients
                .iter()
                .filter(|(session, _)| !self.allowed(&users, **session))
                .filter_map(|(_, hash)| hash.clone())
                .filter(|hash| self.open_pairs(hash) > 0)
                .collect();
            for hash in lost {
                self.remove_peer(&hash).await;
                self.pairs.retain(|(a, b)| *a != hash && *b != hash);
            }
        }
        let members = members.context("peer: cannot read the allowed groups")?;
        tracing::info!(members = members.len(), "peer: allowed groups read");
        Ok(())
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
    fn announcement(&self) -> Option<Relay> {
        let status = self.status.as_ref()?;
        Some(Relay {
            host: status.host.clone(),
            udp: status.udp,
            tcp: status.tcp,
            fp: status.fp.clone(),
            state: self.state,
            seq: self.seq,
        })
    }

    async fn announce(&self, receivers: &[u32]) {
        if let Some(relay) = self.announcement() {
            self.send(receivers, wire::encode_relay(&relay)).await;
        }
    }

    async fn on_hello(&mut self, sender: u32) {
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

    async fn send(&self, receivers: &[u32], data: Vec<u8>) {
        if let Err(error) = self.link.send(receivers, data).await {
            tracing::warn!(%error, "peer: plugin data not sent");
        }
    }

    async fn on_request(&mut self, sender: u32, request: TokenRequest, now: Instant) {
        let result = match self.check(sender, &request, now).await {
            Ok((a, b)) => {
                let pair = self.pair(&a, &b).await;
                if pair.is_ok() {
                    self.pairs.insert(pair_key(&a, &b));
                }
                pair
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(pair) => {
                let token = Token {
                    rid: request.rid,
                    a: sender,
                    b: request.peer,
                    tok: pair.tok,
                    ttl: u16::try_from(pair.expires_in).unwrap_or(u16::MAX),
                };
                self.send(&[sender, request.peer], wire::encode_token(&token))
                    .await;
            }
            Err(error) => {
                tracing::debug!(?error, "peer: token request refused");
                self.send(&[sender], wire::encode_token_err(&request.rid, error))
                    .await;
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
        let own_cert = match users.certs.get(&sender) {
            Some(Some(cert)) if self.clients.contains_key(&sender) => cert.clone(),
            _ => return Err(TokenError::Denied),
        };
        if !self.cert_limiter.allow(own_cert.clone(), now) {
            return Err(TokenError::RateLimited);
        }
        // Read right before deciding: the server never reports changed group members.
        if let Err(error) = self.refresh_members().await {
            tracing::warn!(%error, "peer: allowing nobody until the groups can be read");
        }
        // Guests and users outside the allowed groups; the client has no finer reason (the
        // reasons are a closed list in Mumcord), so the bot's log names it.
        if !self.allowed(&users, sender) {
            tracing::debug!("peer: token request from a user without relay access");
            return Err(TokenError::Denied);
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
        if !self.allowed(&users, request.peer) {
            tracing::debug!("peer: token request for a user without relay access");
            return Err(TokenError::Denied);
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

    async fn pair(&self, a: &str, b: &str) -> Result<Pair, TokenError> {
        match self.relay.create_pair(a, b).await {
            Ok(pair) if valid_token(&pair.tok) => Ok(pair),
            Ok(_) => {
                tracing::warn!("peer: the relay sent a malformed token");
                Err(TokenError::RelayDown)
            }
            Err(PairError::Full) => Err(TokenError::RelayFull),
            Err(PairError::RateLimited) => Err(TokenError::RateLimited),
            Err(PairError::Unavailable(error)) => {
                tracing::warn!(%error, "peer: no token from the relay");
                Err(TokenError::RelayDown)
            }
        }
    }

    /// Registered, and a member of an allowed group when groups are configured.
    fn allowed(&self, users: &Users, session: u32) -> bool {
        let Some(id) = users.user_ids.get(&session) else {
            return false;
        };
        self.rules.allowed_groups.is_empty()
            || self
                .members
                .as_ref()
                .is_some_and(|members| members.contains(id))
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

fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

/// 43 base64url characters: 32 random bytes from the relay.
fn valid_token(tok: &str) -> bool {
    tok.len() == 43
        && tok
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Runs the module until the bot's event stream ends.
/// Fails right away when `allowed_groups` are configured but cannot be read, so a missing
/// permission never lets everyone in.
pub async fn run<R: RelayApi, L: Link>(
    relay: R,
    link: L,
    rules: Rules,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let mut peer = Peer::new(relay, link, rules);
    peer.refresh_members().await?;
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut members = tokio::time::interval(MEMBERS_INTERVAL);
    members.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    members.reset();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => peer.on_event(event, Instant::now()).await,
                // A missed hello or request is repeated by the client; a missed leave only
                // keeps a pair until its token expires.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "peer: fell behind the event stream");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = status.tick() => peer.check_status().await,
            _ = members.tick() => {
                if let Err(error) = peer.refresh_members().await {
                    tracing::warn!(%error, "peer: allowing nobody until the groups can be read");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
