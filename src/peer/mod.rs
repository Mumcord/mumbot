//! Peer module: connects the Mumble server to a mumpeer relay (Mumcord issue #62), so two
//! Mumcord clients that cannot reach each other directly can meet on the relay.
//!
//! Discovery runs in two stages over plugin data (see [`wire`]); nothing is added to or required
//! by the Mumble server, and the bot never relays voice itself:
//! 1. A Mumcord client that sends its hello gets the relay addresses and the relay certificate
//!    fingerprint, to that client only. When the relay restarts (new instance id), every known
//!    Mumcord client gets the announcement again with a new epoch, in one message.
//! 2. A client that asks for a token for a session with one peer gets a short-lived token valid
//!    only for that pair, registered with the relay first and then sent to both.
//!
//! The certificate hashes of everyone on the server form the relay's allowlist. The module is
//! off unless `bot.toml` has a `[peer]` section.

pub mod config;
pub mod relay;
pub mod wire;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use mumcord_protocol::Event;
use tokio::sync::broadcast;
use tokio::time::Instant;

pub use config::{PeerConfig, Secret};
pub use relay::{HttpRelay, RelayApi, TokenGrant};

use wire::{DenyReason, Token, TokenRequest};

/// Default token lifetime; long enough for both clients to connect, short enough that a leaked
/// token is soon useless.
pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(120);
/// Longest token lifetime the config accepts.
pub const MAX_TOKEN_TTL: Duration = Duration::from_secs(600);
/// Token requests per requester (by certificate) and minute.
pub const REQUESTS_PER_MINUTE: usize = 6;
/// How often the relay's instance id is checked.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Joins and leaves are collected this long before the allowlist is pushed.
const ALLOWLIST_DEBOUNCE: Duration = Duration::from_secs(1);
/// Wait before pushing the allowlist again after the relay refused or did not answer.
const ALLOWLIST_RETRY: Duration = Duration::from_secs(10);

/// What the module sees of the Mumble connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Users {
    /// The bot's own session.
    pub own: u32,
    /// Everyone online with their certificate hash (SHA-1 hex, `None` without a certificate).
    pub certs: HashMap<u32, Option<String>>,
}

