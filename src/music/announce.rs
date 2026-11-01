//! Plugin announce of the music module: tells Mumcord clients that this bot speaks the official
//! Mumbot Music plugin, so the plugin can offer "Configure bot" on it. Mumcord spec
//! `specs/06-plugin-system-specification.md` (FR-6.35), task M2b.
//!
//! A Mumcord client whose hello lists the feature `plugins` gets plugin data `mumcord.plugins`
//! with the compact JSON `{"v":1,"p":["mumbot-music@1"]}`: the plugin id, its API major and no
//! source suffix, which means "official". Clients greet a joining bot and the bot sees every
//! joining client's hello, so both join orders are covered; the bot itself never sends a hello,
//! it only answers the hellos it receives. The announce never changes while the bot runs, so a
//! hello is the only trigger. At most one announce per
//! session every 10 s, the same rule as the client's own announce. Answers to several sessions
//! go out together in one message per second.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use anyhow::Result;
use mumcord_protocol::Event;
use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::peer::wire::{HELLO_ID, hello_lists};

/// Plugin data id of the announce.
pub const DATA_ID: &str = "mumcord.plugins";
/// The hello feature of Mumcord clients that run plugins.
pub const FEATURE: &str = "plugins";
/// The announce: the official `mumbot-music` plugin, API major 1.
pub const MESSAGE: &[u8] = br#"{"v":1,"p":["mumbot-music@1"]}"#;
/// Least time between two announces to one session.
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
/// How often waiting announces go out, together in one message.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The narrow view of the bot the announce needs.
pub trait Link {
    /// Sends [`MESSAGE`] as [`DATA_ID`] to `receivers`.
    fn announce(&self, receivers: &[u32]) -> impl Future<Output = Result<()>> + Send;
}

/// Sessions that wait for an announce and when each one last got it.
pub struct Announcer<L> {
    link: L,
    waiting: BTreeSet<u32>,
    sent: HashMap<u32, Instant>,
}

impl<L: Link> Announcer<L> {
    pub fn new(link: L) -> Self {
        Self {
            link,
            waiting: BTreeSet::new(),
            sent: HashMap::new(),
        }
    }

