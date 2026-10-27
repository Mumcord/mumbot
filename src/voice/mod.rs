//! Bot voice: play WAV files as Opus voice, record and analyse what other users say.
//!
//! [`BotVoice::start`] attaches a [`VoiceTransport`] to a connected [`Bot`], forwards the
//! protocol's `CryptSetup`/`UdpTunnel` events to it and decodes every received packet per
//! speaker. Everything is keyed by Mumble session; [`BotVoice::session_of`] resolves a name.
//!
//! Start the receiving bot's voice *before* the sender talks: audio that arrived earlier is not
//! recorded. A [`BotVoice`] belongs to the connection it was started on (not valid after
//! `Bot::reconnect_as`). Time windows are plain [`Instant`]s: take `Instant::now()` before the
//! action under test.

pub mod analysis;
mod receive;
pub mod wav;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use mumcord_audio::codec::{MAX_PACKET_BYTES, OpusEncoder, PacketDuration};
use mumcord_audio::{FRAME_SAMPLES, Frame};
use mumcord_protocol::voice::{OutgoingAudio, VoiceEvent, VoicePacket, VoiceTransport};
use mumcord_protocol::{Event, Handle};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

pub use analysis::{SILENCE_DB, active_duration, levels_db, rms, rms_db};
pub use receive::{TALK_IDLE, TalkEvent};
pub use wav::Clip;

use receive::Inbox;

use crate::Bot;

/// Bitrate of bot speech (bit/s).
pub const BOT_BITRATE: i32 = 40_000;
const PACKET_INTERVAL: Duration = Duration::from_millis(20);
const POLL: Duration = Duration::from_millis(20);

/// How [`BotVoice::play`] plays a clip.
#[derive(Debug, Clone, Copy)]
pub struct PlayOptions {
    /// Number of passes over the clip (ignored with `forever`).
    pub repeat: u32,
    /// Loop until `max_duration` (or until the future is dropped).
    pub forever: bool,
    /// Stop (with terminator) after this much time.
    pub max_duration: Option<Duration>,
    /// Avatar position sent with every packet (positional audio); only reaches people with
    /// the same plugin context (`Handle::set_plugin_context`).
    pub position: Option<[f32; 3]>,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            repeat: 1,
            forever: false,
            max_duration: None,
            position: None,
        }
    }
}

/// What [`BotVoice::play`] did.
#[derive(Debug, Clone, Copy)]
pub struct PlayReport {
    /// When the first packet was queued.
    pub started: Instant,
    pub finished: Instant,
    /// Packets sent, including the terminator.
    pub packets: u64,
}

/// Result of [`BotVoice::wait_audio`].
#[derive(Debug, Clone, Copy)]
pub struct AudioHit {
    pub session: u32,
    /// Loudest 100 ms window so far, dBFS.
    pub level_db: f32,
    /// Time from `since` to the first packet that arrived after it.
    pub latency: Duration,
}

