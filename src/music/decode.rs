//! The decode thread: MP3 to 48 kHz mono samples in a lock-free ring buffer, so the voice sender
//! never waits for the decoder (spec 04, FR-4.19).

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use mumcord_codec::SAMPLE_RATE;
use rtrb::Producer;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// How long the decoder sleeps when the ring buffer is full: far below the buffered time.
const FULL_WAIT: Duration = Duration::from_millis(5);

/// Starts decoding `bytes` into `out`; stops after `max_len`, at the end of the track or when
/// `cancel` is set. The thread drops `out` when it ends, which tells the sender the track is
/// over. Returns how many corrupt packets it skipped.
pub fn spawn(
    bytes: Arc<[u8]>,
    max_len: Duration,
    out: Producer<f32>,
    cancel: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<u64>>> {
    std::thread::Builder::new()
        .name("music-decode".to_owned())
        .spawn(move || decode(bytes, max_len, out, &cancel))
        .context("starting the decode thread")
}

fn decode(
    bytes: Arc<[u8]>,
    max_len: Duration,
    mut out: Producer<f32>,
    cancel: &AtomicBool,
) -> Result<u64> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let mut format = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("not an MP3 file: {e}"))?
        .format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow!("the file has no audio track"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("no decoder: {e}"))?;

    let mut left = u64::from(SAMPLE_RATE).saturating_mul(max_len.as_secs());
    let mut skipped = 0u64;
    let mut samples: Option<SampleBuffer<f32>> = None;
    let mut resampler: Option<Resampler> = None;
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
    while left > 0 && !cancel.load(Ordering::Relaxed) {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(anyhow!("reading the track: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(Error::DecodeError(_)) => {
                skipped += 1;
                continue;
            }
            Err(e) => return Err(anyhow!("decoding the track: {e}")),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let buffer = match &mut samples {
            Some(buffer) if buffer.capacity() >= decoded.capacity() * channels => buffer,
            slot => slot.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
        };
        buffer.copy_interleaved_ref(decoded);
        mono.clear();
        mono.extend(
            buffer
                .samples()
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        let resampler = match &mut resampler {
            Some(r) if r.input_rate == spec.rate => r,
            slot => slot.insert(Resampler::new(spec.rate)),
        };
        resampled.clear();
        resampler.process(&mono, &mut resampled);
        let take = resampled
            .len()
            .min(usize::try_from(left).unwrap_or(usize::MAX));
        left -= take as u64;
        if !push_all(&mut out, &resampled[..take], cancel) {
            break;
        }
    }
    Ok(skipped)
}

/// Pushes every sample, waiting while the sender catches up; false when cancelled or the sender
/// is gone.
fn push_all(out: &mut Producer<f32>, mut samples: &[f32], cancel: &AtomicBool) -> bool {
    while !samples.is_empty() {
        if cancel.load(Ordering::Relaxed) || out.is_abandoned() {
            return false;
        }
        let free = out.slots().min(samples.len());
        if free == 0 {
            std::thread::sleep(FULL_WAIT);
            continue;
        }
        let Ok(mut chunk) = out.write_chunk(free) else {
            continue;
        };
        let (first, second) = chunk.as_mut_slices();
        let split = first.len();
        first.copy_from_slice(&samples[..split]);
        second.copy_from_slice(&samples[split..free]);
        chunk.commit_all();
        samples = &samples[free..];
    }
    true
}

/// Linear interpolation from the track's rate to 48 kHz, continuous across packets. An output
/// sample needs the input sample after it, so the output runs one input sample behind.
struct Resampler {
    input_rate: u32,
    /// Input samples per output sample.
    step: f64,
    /// Read position: 0 is `previous`, 1 the first sample of the next input.
    position: f64,
    previous: f32,
}

impl Resampler {
    fn new(input_rate: u32) -> Self {
        Self {
            input_rate,
            step: f64::from(input_rate.max(1)) / f64::from(SAMPLE_RATE),
            position: 1.0,
            previous: 0.0,
        }
    }

    fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let Some(&last) = input.last() else {
            return;
        };
        let len = input.len() as f64;
        while self.position < len {
            // `position` is at least 0 and below `len`, so `index` is a valid input index.
            let index = self.position as usize;
            let fraction = (self.position - index as f64) as f32;
            let a = if index == 0 {
                self.previous
            } else {
                input[index - 1]
            };
            let b = input[index];
            out.push(a + (b - a) * fraction);
            self.position += self.step;
        }
        self.position -= len;
        self.previous = last;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_passes_samples_through_one_sample_late() {
        let mut resampler = Resampler::new(48_000);
        let mut out = Vec::new();
        resampler.process(&[0.1, 0.2, 0.3], &mut out);
        resampler.process(&[0.4], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn resampling_keeps_the_length_and_is_continuous_across_packets() {
        let input: Vec<f32> = (0..44_100).map(|i| i as f32 / 44_100.0).collect();
        let mut resampler = Resampler::new(44_100);
        let mut out = Vec::new();
        for chunk in input.chunks(1152) {
            resampler.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 1, "{}", out.len());
        // A ramp stays a ramp: no jumps at packet borders.
        let largest_step = out.windows(2).map(|w| w[1] - w[0]).fold(0.0f32, f32::max);
        assert!(largest_step < 2.0 / 44_100.0, "{largest_step}");
    }

    #[test]
    fn a_full_buffer_waits_and_a_cancel_ends_the_push() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(4);
        let cancel = AtomicBool::new(true);
        assert!(!push_all(&mut producer, &[1.0; 8], &cancel));
        cancel.store(false, Ordering::Relaxed);
        assert!(push_all(&mut producer, &[1.0, 2.0, 3.0], &cancel));
        assert_eq!(consumer.pop(), Ok(1.0));
    }
}