    /// Reacts to one event of the Mumble connection.
    pub fn on_event(&mut self, event: Event) {
        match event {
            Event::PluginData(data) => {
                let Some(sender) = data.sender else { return };
                if data.data_id == HELLO_ID && hello_lists(&data.data, FEATURE) {
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

    /// Announces to every waiting session whose last announce is at least [`MIN_INTERVAL`] old.
    /// A session greeting again sooner keeps waiting instead of being dropped, so it still gets
    /// the announce once the interval has passed.
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
        match self.link.announce(&due).await {
            Ok(()) => {
                tracing::debug!(clients = due.len(), "music: plugin announced");
                for session in due {
                    self.sent.insert(session, now);
                }
            }
            // The clients ask again on their next hello; retrying here could loop on a server
            // that refuses plugin data.
            Err(error) => tracing::warn!(%error, "music: plugin announce not sent"),
        }
    }
}

/// Runs the announce until the bot's event stream ends.
pub async fn run<L: Link>(link: L, mut events: broadcast::Receiver<Event>) -> Result<()> {
    let mut announcer = Announcer::new(link);
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => announcer.on_event(event),
                // A missed hello only costs that client this announce until its next hello.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "music: announce fell behind the event stream");
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            _ = flush.tick() => announcer.flush(Instant::now()).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::bail;
    use bytes::Bytes;
    use mumcord_protocol::{DisconnectReason, PluginData, User};

    use super::*;

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
        async fn announce(&self, receivers: &[u32]) -> Result<()> {
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
            br#"{"app":"mumcord","v":1,"features":["p2p","plugins"]}"#,
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

    fn setup() -> (Arc<MockLink>, Announcer<Arc<MockLink>>) {
        let link = Arc::new(MockLink::default());
        (Arc::clone(&link), Announcer::new(link))
    }

    #[test]
    fn the_message_is_the_official_mumbot_music_entry_of_the_announce_format() {
        let json: serde_json::Value = serde_json::from_slice(MESSAGE).expect("json");
        assert_eq!(json, serde_json::json!({"v": 1, "p": ["mumbot-music@1"]}));
        assert!(!MESSAGE.iter().any(u8::is_ascii_whitespace), "compact JSON");
    }

    #[tokio::test]
    async fn only_clients_with_the_plugins_feature_get_the_announce() {
        let (link, mut announcer) = setup();
        announcer.on_event(hello(5));
        announcer.on_event(plugin(
            6,
            HELLO_ID,
            br#"{"app":"mumcord","v":1,"features":["p2p","server-plugins"]}"#,
        ));
        announcer.on_event(plugin(
            7,
            DATA_ID,
            br#"{"app":"mumcord","v":1,"features":["plugins"]}"#,
        ));
        announcer.on_event(plugin(8, HELLO_ID, b"not json"));
        announcer.flush(Instant::now()).await;
        assert_eq!(link.take_sent(), vec![vec![5]]);
        announcer.flush(Instant::now()).await;
        assert!(link.take_sent().is_empty(), "nothing waits any more");
    }

    #[tokio::test]
    async fn waiting_clients_get_one_message_together() {
        let (link, mut announcer) = setup();
        for session in [9, 3, 5, 3] {
            announcer.on_event(hello(session));
        }
        announcer.flush(Instant::now()).await;
        assert_eq!(link.take_sent(), vec![vec![3, 5, 9]]);
    }

    #[tokio::test]
    async fn at_most_one_announce_per_client_every_ten_seconds() {
        let (link, mut announcer) = setup();
        let start = Instant::now();
        announcer.on_event(hello(5));
        announcer.flush(start).await;
        assert_eq!(link.take_sent(), vec![vec![5]]);

        announcer.on_event(hello(5));
        announcer.on_event(hello(6));
        announcer.flush(start + Duration::from_secs(3)).await;
        assert_eq!(link.take_sent(), vec![vec![6]], "5 has to wait");

        announcer
            .flush(start + MIN_INTERVAL - Duration::from_millis(1))
            .await;
        assert!(link.take_sent().is_empty());
        announcer.flush(start + MIN_INTERVAL).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "a repeated hello is answered once the interval has passed"
        );
    }

    #[tokio::test]
    async fn sessions_are_forgotten_when_they_leave_or_the_bot_disconnects() {
        let (link, mut announcer) = setup();
        let now = Instant::now();
        announcer.on_event(hello(5));
        announcer.on_event(hello(6));
        announcer.on_event(removed(6));
        announcer.flush(now).await;
        assert_eq!(link.take_sent(), vec![vec![5]], "a leaver gets nothing");

        announcer.on_event(removed(5));
        announcer.on_event(hello(5));
        announcer.flush(now).await;
        assert_eq!(link.take_sent(), vec![vec![5]], "a reused session is new");

        announcer.on_event(hello(7));
        announcer.on_event(Event::Disconnected(DisconnectReason::Closed));
        announcer.flush(now).await;
        assert!(
            link.take_sent().is_empty(),
            "nothing waits after a disconnect"
        );
        announcer.on_event(hello(5));
        announcer.flush(now).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "a new connection starts fresh"
        );
    }

    #[tokio::test]
    async fn a_failed_send_is_not_retried() {
        let (link, mut announcer) = setup();
        *link.fail.lock().expect("lock") = true;
        announcer.on_event(hello(5));
        announcer.flush(Instant::now()).await;
        *link.fail.lock().expect("lock") = false;
        announcer.flush(Instant::now()).await;
        assert!(link.take_sent().is_empty());
        announcer.on_event(hello(5));
        announcer.flush(Instant::now()).await;
        assert_eq!(
            link.take_sent(),
            vec![vec![5]],
            "the next hello is answered"
        );
    }

    #[tokio::test]
    async fn runs_until_the_events_end() {
        let (link, announcer) = setup();
        drop(announcer);
        let (sender, events) = broadcast::channel(4);
        let task = tokio::spawn(run(Arc::clone(&link), events));
        sender.send(hello(5)).expect("send");
        drop(sender);
        task.await.expect("join").expect("run");
    }
}
