//! The configured avatar, against the local 1.5 test server. Start it first (`testenv/up.sh`),
//! then: `MUMBOT_LIVE=1 cargo test --features tester --test avatar_live -- --ignored --test-threads=1`
//!
//! Mumcord issue #460: the bot sends the `[bot]` image as its standard Mumble user texture, so an
//! ordinary client sees it with nothing on the server side; the setting is read once at startup; a
//! problem is reported as a warning and the bot runs without an avatar; a reconnect sets it again.

#![cfg(feature = "tester")]

use std::path::Path;
use std::sync::Arc;

use mumbot::avatar::{self, AvatarConfig, DEFAULT_LIMIT};
use mumbot::samples::{PNG, base64};
use mumbot::webhook::{BotApi, HttpConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, UserCheck, env};

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MUMBOT_LIVE=1 and start testenv/up.sh");
    }
    on
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{:06x}", std::process::id() ^ rand_suffix())
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() & 0xff_ffff)
        .unwrap_or(0)
}

fn config(dir: &Path, name: &str, bot: AvatarConfig) -> Arc<WebhookConfig> {
    Arc::new(WebhookConfig {
        modules: Default::default(),
        bot,
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            cert_name: None,
            channel: Some("Lobby".to_owned()),
            password: None,
            data_dir: Some(dir.to_owned()),
            superuser: false,
            server_cert_sha256: None,
        },
        http: HttpConfig {
            listen: "127.0.0.1:0".to_owned(),
            rate_limit_per_minute: 20,
            delivery_timeout_secs: 3,
        },
        keys: Vec::new(),
        tokens: Vec::new(),
        peer: None,
        music: None,
        history: None,
        plugins: None,
        voice: Default::default(),
    })
}

fn observer(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn the_configured_avatar_shows_to_another_client_and_survives_a_reconnect() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (bot_name, observer_name) = (unique("avatar-bot"), unique("avatar-observer"));
    let settings = AvatarConfig {
        avatar_url: None,
        avatar_base64: Some(base64(PNG)),
    };
    let avatar = avatar::prepare(&settings)
        .await
        .expect("base64 image")
        .expect("an image");
    let api = Arc::new(BotApi::new(
        config(dir.path(), &bot_name, settings),
        Some(avatar.clone()),
    ));
    api.connect().await.expect("bot connects");

    let observer = Bot::connect(observer(dir.path(), &observer_name))
        .await
        .expect("observer");
    observer.join("Lobby").await.expect("observer joins");
    let seen = observer
        .wait_user(
            &bot_name,
            &UserCheck {
                has_avatar: Some(true),
                avatar: Some(avatar.encode(DEFAULT_LIMIT).expect("fits")),
                ..Default::default()
            },
        )
        .await
        .expect("the other client sees the avatar");
    assert_eq!(seen.name, bot_name);

    // After a reconnect the bot is a new session, and `settle` sends the avatar again.
    api.reconnect().await.expect("reconnect");
    observer
        .wait_user(
            &bot_name,
            &UserCheck {
                has_avatar: Some(true),
                avatar: Some(avatar.encode(DEFAULT_LIMIT).expect("fits")),
                ..Default::default()
            },
        )
        .await
        .expect("the avatar is set again after the reconnect");
    observer.disconnect().await.expect("observer closes");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn a_bad_avatar_setting_is_reported_and_the_bot_runs_without_it() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (bot_name, observer_name) = (unique("avatar-bad"), unique("avatar-bad-observer"));
    let settings = AvatarConfig {
        avatar_url: None,
        avatar_base64: Some("not base64!".to_owned()),
    };
    assert!(
        avatar::prepare(&settings).await.is_err(),
        "the problem is reported"
    );
    assert!(
        AvatarConfig {
            avatar_url: Some("https://example.org/a.png".to_owned()),
            avatar_base64: Some(base64(PNG)),
        }
        .validate()
        .is_err(),
        "setting both is a config error"
    );

    let api = Arc::new(BotApi::new(
        config(dir.path(), &bot_name, AvatarConfig::default()),
        None,
    ));
    api.connect()
        .await
        .expect("the bot connects without an avatar");
    let observer = Bot::connect(observer(dir.path(), &observer_name))
        .await
        .expect("observer");
    observer.join("Lobby").await.expect("observer joins");
    observer
        .wait_user(
            &bot_name,
            &UserCheck {
                has_avatar: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("the other client sees no avatar");
    observer.disconnect().await.expect("observer closes");
}
