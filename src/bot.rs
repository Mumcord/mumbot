//! A connected bot: commands plus waiting helpers over what it observes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use mumcord_protocol::{
    ConnectConfig, DisconnectReason, Event, Handle, PermissionDenied, ServerState, TextMessage,
    TextTarget, User, connect, proto,
};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::checks::{MessageCheck, UserCheck, find_user, resolve_channel};
use crate::{cert, env};

/// Default timeout of the `wait_*` helpers and of the login.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Name of the built-in admin account of murmur.
const SUPERUSER: &str = "SuperUser";

/// Capacity of the raw event broadcast; slow subscribers lose the oldest events.
pub(crate) const BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub host: String,
    pub port: u16,
    /// User name; ignored for the superuser.
    pub name: String,
    /// Name of the persistent certificate; defaults to `name`. Keep it when renaming.
    pub cert_name: Option<String>,
    /// Where certificates are kept; defaults to [`cert::default_dir`].
    pub data_dir: Option<PathBuf>,
    /// Log in as `SuperUser` with `SUPERUSER_PASSWORD` (no certificate).
    pub superuser: bool,
    /// Server password; defaults to `SERVER_PASSWORD` from the test environment.
    pub password: Option<String>,
    /// Channel passwords (Mumble access tokens) presented at login (mumcord FR-2.78).
    pub tokens: Vec<String>,
    /// SHA-256 (lowercase hex) of the server certificate; when set, any other certificate is
    /// refused. Only the peer bot sets it; without it every certificate is accepted (local test
    /// servers are self-signed).
    pub server_cert_sha256: Option<String>,
    pub timeout: Duration,
}

