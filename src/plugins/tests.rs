use std::sync::{Arc, Mutex};

use anyhow::bail;
use bytes::Bytes;
use mumcord_protocol::{DisconnectReason, PluginData, User};

use super::*;

const MESSAGE: &[u8] = br#"{"v":1,"seq":1,"plugins":[]}"#;

#[derive(Default)]
struct MockLink {
    registered: Mutex<Option<bool>>,
    fail: Mutex<bool>,
    sent: Mutex<Vec<Vec<u32>>>,
}

impl MockLink {
    fn take_sent(&self) -> Vec<Vec<u32>> {
        std::mem::take(&mut *self.sent.lock().expect("lock"))
    }
}

impl Link for Arc<MockLink> {
    async fn registered(&self) -> Option<bool> {
        *self.registered.lock().expect("lock")
    }

    async fn send(&self, receivers: &[u32], data: Vec<u8>) -> Result<()> {
        if *self.fail.lock().expect("lock") {
            bail!("refused");
        }
        assert_eq!(data, MESSAGE);
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
        br#"{"app":"mumcord","v":1,"features":["p2p","server-plugins"]}"#,
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

fn setup() -> (Arc<MockLink>, Plugins<Arc<MockLink>>) {
    let link = Arc::new(MockLink::default());
    let plugins = Plugins::new(Arc::clone(&link), MESSAGE.to_vec());
    (link, plugins)
}

#[tokio::test]
async fn only_clients_with_the_feature_get_the_list() {
    let (link, mut plugins) = setup();
    let now = Instant::now();
    plugins.on_event(hello(5), now);
    plugins.on_event(
        plugin(
            6,
            HELLO_ID,
            br#"{"app":"mumcord","v":1,"features":["p2p"]}"#,
        ),
        now,
    );
    plugins.on_event(
        plugin(
            7,
            "mumcord.peer",
            br#"{"app":"mumcord","v":1,"features":["server-plugins"]}"#,
        ),
        now,
    );
    plugins.on_event(plugin(8, HELLO_ID, b"not json"), now);
    plugins.flush(now).await;
    assert_eq!(link.take_sent(), vec![vec![5]]);
    plugins.flush(now).await;
    assert!(link.take_sent().is_empty(), "nothing waits any more");
}

#[tokio::test]
async fn waiting_clients_get_one_message_together() {
    let (link, mut plugins) = setup();
    let now = Instant::now();
    for session in [9, 3, 5, 3] {
        plugins.on_event(hello(session), now);
    }
    plugins.flush(now).await;
    assert_eq!(link.take_sent(), vec![vec![3, 5, 9]]);
}

#[tokio::test]
async fn one_announcement_per_client_and_minute() {
    let (link, mut plugins) = setup();
    let start = Instant::now();
    plugins.on_event(hello(5), start);
    plugins.flush(start).await;
    assert_eq!(link.take_sent().len(), 1);

    let soon = start + Duration::from_secs(59);
    plugins.on_event(hello(5), soon);
    plugins.flush(soon).await;
    assert!(
        link.take_sent().is_empty(),
        "a repeated hello within a minute"
    );

    let later = start + RESEND_AFTER;
    plugins.on_event(hello(5), later);
    plugins.flush(later).await;
    assert_eq!(link.take_sent(), vec![vec![5]], "after a minute again");
}

#[tokio::test]
async fn sessions_are_forgotten_when_they_leave_or_the_bot_disconnects() {
    let (link, mut plugins) = setup();
    let now = Instant::now();
    plugins.on_event(hello(5), now);
    plugins.on_event(hello(6), now);
    plugins.on_event(removed(6), now);
    plugins.flush(now).await;
    assert_eq!(link.take_sent(), vec![vec![5]], "a leaver gets nothing");

    plugins.on_event(removed(5), now);
    plugins.on_event(hello(5), now);
    plugins.flush(now).await;
    assert_eq!(link.take_sent(), vec![vec![5]], "a reused session is new");

    plugins.on_event(hello(7), now);
    plugins.on_event(Event::Disconnected(DisconnectReason::Closed), now);
    plugins.flush(now).await;
    assert!(
        link.take_sent().is_empty(),
        "nothing waits across a reconnect"
    );
    plugins.on_event(hello(5), now);
    plugins.flush(now).await;
    assert_eq!(
        link.take_sent(),
        vec![vec![5]],
        "after a reconnect everyone is new"
    );
}

#[tokio::test]
async fn a_failed_send_is_not_retried() {
    let (link, mut plugins) = setup();
    let now = Instant::now();
    *link.fail.lock().expect("lock") = true;
    plugins.on_event(hello(5), now);
    plugins.flush(now).await;
    *link.fail.lock().expect("lock") = false;
    plugins.flush(now).await;
    assert!(link.take_sent().is_empty());
    plugins.on_event(hello(5), now);
    plugins.flush(now).await;
    assert_eq!(
        link.take_sent(),
        vec![vec![5]],
        "the next hello is answered"
    );
}

#[tokio::test]
async fn an_unregistered_bot_refuses_to_run() {
    let link = Arc::new(MockLink::default());
    *link.registered.lock().expect("lock") = Some(false);
    let (_sender, events) = broadcast::channel(4);
    let error = run(Arc::clone(&link), MESSAGE.to_vec(), events)
        .await
        .expect_err("refused");
    assert!(error.to_string().contains("registered"), "{error}");
}

#[tokio::test]
async fn a_registered_bot_runs_until_the_events_end() {
    let link = Arc::new(MockLink::default());
    *link.registered.lock().expect("lock") = Some(true);
    let (sender, events) = broadcast::channel(4);
    drop(sender);
    run(link, MESSAGE.to_vec(), events)
        .await
        .expect("ends cleanly");
}
