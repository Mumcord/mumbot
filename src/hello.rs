//! The bot's own Mumcord hello: tells Mumcord clients that this user is a bot, which one, and
//! which modules it runs, so the client can show it as a bot.
//!
//! Mumcord clients greet everyone on connect and everyone who joins later with plugin data
//! `mumcord.hello`. The bot answers each such hello with its own, to that client only:
//! `{"app":"mumcord","v":1,"features":[],"bot":{"name":"mumbot","version":…,"modules":[…]}}`.
//! The `bot` object is additive; clients that do not know it ignore unknown fields. The bot never
//! greets first, so two bots never answer each other. The modules are fixed for as long as the
//! process runs; a module change needs a restart, after which the clients greet the bot again
//! (it is a new joiner for them) and get the new hello that way. At most one hello per session
//! every 10 s; answers to several sessions go out together in one message per second, which
//! leaves the server's plugin data limit (4 per second) to the other modules.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::Result;
use mumcord_protocol::Event;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::modules::Module;
use crate::peer::wire::{HELLO_ID, is_hello};

/// The bot's name in the hello.
pub const BOT_NAME: &str = "mumbot";
/// Least time between two hellos to one session.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// How often waiting hellos go out, together in one message.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Serialize)]
struct Wire<'a> {
    app: &'a str,
    v: u32,
    features: [&'a str; 0],
    bot: BotInfo<'a>,
}

#[derive(Serialize)]
struct BotInfo<'a> {
    name: &'a str,
    version: &'a str,
    modules: Vec<Module>,
}

/// The hello of a bot running `modules`: sorted, without duplicates, as compact JSON.
pub fn encode(modules: &[Module]) -> Vec<u8> {
    let modules: BTreeSet<Module> = modules.iter().copied().collect();
    let wire = Wire {
        app: "mumcord",
        v: 1,
        features: [],
        bot: BotInfo {
            name: BOT_NAME,
            version: env!("CARGO_PKG_VERSION"),
            modules: modules.into_iter().collect(),
        },
    };
    serde_json::to_vec(&wire).expect("the hello serialises")
}

/// The narrow view of the bot the hello needs.
pub trait Link {
    /// Sends `message` as `mumcord.hello` to `receivers`.
    fn send_hello(
        &self,
        receivers: &[u32],
        message: &[u8],
    ) -> impl Future<Output = Result<()>> + Send;
}

/// Sessions that wait for the bot's hello and when each one last got it.
pub struct Greeter<L> {
    link: L,
    message: Vec<u8>,
    waiting: BTreeSet<u32>,
    sent: HashMap<u32, Instant>,
}

impl<L: Link> Greeter<L> {
    pub fn new(link: L, message: Vec<u8>) -> Self {
        Self {
            link,
            message,
            waiting: BTreeSet::new(),
            sent: HashMap::new(),
        }
    }

