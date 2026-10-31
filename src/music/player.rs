//! The queue and the player: one track at a time, moved to its channel, decoded on a thread and
//! sent as voice in real time (spec 04, FR-4.19 to FR-4.21).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use mumcord_codec::{FRAME_SAMPLES, Frame, SAMPLE_RATE};
use mumcord_protocol::Handle;
use mumcord_protocol::voice::Transport;
use rtrb::{Consumer, RingBuffer};
use serde::Serialize;
use tokio::sync::Notify;
use tokio::time::{Instant, MissedTickBehavior};

use crate::checks::resolve_channel;
use crate::music::config::MusicConfig;
use crate::music::decode;
use crate::music::source::Loaded;
use crate::voice_out::{PacketEncoder, VoiceLink};
use crate::voice_settings::{Route, VoiceSettings};
use crate::webhook::{BotApi, ChannelRef, WebhookConfig};

/// Two seconds of decoded audio between the decode thread and the sender.
const RING_SAMPLES: usize = 2 * SAMPLE_RATE as usize;
/// Audio buffered before the first packet, so a slow start does not stutter.
const PREBUFFER_SAMPLES: usize = SAMPLE_RATE as usize / 2;
/// Longest wait for the prebuffer and for the server to confirm a channel move.
const START_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the player looks for the connection to come back.
const OFFLINE_POLL: Duration = Duration::from_secs(1);

/// One queued track.
#[derive(Debug, Clone)]
pub struct Track {
    pub title: String,
    pub source: &'static str,
    pub channel: u32,
    pub duration: Option<Duration>,
    pub bytes: Arc<[u8]>,
}

impl Track {
    pub fn new(loaded: Loaded, source: &'static str, channel: u32) -> Self {
        Self {
            title: loaded.title,
            source,
            channel,
            duration: loaded.duration,
            bytes: loaded.bytes,
        }
    }
}

/// A track as the API shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrackView {
    pub id: u64,
    pub title: String,
    pub source: &'static str,
    pub channel: u32,
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueView {
    pub now_playing: Option<TrackView>,
    pub queue: Vec<TrackView>,
    pub volume: u8,
}

/// The queue is full (FR-4.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFull;

/// Waiting tracks plus the one playing. Only touched by the HTTP handlers and by the player
/// between tracks, never while a packet is sent.
#[derive(Debug, Default)]
struct Queue {
    waiting: VecDeque<(TrackView, Track)>,
    now: Option<TrackView>,
    next_id: u64,
}

impl Queue {
    /// Adds `track`; `interrupt` puts it first. Returns its id and position (0 = plays next,
    /// right away when nothing plays).
    fn push(
        &mut self,
        track: Track,
        interrupt: bool,
        max: usize,
    ) -> Result<(u64, usize), QueueFull> {
        if self.waiting.len() >= max {
            return Err(QueueFull);
        }
        self.next_id += 1;
        let view = TrackView {
            id: self.next_id,
            title: track.title.clone(),
            source: track.source,
            channel: track.channel,
            duration_secs: track.duration.map(|d| d.as_secs()),
        };
        let position = if interrupt {
            self.waiting.push_front((view, track));
            0
        } else {
            self.waiting.push_back((view, track));
            self.waiting.len() - usize::from(self.now.is_none())
        };
        Ok((self.next_id, position))
    }

    fn next(&mut self) -> Option<Track> {
        let (view, track) = self.waiting.pop_front()?;
        self.now = Some(view);
        Some(track)
    }

    fn view(&self, volume: u8) -> QueueView {
        QueueView {
            now_playing: self.now.clone(),
            queue: self.waiting.iter().map(|(view, _)| view.clone()).collect(),
            volume,
        }
    }
}

/// Shared between the HTTP handlers and the player task. The sender reads `skip` and `volume`
/// as atomics, so a request never blocks it.
pub struct Player {
    queue: Mutex<Queue>,
    max_queue: usize,
    wake: Notify,
    skip: AtomicBool,
    volume: AtomicU8,
}

