//! Turns per-chunk speech probabilities into segments: hysteresis, pre-roll, end on silence, max length.

use std::collections::VecDeque;

use crate::resample::TARGET_RATE;

#[derive(Debug, Clone)]
pub struct SegmenterConfig {
    /// Speech starts at this probability or above.
    pub on: f32,
    /// During speech, chunks below this count as silence.
    pub off: f32,
    pub pre_roll_ms: u32,
    pub end_silence_ms: u32,
    /// Silence after which a `Pause` with the speech so far is reported, so work on it can start before the
    /// segment ends; 0 turns this off.
    pub pause_ms: u32,
    /// Trailing silence kept at the end of a segment.
    pub tail_ms: u32,
    /// Segments with less speech than this are dropped.
    pub min_speech_ms: u32,
    pub max_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            on: 0.5,
            off: 0.35,
            pre_roll_ms: 250,
            end_silence_ms: 700,
            pause_ms: 250,
            tail_ms: 200,
            min_speech_ms: 250,
            max_ms: 28_000,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum SegOut {
    /// Sample position (16 kHz) where speech was detected.
    Start {
        at: u64,
    },
    Done {
        start: u64,
        pcm: Vec<f32>,
        forced: bool,
    },
    /// Speech paused for `pause_ms`; `pcm` is what a `Done` right now would hold. Followed by `Resume` or `Done`.
    Pause {
        start: u64,
        pcm: Vec<f32>,
    },
    /// Speech went on after a `Pause`.
    Resume,
}

struct Speech {
    start: u64,
    pcm: Vec<f32>,
    silence: usize,
    speech: usize,
    paused: bool,
}

impl Speech {
    /// The segment so far, keeping `tail` samples of the trailing silence.
    fn trimmed(&self, tail: usize) -> &[f32] {
        &self.pcm[..self.pcm.len() - self.silence.saturating_sub(tail).min(self.pcm.len())]
    }
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    pre: VecDeque<f32>,
    speech: Option<Speech>,
    pos: u64,
}

fn samples(ms: u32) -> usize {
    (ms as u64 * TARGET_RATE as u64 / 1000) as usize
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self { pre: VecDeque::with_capacity(samples(cfg.pre_roll_ms) + 1024), cfg, speech: None, pos: 0 }
    }

    pub fn in_speech(&self) -> bool {
        self.speech.is_some()
    }

    pub fn push(&mut self, chunk: &[f32], prob: f32, out: &mut Vec<SegOut>) {
        let at = self.pos;
        self.pos += chunk.len() as u64;
        let Some(sp) = &mut self.speech else {
            if prob >= self.cfg.on {
                let mut pcm: Vec<f32> = self.pre.drain(..).collect();
                let start = at - pcm.len() as u64;
                pcm.extend_from_slice(chunk);
                self.speech = Some(Speech { start, pcm, silence: 0, speech: chunk.len(), paused: false });
                out.push(SegOut::Start { at });
            } else {
                self.pre.extend(chunk);
                let excess = self.pre.len().saturating_sub(samples(self.cfg.pre_roll_ms));
                self.pre.drain(..excess);
            }
            return;
        };
        sp.pcm.extend_from_slice(chunk);
        if prob >= self.cfg.off {
            sp.silence = 0;
            sp.speech += chunk.len();
            if std::mem::take(&mut sp.paused) {
                out.push(SegOut::Resume);
            }
        } else {
            sp.silence += chunk.len();
        }
        if sp.silence >= samples(self.cfg.end_silence_ms) {
            self.end(out);
        } else if self.cfg.pause_ms > 0
            && !sp.paused
            && sp.silence >= samples(self.cfg.pause_ms)
            && sp.speech >= samples(self.cfg.min_speech_ms)
        {
            sp.paused = true;
            out.push(SegOut::Pause { start: sp.start, pcm: sp.trimmed(samples(self.cfg.tail_ms)).to_vec() });
        } else if sp.pcm.len() >= samples(self.cfg.max_ms) {
            let pcm = std::mem::take(&mut sp.pcm);
            out.push(SegOut::Done { start: sp.start, pcm, forced: true });
            *sp = Speech { start: self.pos, pcm: Vec::new(), silence: 0, speech: 0, paused: false };
        }
    }

    /// Drops an open segment without reporting it.
    pub fn discard(&mut self) {
        self.speech = None;
        self.pre.clear();
    }

    /// Closes an open segment at the end of the stream.
    pub fn finish(&mut self, out: &mut Vec<SegOut>) {
        if self.speech.is_some() {
            self.end(out);
        }
    }

