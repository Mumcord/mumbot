//! Load generator for scale measurements: fills a local test server with many users spread over
//! its channels, then (on demand) adds churn: moves, leaves/joins, chat and talking.
//!
//! Only for the local load-test server (`testenv/scale/up.sh`, port 44850). Run with:
//!
//!     cargo run --release --example loadtest -- --users 1000
//!
//! Churn and talking are toggled at runtime by creating/removing the files named by `--churn-file`
//! and `--talk-file`, so one connected crowd serves both the steady-state and the churn phase.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use mumbot::protocol::{ConnectConfig, Handle, TextTarget, connect};
use mumbot::voice::{BotVoice, Clip, PlayOptions};
use mumbot::{Bot, BotConfig, env};
use tokio::sync::{Mutex, Semaphore};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 44850)]
    port: u16,
    #[arg(long, default_value_t = 1000)]
    users: usize,
    /// Bots that speak in bursts (they join `--talk-channel`).
    #[arg(long, default_value_t = 6)]
    talkers: usize,
    #[arg(long, default_value_t = 1)]
    talk_channel: u32,
    /// Moves per second while churning.
    #[arg(long, default_value_t = 10.0)]
    moves: f64,
    /// Leave + join pairs per second while churning.
    #[arg(long, default_value_t = 2.0)]
    joins: f64,
    /// Chat messages per second while churning (half of them to `--talk-channel`).
    #[arg(long, default_value_t = 1.0)]
    chat: f64,
    #[arg(long, default_value_t = 50)]
    connect_parallel: usize,
    #[arg(long)]
    churn_file: PathBuf,
    #[arg(long)]
    talk_file: PathBuf,
    #[arg(long)]
    cert_dir: PathBuf,
}

/// Small deterministic PRNG (xorshift64*); the bot crate has no `rand` dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

const FIRST: &[&str] = &[
    "Alex", "Mia", "Noah", "Emma", "Liam", "Olivia", "Elias", "Sofia", "Finn", "Lena", "Jonas",
    "Hanna", "Ben", "Clara", "Paul", "Lea", "Leon", "Marie", "Luca", "Anna", "Felix", "Ella",
    "Max", "Ida", "Theo", "Nora", "Emil", "Frieda", "Oskar", "Greta", "Samir", "Aylin", "Kenji",
    "Yuki", "Priya", "Ravi", "Diego", "Lucia", "Omar", "Zara",
];
const SUFFIX: &[&str] = &[
    "", "_gg", "92", "TheGreat", "_x", "77", "Wolf", "_live", "Pixel", "07", "Nova", "_afk",
    "Storm", "2k", "Byte", "_88", "Fox", "Ace", "_pro", "Sky", "Moon", "Rex", "_tv", "Zen", "Dash",
];

fn user_name(n: usize) -> String {
    let first = FIRST[n % FIRST.len()];
    let suffix = SUFFIX[(n / FIRST.len()) % SUFFIX.len()];
    let round = n / (FIRST.len() * SUFFIX.len());
    if round == 0 {
        format!("{first}{suffix}")
    } else {
        format!("{first}{suffix}{round}")
    }
}

/// Channel weights: three crowded rooms, one busy game room, a long tail over ~110 channels,
/// and the rest empty (like a real community server).
fn channel_plan(users: usize, channel_count: u32, rng: &mut Rng) -> Vec<u32> {
    let mut plan = Vec::with_capacity(users);
    let fixed: [(u32, f64); 4] = [(1, 0.14), (2, 0.11), (3, 0.12), (8, 0.06)];
    for (channel, share) in fixed {
        let n = (users as f64 * share).round() as usize;
        plan.extend(std::iter::repeat_n(channel, n));
    }
    let mut tail: Vec<u32> = (4..=channel_count).filter(|c| *c != 8).collect();
    // Shuffle, then take a Zipf-like share for the first 110.
    for i in (1..tail.len()).rev() {
        tail.swap(i, rng.below(i + 1));
    }
    tail.truncate(110);
    let weights: Vec<f64> = (0..tail.len())
        .map(|i| 1.0 / (i as f64 + 1.0).powf(0.9))
        .collect();
    let total: f64 = weights.iter().sum();
    while plan.len() < users {
        let mut pick = rng.unit() * total;
        let mut chosen = tail[0];
        for (w, c) in weights.iter().zip(&tail) {
            if pick < *w {
                chosen = *c;
                break;
            }
            pick -= w;
        }
        plan.push(chosen);
    }
    plan
}

async fn connect_user(port: u16, name: String, channel: u32) -> Result<Handle> {
    let conn = connect(ConnectConfig {
        host: env::HOST.to_owned(),
        port,
        username: name.clone(),
        password: env::testenv_var("SERVER_PASSWORD").filter(|p| !p.is_empty()),
        tokens: Vec::new(),
        identity: None,
        // Local load-test server only; its certificate is self-signed.
        verify_cert: Arc::new(|_| true),
        login_timeout: Duration::from_secs(60),
    })
    .await
    .with_context(|| format!("connecting {name}"))?;
    let mut events = conn.events;
    // Drain events so the client's unbounded queue does not grow.
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    if channel != 0 {
        conn.handle
            .join_channel(channel)
            .with_context(|| format!("{name} joining {channel}"))?;
    }
    Ok(conn.handle)
}

struct Crowd {
    handles: Vec<Option<Handle>>,
    next_name: usize,
}