/// The narrow view of the bot the module needs: who is online, and sending plugin data.
pub trait Link {
    /// `None` while the bot is not connected.
    fn users(&self) -> impl Future<Output = Option<Users>> + Send;
    fn send(&self, receivers: &[u32], data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;
}

/// Sliding one-minute window of token requests per certificate.
#[derive(Default)]
struct RequestLimiter {
    seen: HashMap<String, VecDeque<Instant>>,
}

impl RequestLimiter {
    fn allow(&mut self, cert: &str, now: Instant) -> bool {
        let window = Duration::from_secs(60);
        // Forget idle requesters so the map cannot grow with every certificate ever seen.
        self.seen.retain(|_, times| {
            while times.front().is_some_and(|t| now - *t >= window) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = self.seen.entry(cert.to_owned()).or_default();
        if times.len() >= REQUESTS_PER_MINUTE {
            return false;
        }
        times.push_back(now);
        true
    }
}

/// State of the module between events.
pub struct Peer<R, L> {
    config: PeerConfig,
    relay: R,
    link: L,
    epoch: String,
    /// Last instance id the relay reported.
    instance: Option<String>,
    /// Sessions that sent a Mumcord hello on this connection.
    clients: BTreeSet<u32>,
    limiter: RequestLimiter,
    /// The allowlist the relay has; `None` forces the next push.
    pushed: Option<Vec<String>>,
    /// When the allowlist should be compared and pushed next.
    push_at: Option<Instant>,
}

impl<R: RelayApi, L: Link> Peer<R, L> {
    pub fn new(config: PeerConfig, relay: R, link: L) -> Result<Self> {
        Ok(Self {
            config,
            relay,
            link,
            epoch: new_epoch()?,
            instance: None,
            clients: BTreeSet::new(),
            limiter: RequestLimiter::default(),
            pushed: None,
            push_at: Some(Instant::now()),
        })
    }

    /// Sessions known as Mumcord clients (tests).
    pub fn clients(&self) -> &BTreeSet<u32> {
        &self.clients
    }

    /// Reacts to one event of the Mumble connection.
    pub async fn on_event(&mut self, event: Event, now: Instant) {
        match event {
            Event::PluginData(data) => {
                let Some(sender) = data.sender else { return };
                if data.data_id == wire::HELLO_ID && wire::is_hello(&data.data) {
                    // Only the first hello per session is answered, so a client repeating its
                    // hello cannot make the bot flood the server's plugin data limit.
                    if self.clients.insert(sender) {
                        self.announce(&[sender]).await;
                    }
                } else if data.data_id == wire::DATA_ID
                    && let Some(request) = wire::decode_request(&data.data)
                {
                    self.on_request(sender, request, now).await;
                }
            }
            Event::UserUpdated(_) => self.schedule_push(now),
            Event::UserRemoved { user, .. } => {
                self.clients.remove(&user.session);
                self.schedule_push(now);
            }
            // Sessions are only valid per connection; after a reconnect the clients greet the
            // bot again because it is a new joiner for them.
            Event::Disconnected(_) => {
                self.clients.clear();
                self.schedule_push(now);
            }
            _ => {}
        }
    }

    /// Pushes the allowlist when it is due and changed.
    pub async fn tick(&mut self, now: Instant) {
        if self.push_at.is_none_or(|at| now < at) {
            return;
        }
        let Some(users) = self.link.users().await else {
            // Keep the relay's list while the bot reconnects; push again once connected.
            self.push_at = Some(now + ALLOWLIST_RETRY);
            return;
        };
        let mut hashes: Vec<String> = users
            .certs
            .iter()
            .filter(|(session, _)| **session != users.own)
            .filter_map(|(_, cert)| cert.clone())
            .collect();
        hashes.sort_unstable();
        hashes.dedup();
        if self.pushed.as_ref() == Some(&hashes) {
            self.push_at = None;
            return;
        }
        match self.relay.put_allowlist(&hashes).await {
            Ok(()) => {
                tracing::info!(users = hashes.len(), "peer: relay allowlist updated");
                self.pushed = Some(hashes);
                self.push_at = None;
            }
            Err(error) => {
                tracing::warn!(%error, "peer: updating the relay allowlist failed");
                self.push_at = Some(now + ALLOWLIST_RETRY);
            }
        }
    }

    /// Checks the relay's instance id; a restarted relay lost its allowlist and tokens, so it
    /// gets the list again and every Mumcord client a fresh announcement.
    pub async fn check_status(&mut self, now: Instant) {
        let instance = match self.relay.status().await {
            Ok(instance) => instance,
            Err(error) => {
                tracing::warn!(%error, "peer: relay API unreachable");
                return;
            }
        };
        let previous = self.instance.replace(instance.clone());
        if previous.is_none() {
            tracing::info!("peer: relay API reachable");
        }
        if previous.is_some_and(|previous| previous != instance) {
            tracing::info!("peer: relay restarted, announcing it again");
            match new_epoch() {
                Ok(epoch) => self.epoch = epoch,
                Err(error) => tracing::warn!(%error, "peer: keeping the old epoch"),
            }
            self.pushed = None;
            self.push_at = Some(now);
            let clients: Vec<u32> = self.clients.iter().copied().collect();
            if !clients.is_empty() {
                self.announce(&clients).await;
            }
        }
    }

    fn schedule_push(&mut self, now: Instant) {
        if self.push_at.is_none() {
            self.push_at = Some(now + ALLOWLIST_DEBOUNCE);
        }
    }

    async fn announce(&self, receivers: &[u32]) {
        let relay = match self.config.relay(self.epoch.clone()) {
            Ok(relay) => relay,
            Err(error) => {
                tracing::warn!(%error, "peer: no announcement");
                return;
            }
        };
        self.send(receivers, wire::encode_relay(&relay), "announcement")
            .await;
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>, what: &str) {
        if let Err(error) = self.link.send(receivers, data).await {
            tracing::warn!(%error, "peer: {what} not sent");
        }
    }

    async fn on_request(&mut self, sender: u32, request: TokenRequest, now: Instant) {
        let Some(users) = self.link.users().await else {
            return;
        };
        let result = match self.grant(sender, request, &users, now) {
            Ok(grant) => self.register(grant).await,
            Err(reason) => Err(reason),
        };
        match result {
            Ok(grant) => {
                let token = Token {
                    id: request.id,
                    a: sender,
                    b: request.peer,
                    token: grant.token,
                    exp: grant.expires_at,
                };
                self.send(&[sender, request.peer], wire::encode_token(&token), "token")
                    .await;
            }
            Err(reason) => {
                tracing::debug!(?reason, "peer: token request denied");
                self.send(&[sender], wire::encode_deny(request.id, reason), "deny")
                    .await;
            }
        }
    }

    /// Checks a request and builds the token; nothing is registered yet.
    fn grant(
        &mut self,
        sender: u32,
        request: TokenRequest,
        users: &Users,
        now: Instant,
    ) -> Result<TokenGrant, DenyReason> {
        let Some(Some(a)) = users.certs.get(&sender) else {
            return Err(DenyReason::NoCert);
        };
        if !self.limiter.allow(a, now) {
            return Err(DenyReason::RateLimited);
        }
        if request.peer == sender
            || request.peer == users.own
            || !self.clients.contains(&request.peer)
        {
            return Err(DenyReason::UnknownPeer);
        }
        let b = match users.certs.get(&request.peer) {
            None => return Err(DenyReason::UnknownPeer),
            Some(None) => return Err(DenyReason::NoCert),
            Some(Some(b)) => b,
        };
        let token = random_hex::<32>().map_err(|error| {
            tracing::warn!(%error, "peer: no random token");
            DenyReason::RelayUnavailable
        })?;
        Ok(TokenGrant {
            token,
            a: a.clone(),
            b: b.clone(),
            expires_at: unix_now() + self.config.token_ttl().as_secs(),
        })
    }

    async fn register(&self, grant: TokenGrant) -> Result<TokenGrant, DenyReason> {
        match self.relay.register_token(&grant).await {
            Ok(()) => Ok(grant),
            Err(error) => {
                tracing::warn!(%error, "peer: the relay did not take the token");
                Err(DenyReason::RelayUnavailable)
            }
        }
    }
}

/// Runs the module until the bot's event stream ends.
pub async fn run<R: RelayApi, L: Link>(
    config: PeerConfig,
    relay: R,
    link: L,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let mut peer = Peer::new(config, relay, link)?;
    let mut ticks = tokio::time::interval(ALLOWLIST_DEBOUNCE);
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    for interval in [&mut ticks, &mut status] {
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    }
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => peer.on_event(event, Instant::now()).await,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    // Missed joins or leaves are caught by the next allowlist comparison;
                    // a missed hello or request is repeated by the client.
                    tracing::warn!(missed, "peer: fell behind the event stream");
                    peer.schedule_push(Instant::now());
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = ticks.tick() => peer.tick(Instant::now()).await,
            _ = status.tick() => peer.check_status(Instant::now()).await,
        }
    }
}

/// A fresh epoch: 8 random bytes as hex ([`wire::EPOCH_LEN`] characters).
fn new_epoch() -> Result<String> {
    random_hex::<8>()
}

fn random_hex<const N: usize>() -> Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("no system randomness")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests;
