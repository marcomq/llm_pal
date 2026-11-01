//! Godot binding: `PalNode` runs the conversation agent and exposes its state, Pal's voice level and texts.
//! Pal speaks with Kyutai Pocket TTS.

use godot::classes::{INode, Node};
use godot::prelude::*;
use pal_brain::agent::{Agent, AgentConfig, AgentEvent, State};
use pal_brain::language::Language;
use pal_brain::remote::{Provider, RemoteConfig};

struct PalExtension;

#[gdextension]
unsafe impl ExtensionLibrary for PalExtension {
    fn on_stage_init(stage: godot::init::InitStage) {
        #[cfg(target_os = "android")]
        if stage == godot::init::InitStage::Scene {
            logcat::forward_stderr();
        }
        let _ = stage;
    }
}

/// Android drops a native library's stderr; this sends its lines (`llm_pal: ...`) to logcat, tag "llm_pal".
#[cfg(target_os = "android")]
mod logcat {
    use std::ffi::{CString, c_char};
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    const INFO: i32 = 4;

    pub fn forward_stderr() {
        let mut fds = [0; 2];
        // SAFETY: `pipe` fills both fds; fd 2 becomes a copy of the write end, which is then closed.
        let read = unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 || libc::dup2(fds[1], 2) < 0 {
                return;
            }
            libc::close(fds[1]);
            std::fs::File::from_raw_fd(fds[0])
        };
        std::thread::spawn(move || {
            for line in BufReader::new(read).lines().map_while(Result::ok) {
                if let Ok(text) = CString::new(line) {
                    // SAFETY: both strings are NUL-terminated and outlive the call.
                    unsafe { __android_log_write(INFO, c"llm_pal".as_ptr(), text.as_ptr()) };
                }
            }
        });
    }
}

#[derive(GodotClass)]
#[class(base = Node)]
pub struct PalNode {
    agent: Option<Agent>,
    /// The stopped agent until its thread has finished, and a `start` (model dir) waiting for that.
    stopping: Option<Agent>,
    pending_start: Option<GString>,
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
            stt_model: "parakeet-tdt-0.6b-v3".into(),
            user_name: pal_brain::persona::DEFAULT_NAME.into(),
            llm_provider: "local".into(),
            llm_url: GString::new(),
            llm_remote_model: GString::new(),
            llm_api_key: GString::new(),
            llm_fallback: false,
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

    /// Absolute path: `model_dir` holds silero_vad.onnx, the STT and the Qwen model.
    /// After `stop`, the new agent starts once the old one has finished.
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
        // Pocket's model comes from `model_dir/pocket` if it's there (`push-models` copies it). Android can't
        // download it (its TLS check needs Java code); the desktop downloads it otherwise.
        let local = pal_brain::pocket_speaker::use_local_files(&dir.join("pocket"), language);
        if cfg!(target_os = "android") && !local {
            let message = format!("no Pocket TTS model in {}/pocket", dir.display());
            self.signals().failed().emit(&message);
            return;
        }
        self.agent = Some(Agent::start(AgentConfig {
            vad_model: dir.join("silero_vad.onnx"),
            turn_model: Some(dir.join("smart-turn-v3.2-cpu.onnx")).filter(|p| self.smart_turn && p.exists()),
            device: None,
            stt_model: dir.join(self.stt_model.to_string()),
            llm_model: dir.join(self.llm_model.to_string()),
            voice: self.pocket_voice.to_string(),
            barge_in: self.barge_in,
            language,
            user_name: if user_name.trim().is_empty() { pal_brain::persona::DEFAULT_NAME.into() } else { user_name },
            remote,
            fallback: self.llm_fallback,
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
