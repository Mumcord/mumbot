//! Receive side: decodes voice per speaker into a timeline and observes talking state.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use mumcord_audio::FRAME_SAMPLES;
use mumcord_audio::codec::{MAX_DECODED_SAMPLES, OpusDecoder};
use mumcord_protocol::VoicePacket;

use super::analysis::{duration_of, peak_window_db};

/// A speaker without packets for this long counts as stopped talking (when no terminator came).
pub const TALK_IDLE: Duration = Duration::from_millis(300);
/// Longest run of lost 10 ms frames that is concealed (longer gaps stay silent).
const MAX_CONCEAL_FRAMES: u64 = 10;
const FRAME_10MS: Duration = Duration::from_millis(10);

/// Start or stop of received voice from one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalkEvent {
    pub session: u32,
    pub talking: bool,
    /// When the first packet (start) or the terminator/idle timeout (stop) was observed.
    pub at: Instant,
}

/// Decoded audio of one packet, placed on the speaker's timeline.
#[derive(Debug, Clone)]
pub(super) struct Chunk {
    /// Ideal playout time (spurt start plus sequence offset): immune to network jitter.
    pub at: Instant,
    /// When the packet actually arrived.
    pub arrived: Instant,
    pub samples: Vec<f32>,
}

struct Speaker {
    decoder: OpusDecoder,
    chunks: Vec<Chunk>,
    talking: bool,
    last_arrival: Instant,
    next_seq: u64,
    /// Arrival time and sequence of the first packet of the current spurt.
    spurt: Option<(Instant, u64)>,
    packets: u64,
}

#[derive(Default)]
pub(super) struct Inbox {
    speakers: BTreeMap<u32, Speaker>,
    pub talk_log: Vec<TalkEvent>,
    pub decode_errors: u64,
}

impl Inbox {
    /// Decodes `packet` and returns talking-state changes it caused.
    pub fn ingest(&mut self, packet: &VoicePacket, now: Instant) -> Vec<TalkEvent> {
        let speaker = match self.speakers.entry(packet.session) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => match OpusDecoder::new() {
                Ok(decoder) => e.insert(Speaker {
                    decoder,
                    chunks: Vec::new(),
                    talking: false,
                    last_arrival: now,
                    next_seq: 0,
                    spurt: None,
                    packets: 0,
                }),
                Err(_) => {
                    self.decode_errors += 1;
                    return Vec::new();
                }
            },
        };
        let new_spurt = match speaker.spurt {
            None => true,
            Some(_) => {
                !speaker.talking
                    || now.duration_since(speaker.last_arrival) > TALK_IDLE
                    || packet.seq < speaker.next_seq.saturating_sub(MAX_CONCEAL_FRAMES)
            }
        };
        if new_spurt {
            speaker.spurt = Some((now, packet.seq));
            speaker.next_seq = packet.seq;
            // A fresh decoder state avoids bleeding the previous spurt's tail into this one.
            let _ = speaker.decoder.reset();
        }
        speaker.last_arrival = now;
        speaker.packets += 1;
        let (spurt_at, spurt_seq) = speaker.spurt.unwrap_or((now, packet.seq));
        let at_of = |seq: u64| spurt_at + FRAME_10MS * seq.saturating_sub(spurt_seq) as u32;

        let gap = packet.seq.saturating_sub(speaker.next_seq);
        if !new_spurt && gap > 0 && gap <= MAX_CONCEAL_FRAMES {
            let mut out = vec![0.0; gap as usize * FRAME_SAMPLES];
            if speaker.decoder.conceal(&mut out).is_ok() {
                speaker.chunks.push(Chunk {
                    at: at_of(speaker.next_seq),
                    arrived: now,
                    samples: out,
                });
            }
        }
        if !packet.payload.is_empty() {
            let mut out = vec![0.0; MAX_DECODED_SAMPLES];
            match speaker.decoder.decode(&packet.payload, &mut out) {
                Ok(n) => {
                    out.truncate(n);
                    speaker.next_seq = packet.seq + (n / FRAME_SAMPLES) as u64;
                    speaker.chunks.push(Chunk {
                        at: at_of(packet.seq),
                        arrived: now,
                        samples: out,
                    });
                }
                Err(_) => self.decode_errors += 1,
            }
        }
        let mut events = Vec::new();
        let ev = |talking| TalkEvent {
            session: packet.session,
            talking,
            at: now,
        };
        if !speaker.talking {
            speaker.talking = true;
            events.push(ev(true));
        }
        if packet.is_last {
            speaker.talking = false;
            speaker.spurt = None;
            events.push(ev(false));
        }
        self.talk_log.extend(events.iter().copied());
        events
    }

    /// Stops speakers that went quiet without a terminator.
    pub fn expire_idle(&mut self, now: Instant) -> Vec<TalkEvent> {
        let mut events = Vec::new();
        for (session, speaker) in &mut self.speakers {
            if speaker.talking && now.duration_since(speaker.last_arrival) > TALK_IDLE {
                speaker.talking = false;
                speaker.spurt = None;
                events.push(TalkEvent {
                    session: *session,
                    talking: false,
                    at: speaker.last_arrival + TALK_IDLE,
                });
            }
        }
        self.talk_log.extend(events.iter().copied());
        events
    }

    pub fn sessions(&self) -> Vec<u32> {
        self.speakers.keys().copied().collect()
    }

    pub fn is_talking(&self, session: u32) -> bool {
        self.speakers.get(&session).is_some_and(|s| s.talking)
    }

    pub fn packet_count(&self, session: u32) -> u64 {
        self.speakers.get(&session).map_or(0, |s| s.packets)
    }

    /// Arrival time of the first packet from `session` at or after `since`.
    pub fn first_arrival_since(&self, session: u32, since: Instant) -> Option<Instant> {
        self.speakers
            .get(&session)?
            .chunks
            .iter()
            .map(|c| c.arrived)
            .find(|a| *a >= since)
    }

    /// Renders `session` (or the sum of all speakers) on a timeline starting at `from` and
    /// ending at `to`; silence where nothing was received.
    pub fn render(&self, session: Option<u32>, from: Instant, to: Instant) -> Vec<f32> {
        let len = (to.saturating_duration_since(from).as_secs_f64()
            * f64::from(mumcord_audio::SAMPLE_RATE)) as usize;
        let mut out = vec![0.0; len];
        for (id, speaker) in &self.speakers {
            if session.is_some_and(|s| s != *id) {
                continue;
            }
            for chunk in &speaker.chunks {
                if chunk.at < from {
                    continue;
                }
                let start = (chunk.at.duration_since(from).as_secs_f64()
                    * f64::from(mumcord_audio::SAMPLE_RATE)) as usize;
                for (slot, s) in out.iter_mut().skip(start).zip(&chunk.samples) {
                    *slot += s;
                }
            }
        }
        out
    }

    /// Loudest 100 ms window of `session` (or everyone) in `from..to`, dBFS.
    pub fn peak_db(&self, session: Option<u32>, from: Instant, to: Instant) -> f32 {
        peak_window_db(&self.render(session, from, to), 100)
    }

    pub fn total_duration(&self, session: u32) -> Duration {
        duration_of(
            self.speakers
                .get(&session)
                .map_or(0, |s| s.chunks.iter().map(|c| c.samples.len()).sum()),
        )
    }
}
