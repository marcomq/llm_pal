//! Kyutai Pocket TTS in-process. All modules except this one are copied from kyutai-labs/pocket-tts v4.0.1
//! (`rust/src`, MIT, see LICENSE-KYUTAI); local edits are marked "pal-tts change".

#[allow(dead_code)]
mod audio;
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod defaults;
mod download;
#[allow(dead_code)]
mod model;
mod rng;
#[allow(dead_code)]
mod text;
mod tokenizer;
mod weights;

use std::sync::atomic::AtomicBool;

pub use anyhow::{Error, Result};
pub use download::use_local_files;

type Backend = xn::Unquantized<f32, xn::CpuDevice>;

pub struct PocketTts {
    model: model::TtsModel<Backend>,
    voice: model::VoiceState<Backend>,
}

impl PocketTts {
    /// `language` such as "german"; `voice` a catalog name such as "juergen" or a `.safetensors` voice state.
    /// Downloads model and voice into the Hugging Face cache on first use.
    pub fn load(language: &str, voice: &str) -> Result<Self> {
        let (config, origin) = config::Config::load(Some(language), None)?;
        let model = model::TtsModel::load(config, origin, &model::LoadOptions::default())?;
        let voice = model.get_state_for_audio_prompt(voice, false)?;
        Ok(Self { model, voice })
    }

    pub fn sample_rate(&self) -> u32 {
        self.model.sample_rate() as u32
    }

    /// Streams `text` as mono samples to `on_audio`; setting `stop` ends it early.
    pub fn synthesize(
        &self,
        text: &str,
        stop: &AtomicBool,
        on_audio: &mut (dyn FnMut(&[f32]) -> Result<()> + Send),
    ) -> Result<()> {
        self.model.generate_audio_stream(&self.voice, text, defaults::MAX_TOKEN_PER_CHUNK, None, stop, on_audio)
    }
}
