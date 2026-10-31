//! Voice: playing a WAV file, recording, level checks and the positional-audio game context.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Subcommand;
use mumbot::voice::PlayOptions;

use super::{Session, secs};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Play a WAV file as voice into the current channel (terminator sent at the end).
    Play {
        file: PathBuf,
        /// Repeat until --seconds is over (forever without it).
        #[arg(long = "loop")]
        looping: bool,
        /// Stop after this many seconds.
        #[arg(long)]
        seconds: Option<f64>,
        /// Send this avatar position "x,y,z" with every packet (positional audio).
        #[arg(long, value_parser = parse_position, allow_hyphen_values = true)]
        position: Option<[f32; 3]>,
    },
    /// Announce a Mumble Link game context (like a game running next to the client), so people
    /// in the same game hear this bot's positional voice placed; an empty context means no game.
    GameContext {
        context: String,
        #[arg(long, default_value = "")]
        identity: String,
    },
    /// Record received voice for --seconds into a 48 kHz mono WAV (sum of all speakers, or
    /// only --from).
    Record {
        out: PathBuf,
        #[arg(long)]
        seconds: f64,
        #[arg(long)]
        from: Option<String>,
    },
    /// Wait until a user's voice reaches --min-rms dBFS (100 ms window); prints
    /// "<level dB> <latency ms>". Only audio after this command (or the first voice command of
    /// a script) is considered, so start it before the sender plays.
    ExpectAudio {
        #[arg(long)]
        from: Option<String>,
        #[arg(long, default_value_t = -40.0, allow_negative_numbers = true)]
        min_rms: f32,
        /// Seconds to wait (default: the global --timeout).
        #[arg(long)]
        timeout: Option<f64>,
    },
}

pub async fn run(session: &mut Session, command: Command) -> Result<()> {
    match command {
        Command::GameContext { context, identity } => {
            // The official client prefixes the context with the plugin name ("Link\0").
            let context = if context.is_empty() {
                Vec::new()
            } else {
                [b"Link\0".as_slice(), context.as_bytes()].concat()
            };
            session.bot.handle().set_plugin_context(context, identity)?;
        }
        Command::Play {
            file,
            looping,
            seconds,
            position,
        } => {
            let opts = PlayOptions {
                forever: looping,
                max_duration: seconds.map(secs).transpose()?,
                position,
                settings: session.voice_settings,
                ..Default::default()
            };
            let report = session.voice().await?.play_file(&file, opts).await?;
            println!(
                "played {} packets in {} ms",
                report.packets,
                report.finished.duration_since(report.started).as_millis()
            );
        }
        Command::Record { out, seconds, from } => {
            let v = session.voice().await?;
            let speaker = from.as_deref().map(|n| v.session_of(n)).transpose()?;
            v.record_to_wav(&out, speaker, secs(seconds)?).await?;
            println!("recorded {seconds} s to {}", out.display());
        }
        Command::ExpectAudio {
            from,
            min_rms,
            timeout,
        } => {
            let wait = match timeout {
                Some(t) => secs(t)?,
                None => session.bot.config().timeout,
            };
            let hit = session
                .voice()
                .await?
                .wait_audio(from.as_deref(), min_rms, std::time::Instant::now(), wait)
                .await?;
            println!("{:.1} {}", hit.level_db, hit.latency.as_millis());
        }
    }
    Ok(())
}

fn parse_position(text: &str) -> Result<[f32; 3]> {
    let parts = text
        .split(',')
        .map(|p| p.trim().parse::<f32>().context("not a number"))
        .collect::<Result<Vec<_>>>()?;
    <[f32; 3]>::try_from(parts)
        .ok()
        .context("give three numbers \"x,y,z\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_positions() {
        assert_eq!(
            parse_position("-3, 0,1.5").expect("position"),
            [-3.0, 0.0, 1.5]
        );
        assert!(parse_position("1,2").is_err());
        assert!(parse_position("1,2,x").is_err());
    }
}