fn every(rate: f64) -> Duration {
    Duration::from_secs_f64(1.0 / rate.max(0.001))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let args = Args::parse();
    const CHANNELS: u32 = 304;
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let plan = channel_plan(args.users, CHANNELS, &mut rng);

    let started = Instant::now();
    let gate = Arc::new(Semaphore::new(args.connect_parallel));
    let done = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for (n, channel) in plan.iter().copied().enumerate() {
        let gate = gate.clone();
        let done = done.clone();
        let port = args.port;
        tasks.push(tokio::spawn(async move {
            let _permit = gate.acquire_owned().await.ok();
            let result = connect_user(port, user_name(n), channel).await;
            let count = done.fetch_add(1, Ordering::Relaxed) + 1;
            if count.is_multiple_of(100) {
                eprintln!("connected {count}");
            }
            result
        }));
    }
    let mut handles = Vec::new();
    for task in tasks {
        match task.await? {
            Ok(h) => handles.push(Some(h)),
            Err(e) => eprintln!("connect failed: {e:#}"),
        }
    }
    eprintln!(
        "READY {} users in {:.1}s",
        handles.len(),
        started.elapsed().as_secs_f64()
    );

    let talkers = start_talkers(&args).await?;
    let clip = Arc::new(
        Clip::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("testenv/audio/speech.wav"))
            .context("speech clip")?,
    );
    for (i, pair) in talkers.into_iter().enumerate() {
        let clip = clip.clone();
        let talk_file = args.talk_file.clone();
        tokio::spawn(async move {
            // Keep the bot alive (it owns the connection) for as long as it talks.
            let (voice, _bot) = pair;
            let mut rng = Rng(0xA5A5_0000 + i as u64);
            // Staggered bursts of 1-4 s with 0.5-3 s pauses: rings switch on and off often.
            loop {
                if talk_file.exists() {
                    let burst = Duration::from_millis(1000 + rng.below(3000) as u64);
                    let opts = PlayOptions {
                        forever: true,
                        max_duration: Some(burst),
                        ..PlayOptions::default()
                    };
                    if let Err(e) = voice.play(&clip, opts).await {
                        eprintln!("talker {i}: {e:#}");
                    }
                }
                tokio::time::sleep(Duration::from_millis(500 + rng.below(2500) as u64)).await;
            }
        });
    }

    let crowd = Arc::new(Mutex::new(Crowd {
        next_name: plan.len(),
        handles,
    }));
    churn(&args, crowd, plan).await
}

/// Talkers need a certificate-backed [`Bot`] for the voice transport.
async fn start_talkers(args: &Args) -> Result<Vec<(BotVoice, Bot)>> {
    let mut out = Vec::new();
    for i in 0..args.talkers {
        let bot = Bot::connect(BotConfig {
            data_dir: Some(args.cert_dir.clone()),
            timeout: Duration::from_secs(30),
            ..BotConfig::new(&format!("Speaker{}", i + 1), args.port)
        })
        .await?;
        bot.handle().join_channel(args.talk_channel)?;
        let voice = BotVoice::start(&bot).await?;
        out.push((voice, bot));
    }
    Ok(out)
}

async fn churn(args: &Args, crowd: Arc<Mutex<Crowd>>, plan: Vec<u32>) -> Result<()> {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut moves = tokio::time::interval(every(args.moves));
    let mut joins = tokio::time::interval(every(args.joins));
    let mut chats = tokio::time::interval(every(args.chat));
    let mut report = tokio::time::interval(Duration::from_secs(10));
    let (mut n_moves, mut n_joins, mut n_chat) = (0u64, 0u64, 0u64);
    let mut msg = 0u64;
    loop {
        tokio::select! {
            _ = moves.tick() => {
                if !args.churn_file.exists() { continue; }
                let c = crowd.lock().await;
                let i = rng.below(c.handles.len());
                if let Some(h) = &c.handles[i] {
                    let target = plan[rng.below(plan.len())];
                    if h.join_channel(target).is_ok() { n_moves += 1; }
                }
            }
            _ = joins.tick() => {
                if !args.churn_file.exists() { continue; }
                let (old, name, channel, i) = {
                    let mut c = crowd.lock().await;
                    let i = rng.below(c.handles.len());
                    let name = user_name(c.next_name);
                    c.next_name += 1;
                    (c.handles[i].take(), name, plan[rng.below(plan.len())], i)
                };
                if let Some(old) = old { old.disconnect(); }
                let crowd = crowd.clone();
                let port = args.port;
                tokio::spawn(async move {
                    match connect_user(port, name, channel).await {
                        Ok(h) => crowd.lock().await.handles[i] = Some(h),
                        Err(e) => eprintln!("rejoin failed: {e:#}"),
                    }
                });
                n_joins += 1;
            }
            _ = chats.tick() => {
                if !args.churn_file.exists() { continue; }
                let c = crowd.lock().await;
                let i = rng.below(c.handles.len());
                let channel = if rng.below(2) == 0 { args.talk_channel } else { plan[rng.below(plan.len())] };
                if let Some(h) = &c.handles[i] {
                    msg += 1;
                    let text = format!("Load message {msg}: anyone up for a round later?");
                    if h.send_text(&TextTarget::channel(channel), &text).is_ok() { n_chat += 1; }
                }
            }
            _ = report.tick() => {
                eprintln!("churn totals: moves {n_moves}, leave+join {n_joins}, chat {n_chat}");
            }
        }
    }
}
