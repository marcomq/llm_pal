//! Evens out the TTS loudness while it streams: Pocket TTS speaks quietly (about -30 dBFS) and varies by up to
//! 15 dB from one sentence to the next. The gain follows each chunk's level smoothly; a soft limiter catches peaks.

const TARGET_DB: f32 = -20.0;
const START_GAIN_DB: f32 = 10.0;
const MIN_GAIN_DB: f32 = -6.0;
const MAX_GAIN_DB: f32 = 24.0;
/// Chunks quieter than this are pauses and leave the gain alone.
const SILENCE_DB: f32 = -55.0;
/// Share of the way to the wanted gain per chunk (Pocket's chunks are 80 ms); faster down than up.
const RISE: f32 = 0.3;
const FALL: f32 = 0.6;
/// The limiter leaves samples below this untouched.
const KNEE: f32 = 0.7;

pub struct Leveler {
    gain_db: f32,
}

impl Default for Leveler {
    fn default() -> Self {
        Self { gain_db: START_GAIN_DB }
    }
}

impl Leveler {
    /// `pcm` with the gain applied; the gain moves linearly across the chunk, so there are no steps.
    pub fn apply(&mut self, pcm: &[f32]) -> Vec<f32> {
        let from = db_to_gain(self.gain_db);
        let rms = (pcm.iter().map(|s| s * s).sum::<f32>() / pcm.len().max(1) as f32).sqrt();
        let rms_db = 20.0 * rms.max(1e-9).log10();
        if rms_db > SILENCE_DB {
            let wanted = (TARGET_DB - rms_db).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            let rate = if wanted < self.gain_db { FALL } else { RISE };
            self.gain_db += rate * (wanted - self.gain_db);
        }
        let to = db_to_gain(self.gain_db);
        let n = pcm.len().max(1) as f32;
        pcm.iter().enumerate().map(|(i, &s)| limit(s * (from + (to - from) * (i + 1) as f32 / n))).collect()
    }
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Linear up to `KNEE`, then bends smoothly towards 1.
fn limit(s: f32) -> f32 {
    let a = s.abs();
    if a <= KNEE {
        return s;
    }
    s.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_db(x: &[f32]) -> f32 {
        20.0 * (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt().log10()
    }

    fn tone(amplitude: f32) -> Vec<f32> {
        (0..1920).map(|i| amplitude * (i as f32 * 0.05).sin()).collect()
    }

    #[test]
    fn quiet_and_loud_sentences_end_up_near_the_target() {
        for amplitude in [0.01, 0.1] {
            let mut l = Leveler::default();
            let mut out = Vec::new();
            for _ in 0..20 {
                out = l.apply(&tone(amplitude));
            }
            assert!((rms_db(&out) - TARGET_DB).abs() < 1.5, "{amplitude}: {}", rms_db(&out));
        }
    }

    #[test]
    fn silence_keeps_the_gain_and_peaks_stay_below_full_scale() {
        let mut l = Leveler::default();
        l.apply(&[0.0; 1920]);
        assert_eq!(l.gain_db, START_GAIN_DB);
        assert!(l.apply(&tone(1.0)).iter().all(|s| s.abs() < 1.0));
        assert_eq!(limit(0.5), 0.5);
    }
}
