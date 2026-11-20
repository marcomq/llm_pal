//! Streaming conversion of mono audio at any rate to 16 kHz.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::Error;

pub const TARGET_RATE: u32 = 16_000;
const CHUNK: usize = 1024;

/// Feeds arbitrary-length mono input; output is aligned (the resampler delay is dropped).
pub struct Resampler16k {
    fft: Option<Fft<f32>>,
    pending: Vec<f32>,
    out_buf: Vec<f32>,
    skip: usize,
    consumed: u64,
    produced: u64,
    ratio: f64,
}

impl Resampler16k {
    pub fn new(input_rate: u32) -> Result<Self, Error> {
        let fft = if input_rate == TARGET_RATE {
            None
        } else {
            let fft = Fft::new(input_rate as usize, TARGET_RATE as usize, CHUNK, 1, FixedSync::Input)
                .map_err(|e| Error::Resample(e.to_string()))?;
            Some(fft)
        };
        let out_len = fft.as_ref().map_or(0, |f| f.output_frames_max());
        let skip = fft.as_ref().map_or(0, |f| f.output_delay());
        Ok(Self {
            fft,
            pending: Vec::with_capacity(CHUNK),
            out_buf: vec![0.0; out_len],
            skip,
            consumed: 0,
            produced: 0,
            ratio: TARGET_RATE as f64 / input_rate as f64,
        })
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), Error> {
        if self.fft.is_none() {
            out.extend_from_slice(input);
            return Ok(());
        }
        self.consumed += input.len() as u64;
        let mut rest = input;
        while !rest.is_empty() {
            let take = (CHUNK - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == CHUNK {
                self.run_chunk(out, u64::MAX)?;
            }
        }
        Ok(())
    }

    /// Pads the tail with silence so all input reaches the output, then stops at the expected length.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), Error> {
        let Some(fft) = &self.fft else { return Ok(()) };
        let expected = (self.consumed as f64 * self.ratio).round() as u64;
        let rounds = (self.skip + fft.output_frames_max()).div_ceil(CHUNK) + 1;
        for _ in 0..rounds {
            if self.produced >= expected {
                break;
            }
            self.pending.resize(CHUNK, 0.0);
            self.run_chunk(out, expected)?;
        }
        Ok(())
    }

    fn run_chunk(&mut self, out: &mut Vec<f32>, limit: u64) -> Result<(), Error> {
        let fft = self.fft.as_mut().expect("only called when resampling");
        let err = |e: &dyn std::fmt::Display| Error::Resample(e.to_string());
        let input = InterleavedSlice::new(&self.pending[..], 1, CHUNK).map_err(|e| err(&e))?;
        let cap = self.out_buf.len();
        let mut output = InterleavedSlice::new_mut(&mut self.out_buf[..], 1, cap).map_err(|e| err(&e))?;
        let (_, written) = fft.process_into_buffer(&input, &mut output, None).map_err(|e| err(&e))?;
        self.pending.clear();
        let mut fresh = &self.out_buf[..written];
        let drop = self.skip.min(fresh.len());
        self.skip -= drop;
        fresh = &fresh[drop..];
        let room = limit.saturating_sub(self.produced).min(fresh.len() as u64) as usize;
        out.extend_from_slice(&fresh[..room]);
        self.produced += room as u64;
        Ok(())
    }
}

/// One-shot conversion of a whole clip.
pub fn to_16k(input: &[f32], rate: u32) -> Result<Vec<f32>, Error> {
    let mut r = Resampler16k::new(rate)?;
    let mut out = Vec::with_capacity((input.len() as f64 * r.ratio) as usize + CHUNK);
    r.push(input, &mut out)?;
    r.flush(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (TAU * freq * i as f32 / rate as f32).sin() * 0.5).collect()
    }

    fn zero_crossing_freq(x: &[f32], rate: u32) -> f32 {
        let core = &x[x.len() / 10..x.len() * 9 / 10];
        let ups = core.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f32 * rate as f32 / core.len() as f32
    }

    #[test]
    fn downsamples_48k_keeping_length_and_pitch() {
        let input = sine(440.0, 48_000, 2.0);
        let out = to_16k(&input, 48_000).unwrap();
        assert_eq!(out.len(), 32_000);
        let f = zero_crossing_freq(&out, TARGET_RATE);
        assert!((f - 440.0).abs() < 4.4, "freq {f}");
    }

    #[test]
    fn streaming_in_odd_pieces_matches_expected_length() {
        let input = sine(300.0, 44_100, 1.5);
        let mut r = Resampler16k::new(44_100).unwrap();
        let mut out = Vec::new();
        for piece in input.chunks(333) {
            r.push(piece, &mut out).unwrap();
        }
        r.flush(&mut out).unwrap();
        assert_eq!(out.len(), (input.len() as f64 * 16_000.0 / 44_100.0).round() as usize);
        let f = zero_crossing_freq(&out, TARGET_RATE);
        assert!((f - 300.0).abs() < 3.0, "freq {f}");
    }

    #[test]
    fn passes_16k_through() {
        let input = sine(200.0, 16_000, 0.1);
        assert_eq!(to_16k(&input, 16_000).unwrap(), input);
    }
}
