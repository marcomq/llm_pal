use serde::Serialize;

/// Prosody of one segment. Contours have one value per 10 ms frame.
#[derive(Debug, Clone, Default, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub id: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Cut at the maximum length rather than ended by silence.
    pub forced_split: bool,
    /// The app played audio (`Processor::output_active`) while this segment was open, so it may hold that audio.
    pub during_output: bool,
    pub features: Features,
    #[serde(skip)]
    pub pcm: Vec<f32>,
}

#[derive(Debug, Clone)]
pub enum Event {
    SpeechStart {
        at_ms: u64,
    },
    /// Speech started while `output_active` was set.
    BargeIn {
        at_ms: u64,
    },
    Segment(Segment),
    /// The user paused (`SegmenterConfig::pause_ms`): the segment so far, with the id the final one will have.
    /// Lets work start early; `Resume` means the user went on and the final `Segment` will differ.
    Pause(Segment),
    Resume,
}
