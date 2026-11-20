//! What the microphone reports, and its form as an mq-bridge message (`pal_mic` endpoint).

use mq_bridge::CanonicalMessage;
use serde::{Deserialize, Serialize};

/// Prosody of one segment. Contours have one value per 10 ms frame.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Features {
    pub duration_s: f32,
    pub f0_median_hz: Option<f32>,
    /// 5th percentile of the voiced f0, robust against octave errors.
    pub f0_min_hz: Option<f32>,
    /// 95th percentile of the voiced f0.
    pub f0_max_hz: Option<f32>,
    /// Pitch movement over the last ~500 ms of voicing; rising (> 0) hints at a question.
    pub f0_slope_st_per_s: Option<f32>,
    pub voiced_ratio: f32,
    pub rms_mean_db: f32,
    pub rms_max_db: f32,
    /// Energy peaks per second, an estimate of syllables per second.
    pub syllable_rate: f32,
    pub pitch_contour_hz: Vec<Option<f32>>,
    pub rms_contour_db: Vec<f32>,
}

/// A stretch of speech at 16 kHz mono. Times count from the start of the stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Cut at the maximum length rather than ended by silence.
    pub forced_split: bool,
    /// The app played audio (`MicLink::output_active`) while this segment was open, so it may hold that audio.
    pub during_output: bool,
    /// Ended at a pause because Smart Turn heard a finished turn, not after `end_silence_ms`.
    pub turn_complete: bool,
    /// Silence that followed `pcm` until the segment ended.
    pub silence_ms: u64,
    pub features: Features,
    /// In a message, the payload instead.
    #[serde(skip)]
    pub pcm: Vec<f32>,
}

#[derive(Debug, Clone)]
pub enum Event {
    SpeechStart {
        at_ms: u64,
    },
    /// Speech started while `MicLink::output_active` was set.
    BargeIn {
        at_ms: u64,
    },
    Segment(Segment),
    /// The user paused (`SegmenterConfig::pause_ms`): the segment so far, with the id the final one will have.
    /// Lets work start early; `Resume` means the user went on and the final `Segment` will differ.
    Pause(Segment),
    Resume,
}

const EVENT: &str = "mic.event";
const AT_MS: &str = "mic.at_ms";
const SEGMENT: &str = "mic.segment";

impl Event {
    /// A segment's PCM is the payload as mq-bridge-onnx reads audio, so it can go straight to `transcribe`.
    pub fn to_message(&self) -> CanonicalMessage {
        let (kind, at, seg) = match self {
            Event::SpeechStart { at_ms } => ("speech_start", Some(at_ms), None),
            Event::BargeIn { at_ms } => ("barge_in", Some(at_ms), None),
            Event::Pause(seg) => ("pause", None, Some(seg)),
            Event::Segment(seg) => ("segment", None, Some(seg)),
            Event::Resume => ("resume", None, None),
        };
        let mut msg = match seg {
            Some(seg) => pcm_message(&seg.pcm)
                .with_metadata_kv(SEGMENT, serde_json::to_string(seg).expect("a segment serializes")),
            None => CanonicalMessage::from(""),
        };
        msg.set_metadata(EVENT, kind);
        if let Some(at) = at {
            msg.set_metadata(AT_MS, at.to_string());
        }
        msg
    }

    pub fn from_message(msg: &CanonicalMessage) -> Option<Self> {
        let get = |key: &str| msg.metadata.get(key);
        let at_ms = || get(AT_MS)?.parse().ok();
        let segment = || {
            let mut seg: Segment = serde_json::from_str(get(SEGMENT)?).ok()?;
            seg.pcm = msg.payload.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            Some(seg)
        };
        Some(match get(EVENT)?.as_str() {
            "speech_start" => Event::SpeechStart { at_ms: at_ms()? },
            "barge_in" => Event::BargeIn { at_ms: at_ms()? },
            "pause" => Event::Pause(segment()?),
            "segment" => Event::Segment(segment()?),
            "resume" => Event::Resume,
            _ => return None,
        })
    }
}

/// 16 kHz mono speech as mq-bridge-onnx's `transcribe` task reads it.
pub fn pcm_message(pcm: &[f32]) -> CanonicalMessage {
    CanonicalMessage::new(pcm.iter().flat_map(|s| s.to_le_bytes()).collect(), None)
        .with_metadata_kv(mq_bridge_onnx::DTYPE_KEY, "float32")
        .with_metadata_kv(mq_bridge_onnx::SHAPE_KEY, pcm.len().to_string())
        .with_metadata_kv("audio_sample_rate", "16000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_survive_a_message() {
        let seg = Segment {
            id: 3,
            start_ms: 100,
            end_ms: 200,
            forced_split: false,
            during_output: true,
            turn_complete: true,
            silence_ms: 40,
            features: Features {
                f0_median_hz: Some(120.0),
                pitch_contour_hz: vec![None, Some(1.5)],
                ..Default::default()
            },
            pcm: vec![0.25, -1.0, 0.0],
        };
        let back = Event::from_message(&Event::Segment(seg.clone()).to_message());
        let Some(Event::Segment(back)) = back else { panic!("{back:?}") };
        assert_eq!((back.id, back.silence_ms, back.during_output, back.pcm), (3, 40, true, seg.pcm));
        assert_eq!(back.features.pitch_contour_hz, seg.features.pitch_contour_hz);
        let start = Event::from_message(&Event::BargeIn { at_ms: 7 }.to_message());
        assert!(matches!(start, Some(Event::BargeIn { at_ms: 7 })));
    }
}
