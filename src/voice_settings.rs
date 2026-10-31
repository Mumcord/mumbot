//! Voice quality settings of every bot that sends voice: Opus bitrate and packet length.
//!
//! The value of a setting comes from the first layer that sets it: command line, environment,
//! the module's own override (e.g. `[music]`), the shared `[voice]` section, the default. The
//! server's bandwidth limit then caps the result at the time voice is sent ([`SendRate`]).
//! Spec: issue Mumcord/mumbot#7.

use std::fmt;

use anyhow::{Result, bail};
use serde::Deserialize;

/// Environment variable that sets the bitrate for one run (below the command line).
pub const ENV_BITRATE: &str = "MUMBOT_VOICE_BITRATE";
/// Environment variable that sets the packet length for one run (below the command line).
pub const ENV_FRAME_MS: &str = "MUMBOT_VOICE_FRAME_MS";

/// Mumcord's default bitrate (bit/s), so a bot sounds like a default client.
pub const DEFAULT_BITRATE_BPS: u32 = 96_000;
/// Default packet length in milliseconds.
pub const DEFAULT_FRAME_MS: u32 = 20;
/// Opus limits (bit/s).
pub const MIN_BITRATE_BPS: u32 = 6_000;
pub const MAX_BITRATE_BPS: u32 = 510_000;

/// One layer of settings: every field that is set overrides the layers below.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceConfig {
    /// Opus bitrate in bit/s (6000 to 510000).
    pub bitrate: Option<u32>,
    /// Packet length in milliseconds: 10 or 20 (the codec's packet sizes).
    pub frame_ms: Option<u32>,
}

impl VoiceConfig {
    /// Reads the layer from environment variables; `lookup` is `std::env::var(..).ok()` in the
    /// binary and a table in tests. An unparsable value is an error, never ignored.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let number = |name: &str| -> Result<Option<u32>> {
            lookup(name)
                .map(|text| {
                    text.trim()
                        .parse()
                        .map_err(|_| anyhow::anyhow!("{name} must be a whole number, got {text:?}"))
                })
                .transpose()
        };
        let layer = Self {
            bitrate: number(ENV_BITRATE)?,
            frame_ms: number(ENV_FRAME_MS)?,
        };
        layer.validate(" (from the environment)")?;
        Ok(layer)
    }

    /// Checks the set fields; `origin` names where they came from in the error.
    pub fn validate(&self, origin: &str) -> Result<()> {
        if let Some(bitrate) = self.bitrate
            && !(MIN_BITRATE_BPS..=MAX_BITRATE_BPS).contains(&bitrate)
        {
            bail!(
                "voice bitrate {bitrate}{origin} is out of range: Opus accepts \
                 {MIN_BITRATE_BPS} to {MAX_BITRATE_BPS} bit/s"
            );
        }
        if let Some(frame_ms) = self.frame_ms
            && ![10, 20].contains(&frame_ms)
        {
            bail!("voice frame_ms {frame_ms}{origin} is not supported: use 10 or 20");
        }
        Ok(())
    }
}

/// The settings a module asked for, before the server's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceSettings {
    pub bitrate_bps: u32,
    pub frame_ms: u32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            bitrate_bps: DEFAULT_BITRATE_BPS,
            frame_ms: DEFAULT_FRAME_MS,
        }
    }
}

impl VoiceSettings {
    /// Applies the precedence cli > env > module > voice > default and validates the result.
    pub fn resolve(
        cli: VoiceConfig,
        env: VoiceConfig,
        module: Option<VoiceConfig>,
        voice: VoiceConfig,
    ) -> Result<Self> {
        let layers = [cli, env, module.unwrap_or_default(), voice];
        for (layer, origin) in layers.iter().zip([
            " (command line)",
            " (environment)",
            " (module section)",
            " ([voice])",
        ]) {
            layer.validate(origin)?;
        }
        let defaults = Self::default();
        Ok(Self {
            bitrate_bps: layers
                .iter()
                .find_map(|l| l.bitrate)
                .unwrap_or(defaults.bitrate_bps),
            frame_ms: layers
                .iter()
                .find_map(|l| l.frame_ms)
                .unwrap_or(defaults.frame_ms),
        })
    }

