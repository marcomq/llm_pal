//! The control center: Pal's main loop, the handler of the route `pal_conversation`. Everything Pal does starts here.
//!
//! `run` loads the parts on the agent's thread; how they are connected is drawn in `transport.rs`.
//!
//! ```text
//!   Phase        input                    →
//!   Prefilling   Prefilled, then mic live   Listening → AgentEvent::Ready (at the latest after LIVE_TIMEOUT)
//!   Listening    speech (cue → turn)        Replying, or Unfinished                conversation/turn.rs
//!   Unfinished   speech starts or goes on   Listening, the audio kept in `carry`   conversation/unfinished.rs
//!                UNFINISHED_WAIT passed     answered → Replying                    conversation/answer.rs
//!   Replying     reports, held reply, …     Replying until done                    conversation/follow.rs
//!                Done                       Listening
//! ```
//!
//! Alongside: `barge_in.rs` (speech while Pal speaks).
//! Godot reads `Agent::state`, `level` and `hearing` lock-free and drains `Agent::poll` for [`AgentEvent`]s.
//! See AGENTS.md for each step.

mod answer;
mod barge_in;
mod follow;
#[cfg(test)]
mod perf;
#[cfg(test)]
mod tests;
mod turn;
mod unfinished;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use std::time::Instant;

use mq_bridge::blocking_handler::HandlerContext;
use mq_bridge::{CanonicalMessage, Handled};
use tokio::runtime::Handle;

use crate::downloader;
use crate::hear::event::{Event, Segment};
use crate::hear::mic::{self, MicConfig, MicLink};
use crate::speak::playback::PlaybackState;
use crate::speak::tts::{self, PocketSpeaker};
use crate::think::brain::Model;
use crate::think::language::Language;
use crate::think::party::Party;
use crate::think::persona::Persona;
use crate::think::prosody_tags::ProsodyTags;
use crate::think::remote_llm::RemoteConfig;
use crate::transport::{self, Input, Parts, Spoken, Transport, trace_received};
use crate::{Error, Result};
use turn::{Timing, Turn};

pub struct AgentConfig {
    /// Holds every model; missing files are downloaded into it first.
    pub model_dir: PathBuf,
    /// Smart Turn ends a turn at a pause when the speech sounds finished; otherwise after a fixed silence.
    pub smart_turn: bool,
    /// The Parakeet TDT folder in `model_dir`.
    pub stt_model: String,
    /// The GGUF file in `model_dir`.
    pub llm_model: String,
    /// Kyutai Pocket TTS voice, such as "juergen".
    pub voice: String,
    /// Speech during output stops it, with Pal's voice removed from the microphone; otherwise such speech is
    /// ignored.
    pub barge_in: bool,
    pub language: Language,
    /// The user's name, as the texts address them; [`crate::think::persona::DEFAULT_NAME`] by default.
    pub user_name: String,
    /// A server answers instead of `llm_model`, which then isn't loaded unless `fallback` is set.
    pub remote: Option<RemoteConfig>,
    /// With `remote`, the local model answers when the server fails before any text; a reply that breaks off
    /// after some text is not answered again.
    pub fallback: bool,
    /// Party mode: Pal answers only when called by name or asked for a joke, and speaks up when the room's talk
    /// stops.
    pub party: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Loading,
    Listening,
    Thinking,
    Speaking,
    Stopped,
}

#[derive(Debug)]
pub enum AgentEvent {
    Ready {
        device: String,
    },
    /// What the user said, with prosody tags.
    Heard(String),
    /// A sentence of the reply, as it is queued for speech.
    Said(String),
    /// `first_sentence_ms` (until the first sound played) and `total_ms` (until the reply was queued) count from
    /// the end of the segment; a reply prepared during a pause can be ready at 0. `stt_ms` is the transcription
    /// time. Sent once the reply's first sound played, or when the next turn starts.
    TurnDone {
        reply: String,
        stt_ms: u128,
        first_sentence_ms: Option<u128>,
        total_ms: u128,
    },
    Interrupted,
    /// Speech during output without barge-in.
    Ignored,
    /// The model failed on a turn, e.g. a server that isn't reachable; the agent keeps listening.
    ReplyFailed(String),
    /// The agent stopped.
    Failed(String),
}

#[derive(Default)]
struct Shared {
    state: AtomicU8,
    quit: AtomicBool,
    output: OnceLock<Arc<PlaybackState>>,
    mic: OnceLock<Arc<MicLink>>,
}

impl Shared {
    fn set(&self, s: State) {
        self.state.store(s as u8, Ordering::Relaxed);
    }
}

/// The handle to the conversation's thread, held by the UI.
pub struct Agent {
    shared: Arc<Shared>,
    events: Receiver<AgentEvent>,
    worker: std::thread::JoinHandle<()>,
}