/// Voice side of a [`Bot`].
pub struct BotVoice {
    transport: Arc<VoiceTransport>,
    handle: Handle,
    inbox: Arc<Mutex<Inbox>>,
    talk: broadcast::Sender<TalkEvent>,
    packets: broadcast::Sender<VoicePacket>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for BotVoice {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn lock(inbox: &Mutex<Inbox>) -> MutexGuard<'_, Inbox> {
    inbox.lock().unwrap_or_else(|p| p.into_inner())
}

impl BotVoice {
    /// Starts voice for `bot` against the server it is connected to.
    pub async fn start(bot: &Bot) -> Result<Self> {
        let config = bot.config();
        let addr: SocketAddr = tokio::net::lookup_host((config.host.as_str(), config.port))
            .await
            .with_context(|| format!("resolving {}", config.host))?
            .next()
            .ok_or_else(|| anyhow!("{} has no address", config.host))?;
        // Subscribe before starting so no tunnel/crypt event is missed.
        let mut events = bot.subscribe();
        let handle = bot.handle().clone();
        let (transport, mut voice_rx) = VoiceTransport::start(handle.clone(), addr)
            .await
            .context("starting voice transport")?;
        let transport = Arc::new(transport);
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let (talk, _) = broadcast::channel(256);
        let (packets, _) = broadcast::channel(1024);

        let forward = {
            let transport = Arc::clone(&transport);
            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(Event::CryptSetup(setup)) => transport.on_crypt_setup(setup),
                        Ok(Event::UdpTunnel(packet)) => transport.on_tunnel(packet),
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            })
        };
        let receive = {
            let inbox = Arc::clone(&inbox);
            let talk = talk.clone();
            let packets = packets.clone();
            tokio::spawn(async move {
                let mut idle = tokio::time::interval(Duration::from_millis(50));
                idle.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    let events = tokio::select! {
                        event = voice_rx.recv() => match event {
                            Some(VoiceEvent::Packet(packet)) => {
                                let events = lock(&inbox).ingest(&packet, Instant::now());
                                let _ = packets.send(packet);
                                events
                            }
                            Some(VoiceEvent::TransportChanged(t)) => {
                                tracing::debug!(?t, "voice transport changed");
                                Vec::new()
                            }
                            None => break,
                        },
                        _ = idle.tick() => lock(&inbox).expire_idle(Instant::now()),
                    };
                    for event in events {
                        let _ = talk.send(event);
                    }
                }
            })
        };
        Ok(Self {
            transport,
            handle,
            inbox,
            talk,
            packets,
            tasks: vec![forward, receive],
        })
    }

    /// The underlying transport (stats, transport kind).
    pub fn transport(&self) -> &VoiceTransport {
        &self.transport
    }

    // ---- sending ----

    /// Plays `clip` as 20 ms Opus packets in real time to the current channel. The last packet
    /// carries the terminator flag; sequence numbers advance by 2 per packet (10 ms units).
    pub async fn play(&self, clip: &Clip, opts: PlayOptions) -> Result<PlayReport> {
        let frames = clip.frames();
        if frames.is_empty() {
            bail!("clip is empty");
        }
        let mut encoder = OpusEncoder::new(BOT_BITRATE, PacketDuration::Ms20)
            .map_err(|e| anyhow!("encoder: {e}"))?;
        let mut ticker = tokio::time::interval(PACKET_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
        let started = Instant::now();
        let deadline = opts.max_duration.map(|d| started + d);
        let passes = if opts.forever {
            u32::MAX
        } else {
            opts.repeat.max(1)
        };
        let mut packets = 0u64;
        let mut terminated = false;
        'outer: for pass in 0..passes {
            let pairs = frames.chunks(2);
            let count = pairs.len();
            for (i, pair) in pairs.enumerate() {
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    break 'outer;
                }
                ticker.tick().await;
                let last = !opts.forever && pass + 1 == passes && i + 1 == count;
                self.send_packet(&mut encoder, pair, last, opts.position)?;
                packets += 1;
                terminated = last;
            }
        }
        if !terminated {
            ticker.tick().await;
            let silence: [Frame; 2] = [[0.0; FRAME_SAMPLES]; 2];
            self.send_packet(&mut encoder, &silence, true, opts.position)?;
            packets += 1;
        }
        Ok(PlayReport {
            started,
            finished: Instant::now(),
            packets,
        })
    }

    fn send_packet(
        &self,
        encoder: &mut OpusEncoder,
        pair: &[Frame],
        last: bool,
        position: Option<[f32; 3]>,
    ) -> Result<()> {
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let mut len = None;
        for frame in pair {
            len = encoder
                .push_frame(frame, &mut buf)
                .map_err(|e| anyhow!("encode: {e}"))?;
        }
        let len = len.ok_or_else(|| anyhow!("encoder produced no packet"))?;
        self.transport.send_audio(
            OutgoingAudio::new(Bytes::copy_from_slice(&buf[..len]), 2, last)
                .with_position(position),
        );
        Ok(())
    }

    /// Loads `path` and plays it, see [`play`](Self::play).
    pub async fn play_file(&self, path: &Path, opts: PlayOptions) -> Result<PlayReport> {
        self.play(&Clip::load(path)?, opts).await
    }

    // ---- receiving ----

    /// Session of the connected user called `name`.
    pub fn session_of(&self, name: &str) -> Result<u32> {
        self.handle
            .with_state(|s| s.users.values().find(|u| u.name == name).map(|u| u.session))
            .ok_or_else(|| anyhow!("no user {name:?} connected"))
    }

    /// Sessions we received voice from so far.
    pub fn speakers(&self) -> Vec<u32> {
        lock(&self.inbox).sessions()
    }

    /// Whether voice from `session` is currently arriving.
    pub fn is_talking(&self, session: u32) -> bool {
        lock(&self.inbox).is_talking(session)
    }

    /// Received packet count of `session`.
    pub fn packet_count(&self, session: u32) -> u64 {
        lock(&self.inbox).packet_count(session)
    }

    /// Total decoded audio duration received from `session`.
    pub fn received_duration(&self, session: u32) -> Duration {
        lock(&self.inbox).total_duration(session)
    }

    /// Talking start/stop events so far, oldest first.
    pub fn talk_events(&self) -> Vec<TalkEvent> {
        lock(&self.inbox).talk_log.clone()
    }

    /// Every received voice packet as it arrived (with its position, if any).
    pub fn subscribe_packets(&self) -> broadcast::Receiver<VoicePacket> {
        self.packets.subscribe()
    }

    /// Live talking-state events.
    pub fn subscribe_talk(&self) -> broadcast::Receiver<TalkEvent> {
        self.talk.subscribe()
    }

    /// Packets that failed to decode.
    pub fn decode_errors(&self) -> u64 {
        lock(&self.inbox).decode_errors
    }

    /// Decoded audio of `session` (`None` = all speakers summed) on a timeline from `from` to
    /// `to`: 48 kHz mono, silence where nothing was received.
    pub fn render(&self, session: Option<u32>, from: Instant, to: Instant) -> Vec<f32> {
        lock(&self.inbox).render(session, from, to)
    }

    /// RMS (dBFS) over time of `session` in `from..to` in windows of `window_ms`.
    pub fn levels(
        &self,
        session: Option<u32>,
        from: Instant,
        to: Instant,
        window_ms: u32,
    ) -> Vec<f32> {
        levels_db(&self.render(session, from, to), window_ms)
    }

    /// Did `session` talk between `from` and `to`, i.e. is some 100 ms window at least
    /// `min_db` loud?
    pub fn talked_between(&self, session: u32, from: Instant, to: Instant, min_db: f32) -> bool {
        lock(&self.inbox).peak_db(Some(session), from, to) >= min_db
    }

    /// Time from `since` until the first packet of `session` arrived (latency of a clip that
    /// started playing at `since`; valid for bots on one machine sharing a clock).
    pub fn latency_since(&self, session: u32, since: Instant) -> Option<Duration> {
        lock(&self.inbox)
            .first_arrival_since(session, since)
            .map(|a| a.duration_since(since))
    }

    /// Waits until `from` (a user name; `None` = anyone) produced a 100 ms window of at least
    /// `min_db` after `since`.
    pub async fn wait_audio(
        &self,
        from: Option<&str>,
        min_db: f32,
        since: Instant,
        timeout: Duration,
    ) -> Result<AudioHit> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let session = from.and_then(|name| self.session_of(name).ok());
            if from.is_none() || session.is_some() {
                let inbox = lock(&self.inbox);
                let peak = inbox.peak_db(session, since, Instant::now());
                let first = inbox
                    .sessions()
                    .into_iter()
                    .filter(|s| session.is_none_or(|x| x == *s))
                    .filter_map(|s| inbox.first_arrival_since(s, since).map(|a| (s, a)))
                    .min_by_key(|(_, a)| *a);
                drop(inbox);
                if peak >= min_db
                    && let Some((s, arrived)) = first
                {
                    return Ok(AudioHit {
                        session: s,
                        level_db: peak,
                        latency: arrived.duration_since(since),
                    });
                }
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "no audio from {} at >= {min_db} dB within {timeout:?} (speakers heard: {:?})",
                    from.unwrap_or("anyone"),
                    self.speakers()
                );
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Waits for the talking state of `session` to become `talking`.
    pub async fn wait_talking(&self, session: u32, talking: bool, timeout: Duration) -> Result<()> {
        let mut rx = self.subscribe_talk();
        if self.is_talking(session) == talking {
            return Ok(());
        }
        tokio::time::timeout(timeout, async {
            loop {
                match rx.recv().await {
                    Ok(e) if e.session == session && e.talking == talking => return Ok(()),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(e) => return Err(anyhow!("talk events ended: {e}")),
                }
            }
        })
        .await
        .map_err(|_| anyhow!("session {session} did not become talking={talking} in {timeout:?}"))?
    }

    /// Records for `duration` from now and writes the result as a 48 kHz mono WAV: one
    /// speaker, or the sum of all.
    pub async fn record_to_wav(
        &self,
        path: &Path,
        session: Option<u32>,
        duration: Duration,
    ) -> Result<()> {
        let from = Instant::now();
        tokio::time::sleep(duration).await;
        wav::save(path, &self.render(session, from, from + duration))
    }
}
