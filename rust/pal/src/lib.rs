//! Pal: a voice assistant.
//!
//! ```text
//! godot          entry: PalNode::start, signals back to the UI
//! conversation   the control: one input at a time, a match over the phase   ← start reading here
//! transport      how the parts connect: routes, topics, reports
//! hear           microphone → speech events → text
//! think          persona, prompts, the LLM (brain: local_llm or remote_llm)
//! speak          sentences → Pocket TTS → loudspeaker
//! ```
//!
//! Each module's doc shows only what happens inside it; how they connect is drawn once, in `transport.rs`.
//! See AGENTS.md for each step.

pub mod conversation;
mod cpu;
pub mod downloader;
mod godot;
pub mod hear;
mod platform;
pub mod speak;
pub mod think;
pub mod transport;

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
    #[error("stt: {0}")]
    Stt(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("tts: {0}")]
    Tts(String),
    #[error("routes: {0}")]
    Routes(String),
    #[error("download: {0}")]
    Download(String),
    /// `Agent::stop` came during loading.
    #[error("stopped")]
    Stopped,
}

pub type Result<T> = std::result::Result<T, Error>;
