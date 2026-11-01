//! Plugins module: announces the Mumcord plugins a server community uses, so Mumcord clients can
//! suggest them in their notification centre. Spec: Mumcord `specs/06-plugin-system-
//! specification.md` (FR-6.77 to FR-6.79, section 6.4), mumbot#20.
//!
//! A Mumcord client whose hello lists `server-plugins` gets the list (see [`wire`]), to that
//! client only, at most once a minute. Answers to many clients go out together in one message
//! per second, which leaves the server's plugin data limit (4 per second, burst 15) to the other
//! modules. Clients trust the list only from a registered user on a pinned server, so the module
//! refuses to run otherwise. It installs nothing, fetches nothing and receives nothing back.
//!
//! A changed list takes effect on restart with a higher `seq` (kept in the data directory); the
//! clients greet the reconnected bot as a new joiner and get the new list.

pub mod config;
pub mod wire;

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::{Result, bail};
use mumcord_protocol::Event;
use tokio::sync::broadcast;
use tokio::time::Instant;

pub use config::PluginsConfig;

use crate::peer::wire::{HELLO_ID, hello_lists};

/// Least time between two announcements to one session (FR-6.77, mumbot#20).
pub const RESEND_AFTER: Duration = Duration::from_secs(60);
/// How often waiting answers go out, together in one message.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The narrow view of the bot the module needs.
pub trait Link {
    /// Whether the bot's own user is registered; `None` while the bot is not connected.
    fn registered(&self) -> impl Future<Output = Option<bool>> + Send;
    fn send(&self, receivers: &[u32], data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;
}

/// State of the module between events.
pub struct Plugins<L> {
    link: L,
    message: Vec<u8>,
    /// Sessions that asked and wait for the next flush.
    waiting: BTreeSet<u32>,
    /// When each session last got the list.
    sent: HashMap<u32, Instant>,
}

impl<L: Link> Plugins<L> {
    /// `message` is the encoded announcement ([`wire::encode`]).
    pub fn new(link: L, message: Vec<u8>) -> Self {
        Self {
            link,
            message,
            waiting: BTreeSet::new(),
            sent: HashMap::new(),
        }
    }

    /// Reacts to one event of the Mumble connection.
    pub fn on_event(&mut self, event: Event, now: Instant) {
        match event {
            Event::PluginData(data) => {
                let Some(sender) = data.sender else { return };
                if data.data_id == HELLO_ID && hello_lists(&data.data, wire::FEATURE) {
                    let recent = self
                        .sent
                        .get(&sender)
                        .is_some_and(|at| now - *at < RESEND_AFTER);
                    if !recent {
                        self.waiting.insert(sender);
                    }
                }
            }
            Event::UserRemoved { user, .. } => {
                self.waiting.remove(&user.session);
                self.sent.remove(&user.session);
            }
            // Sessions are only valid per connection; after a reconnect the clients greet the
            // bot again because it is a new joiner for them.
            Event::Disconnected(_) => {
                self.waiting.clear();
                self.sent.clear();
            }
            _ => {}
        }
    }

    /// Sends the list to every waiting session in one message.
    pub async fn flush(&mut self, now: Instant) {
        if self.waiting.is_empty() {
            return;
        }
        let receivers: Vec<u32> = std::mem::take(&mut self.waiting).into_iter().collect();
        match self.link.send(&receivers, self.message.clone()).await {
            Ok(()) => {
                tracing::debug!(clients = receivers.len(), "plugins: list announced");
                for session in receivers {
                    self.sent.insert(session, now);
                }
            }
            // The clients ask again on their next hello; retrying here could loop on a server
            // that refuses plugin data.
            Err(error) => tracing::warn!(%error, "plugins: list not sent"),
        }
    }
}

/// Fails unless the bot's user is registered: clients ignore suggestions from anyone else
/// (FR-6.77).
async fn check_registered<L: Link>(link: &L) -> Result<()> {
    let deadline = Instant::now() + crate::DEFAULT_TIMEOUT;
    loop {
        match link.registered().await {
            Some(true) => return Ok(()),
            Some(false) => bail!(
                "the plugins module needs a registered bot user, Mumcord ignores suggestions \
                 from anyone else: register the bot on the server (an admin, or `mumbot \
                 register`) and start it again"
            ),
            None if Instant::now() >= deadline => {
                bail!("plugins: the bot did not connect in time")
            }
            None => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

/// Runs the module until the bot's event stream ends.
pub async fn run<L: Link>(
    link: L,
    message: Vec<u8>,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    check_registered(&link).await?;
    tracing::info!("plugins: announcing to Mumcord clients");
    let mut plugins = Plugins::new(link, message);
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => plugins.on_event(event, Instant::now()),
                // A missed hello only costs that client this announcement.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "plugins: fell behind the event stream");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = flush.tick() => plugins.flush(Instant::now()).await,
        }
    }
}

#[cfg(test)]
mod tests;