impl BotConfig {
    /// A normal bot on the given local test-server port.
    pub fn new(name: &str, port: u16) -> Self {
        Self {
            host: env::HOST.to_owned(),
            port,
            name: name.to_owned(),
            cert_name: None,
            data_dir: None,
            superuser: false,
            password: None,
            tokens: Vec::new(),
            server_cert_sha256: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// The `SuperUser` admin on the given local test-server port.
    pub fn superuser(port: u16) -> Self {
        Self {
            superuser: true,
            ..Self::new(SUPERUSER, port)
        }
    }

    fn connect_config(&self) -> Result<ConnectConfig> {
        let (username, password, identity) = if self.superuser {
            let password = env::testenv_var("SUPERUSER_PASSWORD")
                .context("SUPERUSER_PASSWORD is not set (environment or testenv/.env)")?;
            (SUPERUSER.to_owned(), Some(password), None)
        } else {
            let dir = self.data_dir.clone().unwrap_or_else(cert::default_dir);
            let cert_name = self.cert_name.as_deref().unwrap_or(&self.name);
            let identity = cert::load_or_create(&dir, cert_name)?;
            let password = self
                .password
                .clone()
                .or_else(|| env::testenv_var("SERVER_PASSWORD"));
            (self.name.clone(), password, Some(identity))
        };
        Ok(ConnectConfig {
            host: self.host.clone(),
            port: self.port,
            username,
            password,
            tokens: self.tokens.clone(),
            identity,
            verify_cert: match self.server_cert_sha256.clone() {
                Some(pin) => Arc::new(move |cert| cert.sha256_hex == pin),
                // The test bot only talks to local test servers, whose certificates are
                // self-signed.
                None => Arc::new(|_| true),
            },
            login_timeout: self.timeout,
        })
    }
}

/// Admin changes to another user's voice state; `None` leaves a flag as it is. Suppression is
/// not here: servers refuse to suppress by request, it follows from a missing Speak permission
/// (see [`Bot::deny_speak`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Moderation {
    pub mute: Option<bool>,
    pub deaf: Option<bool>,
}

/// What the bot has received since it connected.
#[derive(Default)]
struct Log {
    messages: Vec<TextMessage>,
    denials: Vec<PermissionDenied>,
    /// Ids of `ChannelUpdated` events, in arrival order.
    channel_updates: Vec<u32>,
    disconnected: Option<DisconnectReason>,
}

struct Observed {
    log: Mutex<Log>,
    /// Bumped after every event so waiters re-check their condition.
    tick: watch::Sender<u64>,
}

impl Observed {
    fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A connected bot. Dropping it disconnects.
pub struct Bot {
    config: BotConfig,
    handle: Handle,
    observed: Arc<Observed>,
    events: broadcast::Sender<Event>,
    pump: JoinHandle<()>,
}

impl Bot {
    pub async fn connect(config: BotConfig) -> Result<Self> {
        let (events, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self::connect_with(config, events).await
    }

    /// Connects and forwards events to `events`, so a subscriber can listen from the very first
    /// event on.
    pub(crate) async fn connect_with(
        config: BotConfig,
        events: broadcast::Sender<Event>,
    ) -> Result<Self> {
        let conn = connect(config.connect_config()?).await.with_context(|| {
            format!(
                "connecting {:?} to {}:{}",
                config.name, config.host, config.port
            )
        })?;
        let observed = Arc::new(Observed {
            log: Mutex::new(Log::default()),
            tick: watch::Sender::new(0),
        });
        let pump = tokio::spawn(pump(
            conn.handle.clone(),
            conn.events,
            observed.clone(),
            events.clone(),
        ));
        Ok(Self {
            config,
            handle: conn.handle,
            observed,
            events,
            pump,
        })
    }

    /// Disconnects and logs in again under `new_name` with the same certificate.
    pub async fn reconnect_as(&mut self, new_name: &str) -> Result<()> {
        if self.config.superuser {
            bail!("the superuser cannot be renamed");
        }
        let mut config = self.config.clone();
        config.cert_name.get_or_insert_with(|| config.name.clone());
        config.name = new_name.to_owned();
        if self.handle.is_connected() {
            self.flush().await?;
        }
        // Let the server drop the old session before the same certificate logs in again.
        self.close().await;
        let fresh = Self::connect_with(config, self.events.clone()).await?;
        let old = std::mem::replace(self, fresh);
        drop(old);
        Ok(())
    }

    /// Disconnects cleanly: first waits until the server has everything sent so far (see
    /// [`Bot::flush`]), then closes the connection and waits until it is closed. The connection
    /// is closed even when the flush fails; the error is returned afterwards.
    pub async fn disconnect(mut self) -> Result<()> {
        let flushed = if self.handle.is_connected() {
            self.flush().await
        } else {
            Ok(())
        };
        self.close().await;
        flushed
    }

    /// Waits until the server has received everything this bot sent so far. Chat messages and
    /// state changes are rate-limited on the client side and can still wait in the send queue,
    /// which a disconnect drops. That queue is first in, first out, so a no-op self-mute change
    /// (the own current value, which the server echoes to everyone) comes back only after all
    /// that was queued before it has been written.
    pub async fn flush(&self) -> Result<()> {
        let own = self.own_user()?;
        let mut events = self.subscribe();
        self.handle.set_self_mute(own.self_mute)?;
        let deadline = Instant::now() + self.config.timeout;
        loop {
            let event = tokio::time::timeout_at(deadline, events.recv())
                .await
                .map_err(|_| {
                    anyhow!(
                        "timed out after {:?} waiting for the server to receive the sent messages",
                        self.config.timeout
                    )
                })?;
            match event {
                Ok(Event::UserUpdated(change)) if change.user.session == own.session => {
                    return Ok(());
                }
                Ok(Event::Disconnected(reason)) => {
                    bail!("disconnected ({reason:?}) before the sent messages were confirmed")
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    bail!("connection closed before the sent messages were confirmed")
                }
            }
        }
    }

    /// Closes the connection and waits until it is closed, so the server sees the session end.
    async fn close(&mut self) {
        self.handle.disconnect();
        if tokio::time::timeout(self.config.timeout, &mut self.pump)
            .await
            .is_err()
        {
            tracing::warn!("connection did not close in time");
        }
    }

    /// Raw protocol handle, e.g. for the voice module.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// All protocol events (including voice tunnel/crypt events). Survives `reconnect_as`.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn config(&self) -> &BotConfig {
        &self.config
    }

    pub fn state(&self) -> ServerState {
        self.handle.state()
    }

    pub fn own_user(&self) -> Result<User> {
        self.handle
            .with_state(|s| s.own_user().cloned())
            .ok_or_else(|| anyhow!("own user not known"))
    }

    pub fn channel_id(&self, name: &str) -> Result<u32> {
        self.handle.with_state(|s| resolve_channel(s, name))
    }

    fn session_of(&self, name: &str) -> Result<u32> {
        self.handle
            .with_state(|s| find_user(s, name).map(|u| u.session))
            .ok_or_else(|| anyhow!("no user {name:?} connected"))
    }

