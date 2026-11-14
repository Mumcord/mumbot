//! Broadcast admission (Mumcord spec 07, FR-7.40 to FR-7.50) against a real mumpeer relay and its
//! Mumble server from the mumpeer `testenv/`. The peer bot runs in this process; the testenv's
//! own relay bot stays stopped. In a mumpeer checkout:
//!
//! ```sh
//! MUMPEER_PROFILE=all testenv/up.sh --no-bot      # or MUMPEER_PROFILE=no-broadcast
//! ```
//!
//! then here:
//!
//! ```sh
//! MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1 MUMBOT_TESTENV_DIR=<mumpeer>/testenv MUMBOT_PORT_BASE=44758 \
//!   cargo test --features tester --test peer_broadcast_live -- --ignored --test-threads=1
//! ```
//!
//! For cameras (Mumcord spec 08) run it once more with `MUMPEER_PROFILE=no-camera`.
//!
//! Each test checks the relay's status and skips itself when the running profile is not its own,
//! so both profiles are covered by running the suite once per profile. Three test bots play
//! Mumcord clients (hello with `p2p` and `screen-share`); the test also calls the relay API
//! directly to see what the bot changed there.

#![cfg(feature = "tester")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mumble_client_protocol::proto;
use mumbot::peer::{BroadcastError, HttpRelay, PeerConfig, RelayApi, Secret};
use mumbot::protocol::Event;
use mumbot::voice_settings::VoiceSettings;
use mumbot::webhook::{ModulesConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, Module, UserCheck, env, modules};
use mumcord_p2p::BroadcastId;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::sync::broadcast;

const RID: &str = "0123456789abcdef";
/// A second channel, created on first use and kept: the test bot has no call to remove channels,
/// and an empty channel does no harm on a test server.
const ROOM: &str = "broadcast-elsewhere";

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1")
        && std::env::var("MUMBOT_PEER_RELAY").is_ok_and(|v| v == "1");
    if !on {
        eprintln!(
            "skipped: set MUMBOT_LIVE=1, MUMBOT_PEER_RELAY=1, MUMBOT_TESTENV_DIR and \
             MUMBOT_PORT_BASE, and start the mumpeer testenv/up.sh --no-bot"
        );
    }
    on
}

fn testenv() -> PathBuf {
    PathBuf::from(std::env::var_os("MUMBOT_TESTENV_DIR").expect("MUMBOT_TESTENV_DIR"))
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() & 0xff_ffff)
        .unwrap_or(0);
    format!("{prefix}-{:06x}", std::process::id() ^ nanos)
}

/// SHA-256 of the mumpeer test server's certificate.
fn server_pin() -> String {
    let path = testenv().join("certs/cert.pem");
    let der = CertificateDer::from_pem_file(path).expect("<mumpeer>/testenv/certs/cert.pem");
    ring::digest::digest(&ring::digest::SHA256, der.as_ref())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn api_url() -> String {
    format!("http://127.0.0.1:{}", env::test_port() + 2)
}

fn api_secret() -> String {
    env::testenv_var("MUMPEER_API_SECRET").expect("MUMPEER_API_SECRET in <mumpeer>/testenv/.env")
}

/// The relay API as the test calls it, next to the bot.
fn relay() -> HttpRelay {
    HttpRelay::new(&PeerConfig::new(api_url()), Secret::new(api_secret())).expect("relay API")
}

fn client(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

fn peer_config(dir: &Path, name: &str) -> Arc<WebhookConfig> {
    peer_config_with(
        dir,
        name,
        PeerConfig::new(api_url()).max_cameras_per_channel,
    )
}

fn peer_config_with(dir: &Path, name: &str, max_cameras_per_channel: usize) -> Arc<WebhookConfig> {
    let secret_file = dir.join("relay.secret");
    std::fs::write(&secret_file, format!("{}\n", api_secret())).expect("secret");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret_file, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
    }
    Arc::new(WebhookConfig {
        modules: ModulesConfig {
            enabled: vec![Module::Peer],
        },
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            data_dir: Some(dir.to_owned()),
            server_cert_sha256: Some(server_pin()),
            ..ServerConfig::default()
        },
        http: Default::default(),
        keys: Vec::new(),
        tokens: Vec::new(),
        peer: Some(PeerConfig {
            api_secret_file: Some(secret_file),
            max_cameras_per_channel,
            ..PeerConfig::new(api_url())
        }),
        history: None,
        plugins: None,
        voice: Default::default(),
        music: None,
    })
}