impl Agent {
    /// Returns at once; models load on the agent thread, which then sends `Ready` (or `Failed`).
    pub fn start(cfg: AgentConfig) -> Self {
        let shared = Arc::new(Shared::default());
        let (tx, events) = channel();
        let sh = shared.clone();
        let worker = std::thread::spawn(move || {
            match run(cfg, sh.clone(), tx.clone()) {
                Ok(()) | Err(Error::Stopped) => {}
                Err(e) => drop(tx.send(AgentEvent::Failed(e.to_string()))),
            }
            sh.set(State::Stopped);
        });
        Self { shared, events, worker }
    }

    /// Asks the agent thread to quit; `is_finished` tells when it has released microphone and models.
    pub fn stop(&self) {
        self.shared.quit.store(true, Ordering::SeqCst);
    }

    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    pub fn state(&self) -> State {
        match self.shared.state.load(Ordering::Relaxed) {
            0 => State::Loading,
            1 => State::Listening,
            2 => State::Thinking,
            3 => State::Speaking,
            _ => State::Stopped,
        }
    }

    /// Level of Pal's voice, 0..1.
    pub fn level(&self) -> f32 {
        self.shared.output.get().map_or(0.0, |o| o.level())
    }

    /// True while the user's speech is heard.
    pub fn hearing(&self) -> bool {
        self.shared.mic.get().is_some_and(|m| m.in_speech.load(Ordering::Relaxed))
    }

    pub fn poll(&self) -> Option<AgentEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Agent {
    /// Waits for the models to be released: llama.cpp aborts the process if it exits with Metal buffers still alive.
    fn drop(&mut self) {
        self.stop();
        let deadline = Instant::now() + STOP_WAIT;
        while !self.worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

const LIVE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long closing the app waits for the agent's thread, so a stuck one can't keep the app open.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// The agent's thread: loads everything, connects it (`transport::deploy`) with the conversation as the handler of
/// `[pal.inbox]` and waits until it ends, on `stop` or a lost microphone.
fn run(cfg: AgentConfig, shared: Arc<Shared>, events: Sender<AgentEvent>) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| Error::Routes(e.to_string()))?;
    let persona = Persona::kitt(cfg.language, &cfg.user_name);
    let (parts, mic, loading_since) = load(&cfg, &shared, &persona, rt.handle())?;
    // Kept to cut the voice off at the end; the conversation reaches it through `pal.speech`.
    let voice = parts.speaker.clone();
    let (ended_tx, ended) = channel();
    transport::deploy(rt.handle(), parts, move |transport| {
        transport.prefill(&persona.context())?;
        let conversation = Conversation::new(cfg, shared, events, transport, mic, persona, loading_since);
        Ok(conversation_handler(conversation, voice, ended_tx))
    })?;
    let _ = ended.recv();
    transport::stop_inbox(rt.handle());
    Ok(())
}

/// The handler of `[pal.inbox]`: `Conversation::handle` per input, until it fails.
fn conversation_handler(
    conversation: Conversation,
    voice: PocketSpeaker,
    ended_tx: Sender<()>,
) -> impl FnMut(CanonicalMessage, &HandlerContext) -> std::result::Result<Handled, mq_bridge::HandlerError> {
    let mut conversation = Some(conversation);
    move |msg: CanonicalMessage, _: &HandlerContext| {
        trace_received("conversation", &msg);
        // The route would swallow a panic and go on with a broken conversation: it ends it instead.
        let handled = |c: &mut Conversation| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.handle(Input::from_message(&msg))))
                .unwrap_or_else(|_| Err(Error::Routes("the conversation panicked".into())))
        };
        if let Some(c) = &mut conversation
            && let Err(e) = handled(c)
        {
            if !matches!(e, Error::Stopped) {
                c.send(AgentEvent::Failed(e.to_string()));
            }
            voice.stop();
            // A running reply would hold up its route's stop for seconds.
            if let Phase::Replying(turn) = &c.phase {
                c.transport.cancel(&turn.id);
            }
            // Releases the models and the reply routes on this thread, before the runtime goes.
            conversation = None;
            let _ = ended_tx.send(());
        }
        Ok(Handled::Ack)
    }
}

