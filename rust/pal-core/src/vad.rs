//! Silero VAD (v5/v6 ONNX) through ONNX Runtime. Stateful: the LSTM state and the last 64 samples of
//! context carry over from one 512-sample chunk to the next.

use std::path::Path;
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::Session;
use ort::session::builder::{AutoDevicePolicy, GraphOptimizationLevel};
use ort::value::TensorRef;

use crate::Error;
use crate::resample::TARGET_RATE;

pub const CHUNK: usize = 512;
const CONTEXT: usize = 64;
const STATE_LEN: usize = 2 * 128;

static ENVIRONMENT: OnceLock<Result<Environment, String>> = OnceLock::new();

fn environment() -> Result<&'static Environment, Error> {
    ENVIRONMENT
        .get_or_init(|| ort::init().with_name("llm_pal").build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error::Vad(format!("ONNX Runtime did not start: {e}")))
}

pub struct SileroVad {
    session: Session,
    state: [f32; STATE_LEN],
    input: [f32; CONTEXT + CHUNK],
}

impl SileroVad {
    pub fn new(model: &Path) -> Result<Self, Error> {
        let err = |e: &dyn std::fmt::Display| Error::Vad(format!("{}: {e}", model.display()));
        let session = Session::builder(environment()?)
            .map_err(|e| err(&e))?
            .with_optimization_level(GraphOptimizationLevel::All)
            .map_err(|e| err(&e))?
            .with_intra_threads(1)
            .map_err(|e| err(&e))?
            // ONNX Runtime would otherwise pick CoreML on macOS.
            .with_auto_device(AutoDevicePolicy::PreferCPU)
            .map_err(|e| err(&e))?
            .commit_from_file(model)
            .map_err(|e| err(&e))?;
        Ok(Self { session, state: [0.0; STATE_LEN], input: [0.0; CONTEXT + CHUNK] })
    }

    pub fn reset(&mut self) {
        self.state = [0.0; STATE_LEN];
        self.input = [0.0; CONTEXT + CHUNK];
    }

    /// Speech probability of the next 512 samples (32 ms at 16 kHz).
    pub fn process(&mut self, chunk: &[f32]) -> Result<f32, Error> {
        assert_eq!(chunk.len(), CHUNK, "Silero takes chunks of {CHUNK} samples");
        self.input.copy_within(CHUNK.., 0);
        self.input[CONTEXT..].copy_from_slice(chunk);
        let err = |e: ort::Error| Error::Vad(e.to_string());
        let sr = [TARGET_RATE as i64];
        let outputs = self
            .session
            .run(ort::inputs![
                "input" => TensorRef::from_array_view(([1usize, CONTEXT + CHUNK], &self.input[..])).map_err(err)?,
                "state" => TensorRef::from_array_view(([2usize, 1, 128], &self.state[..])).map_err(err)?,
                "sr" => TensorRef::from_array_view(((), &sr[..])).map_err(err)?,
            ])
            .map_err(err)?;
        let (_, prob) = outputs["output"].try_extract_tensor::<f32>().map_err(err)?;
        let prob = prob[0];
        let (_, state) = outputs["stateN"].try_extract_tensor::<f32>().map_err(err)?;
        self.state.copy_from_slice(state);
        Ok(prob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(crate) fn model_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/silero_vad.onnx")
    }

    #[test]
    #[ignore = "needs models/silero_vad.onnx (scripts/fetch-models.sh)"]
    fn silence_is_not_speech_and_state_carries_over() {
        let mut vad = SileroVad::new(&model_path()).unwrap();
        for _ in 0..20 {
            assert!(vad.process(&[0.0; CHUNK]).unwrap() < 0.1);
        }
        assert!(vad.state.iter().any(|&s| s != 0.0), "state is updated");
        vad.reset();
        assert!(vad.state.iter().all(|&s| s == 0.0));
    }
}
