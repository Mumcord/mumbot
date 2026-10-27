//! Smoke test of every `mumbot` subcommand against the local 1.5 test server, observed by
//! a library bot. Start the server first (`testenv/up.sh`), then:
//! `MUMBOT_LIVE=1 cargo test --test cli -- --ignored --test-threads=1`

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use mumbot::samples::PNG;
use mumbot::{Bot, BotConfig, MessageCheck, UserCheck, env};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

fn unique(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{prefix}-{:06x}", (nanos ^ std::process::id()) & 0xff_ffff)
}

fn cli(data: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mumbot"));
    cmd.env("MUMBOT_DATA", data)
        .args(["--port", &env::test_port().to_string()])
        .args(args)
        .kill_on_drop(true);
    cmd
}

/// Runs a one-shot subcommand and returns its stdout; panics on failure.
async fn run(data: &Path, args: &[&str]) -> String {
    let out = cli(data, args).output().await.expect("spawn mumbot");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "mumbot {args:?} failed: {}{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn cert_hash(identity_line: &str) -> String {
    identity_line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("cert_hash="))
        .expect("cert_hash in output")
        .to_owned()
}

#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn every_subcommand_works() {
    if std::env::var("MUMBOT_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let data = tempfile::tempdir().expect("tempdir");
    let data = data.path();
    let (obs_name, x) = (unique("obs"), unique("cli-x"));
    let obs = Bot::connect(BotConfig {
        data_dir: Some(data.to_owned()),
        timeout: Duration::from_secs(20),
        ..BotConfig::new(&obs_name, env::test_port())
    })
    .await
    .expect("observer");
    obs.join("Lobby").await.expect("observer joins");

    // One connection running a script; the observer checks each effect and gates the rest
    // with messages ("pong", "done").
    let script = format!(
        "connect\n\
         join Lobby\n\
         # comments and blank lines are skipped\n\
         \n\
         say-sample rich\n\
         pm {obs_name} '<u>private</u>'\n\
         comment \"cli <b>comment</b>\"\n\
         avatar --sample\n\
         mute on\n\
         deaf on\n\
         listen Root/AFK\n\
         expect message --from {obs_name} --contains pong\n\
         unlisten Root/AFK\n\
         avatar --clear\n\
         say --channel Lobby \"<i>plain</i>\"\n\
         register\n\
         expect user {obs_name} --in Lobby --registered false\n\
         sleep 0.1\n\
         expect message --contains done --private false\n"
    );
    let mut child = cli(data, &["--name", &x, "--timeout", "30", "script"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn script");
    let mut stdin = child.stdin.take().expect("stdin");
    stdin
        .write_all(script.as_bytes())
        .await
        .expect("write script");
    drop(stdin);

    let from_x = |contains: &str, private: bool| MessageCheck {
        from: Some(x.clone()),
        contains: Some(contains.to_owned()),
        private: Some(private),
        ..Default::default()
    };
    obs.wait_message(&from_x("<b>bold text</b>", false))
        .await
        .expect("say-sample");
    obs.wait_message(&from_x("<u>private</u>", true))
        .await
        .expect("pm");
    obs.wait_user(
        &x,
        &UserCheck {
            in_channel: Some("Lobby".into()),
            comment_contains: Some("cli <b>comment</b>".into()),
            avatar: Some(PNG.to_vec()),
            muted: Some(true),
            deaf: Some(true),
            listening: Some("Root/AFK".into()),
            ..Default::default()
        },
    )
    .await
    .expect("join, comment, avatar, mute, deaf, listen");
    obs.send_channel(None, "pong").expect("pong");
    obs.wait_user(
        &x,
        &UserCheck {
            not_listening: Some("Root/AFK".into()),
            has_avatar: Some(false),
            registered: Some(true),
            ..Default::default()
        },
    )
    .await
    .expect("unlisten, avatar --clear, register");
    obs.wait_message(&from_x("<i>plain</i>", false))
        .await
        .expect("say");
    obs.send_channel(None, "done").expect("done");
    let out = child.wait_with_output().await.expect("script exit");
    assert!(out.status.success(), "script failed");
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok register"));

    // rename keeps the certificate.
    let y = unique("cli-y");
    let before = cert_hash(&run(data, &["--name", &y, "connect"]).await);
    let renamed = run(data, &["--name", &y, "rename", &format!("{y}-2")]).await;
    assert_eq!(cert_hash(&renamed), before);

    // --hold keeps the bot online; expect user-gone sees it leave.
    let h = unique("cli-h");
    let mut held = cli(data, &["--name", &h, "--hold", "2", "join", "Root/AFK"])
        .spawn()
        .expect("spawn hold");
    obs.wait_user(
        &h,
        &UserCheck {
            in_channel: Some("AFK".into()),
            ..Default::default()
        },
    )
    .await
    .expect("held bot in AFK");
    run(
        data,
        &[
            "--name",
            &unique("cli-g"),
            "--timeout",
            "15",
            "expect",
            "user-gone",
            &h,
        ],
    )
    .await;
    assert!(held.wait().await.expect("hold exit").success());

    // deny-listen / expect denied / allow-listen.
    run(data, &["--superuser", "deny-listen", "Root/Raid"]).await;
    let mut denied_child = cli(data, &["--name", &unique("cli-d"), "script"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn denied script");
    let mut stdin = denied_child.stdin.take().expect("stdin");
    stdin
        .write_all(b"listen Root/Raid\nexpect denied --permission listen\n")
        .await
        .expect("write script");
    drop(stdin);
    let out = denied_child.wait_with_output().await.expect("denied exit");
    run(data, &["--superuser", "allow-listen", "Root/Raid"]).await;
    assert!(
        out.status.success(),
        "listen was not denied: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A failing expectation exits non-zero.
    let out = cli(
        data,
        &[
            "--name",
            &unique("cli-f"),
            "--timeout",
            "1",
            "expect",
            "message",
            "--from",
            "nobody-here",
        ],
    )
    .output()
    .await
    .expect("spawn");
    assert!(!out.status.success());
}

/// Issue #14: the bot must stay online for the whole `--hold`, and everything it sent must
/// reach the server before it disconnects, also with `--hold 0`.
#[tokio::test]
#[ignore = "needs testenv and MUMBOT_LIVE=1"]
async fn hold_keeps_the_bot_online_and_every_message_is_delivered() {
    if std::env::var("MUMBOT_LIVE").ok().as_deref() != Some("1") {
        return;
    }
    let data = tempfile::tempdir().expect("tempdir");
    let data = data.path();
    let obs_name = unique("obs");
    let obs = Bot::connect(BotConfig {
        data_dir: Some(data.to_owned()),
        timeout: Duration::from_secs(20),
        ..BotConfig::new(&obs_name, env::test_port())
    })
    .await
    .expect("observer");
    obs.join("Lobby").await.expect("observer joins");

    let h = unique("cli-h");
    let started = std::time::Instant::now();
    let mut held = cli(
        data,
        &[
            "--name",
            &h,
            "--hold",
            "3",
            "say",
            "--channel",
            "Lobby",
            "held hello",
        ],
    )
    .spawn()
    .expect("spawn hold");
    obs.wait_message(&MessageCheck {
        from: Some(h.clone()),
        contains: Some("held hello".into()),
        ..Default::default()
    })
    .await
    .expect("message of the held bot");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "too slow to check the hold"
    );
    assert!(
        obs.state().users.values().any(|u| u.name == h),
        "the bot left during --hold"
    );
    assert!(held.wait().await.expect("hold exit").success());
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "--hold cut short"
    );
    obs.wait_user_gone(&h)
        .await
        .expect("held bot leaves after the hold");

    // More messages than the client-side rate limit sends at once, then an immediate exit:
    // the queued ones must still go out before the disconnect.
    let b = unique("cli-b");
    let script: String = (1..=6)
        .map(|n| format!("say --channel Lobby \"{b} burst {n}\"\n"))
        .collect();
    let mut burst = cli(data, &["--name", &b, "script"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn burst");
    let mut stdin = burst.stdin.take().expect("stdin");
    stdin
        .write_all(script.as_bytes())
        .await
        .expect("write script");
    drop(stdin);
    assert!(burst.wait().await.expect("burst exit").success());
    for n in 1..=6 {
        obs.wait_message(&MessageCheck {
            from: Some(b.clone()),
            contains: Some(format!("{b} burst {n}")),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("burst message {n} lost: {e:#}"));
    }
}