/// Downloads what is missing, then loads speech recognition, voice and LLM side by side; the first transcription
/// is slow, so it runs here too, and the local LLM reads the persona's context. Returns when all are ready.
fn load(cfg: &AgentConfig, shared: &Shared, persona: &Persona, rt: &Handle) -> Result<(Parts, Arc<MicLink>, Instant)> {
    let dir = &cfg.model_dir;
    let local = cfg.remote.is_none() || cfg.fallback;
    let pocket_files = downloader::pocket_files(cfg.language, &cfg.voice);
    let voice_file = dir.join(pocket_files.last().expect("pocket_files ends with the voice"));
    let files = [
        vec!["silero_vad.onnx".to_string()],
        if cfg.smart_turn { vec!["smart-turn-v3.2-cpu.onnx".into()] } else { vec![] },
        downloader::stt_files(&cfg.stt_model),
        pocket_files.clone(),
        if local { vec![cfg.llm_model.clone()] } else { vec![] },
    ];
    downloader::ensure(dir, &files.concat(), &shared.quit)?;
    tts::use_local_files(dir);

    let t0 = Instant::now();
    let took = move |what: &str| eprintln!("llm_pal: {what} after {} ms", t0.elapsed().as_millis());
    let stt = std::thread::spawn({
        let (rt, model) = (rt.clone(), dir.join(&cfg.stt_model));
        move || {
            let stt = transport::start_stt(&rt, &model)?;
            took("speech recognition ready");
            Ok(stt)
        }
    });
    let pocket = std::thread::spawn({
        let (voice, language) = (voice_file.to_string_lossy().into_owned(), cfg.language);
        move || {
            let speaker = PocketSpeaker::new(&voice, language)?;
            took("voice ready");
            Ok(speaker)
        }
    });
    // With a server, the local model is only the fallback: it isn't warmed.
    let warm = cfg.remote.is_none().then(|| persona.context());
    let (path, fixed) = (dir.join(&cfg.llm_model), persona.fixed_messages());
    let model = local.then(|| {
        std::thread::spawn(move || {
            let (model, thread) = Model::start(&path, fixed)?;
            took("llm loaded");
            if let Some(context) = warm {
                model.prefill(context, None).map_err(|e| Error::Llm(e.to_string()))?;
            }
            Ok((model, thread))
        })
    });
    // All loaders finish before any error returns, so none outlives the runtime or overlaps a restart.
    let (stt, speaker) = (joined(stt, Error::Stt), joined(pocket, Error::Tts));
    let model = model.map(|m| joined(m, Error::Llm)).transpose();
    let (stt, speaker, model) = (stt?, speaker?, model?);

    let _ = shared.output.set(speaker.output());
    let echo = cfg.barge_in.then(|| speaker.output().take_echo_reference()).flatten();
    let mic = MicLink::new(echo);
    let _ = shared.mic.set(mic.clone());
    let mic_config = MicConfig {
        vad_model: dir.join("silero_vad.onnx"),
        turn_model: cfg.smart_turn.then(|| dir.join("smart-turn-v3.2-cpu.onnx")),
        device: None,
        drop_echo: !cfg.barge_in,
    };
    let parts = Parts {
        mic: mic::endpoint(&mic_config, mic.clone())?,
        stt,
        model,
        server: cfg.remote.clone(),
        speaker,
        language: cfg.language,
    };
    Ok((parts, mic, t0))
}

fn joined<T>(loader: std::thread::JoinHandle<Result<T>>, error: fn(String) -> Error) -> Result<T> {
    loader.join().unwrap_or_else(|_| Err(error("loading panicked".into())))
}

struct Conversation {
    cfg: AgentConfig,
    shared: Arc<Shared>,
    events: Sender<AgentEvent>,
    transport: Transport,
    mic: Arc<MicLink>,
    persona: Persona,
    party: Option<Party>,
    tags: ProsodyTags,
    phase: Phase,
    /// Speech that came while a reply ran, answered once it is done.
    pending: Option<Segment>,
    /// Speech of an unfinished sentence, put before the next segment.
    carry: Option<Vec<f32>>,
    turns: u64,
    timing: Option<Timing>,
    /// Pal's voice is paused for speech during output since then (`None` while it keeps quiet): words stop the
    /// reply, a cough or noise lets it go on.
    barge: Option<Option<Instant>>,
    /// The sentences of the last reply in the history, cut to what was heard if the user interrupts it.
    last_reply: Option<Vec<String>>,
    /// How many sentences of which turn began to play (`Spoken::Audible`).
    heard: (String, usize),
    record_dir: Option<PathBuf>,
}

impl Conversation {
    fn new(
        cfg: AgentConfig,
        shared: Arc<Shared>,
        events: Sender<AgentEvent>,
        transport: Transport,
        mic: Arc<MicLink>,
        persona: Persona,
        loading_since: Instant,
    ) -> Self {
        Self {
            party: cfg.party.then(|| Party::new(cfg.language)),
            cfg,
            shared,
            events,
            transport,
            mic,
            persona,
            tags: ProsodyTags::default(),
            phase: Phase::Prefilling { since: loading_since, live_by: None },
            pending: None,
            carry: None,
            turns: 0,
            timing: None,
            barge: None,
            last_reply: None,
            heard: (String::new(), 0),
            record_dir: std::env::var_os("PAL_RECORD_DIR").map(PathBuf::from),
        }
    }

    fn quit(&self) -> bool {
        self.shared.quit.load(Ordering::SeqCst)
    }

