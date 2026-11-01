//! Music module against the local 1.5 test server. Start it first (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test music_live -- --ignored --test-threads=1`
//!
//! Checks the acceptance criteria of `specs/05-music-specification.md` in the running bot:
//! AC-4.1 a played file is heard by another user, AC-4.2 queue, skip and stop, AC-4.3 refused
//! requests, AC-4.4 music and webhook on one connection, AC-4.5 the configured bitrate; and the
//! plugin announce `mumbot-music@1` to Mumcord clients whose hello lists `plugins` (task M2b).

#![cfg(feature = "tester")]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use mumbot::music::MusicConfig;
use mumbot::protocol::Event;
use mumbot::voice::BotVoice;
use mumbot::voice_settings::VoiceSettings;
use mumbot::webhook::{ApiKey, ChannelRef, HttpConfig, ModulesConfig, ServerConfig, WebhookConfig};
use mumbot::{Bot, BotConfig, Module, UserCheck, env, modules};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

const KEY: &str = "test-key-music";
/// Lower than the 96 kbit/s default, so the packet sizes show the setting reached the wire.
const MUSIC_BITRATE: u32 = 16_000;

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
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{prefix}-{:06x}", (nanos ^ std::process::id()) & 0xff_ffff)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|addr| addr.port())
        .expect("free port")
}

/// Music and webhook on one connection, the media directory holding the bundled sample.
fn config(
    dir: &Path,
    media: &Path,
    name: &str,
    music_port: u16,
    webhook_port: u16,
) -> WebhookConfig {
    let key = |channel: &str| ApiKey {
        key: KEY.to_owned(),
        label: "ci".to_owned(),
        channels: vec![ChannelRef::Name(channel.to_owned())],
    };
    let mut music: MusicConfig = toml::from_str("").expect("music defaults");
    music.listen = format!("127.0.0.1:{music_port}");
    music.media_dir = Some(media.to_owned());
    music.bitrate = Some(MUSIC_BITRATE);
    music.keys = vec![key("Lobby")];
    WebhookConfig {
        modules: ModulesConfig {
            enabled: vec![Module::Music, Module::Webhook],
        },
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
            listen: format!("127.0.0.1:{webhook_port}"),
            rate_limit_per_minute: 20,
            delivery_timeout_secs: 3,
        },
        keys: vec![key("Lobby")],
        tokens: Vec::new(),
        peer: None,
        history: None,
        plugins: None,
        voice: Default::default(),
        music: Some(music),
    }
}

