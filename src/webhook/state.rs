//! Live view of the bot for the HTTP handler: connection state, live channel tree, delivery.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use mumcord_protocol::{Event, Handle, ServerState, TextTarget, permissions};
use tokio::sync::{Mutex, broadcast};

use crate::bot::BROADCAST_CAPACITY;
use crate::checks::resolve_channel;
use crate::hello;
use crate::history::{self, HistoryLink};
use crate::peer::wire::HELLO_ID;
use crate::peer::{Link, Users};
use crate::plugins;
use crate::webhook::MAX_MESSAGE_CHARS;
use crate::webhook::config::{ApiKey, ChannelRef, WebhookConfig};
use crate::webhook::http::{Api, DeliveryError};
use crate::{Bot, Module};

/// Wait between reconnect attempts (FR-1.1).
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// The bot behind a mutex, so several requests can use the one connection (Q3.6).
pub struct BotApi {
    bot: Arc<Mutex<Option<Bot>>>,
    config: Arc<WebhookConfig>,
    started: Instant,
    /// Outlives reconnects, so a subscriber keeps one stream across them.
    events: broadcast::Sender<Event>,
}

impl BotApi {
    pub fn new(config: Arc<WebhookConfig>) -> Self {
        Self {
            bot: Arc::new(Mutex::new(None)),
            config,
            started: Instant::now(),
            events: broadcast::channel(BROADCAST_CAPACITY).0,
        }
    }

    /// Events of the connection, from the next one on; subscribe before [`BotApi::connect`] to
    /// see everything from the login on.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Logs in and joins the configured channel.
    pub async fn connect(&self) -> Result<()> {
        let bot = Bot::connect_with(self.config.bot_config(), self.events.clone()).await?;
        let mut guard = self.bot.lock().await;
        let bot = guard.insert(bot);
        self.settle(bot).await
    }

    /// Reconnects after the link dropped (FR-1.1).
    pub async fn reconnect(&self) -> Result<()> {
        let mut guard = self.bot.lock().await;
        let bot = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("bot is gone"))?;
        bot.reconnect_as(&self.config.server.name).await?;
        self.settle(bot).await
    }

    /// After every login: joins the configured channel, then the history module listens to its
    /// channels and shows its notice (spec 04, FR-4.4, FR-4.17).
    async fn settle(&self, bot: &Bot) -> Result<()> {
        if let Some(channel) = &self.config.server.channel {
            bot.join(channel).await?;
        }
        if let Some(history) = &self.config.history
            && self.config.modules.enabled.contains(&Module::History)
        {
            history::announce(bot, history)?;
        }
        Ok(())
    }

    /// The config this view was built from (tests and the CLI).
    pub fn config(&self) -> &Arc<WebhookConfig> {
        &self.config
    }

    /// The live command handle, if the bot is connected right now.
    pub async fn current_handle(&self) -> Option<Handle> {
        let guard = self.bot.lock().await;
        guard
            .as_ref()
            .map(|bot| bot.handle().clone())
            .filter(Handle::is_connected)
    }

    /// Clone of the live command handle, so a test can force a reconnect.
    pub async fn handle(&self) -> Handle {
        let guard = self.bot.lock().await;
        guard
            .as_ref()
            .expect("bot is not connected")
            .handle()
            .clone()
    }
}

impl Api for BotApi {
    async fn connected(&self) -> bool {
        let guard = self.bot.lock().await;
        guard
            .as_ref()
            .is_some_and(|bot| bot.handle().is_connected())
    }

    async fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    async fn channel_exists(&self, id: u32) -> bool {
        let guard = self.bot.lock().await;
        guard
            .as_ref()
            .is_some_and(|bot| bot.state().channels.contains_key(&id))
    }

    /// Config entries may be a name or an ID; names resolve against the live tree (FR-1.3, FR-1.4).
    async fn message_cap(&self) -> usize {
        let guard = self.bot.lock().await;
        let reported = guard
            .as_ref()
            .and_then(|bot| bot.state().server.message_length)
            .unwrap_or(0);
        if reported == 0 {
            return MAX_MESSAGE_CHARS;
        }
        (reported as usize).min(MAX_MESSAGE_CHARS)
    }

