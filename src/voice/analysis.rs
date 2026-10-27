//! Level and timing helpers on mono f32 audio.

use std::time::Duration;

use mumcord_audio::SAMPLE_RATE;

/// Level reported for silence (and empty input), in dBFS.
pub const SILENCE_DB: f32 = -120.0;

/// Linear RMS of `samples` (0 for empty input).
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

/// Linear amplitude to dBFS, floored at [`SILENCE_DB`].
pub fn to_db(linear: f32) -> f32 {
    if linear <= 1e-6 {
        SILENCE_DB
    } else {
        20.0 * linear.log10()
    }
}

/// RMS of `samples` in dBFS.
pub fn rms_db(samples: &[f32]) -> f32 {
    to_db(rms(samples))
}

/// Samples in `ms` milliseconds at 48 kHz.
pub fn samples_for(ms: u32) -> usize {
    (SAMPLE_RATE as usize * ms as usize) / 1000
}

/// RMS level (dBFS) of consecutive non-overlapping windows of `window_ms`; a trailing partial
/// window is included.
pub fn levels_db(samples: &[f32], window_ms: u32) -> Vec<f32> {
    samples
        .chunks(samples_for(window_ms).max(1))
        .map(rms_db)
        .collect()
}

/// Loudest window of `window_ms` in dBFS.
pub fn peak_window_db(samples: &[f32], window_ms: u32) -> f32 {
    levels_db(samples, window_ms)
        .into_iter()
        .fold(SILENCE_DB, f32::max)
}

/// Total duration of windows (of `window_ms`) whose level is at least `threshold_db`.
pub fn active_duration(samples: &[f32], window_ms: u32, threshold_db: f32) -> Duration {
    let windows = levels_db(samples, window_ms)
        .into_iter()
        .filter(|l| *l >= threshold_db)
        .count();
    Duration::from_millis(u64::from(window_ms) * windows as u64)
}

/// Duration of `samples` at 48 kHz.
pub fn duration_of(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(SAMPLE_RATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        let full = vec![1.0_f32; 4800];
        assert!(rms_db(&full).abs() < 1e-3);
        let half = vec![0.5_f32; 4800];
        assert!((rms_db(&half) + 6.02).abs() < 0.01);
        assert_eq!(rms_db(&[]), SILENCE_DB);
        let mut x = vec![0.0; 4800];
        x.extend(&full);
        assert_eq!(levels_db(&x, 100).len(), 2);
        assert_eq!(active_duration(&x, 100, -20.0), Duration::from_millis(100));
        assert!(peak_window_db(&x, 100) > -1.0);
    }
}
