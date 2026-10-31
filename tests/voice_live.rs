//! Voice test against the local 1.5 test server: bot A plays the speech clip, bot B records it.
//! `testenv/up.sh`, then:
//! `MUMBOT_LIVE=1 cargo test --features tester --test voice_live -- --ignored --test-threads=1`

#![cfg(feature = "tester")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use mumbot::voice::{BotVoice, Clip, PlayOptions, rms_db};
use mumbot::{Bot, BotConfig, UserCheck, env};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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

fn clip_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testenv/audio")
        .join(name)
}

async fn lobby_bot(dir: &Path, name: &str) -> Bot {
    let bot = Bot::connect(BotConfig {
        data_dir: Some(dir.to_owned()),
        ..BotConfig::new(name, env::test_port())
    })
    .await
    .expect("connect");
    bot.join("Lobby").await.expect("join");
    bot
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn bot_a_speaks_and_bot_b_records_it() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (a_name, b_name) = (unique("talker"), unique("listener"));
    let a = lobby_bot(dir.path(), &a_name).await;
    let b = lobby_bot(dir.path(), &b_name).await;
    b.wait_user(
        &a_name,
        &UserCheck {
            in_channel: Some("Lobby".into()),
            ..Default::default()
        },
    )
    .await
    .expect("b sees a");
    let a_voice = BotVoice::start(&a).await.expect("a voice");
    let b_voice = BotVoice::start(&b).await.expect("b voice");
    // Let the transport settle (UDP ping reply) like a real client.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let clip = Clip::load(&clip_path("speech.wav")).expect("clip");
    let a_session = b_voice.session_of(&a_name).expect("a session");
    let before = Instant::now();
    let report = a_voice
        .play(&clip, PlayOptions::default())
        .await
        .expect("play");
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert!(b_voice.decode_errors() == 0);
    assert!(b_voice.talked_between(a_session, before, Instant::now(), -40.0));
    let events = b_voice.talk_events();
    assert!(
        events.iter().any(|e| e.session == a_session && e.talking),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e.session == a_session && !e.talking),
        "{events:?}"
    );
    assert!(!b_voice.is_talking(a_session));
    assert!(
        b_voice.latency_since(a_session, before).expect("latency") < Duration::from_millis(500)
    );

    let heard = b_voice.render(Some(a_session), before, Instant::now());
    let sent_db = rms_db(&clip.samples);
    // Whole-clip RMS includes the leading/trailing silence of the timeline; compare the clip span.
    let span = &heard[..heard.len().min(clip.samples.len() + 48_000)];
    let heard_db = rms_db(span);
    eprintln!(
        "sent {sent_db:.1} dB, heard {heard_db:.1} dB, {} packets",
        report.packets
    );
    assert!(heard_db > -45.0, "heard {heard_db} dB");
    let heard_secs = b_voice.received_duration(a_session).as_secs_f64();
    assert!(
        (heard_secs - clip.duration().as_secs_f64()).abs() < 0.5,
        "heard {heard_secs} s"
    );
    // Nothing from B itself, and A heard nothing.
    assert!(a_voice.speakers().is_empty());
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn cli_play_record_and_expect_audio() {
    if !live() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (a_name, b_name, c_name) = (unique("clip-a"), unique("clip-b"), unique("clip-c"));
    let wav_out = dir.path().join("rec.wav");
    let speech = clip_path("speech.wav");
    let q = |p: &Path| format!("'{}'", p.display());
    // Every CLI run is a script so it can join Lobby first.
    let spawn = |name: &str, script: String| {
        // The old variable name on purpose: Mumcord's smoke script sets MUMCORD_BOT_DATA.
        let mut child = Command::new(env!("CARGO_BIN_EXE_mumbot"))
            .env("MUMCORD_BOT_DATA", dir.path())
            .env_remove("MUMBOT_DATA")
            .args(["--port", &env::test_port().to_string()])
            .args(["--name", name, "--timeout", "15", "script"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let mut stdin = child.stdin.take().expect("stdin");
        tokio::spawn(async move {
            let _ = stdin.write_all(script.as_bytes()).await;
        });
        child
    };
    let rec = spawn(
        &b_name,
        format!("join Lobby\nrecord {} --seconds 9\n", q(&wav_out)),
    );
    let exp = spawn(
        &c_name,
        format!("join Lobby\nexpect-audio --from {a_name} --min-rms -50 --timeout 12\n"),
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let play = spawn(&a_name, format!("join Lobby\nplay {}\n", q(&speech)))
        .wait_with_output()
        .await
        .expect("play");
    assert!(
        play.status.success(),
        "{}",
        String::from_utf8_lossy(&play.stderr)
    );
    let exp = exp.wait_with_output().await.expect("expect-audio");
    assert!(
        exp.status.success(),
        "{}",
        String::from_utf8_lossy(&exp.stderr)
    );
    let rec = rec.wait_with_output().await.expect("record");
    assert!(
        rec.status.success(),
        "{}",
        String::from_utf8_lossy(&rec.stderr)
    );
    let recorded = Clip::load(&wav_out).expect("wav");
    assert!((recorded.duration().as_secs_f64() - 9.0).abs() < 0.1);
    assert!(rms_db(&recorded.samples) > -50.0);
}
