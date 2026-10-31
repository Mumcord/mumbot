//! History module: records the chat of the configured channels and answers Mumcord's history
//! sync (mumcord FR-1.60) like a Mumcord client that shares its history, so Mumcord users who
//! were offline can load what they missed. Spec: `specs/04-history-specification.md`.
//!
//! Nothing is added to or required by the Mumble server: the bot sits in a channel, listens to
//! the other recorded channels, stores what it receives in a local SQLite file ([`store`]) and
//! answers plugin data requests ([`wire`], [`hash`]) through [`core::Core`]. Runs as a module
//! (`mumbot history --config bot.toml`, or `mumbot run` with other modules) with a `[history]`
//! section.

pub mod config;
pub mod core;
pub mod hash;
pub mod store;
pub mod wire;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use mumcord_protocol::{Event, ServerState};
use tokio::sync::broadcast;

pub use config::HistoryConfig;

use self::core::{Core, recorded_channels, unresolved};
use self::store::Store;
use crate::Bot;

/// How often queued packets go out and stale answers are dropped.
const TICK: Duration = Duration::from_millis(100);
/// How often the retention runs (FR-4.7).
const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The narrow view of the bot the module needs.
pub trait HistoryLink {
    /// The connection's current state; `None` while the bot is not connected.
    fn state(&self) -> impl Future<Output = Option<ServerState>> + Send;
    /// Sends history sync plugin data.
    fn send(&self, receivers: &[u32], data: Vec<u8>) -> impl Future<Output = Result<()>> + Send;
}

/// Unix milliseconds of the bot's clock: Mumble has no server timestamps.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Runs `f` on the core off the async runtime: store calls block.
async fn blocking<T: Send + 'static>(
    core: &Arc<Mutex<Core>>,
    f: impl FnOnce(&mut Core) -> T + Send + 'static,
) -> Result<T> {
    let core = Arc::clone(core);
    tokio::task::spawn_blocking(move || {
        let mut core = core.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut core)
    })
    .await
    .context("history worker failed")
}

/// Runs the module until the bot's event stream ends. Subscribe `events` before the login, so
/// the module sees everyone arrive.
pub async fn run<L: HistoryLink>(
    config: HistoryConfig,
    store: Store,
    link: L,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let core = Arc::new(Mutex::new(Core::new(config, store, now_millis())));
    blocking(&core, |core| core.prune(now_millis())).await?;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut prune = tokio::time::interval(PRUNE_INTERVAL);
    prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The protocol does not announce the channels and people already there at login, so the
    // core takes a snapshot after every login (and after missed events).
    let mut synced = false;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(Event::TextMessage(message)) => {
                    // The own user (what the bot receives live) is only known in the state.
                    let Some(state) = link.state().await else { continue };
                    blocking(&core, move |core| core.record(&message, &state, now_millis()))
                        .await?;
                }
                Ok(event) => {
                    if matches!(event, Event::Disconnected(_)) {
                        synced = false;
                    }
                    blocking(&core, move |core| core.on_event(event, now_millis())).await?;
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    // Missed joins and moves would hide who may ask; take the current state.
                    tracing::warn!(missed, "history: fell behind the event stream, resyncing");
                    synced = false;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = tick.tick() => {
                if !synced
                    && let Some(state) = link.state().await.filter(|s| s.session.is_some())
                {
                    blocking(&core, move |core| core.resync(&state, now_millis())).await?;
                    synced = true;
                }
                let outgoing = blocking(&core, |core| {
                    let now = now_millis();
                    core.tick(now);
                    core.take_outgoing(now)
                })
                .await?;
                for (receivers, data) in outgoing {
                    if let Err(error) = link.send(&receivers, data).await {
                        tracing::warn!(%error, "history: plugin data not sent");
                    }
                }
            }
            _ = prune.tick() => blocking(&core, |core| core.prune(now_millis())).await?,
        }
    }
}

/// After every login: listens to the recorded channels other than the own one and sets the
/// notice as the comment (FR-4.4, FR-4.17).
pub fn announce(bot: &Bot, config: &HistoryConfig) -> Result<()> {
    let state = bot.state();
    for entry in unresolved(config, &state) {
        tracing::warn!(channel = ?entry, "history: configured channel not found, skipped");
    }
    let recorded = recorded_channels(config, &state);
    let own = bot.own_user()?.channel_id;
    let listen: Vec<u32> = recorded.iter().copied().filter(|&c| c != own).collect();
    if !listen.is_empty() {
        bot.handle()
            .listen(&listen)
            .context("history: listening to the recorded channels")?;
    }
    let mut names: Vec<&str> = recorded
        .iter()
        .filter_map(|id| state.channels.get(id).map(|c| c.name.as_str()))
        .collect();
    names.sort_unstable();
    tracing::info!(channels = recorded.len(), "history: recording");
    bot.set_comment(&notice(config, &names))
}

/// The comment that tells everyone what the bot records (FR-4.17).
pub fn notice(config: &HistoryConfig, channels: &[&str]) -> String {
    let which = if config.records_all() && config.exclude.is_empty() {
        "all channels".to_owned()
    } else if channels.is_empty() {
        "no channel yet".to_owned()
    } else {
        channels
            .iter()
            .map(|name| escape(name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let days = config.retention_days;
    format!(
        "<b>History bot</b><br>This bot records the chat of {which}, so Mumcord users can load \
         messages from before they joined (history sync). Messages are kept for {days} day{} \
         (at most {} per channel). Private messages and voice are never recorded.",
        if days == 1 { "" } else { "s" },
        config.max_messages_per_channel,
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests;