impl Player {
    pub fn new(config: &MusicConfig) -> Self {
        Self {
            queue: Mutex::new(Queue::default()),
            max_queue: config.max_queue,
            wake: Notify::new(),
            skip: AtomicBool::new(false),
            volume: AtomicU8::new(config.volume.min(100)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn enqueue(&self, track: Track, interrupt: bool) -> Result<(u64, usize), QueueFull> {
        let mut queue = self.lock();
        let added = queue.push(track, interrupt, self.max_queue)?;
        if interrupt && queue.now.is_some() {
            self.skip.store(true, Ordering::Relaxed);
        }
        drop(queue);
        self.wake.notify_one();
        Ok(added)
    }

    pub fn view(&self) -> QueueView {
        self.lock().view(self.volume.load(Ordering::Relaxed))
    }

    pub fn now_playing(&self) -> Option<TrackView> {
        self.lock().now.clone()
    }

    /// Ends the current track; false when nothing plays.
    pub fn skip(&self) -> bool {
        let queue = self.lock();
        let playing = queue.now.is_some();
        if playing {
            self.skip.store(true, Ordering::Relaxed);
        }
        playing
    }

    /// Ends the current track and empties the queue; returns how many tracks were dropped.
    pub fn stop(&self) -> usize {
        let mut queue = self.lock();
        let dropped = queue.waiting.len();
        queue.waiting.clear();
        if queue.now.is_some() {
            self.skip.store(true, Ordering::Relaxed);
        }
        dropped
    }

    pub fn set_volume(&self, percent: u8) {
        self.volume.store(percent.min(100), Ordering::Relaxed);
    }

    /// Takes the next track and marks it as playing; a skip meant for the previous one is
    /// forgotten under the same lock.
    fn next(&self) -> Option<Track> {
        let mut queue = self.lock();
        let track = queue.next();
        self.skip.store(false, Ordering::Relaxed);
        track
    }

    fn finish(&self) {
        self.lock().now = None;
    }
}

/// How a track ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Finished,
    Skipped,
    ConnectionLost,
}

/// Plays the queue until the process stops.
pub async fn run(
    player: Arc<Player>,
    api: Arc<BotApi>,
    config: Arc<WebhookConfig>,
    settings: VoiceSettings,
) -> Result<()> {
    let music = config
        .music
        .as_ref()
        .ok_or_else(|| anyhow!("config has no [music] section"))?;
    let mut link: Option<VoiceLink> = None;
    loop {
        let Some(track) = player.next() else {
            // The queue ran empty: free the voice transport and go home.
            link = None;
            if music.return_home
                && let Some(home) = &config.server.channel
                && let Some(handle) = api.current_handle().await
                && let Err(error) = move_to(&handle, &ChannelRef::Name(home.clone())).await
            {
                tracing::warn!(%error, "music: cannot return to the home channel");
            }
            player.wake.notified().await;
            continue;
        };
        let handle = loop {
            if let Some(handle) = api.current_handle().await {
                break handle;
            }
            tokio::time::sleep(OFFLINE_POLL).await;
        };
        tracing::info!(
            source = track.source,
            channel = track.channel,
            "music: track starts"
        );
        let started = Instant::now();
        let result = play(&player, &api, &config, settings, &handle, &mut link, track).await;
        player.finish();
        if let Err(error) = handle.set_comment(String::new()) {
            tracing::warn!(%error, "music: cannot clear the comment");
        }
        match result {
            Ok(ending) => tracing::info!(
                ?ending,
                seconds = started.elapsed().as_secs(),
                "music: track ended"
            ),
            Err(error) => {
                link = None;
                tracing::warn!(%error, "music: track failed");
            }
        }
    }
}

async fn play(
    player: &Player,
    api: &BotApi,
    config: &WebhookConfig,
    settings: VoiceSettings,
    handle: &Handle,
    link: &mut Option<VoiceLink>,
    track: Track,
) -> Result<Ending> {
    let music = config
        .music
        .as_ref()
        .ok_or_else(|| anyhow!("config has no [music] section"))?;
    move_to(handle, &ChannelRef::Id(track.channel)).await?;
    handle.set_comment(format!(
        "Now playing: {}",
        crate::webhook::markdown::escape_html(&track.title)
    ))?;
    let transport = match link {
        Some(current) if current.transport.is_running() => current,
        slot => {
            // Subscribed before the transport starts, so no crypt resync is missed.
            let events = api.subscribe();
            let (started, mut received) = VoiceLink::start(
                handle.clone(),
                &config.server.host,
                config.server.port,
                events,
            )
            .await?;
            // The bot does not listen; the received voice is dropped so it does not pile up.
            tokio::spawn(async move { while received.recv().await.is_some() {} });
            slot.insert(started)
        }
    };
    let transport = Arc::clone(&transport.transport);
    let route = Route {
        tcp_tunnel: transport.transport() == Transport::Tcp,
        positional: false,
    };
    let mut encoder = PacketEncoder::new(
        settings,
        handle.with_state(|s| s.server.max_bandwidth),
        route,
    )?;

    let (producer, mut consumer) = RingBuffer::new(RING_SAMPLES);
    let cancel = Arc::new(AtomicBool::new(false));
    let decoder = decode::spawn(
        track.bytes,
        music.max_track_len(),
        producer,
        Arc::clone(&cancel),
    )?;
    let ending = async {
        if !prebuffer(player, &mut consumer).await {
            return Ok(Ending::Skipped);
        }
        send(
            player,
            &mut consumer,
            &mut encoder,
            &transport,
            settings.frame_ms,
        )
        .await
    }
    .await;
    cancel.store(true, Ordering::Relaxed);
    drop(consumer);
    let skipped = tokio::task::spawn_blocking(move || decoder.join())
        .await?
        .map_err(|_| anyhow!("the decode thread panicked"))??;
    if skipped > 0 {
        tracing::warn!(skipped, "music: corrupt MP3 frames skipped");
    }
    ending
}

/// Waits until enough audio is decoded (or the track is shorter); false when skipped meanwhile.
async fn prebuffer(player: &Player, consumer: &mut Consumer<f32>) -> bool {
    let deadline = Instant::now() + START_TIMEOUT;
    while consumer.slots() < PREBUFFER_SAMPLES
        && !consumer.is_abandoned()
        && Instant::now() < deadline
    {
        if player.skip.load(Ordering::Relaxed) {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

/// The real-time path: one packet per tick from the ring buffer. No locks, no I/O, no logging and
/// no allocation of its own here; the transport takes the packet without waiting.
async fn send(
    player: &Player,
    consumer: &mut Consumer<f32>,
    encoder: &mut PacketEncoder,
    transport: &mumcord_protocol::voice::VoiceTransport,
    frame_ms: u32,
) -> Result<Ending> {
    let frames_per_packet = encoder.frames_per_packet;
    let per_packet = FRAME_SAMPLES * frames_per_packet;
    let mut frames: [Frame; 2] = [[0.0; FRAME_SAMPLES]; 2];
    let mut ticker = tokio::time::interval(Duration::from_millis(u64::from(frame_ms)));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
    loop {
        ticker.tick().await;
        if !transport.is_running() {
            return Ok(Ending::ConnectionLost);
        }
        if player.skip.load(Ordering::Relaxed) {
            let silence = &mut frames[..frames_per_packet];
            silence.as_flattened_mut().fill(0.0);
            transport.send_audio(encoder.encode(silence, true, None)?);
            return Ok(Ending::Skipped);
        }
        // Checked before the fill level: once the decoder is gone, the level is final.
        let finished = consumer.is_abandoned();
        let available = consumer.slots();
        if available < per_packet && !finished {
            // Underrun: the decoder is late; send nothing rather than a gap of silence.
            continue;
        }
        let take = available.min(per_packet);
        let gain = f32::from(player.volume.load(Ordering::Relaxed)) / 100.0;
        let flat = frames[..frames_per_packet].as_flattened_mut();
        let Ok(chunk) = consumer.read_chunk(take) else {
            bail!("ring buffer holds less than it reported");
        };
        let (first, second) = chunk.as_slices();
        for (out, sample) in flat.iter_mut().zip(first.iter().chain(second)) {
            *out = sample * gain;
        }
        flat[take..].fill(0.0);
        chunk.commit_all();
        let last = finished && available <= per_packet;
        transport.send_audio(encoder.encode(&frames[..frames_per_packet], last, None)?);
        if last {
            return Ok(Ending::Finished);
        }
    }
}

/// Moves the bot into `channel` unless it is there, and waits for the server to confirm.
async fn move_to(handle: &Handle, channel: &ChannelRef) -> Result<()> {
    let (target, current) = handle.with_state(|state| {
        let target = match channel {
            ChannelRef::Id(id) => Ok(*id),
            ChannelRef::Name(name) => resolve_channel(state, name),
        };
        (target, state.own_user().map(|user| user.channel_id))
    });
    let target = target?;
    if current == Some(target) {
        return Ok(());
    }
    handle.join_channel(target)?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if handle.with_state(|s| s.own_user().map(|u| u.channel_id)) == Some(target) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("the server did not move the bot to channel {target} (missing permission?)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> Track {
        Track {
            title: title.to_owned(),
            source: "file",
            channel: 1,
            duration: Some(Duration::from_secs(3)),
            bytes: Arc::from(&b""[..]),
        }
    }

    #[test]
    fn queue_order_positions_and_limit() {
        let mut queue = Queue::default();
        assert_eq!(queue.push(track("a"), false, 2), Ok((1, 0)));
        assert_eq!(queue.next().map(|t| t.title), Some("a".to_owned()));
        assert_eq!(queue.push(track("b"), false, 2), Ok((2, 1)));
        assert_eq!(queue.push(track("c"), false, 2), Ok((3, 2)));
        assert_eq!(queue.push(track("d"), false, 2), Err(QueueFull));
        let view = queue.view(50);
        assert_eq!(view.now_playing.map(|t| t.id), Some(1));
        assert_eq!(view.queue.iter().map(|t| t.id).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(view.volume, 50);
    }

    #[test]
    fn interrupt_goes_first_and_skips_the_current_track() {
        let config: MusicConfig = toml::from_str("").expect("defaults");
        let player = Player::new(&config);
        player.enqueue(track("a"), false).expect("room");
        player.enqueue(track("b"), false).expect("room");
        assert!(!player.skip(), "nothing plays yet");
        assert_eq!(player.next().map(|t| t.title), Some("a".to_owned()));
        assert_eq!(player.enqueue(track("now"), true), Ok((3, 0)));
        assert!(player.skip.load(Ordering::Relaxed));
        assert_eq!(player.next().map(|t| t.title), Some("now".to_owned()));
        assert!(
            !player.skip.load(Ordering::Relaxed),
            "the skip was for the old track"
        );
    }

    #[test]
    fn stop_empties_the_queue_and_volume_is_capped() {
        let config: MusicConfig = toml::from_str("").expect("defaults");
        let player = Player::new(&config);
        player.enqueue(track("a"), false).expect("room");
        player.enqueue(track("b"), false).expect("room");
        player.next();
        assert_eq!(player.stop(), 1);
        assert!(player.skip.load(Ordering::Relaxed));
        player.finish();
        assert_eq!(player.view().queue.len(), 0);
        assert_eq!(player.now_playing(), None);
        player.set_volume(250);
        assert_eq!(player.view().volume, 100);
    }
}