    /// Applies the server's bandwidth limit for the way voice is sent right now.
    pub fn capped(self, server_max_bps: Option<u32>, route: Route) -> SendRate {
        let overhead = route.overhead_bps(self.frame_ms);
        let allowed = server_max_bps.map(|max| max.saturating_sub(overhead));
        let bitrate_bps = match allowed {
            Some(allowed) if allowed < self.bitrate_bps => allowed.max(MIN_BITRATE_BPS),
            _ => self.bitrate_bps,
        };
        SendRate {
            configured_bps: self.bitrate_bps,
            bitrate_bps,
            server_max_bps,
            overhead_bps: overhead,
        }
    }
}

/// How voice packets travel; decides the header overhead the server counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// Tunnelled through the TLS control connection instead of UDP.
    pub tcp_tunnel: bool,
    /// Packets carry an avatar position.
    pub positional: bool,
}

impl Route {
    /// Bits per second of headers for `frame_ms` packets: the formula of the official Mumble
    /// client (IP, UDP, crypt and voice headers, plus the tunnel and position extras).
    pub fn overhead_bps(self, frame_ms: u32) -> u32 {
        let frames = (frame_ms / 10).max(1);
        let mut bytes = 20 + 8 + 4 + 1 + 2 + frames;
        if self.tcp_tunnel {
            bytes += 12;
        }
        if self.positional {
            bytes += 12;
        }
        bytes * 800 / frames
    }
}

/// The bitrate a voice stream really uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendRate {
    pub configured_bps: u32,
    pub bitrate_bps: u32,
    pub server_max_bps: Option<u32>,
    pub overhead_bps: u32,
}

impl SendRate {
    /// The server's limit lowered the configured bitrate.
    pub fn is_capped(&self) -> bool {
        self.bitrate_bps < self.configured_bps
    }

    /// Logs the warning for a lowered bitrate; the values are the ones that were used.
    pub fn log_cap(&self) {
        if self.is_capped() {
            tracing::warn!(
                configured_bps = self.configured_bps,
                effective_bps = self.bitrate_bps,
                server_max_bps = ?self.server_max_bps,
                overhead_bps = self.overhead_bps,
                "voice bitrate lowered to fit the server's bandwidth limit"
            );
        }
    }
}

