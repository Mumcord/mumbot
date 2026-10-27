//! WAV in and out, normalised to the engine's format (48 kHz mono f32).

use std::path::Path;

use anyhow::{Context, Result, bail};
use mumcord_audio::{FRAME_SAMPLES, Frame, SAMPLE_RATE};

use super::analysis::duration_of;

/// A clip of mono 48 kHz audio in [-1, 1].
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub samples: Vec<f32>,
}

impl Clip {
    pub fn from_samples(samples: Vec<f32>) -> Self {
        Self { samples }
    }

    /// Reads a WAV file (8-32 bit int or f32, any channel count and rate); down-mixes to mono
    /// and resamples linearly to 48 kHz.
    pub fn load(path: &Path) -> Result<Self> {
        let mut reader =
            hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
        let spec = reader.spec();
        let channels = usize::from(spec.channels.max(1));
        let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Float, 32) => reader
                .samples::<f32>()
                .collect::<Result<_, _>>()
                .context("reading samples")?,
            (hound::SampleFormat::Int, bits @ 8..=32) => {
                let scale = 2f32.powi(i32::from(bits) - 1);
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 / scale))
                    .collect::<Result<_, _>>()
                    .context("reading samples")?
            }
            other => bail!("unsupported WAV format {other:?}"),
        };
        let mono: Vec<f32> = interleaved
            .chunks(channels)
            .map(|c| c.iter().sum::<f32>() / channels as f32)
            .collect();
        Ok(Self::from_samples(resample(&mono, spec.sample_rate)))
    }

    pub fn duration(&self) -> std::time::Duration {
        duration_of(self.samples.len())
    }

    /// 10 ms frames; the tail is zero-padded and the count made even (20 ms packets).
    pub fn frames(&self) -> Vec<Frame> {
        let mut frames: Vec<Frame> = self
            .samples
            .chunks(FRAME_SAMPLES)
            .map(|c| {
                let mut f = [0.0; FRAME_SAMPLES];
                f[..c.len()].copy_from_slice(c);
                f
            })
            .collect();
        if frames.len() % 2 == 1 {
            frames.push([0.0; FRAME_SAMPLES]);
        }
        frames
    }

    /// Writes 48 kHz mono 16-bit PCM.
    pub fn save(&self, path: &Path) -> Result<()> {
        save(path, &self.samples)
    }
}

/// Writes `samples` as 48 kHz mono 16-bit PCM.
pub fn save(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for s in samples {
        writer.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    writer.finalize().context("finishing WAV")
}

fn resample(input: &[f32], rate: u32) -> Vec<f32> {
    if rate == SAMPLE_RATE || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(rate) / f64::from(SAMPLE_RATE);
    let out_len = (input.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let a = pos as usize;
            let frac = (pos - a as f64) as f32;
            let x0 = input[a.min(input.len() - 1)];
            let x1 = input[(a + 1).min(input.len() - 1)];
            x0 + (x1 - x0) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_framing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("x.wav");
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 / 1000.0) - 0.5).collect();
        save(&path, &samples).expect("save");
        let clip = Clip::load(&path).expect("load");
        assert_eq!(clip.samples.len(), 1000);
        assert!((clip.samples[10] - samples[10]).abs() < 1e-3);
        // 1000 samples = 3 frames (padded) -> 4 frames.
        assert_eq!(clip.frames().len(), 4);
    }

    #[test]
    fn resamples_to_48k() {
        let out = resample(&vec![0.25; 16_000], 16_000);
        assert_eq!(out.len(), 48_000);
        assert!((out[100] - 0.25).abs() < 1e-6);
    }
}
