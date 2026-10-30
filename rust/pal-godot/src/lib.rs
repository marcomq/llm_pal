//! Godot binding: `PalNode` runs the conversation agent and exposes its state, Pal's voice level and texts.
//! Pal speaks with Kyutai Pocket TTS; on Android without its model files, Godot's text-to-speech does.

use std::sync::Arc;

use godot::classes::{DisplayServer, INode, Node, Time};
use godot::prelude::*;
use pal_brain::agent::{Agent, AgentConfig, AgentEvent, State, Voice};
use pal_brain::language::Language;
use pal_brain::speaker::ExternalSpeech;

struct PalExtension;

#[gdextension]
unsafe impl ExtensionLibrary for PalExtension {}

#[derive(GodotClass)]
#[class(base = Node)]
pub struct PalNode {
    agent: Option<Agent>,
    /// Set when Godot speaks; `tts_voice` is its voice id.
    speech: Option<Arc<ExternalSpeech>>,
    tts_voice: GString,
    /// Part of the system voice name, for Godot's text-to-speech (Android without Pocket model).
    #[export]
    voice: GString,
    /// Pocket TTS catalog voice, the same in every language.
    #[export]
    pocket_voice: GString,
    /// "de" or "en": what Pal listens for and speaks; takes effect on `start`.
    #[export]
    language: GString,
    /// Speaking rate as a factor of the voice's default.
    #[export]
    rate: f32,
    /// Speech during output stops it (needs headphones).
    #[export]
    barge_in: bool,
    base: Base<Node>,
}

#[godot_api]
impl INode for PalNode {
    fn init(base: Base<Node>) -> Self {
        Self {
            agent: None,
            speech: None,
            tts_voice: GString::new(),
            voice: "Anna".into(),
            pocket_voice: "juergen".into(),
            language: "de".into(),
            rate: 1.0,
            barge_in: false,
            base,
        }
    }

    fn process(&mut self, _delta: f64) {
        let mut to_speak = Vec::new();
        while let Some(event) = self.agent.as_ref().and_then(Agent::poll) {
            let mut s = self.signals();
            match event {
                AgentEvent::Ready { device } => s.ready_to_listen().emit(&GString::from(&device)),
                AgentEvent::Heard(line) => s.heard().emit(&GString::from(&line)),
                AgentEvent::Said(sentence) => {
                    s.said().emit(&GString::from(&sentence));
                    to_speak.push(sentence);
                }
                AgentEvent::TurnDone { stt_ms, first_sentence_ms, .. } => {
                    s.turn_done().emit(stt_ms as i64, first_sentence_ms.map_or(-1, |ms| ms as i64))
                }
                AgentEvent::Interrupted => s.interrupted().emit(),
                AgentEvent::Ignored => s.ignored().emit(),
                AgentEvent::Failed(e) => s.failed().emit(&GString::from(&e)),
            }
        }
        if let Some(speech) = &self.speech {
            let mut ds = DisplayServer::singleton();
            if speech.take_stop() {
                ds.tts_stop();
            }
            for sentence in &to_speak {
                ds.tts_speak_ex(sentence.as_str(), &self.tts_voice).volume(100).rate(self.rate).done();
            }
            speech.set_speaking(ds.tts_is_speaking());
        }
    }
}

#[cfg(not(target_os = "macos"))]
/// Id of a Godot TTS voice for `language`, preferring one whose name contains `wanted`; empty if there is none.
fn voice_for(language: Language, wanted: &str) -> GString {
    let field = |v: &VarDictionary, key: &str| v.get_or_nil(&key.to_variant()).to_string();
    let matching: Vec<_> = DisplayServer::singleton()
        .tts_get_voices()
        .iter_shared()
        .filter(|v| field(v, "language").starts_with(language.code()))
        .collect();
    let pick = matching.iter().find(|v| field(v, "name").contains(wanted)).or(matching.first());
    pick.map(|v| GString::from(&field(v, "id"))).unwrap_or_default()
}