/// Whether the running relay offers broadcasts (profile `all`) or not (`no-broadcast`).
async fn relay_broadcasts() -> bool {
    let status = relay().status().await.expect("mumpeer testenv is up");
    status.broadcast.is_some()
}

/// Registers the announcer's user, which the modules bot then uses with the same data dir and
/// name: Mumcord clients only accept a relay announcement from a registered user. `observer`
/// waits until it forgot the announcer's session, so a later `wait_user` cannot match the
/// announcer instead of the modules bot.
async fn register_announcer(dir: &Path, name: &str, observer: &Bot) {
    let bot = Bot::connect(client(dir, name)).await.expect("announcer");
    bot.register().await.expect("announcer registers");
    bot.disconnect().await.expect("announcer leaves");
    observer
        .wait_user_gone(name)
        .await
        .expect("the announcer left");
}

/// A Mumcord client played by a test bot.
struct Client {
    bot: Bot,
    events: broadcast::Receiver<Event>,
}

impl Client {
    async fn connect(dir: &Path, prefix: &str) -> Self {
        let name = unique(prefix);
        let bot = Bot::connect(client(dir, &name)).await.expect("connect");
        bot.register().await.expect("self-register");
        let events = bot.subscribe();
        Self { bot, events }
    }

    fn session(&self) -> u32 {
        self.bot.own_user().expect("own user").session
    }

    fn cert(&self) -> String {
        self.bot
            .own_user()
            .expect("own user")
            .cert_hash
            .expect("test bots have a certificate")
    }

    fn send(&self, to: u32, id: &str, data: String) {
        self.bot
            .handle()
            .send_plugin_data(&[to], id, data.into_bytes())
            .expect("plugin data");
    }

    /// Hello with `p2p` and `screen-share`, answered by the relay announcement.
    async fn greet(&mut self, bot: u32) -> serde_json::Value {
        self.greet_with(bot, r#""p2p","screen-share""#).await
    }

    /// Hello with the given features (a JSON list without brackets).
    async fn greet_with(&mut self, bot: u32, features: &str) -> serde_json::Value {
        let hello = format!(r#"{{"app":"mumcord","v":1,"features":[{features}]}}"#);
        self.send(bot, "mumcord.hello", hello);
        let relay = self.next(bot).await;
        assert_eq!(relay["t"], "relay");
        relay
    }

    /// The next `mumcord.peer` message from `from`.
    async fn next(&mut self, from: u32) -> serde_json::Value {
        let events = &mut self.events;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(Event::PluginData(data)) = events.recv().await
                    && data.sender == Some(from)
                    && data.data_id == "mumcord.peer"
                {
                    return serde_json::from_slice(&data.data).expect("json");
                }
            }
        })
        .await
        .expect("peer message within 10 s")
    }
}

fn cam_req(sharer: &Client, bot: u32) {
    let message = format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":25,"k":"cam"}}"#);
    sharer.send(bot, "mumcord.peer", message);
}

fn bcast_req(sharer: &Client, bot: u32, max: u16) {
    let message = format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":{max}}}"#);
    sharer.send(bot, "mumcord.peer", message);
}

