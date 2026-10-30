//! Speech understanding and replies on top of pal-core segments: STT, LLM, prosody tags, sentence streaming, TTS.

pub mod agent;
pub mod language;
pub mod llm;
pub mod numbers;
pub mod persona;
pub mod pocket_speaker;
pub mod prosody_tags;
pub mod sentences;
pub mod stt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("stt: {0}")]
    Stt(String),
    #[error("llm: {0}")]
    Llm(String),
    #[error("tts: {0}")]
    Tts(String),
    #[error("microphone: {0}")]
    Audio(#[from] pal_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