#[godot_api]
impl PalNode {
    /// Models load in the background; `ready_to_listen` or `failed` follows.
    #[signal]
    fn ready_to_listen(device: GString);
    #[signal]
    fn heard(text: GString);
    #[signal]
    fn said(sentence: GString);
    /// Milliseconds from the end of the user's speech to the transcript and to Pal's first sentence (-1 if Pal said
    /// nothing).
    #[signal]
    fn turn_done(stt_ms: i64, first_sentence_ms: i64);
    #[signal]
    fn interrupted();
    #[signal]
    fn ignored();
    #[signal]
    fn failed(message: GString);

    #[constant]
    const LOADING: i32 = State::Loading as i32;
    #[constant]
    const LISTENING: i32 = State::Listening as i32;
    #[constant]
    const THINKING: i32 = State::Thinking as i32;
    #[constant]
    const SPEAKING: i32 = State::Speaking as i32;
    #[constant]
    const STOPPED: i32 = State::Stopped as i32;

    /// Absolute paths: `model_dir` holds silero_vad.onnx, the Whisper and the Qwen model; `stt_library` is libpal_stt.
    #[func]
    fn start(&mut self, model_dir: GString, stt_library: GString) {
        let dir = std::path::PathBuf::from(model_dir.to_string());
        let Some(language) = Language::from_code(&self.language.to_string()) else {
            let message = format!("unknown language {}", self.language);
            self.signals().failed().emit(&message);
            return;
        };
        let pocket = Voice::Pocket { voice: self.pocket_voice.to_string() };
        #[cfg(not(target_os = "android"))]
        let voice = pocket;
        // Android can't download Pocket's model (its TLS check needs Java code), so `push-models` copies it.
        #[cfg(target_os = "android")]
        let voice = if pal_brain::pocket_speaker::use_local_files(&dir.join("pocket"), language) {
            pocket
        } else {
            godot_warn!("no Pocket TTS model in {}/pocket, Godot's voice speaks", dir.display());
            self.tts_voice = voice_for(language, &self.voice.to_string());
            if self.tts_voice.is_empty() {
                self.signals().failed().emit(&format!("no system voice for language {}", language.code()));
                return;
            }
            let speech = Arc::new(ExternalSpeech::default());
            self.speech = Some(speech.clone());
            Voice::External(speech)
        };
        self.agent = Some(Agent::start(AgentConfig {
            vad_model: dir.join("silero_vad.onnx"),
            device: None,
            stt_library: stt_library.to_string().into(),
            stt_model: dir.join("ggml-base-q8_0.bin"),
            llm_model: dir.join("Qwen3.5-0.8B-Q4_K_M.gguf"),
            voice,
            barge_in: self.barge_in,
            language,
        }));
    }

    #[func]
    fn stop(&mut self) {
        self.agent = None;
        if self.speech.take().is_some() {
            DisplayServer::singleton().tts_stop();
        }
    }

    /// One of the state constants.
    #[func]
    fn state(&self) -> i32 {
        self.agent.as_ref().map_or(State::Stopped, Agent::state) as i32
    }

    /// Level of Pal's voice, 0..1. Godot's TTS reports none, so while it speaks this is a syllable-like stand-in.
    #[func]
    fn level(&self) -> f32 {
        if let Some(speech) = &self.speech {
            if !speech.speaking() {
                return 0.0;
            }
            let t = Time::singleton().get_ticks_msec() as f32 / 1000.0;
            let syllables = 0.5 + 0.5 * (t * std::f32::consts::TAU * 4.3).sin();
            let phrase = 0.6 + 0.4 * (t * std::f32::consts::TAU * 1.1 + 1.0).sin();
            return 0.02 + 0.08 * syllables * phrase;
        }
        self.agent.as_ref().map_or(0.0, Agent::level)
    }
}
