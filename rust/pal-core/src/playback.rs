//! Speech output through cpal: a lock-free ring feeds the callback, which also measures the output level.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Stream, StreamConfig};

use crate::Error;

const RING_SECONDS: u32 = 30;

/// State shared between the callback, the feeding thread and readers such as the UI.
#[derive(Default)]
pub struct PlaybackState {
    level: AtomicU32,
    queued: AtomicUsize,
    flush: AtomicBool,
    generation: AtomicU64,
}

impl PlaybackState {
    /// RMS of the block played last, 0..1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn busy(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > 0
    }

    /// Drops everything queued; a running `play` returns early.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.flush.store(true, Ordering::SeqCst);
    }
}

/// Owns the output stream; keep it on the thread that created it.
pub struct Playback {
    _stream: Stream,
    tx: rtrb::Producer<f32>,
    pub sample_rate: u32,
    pub state: Arc<PlaybackState>,
}

impl Playback {
    pub fn start() -> Result<Self, Error> {
        #[cfg(target_os = "android")]
        crate::android::init_context();
        let dev =
            cpal::default_host().default_output_device().ok_or_else(|| Error::Audio("no output device".into()))?;
        let default = dev.default_output_config().map_err(|e| Error::Audio(format!("output config: {e}")))?;
        let channels = default.channels().max(1) as usize;
        let sample_rate = default.sample_rate();
        let config = StreamConfig { channels: channels as u16, sample_rate, buffer_size: BufferSize::Default };

        let (tx, mut rx) = rtrb::RingBuffer::<f32>::new((sample_rate * RING_SECONDS) as usize);
        let state = Arc::new(PlaybackState::default());
        let st = state.clone();
        let stream = dev
            .build_output_stream(
                config,
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if st.flush.swap(false, Ordering::SeqCst) {
                        let n = rx.slots();
                        if let Ok(chunk) = rx.read_chunk(n) {
                            chunk.commit_all();
                        }
                    }
                    let mut sum = 0.0;
                    for frame in out.chunks_mut(channels) {
                        let s = rx.pop().unwrap_or(0.0);
                        sum += s * s;
                        frame.fill(s);
                    }
                    let rms = (sum / (out.len() / channels).max(1) as f32).sqrt();
                    st.level.store(rms.to_bits(), Ordering::Relaxed);
                    st.queued.store(rx.slots(), Ordering::Relaxed);
                },
                |e| eprintln!("llm_pal: output stream error: {e}"),
                None,
            )
            .map_err(|e| Error::Audio(format!("output stream: {e}")))?;
        stream.play().map_err(|e| Error::Audio(format!("output play: {e}")))?;
        Ok(Self { _stream: stream, tx, sample_rate, state })
    }

    /// Queues mono samples recorded at `rate`, waiting while the ring is full.
    /// Returns false if `stop` was called meanwhile.
    pub fn play(&mut self, pcm: &[f32], rate: u32) -> bool {
        let generation = self.state.generation.load(Ordering::SeqCst);
        let samples = resample_linear(pcm, rate, self.sample_rate);
        let mut rest = &samples[..];
        while !rest.is_empty() {
            if self.state.generation.load(Ordering::SeqCst) != generation {
                return false;
            }
            let n = self.tx.slots().min(rest.len());
            if n == 0 {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            if let Ok(mut chunk) = self.tx.write_chunk_uninit(n) {
                let (a, b) = chunk.as_mut_slices();
                for (slot, &s) in a.iter_mut().chain(b.iter_mut()).zip(rest) {
                    slot.write(s);
                }
                // SAFETY: all n slots were written above.
                unsafe { chunk.commit_all() };
            }
            self.state.queued.fetch_add(n, Ordering::Relaxed);
            rest = &rest[n..];
        }
        true
    }
}

/// Good enough for speech; the TTS rate (22.05 kHz) rarely matches the device.
fn resample_linear(pcm: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || pcm.is_empty() {
        return pcm.to_vec();
    }
    let n = (pcm.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..n)
        .map(|i| {
            let x = i as f64 * step;
            let j = x as usize;
            let frac = (x - j as f64) as f32;
            let a = pcm[j];
            let b = pcm.get(j + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_resampling_keeps_duration_and_shape() {
        let ramp: Vec<f32> = (0..22_050).map(|i| i as f32 / 22_050.0).collect();
        let out = resample_linear(&ramp, 22_050, 48_000);
        assert_eq!(out.len(), 48_000);
        assert!((out[24_000] - 0.5).abs() < 1e-3);
        assert_eq!(resample_linear(&ramp, 16_000, 16_000).len(), ramp.len());
    }
}
