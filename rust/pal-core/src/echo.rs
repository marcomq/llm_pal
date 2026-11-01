//! Echo cancellation with WebRTC's AEC3 (pure Rust port `sonora`): removes Pal's own voice from the microphone,
//! so the user can talk while Pal speaks. The playback callback hands over what it plays as the reference.

use sonora::config::EchoCanceller as Aec3;
use sonora::{AudioProcessing, Config, StreamConfig};

use crate::Error;
use crate::resample::TARGET_RATE;

/// Rough time from handing a sample to the output device until it is captured and processed again; AEC3
/// finds the exact delay itself.
const DELAY_MS: i32 = 60;

/// What the output device plays, mono at `sample_rate`.
pub struct EchoReference {
    pub played: rtrb::Consumer<f32>,
    pub sample_rate: u32,
}

pub struct EchoCanceller {
    apm: AudioProcessing,
    reference: EchoReference,
    render: Vec<f32>,
    render_frame: usize,
    capture: Vec<f32>,
    out_frame: Vec<f32>,
}

const CAPTURE_FRAME: usize = TARGET_RATE as usize / 100;

impl EchoCanceller {
    pub fn new(mut reference: EchoReference) -> Result<Self, Error> {
        let config = Config { echo_canceller: Some(Aec3::default()), ..Default::default() };
        let mut apm = AudioProcessing::builder()
            .config(config)
            .capture_config(StreamConfig::new(TARGET_RATE, 1))
            .render_config(StreamConfig::new(reference.sample_rate, 1))
            .build();
        apm.set_stream_delay_ms(DELAY_MS).map_err(|e| Error::Audio(format!("echo canceller: {e:?}")))?;
        // What was played before capture started would arrive far too early.
        let stale = reference.played.slots();
        if let Ok(chunk) = reference.played.read_chunk(stale) {
            chunk.commit_all();
        }
        let render_frame = reference.sample_rate as usize / 100;
        Ok(Self {
            apm,
            reference,
            render: Vec::with_capacity(render_frame * 2),
            render_frame,
            capture: Vec::with_capacity(CAPTURE_FRAME * 2),
            out_frame: vec![0.0; CAPTURE_FRAME.max(render_frame)],
        })
    }

    /// Appends `input` (16 kHz) with the echo removed to `out`, in 10 ms frames; the rest waits for more input.
    /// Everything played until now goes in first, so the reference is never later than its echo.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), Error> {
        let err = |e: sonora::Error| Error::Audio(format!("echo canceller: {e:?}"));
        let played = self.reference.played.slots();
        if let Ok(chunk) = self.reference.played.read_chunk(played) {
            let (a, b) = chunk.as_slices();
            self.render.extend_from_slice(a);
            self.render.extend_from_slice(b);
            chunk.commit_all();
        }
        let whole = self.render.len() / self.render_frame * self.render_frame;
        for frame in self.render[..whole].chunks(self.render_frame) {
            let dest = &mut self.out_frame[..self.render_frame];
            self.apm.process_render_f32(&[frame], &mut [dest]).map_err(err)?;
        }
        self.render.drain(..whole);

        self.capture.extend_from_slice(input);
        let whole = self.capture.len() / CAPTURE_FRAME * CAPTURE_FRAME;
        for frame in self.capture[..whole].chunks(CAPTURE_FRAME) {
            let dest = &mut self.out_frame[..CAPTURE_FRAME];
            self.apm.process_capture_f32(&[frame], &mut [dest]).map_err(err)?;
            out.extend_from_slice(dest);
        }
        self.capture.drain(..whole);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(x: &[f32]) -> f32 {
        x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32
    }

    /// Bursts of noise at 48 kHz played, heard at 16 kHz 80 ms later, quieter and filtered.
    #[test]
    fn removes_a_delayed_filtered_echo() {
        let (mut tx, played) = rtrb::RingBuffer::new(48_000);
        let mut aec = EchoCanceller::new(EchoReference { played, sample_rate: 48_000 }).unwrap();
        let mut seed = 1u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        let seconds = 8;
        // Noise made at 16 kHz and held for three samples, so the echo has nothing above the capture band.
        let speech: Vec<f32> =
            (0..16_000 * seconds).map(|i| if (i / 4_000) % 3 == 2 { 0.0 } else { 0.5 * noise() }).collect();
        let render: Vec<f32> = speech.iter().flat_map(|&s| [s; 3]).collect();
        let delay = 80 * 16;
        let mut low = 0.0f32;
        let echo: Vec<f32> = (0..speech.len())
            .map(|i| {
                let x = if i >= delay { speech[i - delay] } else { 0.0 };
                low += 0.5 * (x - low);
                0.4 * low
            })
            .collect();
        let mut out = Vec::new();
        for (r, c) in render.chunks(480).zip(echo.chunks(160)) {
            r.iter().for_each(|&s| tx.push(s).unwrap());
            aec.process(c, &mut out).unwrap();
        }
        let tail = 16_000 * 4..16_000 * seconds;
        let db = 10.0 * (energy(&out[tail.clone()]) / energy(&echo[tail])).log10();
        eprintln!("echo after cancellation: {db:.1} dB");
        assert!(db < -20.0, "echo only {db:.1} dB down");
    }
}