fn admit(sharer: &Client, bot: u32, id: &str, viewer: u32) {
    let message =
        format!(r#"{{"v":1,"t":"bcast-admit","rid":"{RID}","id":"{id}","viewer":{viewer}}}"#);
    sharer.send(bot, "mumcord.peer", message);
}

fn kick(sharer: &Client, bot: u32, id: &str, viewer: u32) {
    let message = format!(r#"{{"v":1,"t":"bcast-kick","id":"{id}","viewer":{viewer}}}"#);
    sharer.send(bot, "mumcord.peer", message);
}

fn broadcast_id(text: &str) -> BroadcastId {
    BroadcastId(u64::from_str_radix(text, 16).expect("16 hex"))
}

/// Retries a direct admission of `viewer` until it gives the expected kind of answer; the bot
/// works on events in its own task, so its relay calls land a moment later.
async fn wait_admit(
    id: BroadcastId,
    viewer: &str,
    expect: fn(&Result<(), &BroadcastError>) -> bool,
) {
    for _ in 0..50 {
        let answer = relay().admit_viewer(id, viewer).await;
        if expect(&answer.as_ref().map(|_| ())) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the relay never gave the expected admission answer");
}

async fn create_room(admin: &Bot, room: &str) {
    if admin.channel_id(room).is_ok() {
        return;
    }
    admin
        .handle()
        .send_channel_state(proto::ChannelState {
            parent: Some(0),
            name: Some(room.to_owned()),
            temporary: Some(false),
            ..Default::default()
        })
        .expect("create channel");
    for _ in 0..100 {
        if admin.channel_id(room).is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("channel {room:?} was not created");
}

#[tokio::test]
#[ignore = "needs the mumpeer testenv and MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1"]
async fn the_sharer_admits_and_removes_viewers_of_its_broadcast() {
    if !live() {
        return;
    }
    if !relay_broadcasts().await {
        eprintln!("skipped: the relay runs without broadcasts (profile no-broadcast)");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("peerbot");
    let room = ROOM.to_owned();
    let admin = Bot::connect(BotConfig::superuser(env::test_port()))
        .await
        .expect("SuperUser (SUPERUSER_PASSWORD in <mumpeer>/testenv/.env)");
    create_room(&admin, &room).await;
    let mut alice = Client::connect(dir.path(), "alice").await;
    let mut bob = Client::connect(dir.path(), "bob").await;
    let mut carol = Client::connect(dir.path(), "carol").await;
    register_announcer(dir.path(), &bot_name, &alice.bot).await;
    let running = tokio::spawn(modules::run(
        peer_config(dir.path(), &bot_name),
        VoiceSettings::default(),
    ));
    let bot = alice
        .bot
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in")
        .session;
    for client in [&mut alice, &mut bob, &mut carol] {
        let relay = client.greet(bot).await;
        assert_eq!(relay["bc"], 25, "the relay's viewer limit is announced");
    }

    // A broadcast for two viewers.
    bcast_req(&alice, bot, 2);
    let created = alice.next(bot).await;
    assert_eq!(created["t"], "bcast", "{created}");
    assert_eq!(created["rid"], RID);
    let id_text = created["id"].as_str().expect("id").to_owned();
    let id = broadcast_id(&id_text);

    // Carol is in another channel.
    carol.bot.join(&room).await.expect("carol moves away");
    admit(&alice, bot, &id_text, carol.session());
    assert_eq!(
        alice.next(bot).await,
        serde_json::json!({"v":1,"t":"bcast-err","rid":RID,"r":"not-same-channel"})
    );

    // Bob is admitted and gets the token; the relay counts him as a viewer.
    admit(&alice, bot, &id_text, bob.session());
    let token = bob.next(bot).await;
    assert_eq!(token["t"], "bcast-tok", "{token}");
    assert_eq!(token["id"], id_text);
    assert_eq!(token["sharer"], alice.session());
    let filler = "f".repeat(40);
    relay()
        .admit_viewer(id, &filler)
        .await
        .expect("second seat");
    let root = admin.state().channels.get(&0).expect("root").name.clone();
    carol.bot.join(&root).await.expect("carol comes back");
    admit(&alice, bot, &id_text, carol.session());
    assert_eq!(
        alice.next(bot).await,
        serde_json::json!({"v":1,"t":"bcast-err","rid":RID,"r":"full"})
    );

    // Kicking Bob frees his seat at the relay, so Carol fits.
    kick(&alice, bot, &id_text, bob.session());
    tokio::time::sleep(Duration::from_millis(500)).await;
    admit(&alice, bot, &id_text, carol.session());
    assert_eq!(carol.next(bot).await["t"], "bcast-tok");

    // Carol moves out of the sharer's channel: the bot removes her, her seat is free again.
    carol.bot.join(&room).await.expect("carol moves away");
    let probe = "e".repeat(40);
    wait_admit(id, &probe, |answer| answer.is_ok()).await;

    // The sharer leaves: the broadcast ends at the relay.
    let alice_cert = alice.cert();
    alice.bot.disconnect().await.expect("alice leaves");
    wait_admit(id, &filler, |answer| {
        matches!(answer, Err(BroadcastError::Gone))
    })
    .await;
    relay().remove_peer(&alice_cert).await.expect("clean up");

    running.abort();
    bob.bot.disconnect().await.expect("bob leaves");
    carol.bot.disconnect().await.expect("carol leaves");
    admin.disconnect().await.expect("admin leaves");
}

#[tokio::test]
#[ignore = "needs the mumpeer testenv and MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1"]
async fn without_broadcasts_at_the_relay_the_sharer_gets_relay_full() {
    if !live() {
        return;
    }
    if relay_broadcasts().await {
        eprintln!("skipped: the relay offers broadcasts (profile all)");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("peerbot");
    let mut alice = Client::connect(dir.path(), "alice").await;
    register_announcer(dir.path(), &bot_name, &alice.bot).await;
    let running = tokio::spawn(modules::run(
        peer_config(dir.path(), &bot_name),
        VoiceSettings::default(),
    ));
    let bot = alice
        .bot
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in")
        .session;
    let relay = alice.greet(bot).await;
    assert!(
        relay.get("bc").is_none(),
        "no broadcasts announced: {relay}"
    );
    bcast_req(&alice, bot, 25);
    assert_eq!(
        alice.next(bot).await,
        serde_json::json!({"v":1,"t":"bcast-err","rid":RID,"r":"relay-full"})
    );
    running.abort();
    alice.bot.disconnect().await.expect("alice leaves");
}

/// Whether the running relay allows camera broadcasts (profile `all`, not `no-camera`).
async fn relay_cameras() -> Option<bool> {
    let status = relay().status().await.expect("mumpeer testenv is up");
    status.broadcast?;
    Some(
        status
            .uses
            .is_none_or(|uses| uses.iter().any(|u| u == "camera")),
    )
}

#[tokio::test]
#[ignore = "needs the mumpeer testenv and MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1"]
async fn a_camera_runs_beside_a_screen_share_and_ends_on_a_channel_change() {
    if !live() {
        return;
    }
    if relay_cameras().await != Some(true) {
        eprintln!("skipped: the relay runs without camera broadcasts");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("peerbot");
    let room = ROOM.to_owned();
    let admin = Bot::connect(BotConfig::superuser(env::test_port()))
        .await
        .expect("SuperUser (SUPERUSER_PASSWORD in <mumpeer>/testenv/.env)");
    create_room(&admin, &room).await;
    let mut alice = Client::connect(dir.path(), "alice").await;
    let mut bob = Client::connect(dir.path(), "bob").await;
    register_announcer(dir.path(), &bot_name, &alice.bot).await;
    let running = tokio::spawn(modules::run(
        peer_config_with(dir.path(), &bot_name, 1),
        VoiceSettings::default(),
    ));
    let bot = alice
        .bot
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in")
        .session;
    let features = r#""p2p","screen-share","camera""#;
    for client in [&mut alice, &mut bob] {
        let relay = client.greet_with(bot, features).await;
        let uses = relay["u"].as_array().cloned().unwrap_or_default();
        assert!(
            relay.get("u").is_none() || uses.contains(&"cam".into()),
            "cameras are announced: {relay}"
        );
    }

    // Alice shares her screen and her camera at the same time.
    bcast_req(&alice, bot, 25);
    let screen = alice.next(bot).await;
    assert_eq!(screen["t"], "bcast", "{screen}");
    cam_req(&alice, bot);
    let camera = alice.next(bot).await;
    assert_eq!(camera["t"], "bcast", "{camera}");
    assert_ne!(screen["id"], camera["id"]);
    let screen_id = broadcast_id(screen["id"].as_str().expect("id"));
    let camera_text = camera["id"].as_str().expect("id").to_owned();
    let camera_id = broadcast_id(&camera_text);

    // The room holds one camera in this test, so Bob's is refused before the relay is asked.
    cam_req(&bob, bot);
    assert_eq!(
        bob.next(bot).await,
        serde_json::json!({"v":1,"t":"bcast-err","rid":RID,"r":"room-full"})
    );

    // Bob watches the camera.
    admit(&alice, bot, &camera_text, bob.session());
    let camera_token = bob.next(bot).await;
    assert_eq!(camera_token["t"], "bcast-tok");
    assert_eq!(camera_token["k"], "cam");

    // Alice changes channel: her camera ends at the relay, her screen share keeps running.
    alice.bot.join(&room).await.expect("alice moves away");
    let probe = "d".repeat(40);
    wait_admit(camera_id, &probe, |answer| {
        matches!(answer, Err(BroadcastError::Gone))
    })
    .await;
    relay()
        .admit_viewer(screen_id, &probe)
        .await
        .expect("the screen share still runs");

    // The room is free again for Bob.
    cam_req(&bob, bot);
    assert_eq!(bob.next(bot).await["t"], "bcast");

    let certs = [alice.cert(), bob.cert(), probe];
    running.abort();
    alice.bot.disconnect().await.expect("alice leaves");
    bob.bot.disconnect().await.expect("bob leaves");
    admin.disconnect().await.expect("admin leaves");
    for cert in certs {
        relay().remove_peer(&cert).await.expect("clean up");
    }
}

#[tokio::test]
#[ignore = "needs the mumpeer testenv and MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1"]
async fn a_relay_without_cameras_answers_use_not_allowed() {
    if !live() {
        return;
    }
    if relay_cameras().await != Some(false) {
        eprintln!("skipped: the relay allows camera broadcasts or none at all");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("peerbot");
    let mut alice = Client::connect(dir.path(), "alice").await;
    register_announcer(dir.path(), &bot_name, &alice.bot).await;
    let running = tokio::spawn(modules::run(
        peer_config(dir.path(), &bot_name),
        VoiceSettings::default(),
    ));
    let bot = alice
        .bot
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in")
        .session;
    let relay = alice
        .greet_with(bot, r#""p2p","screen-share","camera""#)
        .await;
    let uses = relay["u"].as_array().cloned().unwrap_or_default();
    assert!(!uses.contains(&"cam".into()), "{relay}");
    cam_req(&alice, bot);
    assert_eq!(
        alice.next(bot).await,
        serde_json::json!({"v":1,"t":"bcast-err","rid":RID,"r":"use-not-allowed"})
    );
    running.abort();
    alice.bot.disconnect().await.expect("alice leaves");
}

fn plg_req(sharer: &Client, bot: u32) {
    let message = format!(r#"{{"v":1,"t":"bcast-req","rid":"{RID}","max":25,"k":"plg"}}"#);
    sharer.send(bot, "mumcord.peer", message);
}

#[tokio::test]
#[ignore = "needs the mumpeer testenv and MUMBOT_LIVE=1 MUMBOT_PEER_RELAY=1"]
async fn a_viewer_gets_the_tokens_of_a_screen_share_and_plugin_data_tagged_by_kind() {
    if !live() {
        return;
    }
    let status = relay().status().await.expect("mumpeer testenv is up");
    let plugin_data = status.broadcast.is_some()
        && status
            .uses
            .is_none_or(|uses| uses.iter().any(|u| u == "plugin_data"));
    if !plugin_data {
        eprintln!("skipped: the relay runs without plugin data broadcasts");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("peerbot");
    let mut alice = Client::connect(dir.path(), "alice").await;
    let mut bob = Client::connect(dir.path(), "bob").await;
    register_announcer(dir.path(), &bot_name, &alice.bot).await;
    let running = tokio::spawn(modules::run(
        peer_config(dir.path(), &bot_name),
        VoiceSettings::default(),
    ));
    let bot = alice
        .bot
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the peer bot logs in")
        .session;
    for client in [&mut alice, &mut bob] {
        client
            .greet_with(bot, r#""p2p","screen-share","plugins""#)
            .await;
    }

    // One sender runs both broadcasts at the same time.
    bcast_req(&alice, bot, 25);
    let screen = alice.next(bot).await;
    assert_eq!(screen["t"], "bcast", "{screen}");
    plg_req(&alice, bot);
    let plugin = alice.next(bot).await;
    assert_eq!(plugin["t"], "bcast", "{plugin}");
    assert_ne!(screen["id"], plugin["id"]);

    for (broadcast, kind) in [(&screen, "scr"), (&plugin, "plg")] {
        let id = broadcast["id"].as_str().expect("id");
        admit(&alice, bot, id, bob.session());
        let token = bob.next(bot).await;
        assert_eq!(token["t"], "bcast-tok", "{token}");
        assert_eq!(token["id"], id);
        assert_eq!(token["k"], kind, "{token}");
    }

    let certs = [alice.cert(), bob.cert()];
    running.abort();
    alice.bot.disconnect().await.expect("alice leaves");
    bob.bot.disconnect().await.expect("bob leaves");
    for cert in certs {
        relay().remove_peer(&cert).await.expect("clean up");
    }
}