    /// Reacts to one event of the Mumble connection.
    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::PluginData(data) => {
                let Some(sender) = data.sender else { return };
                if data.data_id == HELLO_ID && is_hello(&data.data) {
                    self.waiting.insert(sender);
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

    /// Greets every waiting session whose last hello is at least [`MIN_INTERVAL`] old. A session
    /// greeting again sooner keeps waiting instead of being dropped, so it still gets the hello
    /// once the interval has passed.
    pub async fn flush(&mut self, now: Instant) {
        let due: Vec<u32> = self
            .waiting
            .iter()
            .copied()
            .filter(|session| {
                self.sent
                    .get(session)
                    .is_none_or(|at| now - *at >= MIN_INTERVAL)
            })
            .collect();
        if due.is_empty() {
            return;
        }
        for session in &due {
            self.waiting.remove(session);
        }
        match self.link.send_hello(&due, &self.message).await {
            Ok(()) => {
                tracing::debug!(clients = due.len(), "bot hello sent");
                for session in due {
                    self.sent.insert(session, now);
                }
            }
            // The clients greet again on their next hello; retrying here could loop on a server
            // that refuses plugin data.
            Err(error) => tracing::warn!(%error, "bot hello not sent"),
        }
    }
}

/// Answers Mumcord hellos with `message` until the bot's event stream ends.
pub async fn run<L: Link>(
    link: L,
    message: Vec<u8>,
    mut events: broadcast::Receiver<Event>,
) -> Result<()> {
    let mut greeter = Greeter::new(link, message);
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => greeter.on_event(event),
                // A missed hello only costs that client the bot's hello until its next one.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "bot hello fell behind the event stream");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = flush.tick() => greeter.flush(Instant::now()).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::bail;
    use bytes::Bytes;
    use mumcord_protocol::{DisconnectReason, PLUGIN_DATA_MAX, PluginData, User};

    use super::*;

    const ALL: [Module; 5] = [
        Module::Webhook,
        Module::Peer,
        Module::Music,
        Module::History,
        Module::Plugins,
    ];

    #[derive(Default)]
    struct MockLink {
        fail: Mutex<bool>,
        sent: Mutex<Vec<Vec<u32>>>,
    }

    impl MockLink {
        fn take_sent(&self) -> Vec<Vec<u32>> {
            std::mem::take(&mut *self.sent.lock().expect("lock"))
        }
    }

    impl Link for Arc<MockLink> {
        async fn send_hello(&self, receivers: &[u32], message: &[u8]) -> Result<()> {
            assert_eq!(message, b"hi");
            if *self.fail.lock().expect("lock") {
                bail!("refused");
            }
            self.sent.lock().expect("lock").push(receivers.to_vec());
            Ok(())
        }
    }

    fn plugin(sender: u32, id: &str, data: &[u8]) -> Event {
        Event::PluginData(PluginData {
            sender: Some(sender),
            data_id: id.into(),
            data: Bytes::copy_from_slice(data),
        })
    }

    fn hello(sender: u32) -> Event {
        plugin(
            sender,
            HELLO_ID,
            br#"{"app":"mumcord","v":1,"features":["p2p"]}"#,
        )
    }

    fn removed(session: u32) -> Event {
        Event::UserRemoved {
            user: User {
                session,
                ..User::default()
            },
            actor: None,
            reason: None,
            ban: false,
        }
    }

    fn setup() -> (Arc<MockLink>, Greeter<Arc<MockLink>>) {
        let link = Arc::new(MockLink::default());
        (Arc::clone(&link), Greeter::new(link, b"hi".to_vec()))
    }

    #[test]
    fn the_hello_names_the_bot_its_version_and_its_modules() {
        let json: serde_json::Value =
            serde_json::from_slice(&encode(&[Module::Music, Module::Peer, Module::Music]))
                .expect("json");
        assert_eq!(
            json,
            serde_json::json!({
                "app": "mumcord",
                "v": 1,
                "features": [],
                "bot": {
                    "name": "mumbot",
                    "version": env!("CARGO_PKG_VERSION"),
                    "modules": ["peer", "music"],
                },
            })
        );
    }

    #[test]
    fn the_hello_fits_the_limits_of_the_client() {
        let message = encode(&ALL);
        assert!(message.len() <= PLUGIN_DATA_MAX);
        assert!(!message.iter().any(u8::is_ascii_whitespace), "compact JSON");
        assert!(is_hello(&message), "a valid Mumcord hello");
        let json: serde_json::Value = serde_json::from_slice(&message).expect("json");
        let bot = &json["bot"];
        assert!(BOT_NAME.len() <= 32);
        let modules = bot["modules"].as_array().expect("modules");
        assert_eq!(modules.len(), ALL.len());
        assert!(modules.len() <= 16);
        assert!(
            modules
                .iter()
                .all(|m| m.as_str().is_some_and(|m| m.len() <= 32))
        );
    }

    #[tokio::test]
    async fn every_mumcord_hello_is_answered() {
        let (link, mut greeter) = setup();
        greeter.on_event(hello(5));
        greeter.on_event(plugin(6, HELLO_ID, br#"{"app":"mumcord","v":1}"#));
        greeter.on_event(plugin(7, "mumcord.peer", br#"{"app":"mumcord","v":1}"#));
        greeter.on_event(plugin(8, HELLO_ID, b"not json"));
        greeter.on_event(plugin(9, HELLO_ID, br#"{"app":"other","v":1}"#));
        greeter.flush(Instant::now()).await;
        assert_eq!(link.take_sent(), vec![vec![5, 6]]);
        greeter.flush(Instant::now()).await;
        assert!(link.take_sent().is_empty(), "nothing waits any more");
    }

    #[tokio::test]
    async fn waiting_clients_get_one_message_together() {
        let (link, mut greeter) = setup();
        for session in [9, 3, 5, 3] {
            greeter.on_event(hello(session));
        }
        greeter.flush(Instant::now()).await;
        assert_eq!(link.take_sent(), vec![vec![3, 5, 9]]);
    }

    #[tokio::test]
    async fn at_most_one_hello_per_client_every_ten_seconds() {
        let (link, mut greeter) = setup();
        let start = Instant::now();
        greeter.on_event(hello(5));
        greeter.flush(start).await;
        assert_eq!(link.take_sent(), vec![vec![5]]);

        greeter.on_event(hello(5));
        greeter.on_event(hello(6));
        greeter.flush(start + Duration::from_secs(3)).await;
        assert_eq!(link.take_sent(), vec![vec![6]], "5 has to wait");

        greeter
            .flush(start + MIN_INTERVAL - Duration::from_millis(1))
            .await;
        assert!(link.take_sent().is_empty());
        greeter.flush(start + MIN_INTERVAL).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "a repeated hello is answered once the interval has passed"
        );
    }

    #[tokio::test]
    async fn sessions_are_forgotten_when_they_leave_or_the_bot_disconnects() {
        let (link, mut greeter) = setup();
        let now = Instant::now();
        greeter.on_event(hello(5));
        greeter.on_event(hello(6));
        greeter.on_event(removed(6));
        greeter.flush(now).await;
        assert_eq!(link.take_sent(), vec![vec![5]], "a leaver gets nothing");

        greeter.on_event(removed(5));
        greeter.on_event(hello(5));
        greeter.flush(now).await;
        assert_eq!(link.take_sent(), vec![vec![5]], "a reused session is new");

        greeter.on_event(hello(7));
        greeter.on_event(Event::Disconnected(DisconnectReason::Closed));
        greeter.flush(now).await;
        assert!(
            link.take_sent().is_empty(),
            "nothing waits after a disconnect"
        );
        greeter.on_event(hello(5));
        greeter.flush(now).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "a new connection starts fresh"
        );
    }

    #[tokio::test]
    async fn a_failed_send_is_not_retried() {
        let (link, mut greeter) = setup();
        *link.fail.lock().expect("lock") = true;
        greeter.on_event(hello(5));
        greeter.flush(Instant::now()).await;
        *link.fail.lock().expect("lock") = false;
        greeter.flush(Instant::now()).await;
        assert!(link.take_sent().is_empty());
        greeter.on_event(hello(5));
        greeter.flush(Instant::now()).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "the next hello is answered"
        );
    }

    #[tokio::test]
    async fn runs_until_the_events_end() {
        let link = Arc::new(MockLink::default());
        let (sender, events) = broadcast::channel(4);
        let task = tokio::spawn(run(Arc::clone(&link), b"hi".to_vec(), events));
        sender.send(hello(5)).expect("send");
        drop(sender);
        task.await.expect("join").expect("run");
    }
}
