//! Speech to text: Parakeet TDT behind mq-bridge-onnx's `transcribe` endpoint, asked one segment at a time.

use std::path::Path;
use std::sync::OnceLock;

use mq_bridge::{CanonicalMessage, Publisher};
use serde::Deserialize;

use crate::{Error, Result};

fn err(e: impl std::fmt::Display) -> Error {
    Error::Stt(e.to_string())
}

pub struct Recognizer {
    publisher: Publisher,
    runtime: tokio::runtime::Runtime,
}

#[derive(Deserialize)]
struct Heard {
    text: String,
}

impl Recognizer {
    /// `model` is the Parakeet TDT folder; it detects the language itself. Call this before any other ONNX model
    /// loads: VAD and Smart Turn then share mq-bridge-onnx's ONNX Runtime (ort allows one per process).
    pub fn new(model: &Path) -> Result<Self> {
        static REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
        REGISTERED.get_or_init(|| mq_bridge_onnx::register().map_err(|e| format!("{e:#}"))).clone().map_err(err)?;
        pal_core::vad::use_environment(mq_bridge_onnx::environment().map_err(err)?.clone());
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(4));
        let path = model.to_str().ok_or_else(|| err("model path is not UTF-8"))?;
        let endpoint = serde_json::json!({ "custom": { "name": mq_bridge_onnx::NAME, "config": {
            "task": "transcribe", "model": path, "threads": threads,
        }}});
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).build().map_err(err)?;
        let publisher = runtime
            .block_on(Publisher::from_config(endpoint))
            .map_err(|e| err(format!("{}: {e:#}", model.display())))?;
        Ok(Self { publisher, runtime })
    }

    /// Text of 16 kHz mono speech, empty if nothing was understood.
    pub fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        let mut msg = CanonicalMessage::new(pcm.iter().flat_map(|s| s.to_le_bytes()).collect(), None);
        msg.set_metadata(mq_bridge_onnx::DTYPE_KEY, "float32");
        msg.set_metadata(mq_bridge_onnx::SHAPE_KEY, pcm.len().to_string());
        msg.set_metadata("audio_sample_rate", "16000");
        let answer = self.runtime.block_on(self.publisher.request(msg)).map_err(|e| err(format!("{e:#}")))?;
        let heard: Heard = serde_json::from_slice(&answer.payload).map_err(err)?;
        Ok(heard.text.trim().to_string())
    }
}
