//! Pal: a voice assistant. `hear` turns the microphone into text, `think` decides what to say, `speak` says it.
//! `transport` connects them over mq-bridge, `godot` is the app's entry point. See AGENTS.md for the flow.

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