    fn end(&mut self, out: &mut Vec<SegOut>) {
        let Some(mut sp) = self.speech.take() else { return };
        let tail = sp.pcm.split_off(sp.trimmed(samples(self.cfg.tail_ms)).len());
        let keep = samples(self.cfg.pre_roll_ms).min(tail.len());
        self.pre.extend(&tail[tail.len() - keep..]);
        if sp.speech >= samples(self.cfg.min_speech_ms) && !sp.pcm.is_empty() {
            out.push(SegOut::Done { start: sp.start, pcm: sp.pcm, forced: false });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: usize = 512;

    /// Runs probabilities through the segmenter; chunk i holds the value i so positions can be checked.
    fn run(cfg: SegmenterConfig, probs: &[f32]) -> Vec<SegOut> {
        let mut s = Segmenter::new(cfg);
        let mut out = Vec::new();
        for (i, &p) in probs.iter().enumerate() {
            s.push(&[i as f32; C], p, &mut out);
        }
        s.finish(&mut out);
        out
    }

    fn probs(parts: &[(f32, usize)]) -> Vec<f32> {
        parts.iter().flat_map(|&(p, n)| std::iter::repeat_n(p, n)).collect()
    }

    fn done(out: &[SegOut]) -> Vec<(u64, usize, bool)> {
        out.iter()
            .filter_map(|o| match o {
                SegOut::Done { start, pcm, forced } => Some((*start, pcm.len(), *forced)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn segment_has_pre_roll_and_ends_after_silence() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.0, 20), (0.9, 30), (0.0, 40)]));
        assert_eq!(out[0], SegOut::Start { at: 20 * C as u64 });
        let segs = done(&out);
        assert_eq!(segs.len(), 1);
        let pre = samples(250) as u64;
        let (start, len, forced) = segs[0];
        assert_eq!(start, 20 * C as u64 - pre);
        assert!(!forced);
        // pre-roll + speech + tail (200 ms, rounded up to whole chunks of silence seen).
        let expected = pre as usize + 30 * C + samples(200);
        assert!((len as i64 - expected as i64).abs() <= C as i64, "len {len} vs {expected}");
    }

    #[test]
    fn hysteresis_keeps_speech_between_off_and_on() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 10), (0.4, 40), (0.9, 10), (0.0, 30)]));
        assert_eq!(done(&out).len(), 1);
    }

    #[test]
    fn short_pause_does_not_split() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 10), (0.0, 10), (0.9, 10), (0.0, 30)]));
        assert_eq!(done(&out).len(), 1);
    }

    #[test]
    fn long_pause_splits() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 10), (0.0, 30), (0.9, 10), (0.0, 30)]));
        assert_eq!(done(&out).len(), 2);
    }

    #[test]
    fn pause_reports_the_speech_so_far_and_resume_follows_if_speech_goes_on() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 10), (0.0, 10), (0.9, 10), (0.0, 30)]));
        let kinds: Vec<_> = out
            .iter()
            .map(|o| match o {
                SegOut::Start { .. } => "start",
                SegOut::Pause { .. } => "pause",
                SegOut::Resume => "resume",
                SegOut::Done { .. } => "done",
            })
            .collect();
        assert_eq!(kinds, ["start", "pause", "resume", "pause", "done"]);
        let (SegOut::Pause { start: ps, pcm: last_pause }, SegOut::Done { start: ds, pcm: done, .. }) =
            (&out[3], &out[4])
        else {
            unreachable!()
        };
        assert_eq!(ps, ds);
        assert_eq!(last_pause[..], done[..last_pause.len()], "the pause holds the start of the final segment");
        assert!(done.len() - last_pause.len() <= samples(200), "only trailing silence differs");
    }

    #[test]
    fn short_burst_is_dropped() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 3), (0.0, 30)]));
        assert!(matches!(out[0], SegOut::Start { .. }));
        assert!(done(&out).is_empty());
    }

    #[test]
    fn max_length_forces_split_and_continues() {
        let cfg = SegmenterConfig { max_ms: 2000, ..Default::default() };
        let out = run(cfg, &probs(&[(0.9, 150), (0.0, 30)]));
        let segs = done(&out);
        assert!(segs.len() >= 2);
        assert!(segs[0].2, "first is forced");
        assert!(segs[0].1 <= samples(2000) + C);
        assert!(!segs.last().unwrap().2);
        let starts = out.iter().filter(|o| matches!(o, SegOut::Start { .. })).count();
        assert_eq!(starts, 1, "a forced split is not a new speech start");
    }

    #[test]
    fn discarded_speech_is_not_reported_and_new_speech_starts_fresh() {
        let mut s = Segmenter::new(SegmenterConfig::default());
        let mut out = Vec::new();
        for p in probs(&[(0.9, 30)]) {
            s.push(&[0.0; C], p, &mut out);
        }
        s.discard();
        for p in probs(&[(0.0, 5), (0.9, 30), (0.0, 40)]) {
            s.push(&[0.0; C], p, &mut out);
        }
        let segs = done(&out);
        assert_eq!(segs.len(), 1);
        assert!(segs[0].1 < 36 * C + samples(250), "only the second speech: {}", segs[0].1);
    }

    #[test]
    fn open_segment_is_closed_at_end_of_stream() {
        let out = run(SegmenterConfig::default(), &probs(&[(0.9, 20)]));
        assert_eq!(done(&out).len(), 1);
    }
}