    /// Moves into `channel` and waits until the server confirms it.
    pub async fn join(&self, channel: &str) -> Result<()> {
        let id = self.channel_id(channel)?;
        self.handle.join_channel(id)?;
        self.wait_own(&format!("joining {channel}"), |u| u.channel_id == id)
            .await
    }

    /// Sends HTML to a channel (`None` = the current channel).
    pub fn send_channel(&self, channel: Option<&str>, html: &str) -> Result<()> {
        let id = match channel {
            Some(name) => self.channel_id(name)?,
            None => self.own_user()?.channel_id,
        };
        Ok(self.handle.send_text(&TextTarget::channel(id), html)?)
    }

    /// Sends HTML privately to a connected user.
    pub fn send_private(&self, user: &str, html: &str) -> Result<()> {
        let session = self.session_of(user)?;
        Ok(self.handle.send_text(&TextTarget::user(session), html)?)
    }

    pub fn set_mute(&self, mute: bool) -> Result<()> {
        Ok(self.handle.set_self_mute(mute)?)
    }

    pub fn set_deaf(&self, deaf: bool) -> Result<()> {
        Ok(self.handle.set_self_deaf(deaf)?)
    }

    /// Server-mutes or -deafens the user `name` as an admin (needs the superuser bot).
    pub fn moderate(&self, name: &str, moderation: Moderation) -> Result<()> {
        self.send_admin_user_state(
            name,
            proto::UserState {
                mute: moderation.mute,
                deaf: moderation.deaf,
                ..Default::default()
            },
        )
    }

    /// Moves the user `name` into `channel` as an admin (needs the superuser bot).
    pub fn move_user(&self, name: &str, channel: &str) -> Result<()> {
        let channel_id = self.channel_id(channel)?;
        self.send_admin_user_state(
            name,
            proto::UserState {
                channel_id: Some(channel_id),
                ..Default::default()
            },
        )
    }

    fn send_admin_user_state(&self, name: &str, mut m: proto::UserState) -> Result<()> {
        if !self.config.superuser {
            bail!("changing other users needs the superuser bot");
        }
        m.session = Some(
            find_user(&self.state(), name)
                .with_context(|| format!("no user {name:?}"))?
                .session,
        );
        Ok(self.handle.send_admin_user_state(m)?)
    }

    /// Requests listening; a refusal arrives as a denial (see [`Bot::wait_denied`]).
    pub fn listen(&self, channels: &[&str]) -> Result<()> {
        let ids = self.channel_ids(channels)?;
        Ok(self.handle.listen(&ids)?)
    }

    pub fn unlisten(&self, channels: &[&str]) -> Result<()> {
        let ids = self.channel_ids(channels)?;
        Ok(self.handle.unlisten(&ids)?)
    }

    fn channel_ids(&self, channels: &[&str]) -> Result<Vec<u32>> {
        channels.iter().map(|c| self.channel_id(c)).collect()
    }

    /// Sets the avatar (PNG/JPEG bytes; empty removes it).
    pub fn set_avatar(&self, image: Vec<u8>) -> Result<()> {
        Ok(self.handle.set_texture(image)?)
    }

    /// Sets the comment (HTML; empty removes it).
    pub fn set_comment(&self, html: &str) -> Result<()> {
        Ok(self.handle.set_comment(html.to_owned())?)
    }

    /// Registers the own certificate under the current name and waits for the confirmation.
    pub async fn register(&self) -> Result<()> {
        self.handle.register_self()?;
        self.wait_own("self-registration", |u| u.user_id.is_some())
            .await
    }

    /// Waits until the user `name` exists and matches `check`; returns that user.
    pub async fn wait_user(&self, name: &str, check: &UserCheck) -> Result<User> {
        self.wait_until(&format!("user {name:?} with {check:?}"), |s, _| {
            find_user(s, name).filter(|u| check.matches(u, s)).cloned()
        })
        .await
    }

    /// Waits until no user called `name` is connected.
    pub async fn wait_user_gone(&self, name: &str) -> Result<()> {
        self.wait_until(&format!("user {name:?} to leave"), |s, _| {
            find_user(s, name).is_none().then_some(())
        })
        .await
    }

