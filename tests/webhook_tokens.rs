//! Channel passwords (Mumble access tokens) in the webhook bot, against the local test server.
//! Start it first (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test webhook_tokens -- --ignored --test-threads=1`
//!
//! Spec 03 addendum: the bot presents the configured `[[token]]` passwords at login, so a channel
//! that only the token group may enter becomes usable for it. A bot without the password stays
//! outside and cannot write there.

#![cfg(feature = "tester")]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use mumbot::webhook::{
    Api, ApiKey, BotApi, ChannelRef, ChannelToken, HttpConfig, ServerConfig, WebhookConfig,
};
use mumbot::{Bot, BotConfig, MessageCheck, UserCheck, env};
use mumcord_protocol::proto;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const WAIT: Duration = Duration::from_secs(20);
const PASSWORD: &str = "s3cret-token";

fn live() -> bool {
    let on = std::env::var("MUMBOT_LIVE").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MUMBOT_LIVE=1 and start testenv/up.sh");
    }
    on
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{:06x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() & 0xff_ffff)
            .unwrap_or(0)
    )
}

fn config(
    dir: &std::path::Path,
    name: &str,
    room: &str,
    with_password: bool,
) -> Arc<WebhookConfig> {
    Arc::new(WebhookConfig {
        modules: Default::default(),
        server: ServerConfig {
            host: env::HOST.to_owned(),
            port: env::test_port(),
            name: name.to_owned(),
            cert_name: None,
            channel: Some(room.to_owned()),
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
        keys: vec![ApiKey {
            key: "test-key-token".to_owned(),
            label: "token-test".to_owned(),
            channels: vec![ChannelRef::Name(room.to_owned())],
        }],
        tokens: if with_password {
            vec![ChannelToken {
                channel: ChannelRef::Name(room.to_owned()),
                password: PASSWORD.to_owned(),
            }]
        } else {
            Vec::new()
        },
        peer: None,
        history: None,

        voice: Default::default(),
        music: None,
    })
}

async fn wait_channel(admin: &Bot, room: &str) -> Result<u32> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Ok(id) = admin.channel_id(room) {
            return Ok(id);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("channel {room:?} was not created");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn the_bot_uses_the_configured_channel_password() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let room = unique("mc-token");
    let with_password = unique("bot-with-token");
    let without_password = unique("bot-without-token");
    let observer_name = unique("observer-with-token");

    let admin = Bot::connect(BotConfig::superuser(env::test_port()))
        .await
        .expect("SuperUser bot (SUPERUSER_PASSWORD in testenv/.env)");
    admin
        .handle()
        .send_channel_state(proto::ChannelState {
            parent: Some(0),
            name: Some(room.clone()),
            temporary: Some(false),
            ..Default::default()
        })
        .expect("create channel");
    let id = wait_channel(&admin, &room).await.expect("channel id");
    admin.require_token(&room, PASSWORD).await.expect("ACL");

    // The bot with the password in its config: it joins the locked channel and can write.
    let api = Arc::new(BotApi::new(config(dir.path(), &with_password, &room, true)));
    api.connect()
        .await
        .expect("login and join with the password");
    admin
        .wait_user(
            &with_password,
            &UserCheck {
                in_channel: Some(room.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the bot is inside the locked channel");
    assert!(
        api.allowed_ids(&api.config().keys[0]).await.contains(&id),
        "the key may write to the locked channel"
    );

    // A chat message is delivered to the members of the channel, so the observer must be inside.
    let observer = Bot::connect(config(dir.path(), &observer_name, &room, true).bot_config())
        .await
        .expect("observer with the password");
    observer
        .join(&room)
        .await
        .expect("observer joins the locked channel");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listen");
    let port = listener.local_addr().expect("port").port();
    tokio::spawn({
        let api = Arc::clone(&api);
        let config = Arc::clone(api.config());
        async move { mumbot::webhook::serve(listener, config, api).await }
    });
    let body = format!(r#"{{"channel":{id},"message":"deploy finished"}}"#);
    let answer = request(port, "test-key-token", &body).await.expect("post");
    assert_eq!(answer.0, 200, "answer: {answer:?}");
    observer
        .wait_message(&MessageCheck {
            contains: Some("deploy finished".to_owned()),
            channel: Some(room.clone()),
            ..Default::default()
        })
        .await
        .expect("the message reached the locked channel");

    // A bot without the password stays outside: join is refused, so nothing is written.
    let locked_out = Arc::new(BotApi::new(config(
        dir.path(),
        &without_password,
        &room,
        false,
    )));
    let error = locked_out.connect().await;
    println!("join without password -> {error:?}");
    assert!(
        error.is_err(),
        "join without the password must be refused: {error:?}"
    );
    let state = admin.state();
    let outside = state
        .users
        .values()
        .find(|user| user.name == without_password)
        .expect("the bot without the password is connected");
    assert_ne!(
        outside.channel_id, id,
        "it stays outside the locked channel"
    );

    println!("addendum ✅ token accepted, locked channel usable, no token stays outside");
}

async fn request(port: u16, key: &str, body: &str) -> Result<(u16, String)> {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let request = format!(
        "POST /message HTTP/1.1\r\nHost: 127.0.0.1\r\nX-API-Key: {key}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await?;
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await?;
    let answer = String::from_utf8(answer)?;
    let (head, text) = answer
        .split_once("\r\n\r\n")
        .unwrap_or((answer.as_str(), ""));
    let status = head
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("0")
        .parse()?;
    Ok((status, text.to_owned()))
}
