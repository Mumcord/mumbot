//! Bot harness against the local 1.5 test server. Start it first (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --test live -- --ignored --test-threads=1`

use std::path::Path;

use mumbot::samples::{PNG, Sample};
use mumbot::{Bot, BotConfig, MessageCheck, UserCheck, env};
use mumcord_protocol::{DenyKind, permissions};

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

/// Bots with throw-away certificates in `dir`, so runs never share identities.
fn config(dir: &Path, name: &str) -> BotConfig {
    BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    }
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn messages_state_avatar_and_comment_reach_the_other_bot() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (a_name, b_name) = (unique("alpha"), unique("bravo"));
    let a = Bot::connect(config(dir.path(), &a_name)).await.expect("a");
    let b = Bot::connect(config(dir.path(), &b_name)).await.expect("b");
    a.join("Lobby").await.expect("a joins");
    b.join("Lobby").await.expect("b joins");
    b.wait_user(
        &a_name,
        &UserCheck {
            in_channel: Some("Lobby".into()),
            ..Default::default()
        },
    )
    .await
    .expect("b sees a in Lobby");

    a.send_channel(None, &Sample::Rich.html())
        .expect("channel text");
    let got = b
        .wait_message(&MessageCheck {
            from: Some(a_name.clone()),
            private: Some(false),
            channel: Some("Lobby".into()),
            contains: Some("<b>bold text</b>".into()),
        })
        .await
        .expect("channel message");
    assert!(got.message.contains("data:image/png;base64,"));

    b.send_private(&a_name, "<i>private</i> reply").expect("pm");
    a.wait_message(&MessageCheck {
        from: Some(b_name.clone()),
        private: Some(true),
        contains: Some("<i>private</i>".into()),
        ..Default::default()
    })
    .await
    .expect("private message");

    a.set_comment("hello from <b>alpha</b>").expect("comment");
    a.set_avatar(PNG.to_vec()).expect("avatar");
    a.set_mute(true).expect("mute");
    a.listen(&["Root/Raid"]).expect("listen");
    b.wait_user(
        &a_name,
        &UserCheck {
            comment_contains: Some("from <b>alpha</b>".into()),
            avatar: Some(PNG.to_vec()),
            muted: Some(true),
            deaf: Some(false),
            listening: Some("Root/Raid".into()),
            ..Default::default()
        },
    )
    .await
    .expect("b sees a's comment, avatar, mute and ear");
    a.set_deaf(true).expect("deaf");
    b.wait_user(
        &a_name,
        &UserCheck {
            deaf: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("b sees a deafened");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn rename_keeps_the_certificate_and_registration_is_visible() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (a_name, new_name, obs_name) = (unique("charlie"), unique("charlie2"), unique("obs"));
    let obs = Bot::connect(config(dir.path(), &obs_name))
        .await
        .expect("observer");
    let mut a = Bot::connect(config(dir.path(), &a_name)).await.expect("a");
    let before = obs
        .wait_user(&a_name, &UserCheck::default())
        .await
        .expect("observer sees a")
        .cert_hash
        .expect("cert hash");

    a.reconnect_as(&new_name).await.expect("rename + reconnect");
    obs.wait_user_gone(&a_name).await.expect("old name gone");
    obs.wait_user(
        &new_name,
        &UserCheck {
            cert_hash: Some(before.clone()),
            registered: Some(false),
            ..Default::default()
        },
    )
    .await
    .expect("same cert under the new name");

    a.register().await.expect("self-register");
    obs.wait_user(
        &new_name,
        &UserCheck {
            registered: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("observer sees the registration");
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn superuser_denies_listen_and_undoes_it() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let admin = Bot::connect(BotConfig::superuser(env::test_port()))
        .await
        .expect("superuser");
    let bot = Bot::connect(config(dir.path(), &unique("delta")))
        .await
        .expect("bot");

    admin.deny_listen("Root/Raid").await.expect("deny");
    bot.listen(&["Root/Raid"]).expect("listen request");
    let denied = bot
        .wait_denied("listen", |d| {
            d.kind == DenyKind::Permission && d.permission == Some(permissions::LISTEN)
        })
        .await;
    // Undo before asserting so a failure never leaves the server changed.
    admin.allow_listen("Root/Raid").await.expect("undo");
    denied.expect("listen denied while the ACL is set");

    bot.listen(&["Root/Raid"]).expect("listen again");
    let own = bot.own_user().expect("own user").name;
    bot.wait_user(
        &own,
        &UserCheck {
            listening: Some("Root/Raid".into()),
            ..Default::default()
        },
    )
    .await
    .expect("listening works after undo");
}