impl fmt::Display for SendRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} bit/s", self.bitrate_bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UDP: Route = Route {
        tcp_tunnel: false,
        positional: false,
    };

    fn bitrate(bps: u32) -> VoiceConfig {
        VoiceConfig {
            bitrate: Some(bps),
            frame_ms: None,
        }
    }

    fn resolve(cli: Option<u32>, env: Option<u32>, module: Option<u32>, voice: Option<u32>) -> u32 {
        let layer = |v: Option<u32>| v.map_or(VoiceConfig::default(), bitrate);
        VoiceSettings::resolve(layer(cli), layer(env), module.map(bitrate), layer(voice))
            .expect("valid")
            .bitrate_bps
    }

    #[test]
    fn each_layer_beats_the_ones_below_it() {
        assert_eq!(
            resolve(Some(10_000), Some(20_000), Some(30_000), Some(40_000)),
            10_000
        );
        assert_eq!(
            resolve(None, Some(20_000), Some(30_000), Some(40_000)),
            20_000
        );
        assert_eq!(resolve(None, None, Some(30_000), Some(40_000)), 30_000);
        assert_eq!(resolve(None, None, None, Some(40_000)), 40_000);
        assert_eq!(resolve(None, None, None, None), DEFAULT_BITRATE_BPS);
    }

    #[test]
    fn frame_length_has_its_own_precedence() {
        let voice = VoiceConfig {
            bitrate: Some(64_000),
            frame_ms: Some(10),
        };
        let cli = VoiceConfig {
            bitrate: None,
            frame_ms: Some(20),
        };
        let got = VoiceSettings::resolve(cli, VoiceConfig::default(), None, voice).expect("valid");
        assert_eq!((got.bitrate_bps, got.frame_ms), (64_000, 20));
    }

    #[test]
    fn environment_layer_is_read_and_checked() {
        let env = |name: &str| (name == ENV_BITRATE).then(|| " 48000 ".to_string());
        assert_eq!(VoiceConfig::from_env(env).expect("ok"), bitrate(48_000));
        let bad = |name: &str| (name == ENV_BITRATE).then(|| "fast".to_string());
        let error = VoiceConfig::from_env(bad)
            .expect_err("not a number")
            .to_string();
        assert!(error.contains(ENV_BITRATE), "{error}");
        let low = |name: &str| (name == ENV_BITRATE).then(|| "100".to_string());
        assert!(VoiceConfig::from_env(low).is_err());
    }

    #[test]
    fn invalid_values_are_refused_with_their_origin() {
        for bad in [5_999, 510_001, 0] {
            let error = VoiceSettings::resolve(
                VoiceConfig::default(),
                VoiceConfig::default(),
                None,
                bitrate(bad),
            )
            .expect_err("out of range")
            .to_string();
            assert!(
                error.contains("[voice]") && error.contains("6000"),
                "{error}"
            );
        }
        for ms in [0, 5, 40, 60] {
            let cli = VoiceConfig {
                bitrate: None,
                frame_ms: Some(ms),
            };
            let result =
                VoiceSettings::resolve(cli, VoiceConfig::default(), None, VoiceConfig::default());
            assert!(result.is_err(), "frame_ms {ms}");
        }
        assert!(resolve_ok(MIN_BITRATE_BPS) && resolve_ok(MAX_BITRATE_BPS));
    }

    fn resolve_ok(bps: u32) -> bool {
        VoiceSettings::resolve(
            VoiceConfig::default(),
            VoiceConfig::default(),
            None,
            bitrate(bps),
        )
        .is_ok()
    }

    #[test]
    fn overhead_matches_the_mumble_client_formula() {
        assert_eq!(UDP.overhead_bps(20), 14_800);
        assert_eq!(UDP.overhead_bps(10), 28_800);
        let tunnel_positional = Route {
            tcp_tunnel: true,
            positional: true,
        };
        assert_eq!(tunnel_positional.overhead_bps(20), 24_400);
    }

    #[cfg(feature = "tester")]
    #[test]
    fn overhead_agrees_with_the_codec() {
        use mumcord_codec::bitrate::{Transport, network_overhead_bps};
        for (frame_ms, tcp_tunnel, positional) in
            [(10, false, false), (20, true, false), (20, true, true)]
        {
            let route = Route {
                tcp_tunnel,
                positional,
            };
            let transport = if tcp_tunnel {
                Transport::TcpTunnel
            } else {
                Transport::Udp
            };
            assert_eq!(
                route.overhead_bps(frame_ms),
                network_overhead_bps(frame_ms / 10, transport, positional)
            );
        }
    }

    #[test]
    fn no_server_limit_keeps_the_configured_bitrate() {
        let rate = VoiceSettings::default().capped(None, UDP);
        assert_eq!(rate.bitrate_bps, DEFAULT_BITRATE_BPS);
        assert!(!rate.is_capped());
    }

    #[test]
    fn a_roomy_server_limit_does_not_lower_the_bitrate() {
        let rate = VoiceSettings::default().capped(Some(558_000), UDP);
        assert!(!rate.is_capped());
        // Exactly bitrate plus overhead still fits.
        let rate = VoiceSettings::default().capped(Some(96_000 + 14_800), UDP);
        assert!(!rate.is_capped());
    }

    #[test]
    fn the_server_limit_wins_including_packet_overhead() {
        // Murmur's default of 72000 bit/s leaves 72000 - 14800 for the audio.
        let rate = VoiceSettings::default().capped(Some(72_000), UDP);
        assert_eq!(rate.bitrate_bps, 57_200);
        assert!(rate.is_capped() && rate.configured_bps == DEFAULT_BITRATE_BPS);
        assert!(rate.bitrate_bps + rate.overhead_bps <= 72_000);
        // Shorter packets and positional data cost more overhead, so the audio gets less.
        let settings = VoiceSettings {
            bitrate_bps: 96_000,
            frame_ms: 10,
        };
        let route = Route {
            tcp_tunnel: true,
            positional: true,
        };
        let rate = settings.capped(Some(72_000), route);
        assert_eq!(rate.overhead_bps, route.overhead_bps(10));
        assert!(rate.bitrate_bps + rate.overhead_bps <= 72_000);
    }

    #[test]
    fn a_tiny_server_limit_never_goes_below_the_opus_minimum() {
        let rate = VoiceSettings::default().capped(Some(10_000), UDP);
        assert_eq!(rate.bitrate_bps, MIN_BITRATE_BPS);
    }

    #[test]
    fn config_section_rejects_unknown_keys() {
        let ok: VoiceConfig = toml::from_str("bitrate = 64000\nframe_ms = 20").expect("parses");
        assert_eq!(ok.bitrate, Some(64_000));
        assert!(toml::from_str::<VoiceConfig>("bitrates = 1").is_err());
    }
}
