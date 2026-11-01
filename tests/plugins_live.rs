//! Plugins module against the local 1.5 test server. Start the server first (`testenv/up.sh`),
//! then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test plugins_live -- --ignored --test-threads=1`
//!
//! Test bots play a Mumcord client (hello with `server-plugins`), an older Mumcord without the
//! feature and an official client (no hello at all). Only the first gets the announcement
//! (mumcord spec 06, FR-6.77, section 6.4; mumbot#20).

#![cfg(feature = "tester")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mumbot::plugins::PluginsConfig;
use mumbot::plugins::config::Announce;
use mumbot::protocol::Event;
use mumbot::voice_settings::VoiceSettings;
use mumbot::webhook::{ModulesConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, Module, UserCheck, env, modules};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::sync::broadcast;

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MUMBOT_LIVE=1 and start testenv/up.sh");
    }
    on
}

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() & 0xff_ffff)
        .unwrap_or(0);
    format!("{prefix}-{:06x}", std::process::id() ^ nanos)
}

/// SHA-256 of the test server's certificate (`testenv/gen-certs.sh`).
fn server_pin() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/testenv/certs/cert.pem");
    let der = CertificateDer::from_pem_file(path).expect("testenv/certs/cert.pem");
    ring::digest::digest(&ring::digest::SHA256, der.as_ref())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

fn plugins_config(dir: &Path, name: &str) -> WebhookConfig {
    WebhookConfig {
        modules: ModulesConfig {
            enabled: vec![Module::Plugins],
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
        peer: None,
        history: None,
        plugins: Some(PluginsConfig {
            announce: vec![Announce {
                id: "dice".to_owned(),
                min: "1.0.0".to_owned(),
                source: "official".to_owned(),
                key: None,
                purpose: "board game nights".to_owned(),
            }],
        }),
        voice: Default::default(),
        music: None,
    }
}

/// The module's bot user must be registered, as an admin would do once: the same name and
/// certificate (same data directory) self-register before the module starts.
async fn register_bot(dir: &Path, name: &str) {
    let bot = Bot::connect(client(dir, name)).await.expect("bot user");
    bot.register().await.expect("self-register");
    bot.disconnect().await.expect("bot user leaves");
}

fn hello(features: &str) -> Vec<u8> {
    format!(r#"{{"app":"mumcord","v":1,"features":[{features}]}}"#).into_bytes()
}

fn session_of(bot: &Bot, name: &str) -> u32 {
    bot.state()
        .users
        .values()
        .find(|u| u.name == name)
        .map(|u| u.session)
        .expect("user is online")
}

/// The next `mumcord.srvplugins` message from `from` within `wait`, as JSON.
async fn next_announcement(
    events: &mut broadcast::Receiver<Event>,
    from: u32,
    wait: Duration,
) -> Option<serde_json::Value> {
    tokio::time::timeout(wait, async {
        loop {
            if let Ok(Event::PluginData(data)) = events.recv().await
                && data.sender == Some(from)
                && data.data_id == "mumcord.srvplugins"
            {
                return serde_json::from_slice(&data.data).expect("json");
            }
        }
    })
    .await
    .ok()
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn only_clients_with_the_feature_get_the_announcement() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("pluginbot");
    register_bot(dir.path(), &bot_name).await;
    let (alice_name, old_name, official_name) =
        (unique("alice"), unique("oldmumcord"), unique("official"));
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    let old = Bot::connect(client(dir.path(), &old_name))
        .await
        .expect("old Mumcord");
    let official = Bot::connect(client(dir.path(), &official_name))
        .await
        .expect("official client");
    let mut events: Vec<_> = [&alice, &old, &official]
        .iter()
        .map(|bot| bot.subscribe())
        .collect();
    let running = tokio::spawn(modules::run(
        Arc::new(plugins_config(dir.path(), &bot_name)),
        VoiceSettings::default(),
    ));

    alice
        .wait_user(&bot_name, &UserCheck::default())
        .await
        .expect("the plugins bot logs in with the pinned server certificate");
    let bot = session_of(&alice, &bot_name);
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""p2p","server-plugins""#))
        .expect("hello");
    old.handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""p2p""#))
        .expect("hello");

    let wait = Duration::from_secs(5);
    assert_eq!(
        next_announcement(&mut events[0], bot, wait).await,
        Some(serde_json::json!({"v":1,"seq":1,"plugins":[
            {"id":"dice","min":"1.0.0","src":"official","for":"board game nights"}
        ]}))
    );
    assert_eq!(next_announcement(&mut events[1], bot, wait).await, None);
    assert_eq!(next_announcement(&mut events[2], bot, wait).await, None);

    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""server-plugins""#))
        .expect("hello again");
    assert_eq!(
        next_announcement(&mut events[0], bot, wait).await,
        None,
        "at most once a minute"
    );

    running.abort();
    for bot in [alice, old, official] {
        bot.disconnect().await.expect("leaves");
    }
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn a_changed_list_is_announced_with_a_higher_seq_after_a_restart() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let bot_name = unique("pluginbot");
    register_bot(dir.path(), &bot_name).await;
    let alice_name = unique("alice");
    let alice = Bot::connect(client(dir.path(), &alice_name))
        .await
        .expect("alice");
    let mut events = alice.subscribe();
    let mut config = plugins_config(dir.path(), &bot_name);

    for (round, seq) in [(0, 1), (1, 2)] {
        if round == 1 {
            config
                .plugins
                .as_mut()
                .expect("plugins")
                .announce
                .push(Announce {
                    id: "poll".to_owned(),
                    min: "0.2.0".to_owned(),
                    source: "official".to_owned(),
                    key: None,
                    purpose: "quick votes".to_owned(),
                });
        }
        let running = tokio::spawn(modules::run(
            Arc::new(config.clone()),
            VoiceSettings::default(),
        ));
        alice
            .wait_user(&bot_name, &UserCheck::default())
            .await
            .expect("bot joins");
        let bot = session_of(&alice, &bot_name);
        alice
            .handle()
            .send_plugin_data(&[bot], "mumcord.hello", hello(r#""server-plugins""#))
            .expect("hello");
        let announcement = next_announcement(&mut events, bot, Duration::from_secs(5))
            .await
            .expect("announced");
        assert_eq!(announcement["seq"], seq, "round {round}");
        assert_eq!(
            announcement["plugins"].as_array().map(Vec::len),
            Some(round + 1)
        );
        running.abort();
        alice
            .wait_user_gone(&bot_name)
            .await
            .expect("the bot leaves");
    }
    alice.disconnect().await.expect("alice leaves");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn an_unregistered_bot_refuses_to_start_the_module() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let config = Arc::new(plugins_config(dir.path(), &unique("pluginbot")));
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        modules::run(config, VoiceSettings::default()),
    )
    .await
    .expect("fails fast");
    let error = result.expect_err("refused");
    assert!(error.to_string().contains("registered"), "{error}");
}
