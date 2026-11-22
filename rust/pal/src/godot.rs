//! The app's entry point: Godot's `main.gd` calls `PalNode::start`, which starts the conversation's thread
//! (`conversation::Agent`). `process` turns its events into Godot signals each frame.

use crate::conversation::{Agent, AgentConfig, AgentEvent, State};
use crate::speak::fish::FishConfig;
use crate::think::language::Language;
use crate::think::persona::DEFAULT_NAME;
use crate::think::remote_llm::{Provider, RemoteConfig};
use godot::classes::{INode, Node};
use godot::prelude::*;

struct PalExtension;

#[gdextension]
unsafe impl ExtensionLibrary for PalExtension {
    fn on_stage_init(stage: godot::init::InitStage) {
        if stage == godot::init::InitStage::Scene {
            crate::platform::init_log();
            init_tracing();
        }
    }
}

/// Tracing to stderr, filtered like `RUST_LOG` (e.g. `pal=debug,mq_bridge=debug`; see `platform::log_filter`);
/// without a filter it stays silent.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::LevelFilter;
    let filter =
        EnvFilter::builder().with_default_directive(LevelFilter::OFF.into()).parse_lossy(crate::platform::log_filter());
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::io::stderr).try_init();
}

#[derive(GodotClass)]
#[class(base = Node)]
pub struct PalNode {
    agent: Option<Agent>,
    /// The stopped agent until its thread has finished, and a `start` (model dir) waiting for that.
    stopping: Option<Agent>,
    pending_start: Option<GString>,
    /// Pocket TTS catalog voice for `language`; takes effect on `start`.
    #[export]
    pocket_voice: GString,
    /// Fish Audio speaks when both key and voice ID are set; takes effect on `start`.
    #[export]
    fish_api_key: GString,
    #[export]
    fish_voice_id: GString,
    #[export]
    fish_model: GString,
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
    /// Parakeet TDT folder in the model dir; takes effect on `start`.
    #[export]
    stt_model: GString,
    /// How Pal addresses the user; takes effect on `start`.
    #[export]
    user_name: GString,
    /// "local", "ollama", "openai" or "anthropic"; takes effect on `start`.
    #[export]
    llm_provider: GString,
    /// The server's API base; empty for the provider's default.
    #[export]
    llm_url: GString,
    /// The model name on the server.
    #[export]
    llm_remote_model: GString,
    #[export]
    llm_api_key: GString,
    /// The local model answers when the server fails; it is loaded only then.
    #[export]
    llm_fallback: bool,
    /// Pal answers only when called by name or asked for a joke, and speaks up when the room goes quiet; takes
    /// effect on `start`.
    #[export]
    party_mode: bool,
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
            fish_api_key: GString::new(),
            fish_voice_id: GString::new(),
            fish_model: GString::new(),
            language: "de".into(),
            barge_in: true,
            smart_turn: true,
            llm_model: "Qwen3.5-2B-Q4_K_M.gguf".into(),
            stt_model: "parakeet-tdt-0.6b-v3".into(),
            user_name: DEFAULT_NAME.into(),
            llm_provider: "local".into(),
            llm_url: GString::new(),
            llm_remote_model: GString::new(),
            llm_api_key: GString::new(),
            llm_fallback: false,
            party_mode: false,
            base,
        }
    }

    fn process(&mut self, _delta: f64) {
        if self.stopping.as_ref().is_some_and(Agent::is_finished) {
            self.stopping = None;
            if let Some(model_dir) = self.pending_start.take() {
                self.start(model_dir);
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
                AgentEvent::ReplyFailed(e) => s.reply_failed().emit(&GString::from(&e)),
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
    /// A turn got no reply, e.g. the LLM server isn't reachable; Pal keeps listening.
    #[signal]
    fn reply_failed(message: GString);

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

    /// Starts Pal: builds the `AgentConfig` from the settings and starts the conversation (`Agent::start`).
    /// After `stop`, the new agent starts once the old one has finished.
    /// Absolute path: `model_dir` holds the models; missing ones are downloaded first.
    #[func]
    fn start(&mut self, model_dir: GString) {
        if self.stopping.is_some() {
            self.pending_start = Some(model_dir);
            return;
        }
        let dir = std::path::PathBuf::from(model_dir.to_string());
        let Some(language) = Language::from_code(&self.language.to_string()) else {
            let message = format!("unknown language {}", self.language);
            self.signals().failed().emit(&message);
            return;
        };
        let provider = self.llm_provider.to_string();
        let remote = match Provider::parse(&provider) {
            Some(provider) => Some(RemoteConfig {
                provider,
                url: self.llm_url.to_string(),
                model: self.llm_remote_model.to_string(),
                api_key: self.llm_api_key.to_string(),
            }),
            None if provider == "local" => None,
            None => {
                self.signals().failed().emit(&format!("unknown llm provider {provider}"));
                return;
            }
        };
        let user_name = self.user_name.to_string();
        let fish = (!self.fish_api_key.is_empty() && !self.fish_voice_id.is_empty()).then(|| FishConfig {
            api_key: self.fish_api_key.to_string(),
            voice_id: self.fish_voice_id.to_string(),
            model: self.fish_model.to_string(),
        });
        self.agent = Some(Agent::start(AgentConfig {
            model_dir: dir,
            smart_turn: self.smart_turn,
            stt_model: self.stt_model.to_string(),
            llm_model: self.llm_model.to_string(),
            voice: self.pocket_voice.to_string(),
            fish,
            barge_in: self.barge_in,
            language,
            user_name: if user_name.trim().is_empty() { DEFAULT_NAME.into() } else { user_name },
            remote,
            fallback: self.llm_fallback,
            party: self.party_mode,
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