    async fn allowed_ids(&self, key: &ApiKey) -> Vec<u32> {
        let guard = self.bot.lock().await;
        let Some(bot) = guard.as_ref() else {
            return Vec::new();
        };
        let state = bot.state();
        let mut ids: Vec<u32> = key
            .channels
            .iter()
            .filter_map(|entry| match entry {
                ChannelRef::Id(id) => Some(*id),
                ChannelRef::Name(name) => resolve_channel(&state, name).ok(),
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    async fn allowed_channels(&self, key: &ApiKey) -> Vec<(u32, String)> {
        let guard = self.bot.lock().await;
        let Some(bot) = guard.as_ref() else {
            return Vec::new();
        };
        let state = bot.state();
        key.channels
            .iter()
            .filter_map(|entry| match entry {
                ChannelRef::Id(id) => Some(*id),
                ChannelRef::Name(name) => resolve_channel(&state, name).ok(),
            })
            .filter_map(|id| {
                state
                    .channels
                    .get(&id)
                    .map(|channel| (id, channel.name.clone()))
            })
            .collect()
    }

    async fn deliver(&self, channel_id: u32, html: &str) -> Result<(), DeliveryError> {
        let mut guard = self.bot.lock().await;
        let Some(bot) = guard.as_mut() else {
            return Err(DeliveryError::NotConnected);
        };
        if !bot.handle().is_connected() {
            return Err(DeliveryError::NotConnected);
        }
        let known = bot
            .state()
            .permissions
            .get(&channel_id)
            .is_some_and(|bits| bits & permissions::TEXT_MESSAGE != 0);
        if let Err(error) = bot
            .handle()
            .send_text(&TextTarget::channel(channel_id), html)
        {
            tracing::warn!(%error, "sending the message failed");
            return Err(DeliveryError::Refused);
        }
        // Fast path: the server already granted chat on this channel.
        if known {
            return Ok(());
        }
        // murmur does not acknowledge chat messages, so a denial inside the delivery window is
        // the only refusal we can see. No denial means the server took the message.
        let mut events = bot.subscribe();
        let own = bot.state().session;
        let deadline = tokio::time::Instant::now() + self.config.delivery_timeout();
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Ok(Event::PermissionDenied(denied)))
                    if denied.channel_id == Some(channel_id) =>
                {
                    return Err(DeliveryError::Refused);
                }
                Ok(Ok(Event::TextMessage(message))) if message.actor == own => return Ok(()),
                Ok(Ok(_)) => continue,
                Ok(Err(_)) => return Err(DeliveryError::NotConnected),
                Err(_) => return Ok(()),
            }
        }
    }
}

/// The peer module's view of the bot (`src/peer/`).
impl Link for Arc<BotApi> {
    async fn users(&self) -> Option<Users> {
        let guard = self.bot.lock().await;
        let bot = guard.as_ref().filter(|bot| bot.handle().is_connected())?;
        let state = bot.state();
        Some(Users {
            own: state.session?,
            certs: state
                .users
                .values()
                .map(|user| (user.session, user.cert_hash.clone()))
                .collect(),
            user_ids: state
                .users
                .values()
                .filter_map(|user| Some((user.session, user.user_id?)))
                .collect(),
        })
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        let guard = self.bot.lock().await;
        let bot = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bot is not connected"))?;
        bot.handle()
            .send_plugin_data(receivers, crate::peer::wire::DATA_ID, data)?;
        Ok(())
    }
}

/// The history module's view of the bot (`src/history/`).
impl HistoryLink for Arc<BotApi> {
    async fn state(&self) -> Option<ServerState> {
        let guard = self.bot.lock().await;
        let bot = guard.as_ref().filter(|bot| bot.handle().is_connected())?;
        Some(bot.state())
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        let guard = self.bot.lock().await;
        let bot = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bot is not connected"))?;
        bot.handle()
            .send_plugin_data(receivers, history::wire::DATA_ID, data)?;
        Ok(())
    }
}

/// The plugins module's view of the bot (`src/plugins/`).
impl plugins::Link for Arc<BotApi> {
    async fn registered(&self) -> Option<bool> {
        let guard = self.bot.lock().await;
        let bot = guard.as_ref().filter(|bot| bot.handle().is_connected())?;
        Some(bot.own_user().ok()?.user_id.is_some())
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        let guard = self.bot.lock().await;
        let bot = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bot is not connected"))?;
        bot.handle()
            .send_plugin_data(receivers, plugins::wire::DATA_ID, data)?;
        Ok(())
    }
}

/// The bot's own Mumcord hello (`src/hello.rs`).
impl hello::Link for Arc<BotApi> {
    async fn send_hello(&self, receivers: &[u32], message: &[u8]) -> Result<()> {
        let guard = self.bot.lock().await;
        let bot = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("bot is not connected"))?;
        bot.handle()
            .send_plugin_data(receivers, HELLO_ID, message.to_vec())?;
        Ok(())
    }
}

/// Reconnects whenever the link dropped (FR-1.1); runs until the process is stopped.
pub async fn supervise(api: Arc<BotApi>) -> Result<()> {
    loop {
        if !api.connected().await {
            tracing::warn!("connection lost, reconnecting");
            if let Err(error) = api.reconnect().await {
                tracing::warn!(%error, "reconnect failed");
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}