    /// Waits for a received text message matching `check` (also ones that arrived earlier).
    pub async fn wait_message(&self, check: &MessageCheck) -> Result<TextMessage> {
        self.wait_until(&format!("message with {check:?}"), |s, log| {
            log.messages.iter().find(|m| check.matches(m, s)).cloned()
        })
        .await
    }

    /// Waits for a denial matching `pred` (also ones that arrived earlier).
    pub async fn wait_denied(
        &self,
        what: &str,
        pred: impl Fn(&PermissionDenied) -> bool,
    ) -> Result<PermissionDenied> {
        self.wait_until(&format!("denial: {what}"), |_, log| {
            log.denials.iter().find(|d| pred(d)).cloned()
        })
        .await
    }

    /// Runs `send` and waits for the next `ChannelState` of `channel_id` that follows it.
    pub(crate) async fn send_and_await_channel_update(
        &self,
        channel_id: u32,
        what: &str,
        send: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let seen = self.observed.log().channel_updates.len();
        send()?;
        self.wait_until(what, |_, log| {
            log.channel_updates[seen..]
                .contains(&channel_id)
                .then_some(())
        })
        .await
    }

    async fn wait_own(&self, what: &str, pred: impl Fn(&User) -> bool) -> Result<()> {
        self.wait_until(what, |s, _| s.own_user().filter(|u| pred(u)).map(|_| ()))
            .await
    }

    /// Re-checks `check` after every event until it yields a value or the timeout passes.
    async fn wait_until<T>(
        &self,
        what: &str,
        mut check: impl FnMut(&ServerState, &Log) -> Option<T>,
    ) -> Result<T> {
        let deadline = Instant::now() + self.config.timeout;
        let mut tick = self.observed.tick.subscribe();
        loop {
            tick.borrow_and_update();
            let found = {
                let log = self.observed.log();
                if let Some(reason) = &log.disconnected {
                    bail!("disconnected ({reason:?}) while waiting for {what}");
                }
                self.handle.with_state(|s| check(s, &log))
            };
            if let Some(value) = found {
                return Ok(value);
            }
            // The sender lives in `observed`, so `changed` only ends by timeout.
            if tokio::time::timeout_at(deadline, tick.changed())
                .await
                .is_err()
            {
                bail!(
                    "timed out after {:?} waiting for {what}",
                    self.config.timeout
                );
            }
        }
    }
}

impl Drop for Bot {
    fn drop(&mut self) {
        self.handle.disconnect();
    }
}

/// Records events for the waiters, forwards them to subscribers and fetches avatars/comments
/// that the server announced only by hash.
async fn pump(
    handle: Handle,
    mut events: mpsc::UnboundedReceiver<Event>,
    observed: Arc<Observed>,
    subscribers: broadcast::Sender<Event>,
) {
    let mut requested = HashSet::new();
    fetch_missing_blobs(&handle, &mut requested);
    while let Some(event) = events.recv().await {
        {
            let mut log = observed.log();
            match &event {
                Event::TextMessage(m) => log.messages.push(m.clone()),
                Event::PermissionDenied(d) => log.denials.push(d.clone()),
                Event::ChannelUpdated(c) => log.channel_updates.push(c.id),
                Event::Disconnected(reason) => log.disconnected = Some(reason.clone()),
                Event::UserUpdated(_) => fetch_missing_blobs(&handle, &mut requested),
                _ => {}
            }
        }
        // No subscribers is normal.
        let _ = subscribers.send(event);
        observed.tick.send_modify(|n| *n += 1);
    }
}

/// Requests each announced-but-missing avatar/comment once per hash.
fn fetch_missing_blobs(handle: &Handle, requested: &mut HashSet<(u32, Vec<u8>)>) {
    let (mut textures, mut comments) = (Vec::new(), Vec::new());
    handle.with_state(|s| {
        for u in s.users.values() {
            if let (None, Some(hash)) = (&u.texture, &u.texture_hash)
                && requested.insert((u.session, hash.clone()))
            {
                textures.push(u.session);
            }
            if let (None, Some(hash)) = (&u.comment, &u.comment_hash)
                && requested.insert((u.session, hash.clone()))
            {
                comments.push(u.session);
            }
        }
    });
    if (!textures.is_empty() || !comments.is_empty())
        && let Err(err) = handle.request_blobs(&textures, &comments, &[])
    {
        tracing::warn!(%err, "requesting avatars/comments failed");
    }
}
