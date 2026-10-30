//! Smart Turn v3 (pipecat-ai, BSD-2): from the last 8 s of audio, whether the speaker has finished the turn.
//! Input is Whisper's log-mel spectrogram, computed here as `WhisperFeatureExtractor(chunk_length=8)` does.

use std::path::Path;
use std::sync::Arc;

use ort::session::Session;
use ort::session::builder::{AutoDevicePolicy, GraphOptimizationLevel};
use ort::value::TensorRef;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::Error;
use crate::resample::TARGET_RATE;
use crate::vad::environment;

const WINDOW: usize = 8 * TARGET_RATE as usize;
const N_FFT: usize = 400;
const HOP: usize = 160;
const BINS: usize = N_FFT / 2 + 1;
const MELS: usize = 80;
const FRAMES: usize = WINDOW / HOP;

/// The turn counts as finished at this probability or above.
pub const COMPLETE: f32 = 0.5;

pub struct SmartTurn {
    session: Session,
    fft: Arc<dyn RealToComplex<f64>>,
    window: Vec<f64>,
    /// `MELS` rows of `BINS` weights.
    filters: Vec<f64>,
}

impl SmartTurn {
    pub fn new(model: &Path) -> Result<Self, Error> {
        let err = |e: &dyn std::fmt::Display| Error::Turn(format!("{}: {e}", model.display()));
        let session = Session::builder(environment()?)
            .map_err(|e| err(&e))?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(|e| err(&e))?
            // About 40 ms per call on 4 threads, 70 ms on one; idle threads must not spin while the LLM runs.
            .with_intra_threads(4)
            .map_err(|e| err(&e))?
            .with_intra_op_spinning(false)
            .map_err(|e| err(&e))?
            .with_auto_device(AutoDevicePolicy::PreferCPU)
            .map_err(|e| err(&e))?
            .commit_from_file(model)
            .map_err(|e| err(&e))?;
        let window = (0..N_FFT).map(|n| 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / N_FFT as f64).cos()).collect();
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
        Ok(Self { session, fft, window, filters: mel_filters() })
    }

    /// Probability that the speech in `pcm` (16 kHz, ending now) is a finished turn.
    pub fn complete(&mut self, pcm: &[f32]) -> Result<f32, Error> {
        let features = self.features(pcm);
        let err = |e: ort::Error| Error::Turn(e.to_string());
        let input = TensorRef::from_array_view(([1usize, MELS, FRAMES], &features[..])).map_err(err)?;
        let outputs = self.session.run(ort::inputs!["input_features" => input]).map_err(err)?;
        let (_, prob) = outputs[0].try_extract_tensor::<f32>().map_err(err)?;
        Ok(prob[0])
    }

    /// Log-mel features, `MELS` rows of `FRAMES`, of the last 8 s of `pcm`, zero-padded at the front. In f64 like
    /// the reference: the int8 model turns small differences into noticeably different probabilities.
    fn features(&self, pcm: &[f32]) -> Vec<f32> {
        let mut audio = vec![0.0; WINDOW];
        let take = pcm.len().min(WINDOW);
        audio[WINDOW - take..].copy_from_slice(&pcm[pcm.len() - take..]);
        normalize(&mut audio);

        let half = N_FFT / 2;
        let reflect = |i: isize| {
            let n = WINDOW as isize;
            let i = if i < 0 {
                -i
            } else if i >= n {
                2 * (n - 1) - i
            } else {
                i
            };
            audio[i as usize] as f64
        };
        let mut frame = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        let mut power = [0.0f64; BINS];
        let mut out = vec![0.0f32; MELS * FRAMES];
        for t in 0..FRAMES {
            let start = (t * HOP) as isize - half as isize;
            for (k, x) in frame.iter_mut().enumerate() {
                *x = reflect(start + k as isize) * self.window[k];
            }
            self.fft.process(&mut frame, &mut spectrum).expect("buffers come from the plan");
            for (p, c) in power.iter_mut().zip(&spectrum) {
                *p = Complex::norm_sqr(c);
            }
            for m in 0..MELS {
                let row = &self.filters[m * BINS..(m + 1) * BINS];
                let mel: f64 = row.iter().zip(&power).map(|(w, p)| w * p).sum();
                out[m * FRAMES + t] = mel.max(1e-10).log10() as f32;
            }
        }
        let floor = out.iter().copied().fold(f32::NEG_INFINITY, f32::max) - 8.0;
        out.iter_mut().for_each(|v| *v = (v.max(floor) + 4.0) / 4.0);
        out
    }
}

fn normalize(audio: &mut [f32]) {
    let n = audio.len() as f64;
    let mean = audio.iter().map(|&x| x as f64).sum::<f64>() / n;
    let var = audio.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n;
    let scale = 1.0 / (var + 1e-7).sqrt();
    audio.iter_mut().for_each(|x| *x = ((*x as f64 - mean) * scale) as f32);
}

/// Slaney mel scale: linear below 1 kHz, logarithmic above.
fn hz_to_mel(hz: f64) -> f64 {
    let step = 6.4f64.ln() / 27.0;
    if hz < 1000.0 { hz * 3.0 / 200.0 } else { 15.0 + (hz / 1000.0).ln() / step }
}

fn mel_to_hz(mel: f64) -> f64 {
    let step = 6.4f64.ln() / 27.0;
    if mel < 15.0 { mel * 200.0 / 3.0 } else { 1000.0 * (step * (mel - 15.0)).exp() }
}

/// Triangular filters with Slaney area normalization, as `transformers.audio_utils.mel_filter_bank`.
fn mel_filters() -> Vec<f64> {
    let nyquist = TARGET_RATE as f64 / 2.0;
    let top = hz_to_mel(nyquist);
    let edges: Vec<f64> = (0..MELS + 2).map(|i| mel_to_hz(top * i as f64 / (MELS + 1) as f64)).collect();
    let mut filters = vec![0.0f64; MELS * BINS];
    for m in 0..MELS {
        let (lo, mid, hi) = (edges[m], edges[m + 1], edges[m + 2]);
        let norm = 2.0 / (hi - lo);
        for b in 0..BINS {
            let f = nyquist * b as f64 / (BINS - 1) as f64;
            let w = ((f - lo) / (mid - lo)).min((hi - f) / (hi - mid)).max(0.0);
            filters[m * BINS + b] = w * norm;
        }
    }
    filters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filters_cover_the_band_and_are_area_normalized() {
        let f = mel_filters();
        for m in 0..MELS {
            let row = &f[m * BINS..(m + 1) * BINS];
            assert!(row.iter().any(|&w| w > 0.0), "filter {m} is empty");
        }
        assert!((hz_to_mel(mel_to_hz(30.0)) - 30.0).abs() < 1e-9);
        assert!((hz_to_mel(1000.0) - 15.0).abs() < 1e-9);
    }
}
