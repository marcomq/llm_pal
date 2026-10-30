//! Godot binding: `PalNode` runs the conversation agent and exposes its state, Pal's voice level and texts.
//! Pal speaks with Kyutai Pocket TTS.

use godot::classes::{INode, Node};
use godot::prelude::*;
use pal_brain::agent::{Agent, AgentConfig, AgentEvent, State};
use pal_brain::language::Language;

struct PalExtension;

#[gdextension]
unsafe impl ExtensionLibrary for PalExtension {}

#[derive(GodotClass)]
#[class(base = Node)]
pub struct PalNode {
    agent: Option<Agent>,
    /// The stopped agent until its thread has finished, and a `start` (model dir, STT library) waiting for that.
    stopping: Option<Agent>,
    pending_start: Option<(GString, GString)>,
    /// Pocket TTS catalog voice, the same in every language.
    #[export]
    pocket_voice: GString,
    /// "de" or "en": what Pal listens for and speaks; takes effect on `start`.
    #[export]
    language: GString,
    /// Speech during output stops it; echo cancellation keeps Pal from hearing himself.
    #[export]
    barge_in: bool,
    /// Smart Turn ends a turn at a pause when the speech sounds finished; takes effect on `start`.
    #[export]
    smart_turn: bool,
    /// GGUF file name in the model dir; takes effect on `start`.
    #[export]
    llm_model: GString,
    /// Whisper `.bin` file or Parakeet folder in the model dir; takes effect on `start`.
    #[export]
    stt_model: GString,
    base: Base<Node>,
}

#[godot_api]
impl INode for PalNode {
    fn init(base: Base<Node>) -> Self {
        Self {
            agent: None,
            stopping: None,
            pending_start: None,
            pocket_voice: "juergen".into(),
            language: "de".into(),
            barge_in: true,
            smart_turn: true,
            llm_model: "Qwen3.5-2B-Q4_K_M.gguf".into(),
            stt_model: "ggml-small-q5_1.bin".into(),
            base,
        }
    }

    fn process(&mut self, _delta: f64) {
        if self.stopping.as_ref().is_some_and(Agent::is_finished) {
            self.stopping = None;
            if let Some((model_dir, stt_library)) = self.pending_start.take() {
                self.start(model_dir, stt_library);
            }
        }
        while let Some(event) = self.agent.as_ref().and_then(Agent::poll) {
            let mut s = self.signals();
            match event {
                AgentEvent::Ready { device } => s.ready_to_listen().emit(&GString::from(&device)),
                AgentEvent::Heard(line) => s.heard().emit(&GString::from(&line)),
                AgentEvent::Said(sentence) => s.said().emit(&GString::from(&sentence)),
                AgentEvent::TurnDone { stt_ms, first_sentence_ms, .. } => {
                    s.turn_done().emit(stt_ms as i64, first_sentence_ms.map_or(-1, |ms| ms as i64))
                }
                AgentEvent::Interrupted => s.interrupted().emit(),
                AgentEvent::Ignored => s.ignored().emit(),
                AgentEvent::Failed(e) => s.failed().emit(&GString::from(&e)),
            }
        }
    }
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

    /// Absolute paths: `model_dir` holds silero_vad.onnx, the STT and the Qwen model; `stt_library` is libpal_stt.
    /// After `stop`, the new agent starts once the old one has finished.
    #[func]
    fn start(&mut self, model_dir: GString, stt_library: GString) {
        if self.stopping.is_some() {
            self.pending_start = Some((model_dir, stt_library));
            return;
        }
        let dir = std::path::PathBuf::from(model_dir.to_string());
        let Some(language) = Language::from_code(&self.language.to_string()) else {
            let message = format!("unknown language {}", self.language);
            self.signals().failed().emit(&message);
            return;
        };
        // Android can't download Pocket's model (its TLS check needs Java code), so `push-models` copies it.
        #[cfg(target_os = "android")]
        if !pal_brain::pocket_speaker::use_local_files(&dir.join("pocket"), language) {
            let message = format!("no Pocket TTS model in {}/pocket", dir.display());
            self.signals().failed().emit(&message);
            return;
        }
        self.agent = Some(Agent::start(AgentConfig {
            vad_model: dir.join("silero_vad.onnx"),
            turn_model: Some(dir.join("smart-turn-v3.2-cpu.onnx")).filter(|p| self.smart_turn && p.exists()),
            device: None,
            stt_library: stt_library.to_string().into(),
            stt_model: dir.join(self.stt_model.to_string()),
            llm_model: dir.join(self.llm_model.to_string()),
            voice: self.pocket_voice.to_string(),
            barge_in: self.barge_in,
            language,
        }));
    }

    #[func]
    fn stop(&mut self) {
        self.pending_start = None;
        if let Some(agent) = self.agent.take() {
            agent.stop();
            self.stopping = Some(agent);
        }
    }

    /// One of the state constants.
    #[func]
    fn state(&self) -> i32 {
        if self.pending_start.is_some() {
            return State::Loading as i32;
        }
        self.agent.as_ref().map_or(State::Stopped, Agent::state) as i32
    }

    /// True while the user's speech is heard.
    #[func]
    fn hearing(&self) -> bool {
        self.agent.as_ref().is_some_and(Agent::hearing)
    }

    /// Level of Pal's voice, 0..1.
    #[func]
    fn level(&self) -> f32 {
        self.agent.as_ref().map_or(0.0, Agent::level)
    }
}