/// One raw request, one raw answer.
async fn call(
    port: u16,
    key: Option<&str>,
    method: &str,
    path: &str,
    body: &str,
) -> Result<(u16, String)> {
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    let key_line = key
        .map(|value| format!("X-API-Key: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{key_line}Content-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(request.as_bytes()).await?;
    let mut answer = Vec::new();
    socket.read_to_end(&mut answer).await?;
    let answer = String::from_utf8(answer)?;
    let (head, body) = answer
        .split_once("\r\n\r\n")
        .unwrap_or((answer.as_str(), ""));
    let status = head.split_whitespace().nth(1).unwrap_or("0").parse()?;
    Ok((status, body.to_owned()))
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn music_is_heard_and_controlled_over_http() -> Result<()> {
    if !live() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let media = tempfile::tempdir()?;
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("testenv/audio/speech.mp3"),
        media.path().join("speech.mp3"),
    )?;
    let (music_name, listener_name) = (unique("music"), unique("listener"));
    let (music_port, webhook_port) = (free_port(), free_port());
    let config = config(
        dir.path(),
        media.path(),
        &music_name,
        music_port,
        webhook_port,
    );
    let running = tokio::spawn(modules::run(
        Arc::new(config),
        VoiceSettings {
            bitrate_bps: MUSIC_BITRATE,
            ..Default::default()
        },
    ));

    let listener = Bot::connect(BotConfig {
        data_dir: Some(dir.path().to_owned()),
        ..BotConfig::new(&listener_name, env::test_port())
    })
    .await?;
    listener.join("Lobby").await?;
    listener
        .wait_user(
            &music_name,
            &UserCheck {
                in_channel: Some("Lobby".into()),
                ..Default::default()
            },
        )
        .await?;
    let voice = BotVoice::start(&listener).await?;

    // AC-4.4: both APIs answer, one bot user.
    let (status, body) = call(music_port, None, "GET", "/health", "").await?;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"connected\":true"), "{body}");
    let (status, _) = call(webhook_port, None, "GET", "/health", "").await?;
    assert_eq!(status, 200);

    // AC-4.3: refused requests.
    let play = r#"{"file":"speech.mp3"}"#;
    assert_eq!(call(music_port, None, "POST", "/play", play).await?.0, 401);
    let escape = r#"{"file":"../speech.mp3"}"#;
    assert_eq!(
        call(music_port, Some(KEY), "POST", "/play", escape)
            .await?
            .0,
        403
    );
    let url = r#"{"url":"http://127.0.0.1/speech.mp3"}"#;
    assert_eq!(
        call(music_port, Some(KEY), "POST", "/play", url).await?.0,
        403
    );

    // AC-4.1: the listener hears the music bot.
    let mut packets = voice.subscribe_packets();
    let since = Instant::now();
    let (status, body) = call(music_port, Some(KEY), "POST", "/play", play).await?;
    assert_eq!(status, 200, "{body}");
    let hit = voice
        .wait_audio(Some(&music_name), -45.0, since, Duration::from_secs(10))
        .await?;
    eprintln!(
        "heard the music bot at {:.1} dBFS after {:?}",
        hit.level_db, hit.latency
    );

    // AC-4.2: the queue shows the track, skip ends the audio.
    let (_, body) = call(music_port, Some(KEY), "GET", "/queue", "").await?;
    assert!(
        body.contains("\"title\":\"Mumbot – Speech sample\""),
        "{body}"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (_, body) = call(music_port, Some(KEY), "POST", "/skip", "").await?;
    assert_eq!(body, r#"{"skipped":true}"#);
    voice
        .wait_talking(hit.session, false, Duration::from_secs(1))
        .await?;

    // AC-4.5: the payload size matches the music bitrate, far below the 96 kbit/s default.
    let mut sizes = Vec::new();
    while let Ok(packet) = packets.try_recv() {
        if packet.session == hit.session && !packet.is_last {
            sizes.push(packet.payload.len());
        }
    }
    assert!(sizes.len() > 20, "{} packets", sizes.len());
    let mean = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    let expected = f64::from(MUSIC_BITRATE) / 8.0 * 0.02;
    eprintln!("mean payload {mean:.0} bytes, {expected:.0} expected");
    assert!(mean < expected * 1.75, "{mean} bytes per packet");

    // AC-4.2: stop empties the queue.
    for _ in 0..3 {
        assert_eq!(
            call(music_port, Some(KEY), "POST", "/play", play).await?.0,
            200
        );
    }
    let (_, body) = call(music_port, Some(KEY), "POST", "/stop", "").await?;
    assert_eq!(body, r#"{"dropped":2,"stopped":true}"#);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (_, body) = call(music_port, Some(KEY), "GET", "/queue", "").await?;
    assert!(body.contains("\"now_playing\":null"), "{body}");
    assert!(body.contains("\"queue\":[]"), "{body}");

    running.abort();
    listener.disconnect().await
}

/// The next `mumcord.plugins` message from `from` within `wait`, as JSON.
async fn next_announce(
    events: &mut broadcast::Receiver<Event>,
    from: u32,
    wait: Duration,
) -> Option<serde_json::Value> {
    tokio::time::timeout(wait, async {
        loop {
            if let Ok(Event::PluginData(data)) = events.recv().await
                && data.sender == Some(from)
                && data.data_id == "mumcord.plugins"
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
async fn the_music_bot_announces_its_plugin_to_mumcord_clients() -> Result<()> {
    if !live() {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let media = tempfile::tempdir()?;
    let (music_name, alice_name, old_name) =
        (unique("music"), unique("alice"), unique("oldmumcord"));
    let config = config(
        dir.path(),
        media.path(),
        &music_name,
        free_port(),
        free_port(),
    );
    let running = tokio::spawn(modules::run(Arc::new(config), VoiceSettings::default()));

    let client = |name: &str| BotConfig {
        data_dir: Some(dir.path().to_owned()),
        ..BotConfig::new(name, env::test_port())
    };
    let alice = Bot::connect(client(&alice_name)).await?;
    let old = Bot::connect(client(&old_name)).await?;
    alice.wait_user(&music_name, &UserCheck::default()).await?;
    let bot = alice
        .state()
        .users
        .values()
        .find(|u| u.name == music_name)
        .map(|u| u.session)
        .expect("music bot is online");
    let (mut alice_events, mut old_events) = (alice.subscribe(), old.subscribe());
    let hello = |features: &str| {
        format!(r#"{{"app":"mumcord","v":1,"features":[{features}]}}"#).into_bytes()
    };
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""p2p","plugins""#))?;
    old.handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""p2p""#))?;

    let wait = Duration::from_secs(5);
    assert_eq!(
        next_announce(&mut alice_events, bot, wait).await,
        Some(serde_json::json!({"v": 1, "p": ["mumbot-music@1"]})),
        "official entry, no source suffix"
    );
    let announced = Instant::now();
    assert_eq!(next_announce(&mut old_events, bot, wait).await, None);

    // A second hello within 10 s is answered only once the interval has passed.
    alice
        .handle()
        .send_plugin_data(&[bot], "mumcord.hello", hello(r#""plugins""#))?;
    assert!(
        next_announce(&mut alice_events, bot, Duration::from_secs(15))
            .await
            .is_some()
    );
    assert!(announced.elapsed() >= Duration::from_secs(9), "throttled");

    running.abort();
    old.disconnect().await?;
    alice.disconnect().await
}
