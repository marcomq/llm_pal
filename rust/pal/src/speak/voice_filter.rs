//! KITT's dashboard speaker: a band-pass, a slight boxy presence and a faint metallic comb on Pal's voice.
//! The state carries over between chunks, so a streamed sentence is filtered as one signal.

const HIGH_PASS_HZ: f32 = 250.0;
const LOW_PASS_HZ: f32 = 5000.0;
const PRESENCE_HZ: f32 = 1800.0;
const PRESENCE_DB: f32 = 3.0;
/// A copy this much later is mixed in; the peaks it adds every 1/delay Hz sound metallic.
const COMB_MS: f32 = 2.5;
const COMB_MIX: f32 = 0.25;

/// RBJ audio EQ cookbook biquad, transposed direct form II.
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(b: [f32; 3], a: [f32; 3]) -> Self {
        Self { b: b.map(|v| v / a[0]), a: [a[1] / a[0], a[2] / a[0]], z: [0.0; 2] }
    }

    fn pass(kind: Pass, hz: f32, rate: f32) -> Self {
        let w = std::f32::consts::TAU * hz / rate;
        let (cos, alpha) = (w.cos(), w.sin() / std::f32::consts::SQRT_2);
        let a = [1.0 + alpha, -2.0 * cos, 1.0 - alpha];
        match kind {
            Pass::High => Self::new([(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0], a),
            Pass::Low => Self::new([(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0], a),
        }
    }

    fn peak(hz: f32, db: f32, rate: f32) -> Self {
        let w = std::f32::consts::TAU * hz / rate;
        let (cos, alpha, g) = (w.cos(), w.sin() / 2.0, 10f32.powf(db / 40.0));
        Self::new([1.0 + alpha * g, -2.0 * cos, 1.0 - alpha * g], [1.0 + alpha / g, -2.0 * cos, 1.0 - alpha / g])
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

enum Pass {
    High,
    Low,
}

pub struct VoiceFilter {
    stages: [Biquad; 3],
    delay: Vec<f32>,
    at: usize,
}

impl VoiceFilter {
    pub fn new(rate: u32) -> Self {
        let r = rate as f32;
        Self {
            stages: [
                Biquad::pass(Pass::High, HIGH_PASS_HZ, r),
                Biquad::pass(Pass::Low, LOW_PASS_HZ, r),
                Biquad::peak(PRESENCE_HZ, PRESENCE_DB, r),
            ],
            delay: vec![0.0; ((COMB_MS * r / 1000.0) as usize).max(1)],
            at: 0,
        }
    }

    pub fn apply(&mut self, pcm: &[f32]) -> Vec<f32> {
        pcm.iter()
            .map(|&x| {
                let y = self.stages.iter_mut().fold(x, |s, stage| stage.run(s));
                let late = std::mem::replace(&mut self.delay[self.at], y);
                self.at = (self.at + 1) % self.delay.len();
                (y + COMB_MIX * late) / (1.0 + COMB_MIX)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gain of a sine through the filter, after it settled.
    fn gain_db(hz: f32) -> f32 {
        let mut f = VoiceFilter::new(24_000);
        let x: Vec<f32> = (0..24_000).map(|i| (std::f32::consts::TAU * hz * i as f32 / 24_000.0).sin()).collect();
        let y = f.apply(&x);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        20.0 * (rms(&y[12_000..]) / rms(&x[12_000..])).log10()
    }

    #[test]
    fn keeps_the_voice_band_and_cuts_rumble_and_hiss() {
        assert!(gain_db(60.0) < -18.0, "{}", gain_db(60.0));
        assert!(gain_db(10_000.0) < -10.0, "{}", gain_db(10_000.0));
        let mid = gain_db(1500.0);
        assert!(mid > -6.0 && mid < 6.0, "{mid}");
    }

    #[test]
    fn stays_stable_and_continuous_across_chunks() {
        let x: Vec<f32> = (0..4800).map(|i| ((i * 7919 % 1000) as f32 / 500.0) - 1.0).collect();
        let whole = VoiceFilter::new(24_000).apply(&x);
        let mut f = VoiceFilter::new(24_000);
        let parts: Vec<f32> = x.chunks(1920).flat_map(|c| f.apply(c)).collect();
        assert_eq!(whole, parts);
        assert!(whole.iter().all(|s| s.is_finite() && s.abs() < 4.0));
    }
}
