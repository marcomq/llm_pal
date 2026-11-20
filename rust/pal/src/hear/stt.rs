//! Speech to text: Parakeet TDT behind mq-bridge-onnx's `transcribe` endpoint, called one segment at a time.

use std::path::Path;
use std::sync::OnceLock;

use mq_bridge::models::Endpoint;
use mq_bridge::{Caller, Route};
use serde::Deserialize;

use crate::hear::event::pcm_message;
use crate::{Error, Result};

const NAME: &str = "pal_stt";
const REQUESTS: &str = "pal.stt";
const CAPACITY: usize = 16;
/// Parakeet loads while the route starts; on a phone, and beside the other models, that takes well over
/// mq-bridge's default of 5 s.
const STARTUP_MS: u64 = 120_000;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Stt(e.to_string())
}

#[derive(Deserialize)]
struct Heard {
    text: String,
}

/// Registers mq-bridge-onnx and points VAD and Smart Turn at its ONNX Runtime (ort allows one per process).
/// Call it before any other ONNX model loads.
pub fn share_onnx_runtime() -> Result<()> {
    static REGISTERED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    REGISTERED.get_or_init(|| mq_bridge_onnx::register().map_err(|e| format!("{e:#}"))).clone().map_err(err)?;
    crate::hear::vad::use_environment(mq_bridge_onnx::environment().map_err(err)?.clone());
    Ok(())
}

/// Deploys the `pal_stt` caller; `model` is the Parakeet TDT folder, which detects the language itself.
pub async fn caller(model: &Path) -> Result<Caller> {
    share_onnx_runtime()?;
    let path = model.to_str().ok_or_else(|| err("model path is not UTF-8"))?;
    let endpoint = serde_json::from_value(serde_json::json!({ "custom": { "name": mq_bridge_onnx::NAME, "config": {
        "task": "transcribe", "model": path, "threads": crate::cpu::fast_cores(),
    }}}))
    .map_err(err)?;
    let route = Route::new(Endpoint::new_memory(REQUESTS, CAPACITY), endpoint).with_startup_timeout_ms(STARTUP_MS);
    Caller::deploy(NAME, route).await.map_err(|e| err(format!("{}: {e:#}", model.display())))
}

/// Text of 16 kHz mono speech, empty if nothing was understood.
pub async fn transcribe(caller: &Caller, pcm: &[f32]) -> Result<String> {
    let answer = caller.call_message(pcm_message(pcm)).await.map_err(|e| err(format!("{e:#}")))?;
    let heard: Heard = serde_json::from_slice(&answer.payload).map_err(err)?;
    Ok(heard.text.trim().to_string())
}
