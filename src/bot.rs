//! A connected bot: commands plus waiting helpers over what it observes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use mumcord_protocol::{
    ConnectConfig, DisconnectReason, Event, Handle, ServerState, TextMessage, User, connect,
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
const BROADCAST_CAPACITY: usize = 1024;

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
            // Bots only talk to the local test servers, whose certificates are self-signed.
            verify_cert: Arc::new(|_| true),
            login_timeout: self.timeout,
        })
    }
}

/// What the bot has received since it connected.
#[derive(Default)]
struct Log {
    messages: Vec<TextMessage>,
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

    async fn connect_with(config: BotConfig, events: broadcast::Sender<Event>) -> Result<Self> {
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
        let pump = tokio::spawn(pump(conn.events, observed.clone(), events.clone()));
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
        self.handle.disconnect();
        // Let the server drop the old session before the same certificate logs in again.
        if tokio::time::timeout(self.config.timeout, &mut self.pump)
            .await
            .is_err()
        {
            tracing::warn!("old connection did not close in time");
        }
        let fresh = Self::connect_with(config, self.events.clone()).await?;
        let old = std::mem::replace(self, fresh);
        drop(old);
        Ok(())
    }

    /// Raw protocol handle.
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    /// All protocol events. Survives `reconnect_as`.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn config(&self) -> &BotConfig {
        &self.config
    }

    pub fn state(&self) -> ServerState {
        self.handle.state()
    }

    pub fn channel_id(&self, name: &str) -> Result<u32> {
        self.handle.with_state(|s| resolve_channel(s, name))
    }

    /// Moves into `channel` and waits until the server confirms it.
    pub async fn join(&self, channel: &str) -> Result<()> {
        let id = self.channel_id(channel)?;
        self.handle.join_channel(id)?;
        self.wait_own(&format!("joining {channel}"), |u| u.channel_id == id)
            .await
    }

    /// Waits until the user `name` exists and matches `check`; returns that user.
    pub async fn wait_user(&self, name: &str, check: &UserCheck) -> Result<User> {
        self.wait_until(&format!("user {name:?} with {check:?}"), |s, _| {
            find_user(s, name).filter(|u| check.matches(u, s)).cloned()
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

/// Records events for the waiters and forwards them to subscribers.
async fn pump(
    mut events: mpsc::UnboundedReceiver<Event>,
    observed: Arc<Observed>,
    subscribers: broadcast::Sender<Event>,
) {
    while let Some(event) = events.recv().await {
        {
            let mut log = observed.log();
            match &event {
                Event::TextMessage(m) => log.messages.push(m.clone()),
                Event::ChannelUpdated(c) => log.channel_updates.push(c.id),
                Event::Disconnected(reason) => log.disconnected = Some(reason.clone()),
                _ => {}
            }
        }
        // No subscribers is normal.
        let _ = subscribers.send(event);
        observed.tick.send_modify(|n| *n += 1);
    }
}