    fn hearing(&self) -> bool {
        self.mic.in_speech.load(Ordering::Relaxed)
    }

    /// Pal's voice plays: from the first sentence queued or heard until `Spoken::Quiet`.
    fn speaking(&self) -> bool {
        self.mic.output_active.load(Ordering::Relaxed)
    }

    fn send(&self, event: AgentEvent) {
        let _ = self.events.send(event);
    }

    /// The main loop's body: moves the conversation on by one input from `[pal.inbox]`; `None` is a tick.
    /// An error ends the conversation.
    fn handle(&mut self, input: Option<Input>) -> Result<()> {
        if self.quit() {
            return Err(Error::Stopped);
        }
        match &input {
            Some(Input::Event(Event::Lost)) => return Err(Error::Audio("the microphone stopped".into())),
            Some(Input::Spoken(Spoken::Audible { turn, at })) => self.audible(turn, *at),
            Some(Input::Spoken(Spoken::Quiet)) => self.mic.output_active.store(false, Ordering::Relaxed),
            _ => {}
        }
        self.phase = match (std::mem::replace(&mut self.phase, Phase::Listening), input) {
            (Phase::Prefilling { since, live_by: None }, Some(Input::Spoken(Spoken::Prefilled))) => {
                self.ready(since, None)
            }
            (Phase::Prefilling { .. }, Some(Input::Spoken(Spoken::Failed { error, .. }))) => {
                return Err(Error::Llm(error));
            }
            (Phase::Prefilling { since, live_by: Some(by) }, _) => self.ready(since, Some(by)),
            (prefilling @ Phase::Prefilling { .. }, _) => prefilling,
            (Phase::Listening, input) => self.listening(input),
            // New speech, or after a forced split the rest of the same speech, which comes without a `SpeechStart`.
            (
                Phase::Unfinished { seg, .. },
                input @ Some(Input::Event(Event::SpeechStart { .. } | Event::Pause(_) | Event::Segment(_))),
            ) => {
                self.carry = Some(seg.pcm);
                self.listening(input)
            }
            (Phase::Unfinished { until, seg, text, t0, stt_ms, text_at }, _) if Instant::now() >= until => {
                self.answer(seg, false, text, t0, stt_ms, text_at)
            }
            (unfinished @ Phase::Unfinished { .. }, _) => unfinished,
            (Phase::Replying(turn), input) => self.follow(turn, input),
        };
        if matches!(self.phase, Phase::Listening)
            && let Some(seg) = self.pending.take()
        {
            self.phase = self.listening(Some(Input::Event(Event::Segment(seg))));
        }
        self.shared.set(match self.phase {
            Phase::Prefilling { .. } => State::Loading,
            _ if self.speaking() => State::Speaking,
            Phase::Listening => State::Listening,
            Phase::Unfinished { .. } | Phase::Replying(_) => State::Thinking,
        });
        Ok(())
    }

    /// The first prompt, which is slow, has left the system prompt in the LLM's cache: Pal listens once the
    /// microphone is live, or at `live_by` without it. Checked per input, so `stop` and `Lost` still come through.
    fn ready(&mut self, loading_since: Instant, live_by: Option<Instant>) -> Phase {
        let live_by = live_by.unwrap_or_else(|| {
            eprintln!("llm_pal: llm prefilled after {} ms", loading_since.elapsed().as_millis());
            Instant::now() + LIVE_TIMEOUT
        });
        let live = self.mic.live();
        if !live && Instant::now() < live_by {
            return Phase::Prefilling { since: loading_since, live_by: Some(live_by) };
        }
        if !live {
            eprintln!("llm_pal: no sound from '{}' yet", self.mic.device());
        }
        self.send(AgentEvent::Ready { device: self.mic.device().to_string() });
        Phase::Listening
    }

    /// No turn runs: an input that asks for an answer starts one.
    fn listening(&mut self, input: Option<Input>) -> Phase {
        if input.is_none() {
            self.resume_if_quiet();
        }
        match self.cue(input) {
            Some(cue) => self.turn(cue),
            None => Phase::Listening,
        }
    }
}

/// Where the conversation stands between two inputs.
#[expect(clippy::large_enum_variant, reason = "one move per input, every 20 ms at most")]
enum Phase {
    /// Loaded; the LLM reads the system prompt, then (`live_by` set) the microphone gets live. Its events are dropped.
    Prefilling { since: Instant, live_by: Option<Instant> },
    /// No turn runs; Pal may still be speaking the last reply.
    Listening,
    /// A final segment whose transcript sounds unfinished: answered at `until` unless the user goes on first.
    Unfinished { until: Instant, seg: Segment, text: String, t0: Instant, stt_ms: u128, text_at: Instant },
    /// A started turn, until its reply is done.
    Replying(Turn),
}
