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
//! When someone leaves or changes certificate, the relay drops their hash. Runs as
//! `mumbot peer --config bot.toml` with a `[peer]` section.

pub mod config;
pub mod relay;
pub mod wire;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use mumcord_protocol::Event;
use tokio::sync::broadcast;
use tokio::time::Instant;

pub use config::{PeerConfig, Secret};
pub use relay::{HttpRelay, Pair, PairError, RelayApi, Status};

use crate::webhook::{BotApi, WebhookConfig};
use wire::{Relay, State, Token, TokenError, TokenRequest};

/// How often the relay's status is polled (FR-4.25).
pub const STATUS_INTERVAL: Duration = Duration::from_secs(30);
/// Failed polls in a row before the relay counts as down (FR-4.25).
const FAILURES_UNTIL_DOWN: u32 = 2;
/// Token requests per requesting session and minute (FR-4.26).
pub const REQUESTS_PER_MINUTE: usize = 6;

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

/// Sliding one-minute window of token requests per session.
#[derive(Default)]
struct RequestLimiter {
    seen: HashMap<u32, VecDeque<Instant>>,
}

impl RequestLimiter {
    fn allow(&mut self, session: u32, now: Instant) -> bool {
        let window = Duration::from_secs(60);
        // Forget idle requesters so the map cannot grow with every session ever seen.
        self.seen.retain(|_, times| {
            while times.front().is_some_and(|t| now - *t >= window) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let times = self.seen.entry(session).or_default();
        if times.len() >= REQUESTS_PER_MINUTE {
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
    limiter: RequestLimiter,
}

impl<R: RelayApi, L: Link> Peer<R, L> {
    pub fn new(relay: R, link: L) -> Self {
        Self {
            relay,
            link,
            status: None,
            state: State::Down,
            failures: 0,
            seq: 0,
            clients: HashMap::new(),
            limiter: RequestLimiter::default(),
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
                        self.remove_peer(&hash).await;
                    }
                }
            }
            Event::UserRemoved { user, .. } => {
                if let Some(Some(hash)) = self.clients.remove(&user.session) {
                    self.remove_peer(&hash).await;
                }
            }
            // Sessions are only valid per connection; after a reconnect the clients greet the
            // bot again because it is a new joiner for them (FR-4.24).
            Event::Disconnected(_) => self.clients.clear(),
            _ => {}
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
            Ok((a, b)) => self.pair(&a, &b).await,
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

    async fn remove_peer(&self, hash: &str) {
        if let Err(error) = self.relay.remove_peer(hash).await {
            tracing::warn!(%error, "peer: the relay did not drop a leaving user");
        }
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
pub async fn run<R: RelayApi, L: Link>(
    relay: R,
    link: L,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let mut peer = Peer::new(relay, link);
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    status.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
        }
    }
}

/// `mumbot peer`: logs in, keeps the connection up and runs the module (FR-4.22).
pub async fn run_bot(config: Arc<WebhookConfig>) -> Result<()> {
    let Some(peer) = &config.peer else {
        bail!("the config has no [peer] section");
    };
    let url = peer.url()?;
    if !url.is_local_or_tls() {
        tracing::warn!(
            api_url = %peer.api_url,
            "peer: relay API over plain HTTP to another host: the API secret travels in clear \
             text, use https with api_fp"
        );
    }
    let relay = HttpRelay::new(peer, peer.load_secret()?)?;
    let api = Arc::new(BotApi::new(Arc::clone(&config)));
    // Subscribed before the login: Mumcord clients greet a joining bot right away.
    let events = api.subscribe();
    api.connect().await?;
    tracing::info!(name = %config.server.name, "peer bot connected");
    tokio::select! {
        result = run(relay, Arc::clone(&api), events) => result,
        result = crate::webhook::supervise(Arc::clone(&api)) => result,
    }
}

#[cfg(test)]
mod tests;
