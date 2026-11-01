//! llm_pal audio processor: microphone → 16 kHz mono → Silero VAD → segments with prosody features.
//!
//! Audio-thread rule: the cpal callback only downmixes into a lock-free ring; resampling, inference and
//! analysis run on the worker thread (`mic_pipeline`).

#[cfg(target_os = "android")]
mod android;
pub mod capture;
pub mod echo;
pub mod event;
pub mod mic_pipeline;
pub mod playback;
pub mod prosody;
pub mod resample;
pub mod segmenter;
pub mod turn;
pub mod vad;

pub use event::{Event, Features, Segment};
pub use mic_pipeline::{Processor, ProcessorConfig, process_samples};
pub use segmenter::SegmenterConfig;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("resampler: {0}")]
    Resample(String),
    #[error("VAD: {0}")]
    Vad(String),
    #[error("Smart Turn: {0}")]
    Turn(String),
    #[error("audio: {0}")]
    Audio(String),
}
