//! Sending voice, shared by the music module and the test bot: the voice transport of one
//! connection and the Opus packet encoder at the configured bitrate (issue #7).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use bytes::BytesMut;
use mumcord_codec::Frame;
use mumcord_codec::codec::{MAX_PACKET_BYTES, OpusEncoder, PacketDuration};
use mumcord_protocol::voice::{OutgoingAudio, VoiceEvent, VoiceTransport};
use mumcord_protocol::{Event, Handle};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::voice_settings::{Route, SendRate, VoiceSettings};

/// Packets share one buffer until it runs low, so the steady state allocates nothing per packet.
const ARENA_BYTES: usize = 64 * 1024;

/// The voice transport of a connection plus the task that feeds it the connection's events.
pub struct VoiceLink {
    pub transport: Arc<VoiceTransport>,
    forward: JoinHandle<()>,
}

impl Drop for VoiceLink {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

impl VoiceLink {
    /// Starts voice against `host:port` for the connection behind `handle`. `events` must be a
    /// subscription to that connection's events, taken before this call, so no `CryptSetup` or
    /// tunnelled packet is missed. Returns the received voice as well.
    pub async fn start(
        handle: Handle,
        host: &str,
        port: u16,
        mut events: broadcast::Receiver<Event>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<VoiceEvent>)> {
        let addr: SocketAddr = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("resolving {host}"))?
            .next()
            .ok_or_else(|| anyhow!("{host} has no address"))?;
        let (transport, received) = VoiceTransport::start(handle, addr)
            .await
            .context("starting voice transport")?;
        let transport = Arc::new(transport);
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
        Ok((Self { transport, forward }, received))
    }
}

/// Encodes 10 ms frames into Opus packets of the configured length and bitrate.
pub struct PacketEncoder {
    encoder: OpusEncoder,
    /// 10 ms frames per packet: 1 or 2.
    pub frames_per_packet: usize,
    /// The bitrate the encoder really runs at.
    pub bitrate_bps: u32,
    buf: [u8; MAX_PACKET_BYTES],
    arena: BytesMut,
}

impl PacketEncoder {
    /// An encoder for `settings`, lowered to the server's bandwidth limit for `route` (logged when
    /// it lowers the bitrate).
    pub fn new(settings: VoiceSettings, server_max_bps: Option<u32>, route: Route) -> Result<Self> {
        let (duration, frames_per_packet) = match settings.frame_ms {
            10 => (PacketDuration::Ms10, 1),
            _ => (PacketDuration::Ms20, 2),
        };
        let rate: SendRate = settings.capped(server_max_bps, route);
        rate.log_cap();
        let bitrate = i32::try_from(rate.bitrate_bps).context("bitrate")?;
        let mut encoder =
            OpusEncoder::new(bitrate, duration).map_err(|e| anyhow!("encoder: {e}"))?;
        let bitrate_bps = u32::try_from(encoder.bitrate().map_err(|e| anyhow!("encoder: {e}"))?)
            .context("encoder bitrate")?;
        Ok(Self {
            encoder,
            frames_per_packet,
            bitrate_bps,
            buf: [0; MAX_PACKET_BYTES],
            arena: BytesMut::with_capacity(ARENA_BYTES),
        })
    }

    /// Encodes one packet's frames (`frames_per_packet` of them); `last` sets the terminator.
    /// Allocates only when the shared packet buffer runs low and the transport still holds the
    /// packets cut from it.
    pub fn encode(
        &mut self,
        frames: &[Frame],
        last: bool,
        position: Option<[f32; 3]>,
    ) -> Result<OutgoingAudio> {
        let mut len = None;
        let count = u8::try_from(frames.len()).context("frames per packet")?;
        for frame in frames {
            len = self
                .encoder
                .push_frame(frame, &mut self.buf)
                .map_err(|e| anyhow!("encode: {e}"))?;
        }
        let len = len.ok_or_else(|| anyhow!("encoder produced no packet"))?;
        if self.arena.capacity() < MAX_PACKET_BYTES {
            // Reclaims the old buffer in place once every packet cut from it was sent.
            self.arena.reserve(ARENA_BYTES);
        }
        self.arena.extend_from_slice(&self.buf[..len]);
        Ok(OutgoingAudio::new(self.arena.split().freeze(), count, last).with_position(position))
    }
}
