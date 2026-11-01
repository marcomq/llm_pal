//! The conversation loop on its own thread: microphone -> segment -> STT -> LLM -> sentences -> speech.
//! In `converse`: segments from `pal_core::mic_pipeline`, text from `stt`, canned replies from `persona`,
//! extra knowledge from `enrich`, the reply through `routes` to `tts`. See AGENTS.md for the whole flow.
//! Readers such as a UI poll `state()` and `level()` lock-free and drain `poll()` for texts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pal_core::playback::PlaybackState;
use pal_core::{Event, Processor, ProcessorConfig, Segment, SegmenterConfig};

use crate::enrich;
use crate::language::Language;
use crate::llm::{LlmConfig, LocalLlm};
use crate::persona::Persona;
use crate::prosody_tags::{self, ProsodyTags};
use crate::remote::{RemoteConfig, RemoteLlm};
use crate::routes::{Brain, ReplyRoutes, Spoken};
use crate::stt::Recognizer;
use crate::tts::PocketSpeaker;
use crate::{Error, Result};

pub struct AgentConfig {
    pub vad_model: PathBuf,
    /// Smart Turn model; without it a turn ends after a fixed silence.
    pub turn_model: Option<PathBuf>,
    /// Part of the input device name.
    pub device: Option<String>,
    /// The Parakeet TDT folder.
    pub stt_model: PathBuf,
    pub llm_model: PathBuf,
    /// Kyutai Pocket TTS catalog voice, such as "juergen".
    pub voice: String,
    /// Speech during output stops it, with Pal's voice removed from the microphone; otherwise such speech is
    /// ignored.
    pub barge_in: bool,
    pub language: Language,
    /// The user's name, as the texts address them; [`crate::persona::DEFAULT_NAME`] by default.
    pub user_name: String,
    /// A server answers instead of `llm_model`, which then isn't loaded unless `fallback` is set.
    pub remote: Option<RemoteConfig>,
    /// With `remote`, the local model answers when the server fails.
    pub fallback: bool,
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
    /// `first_sentence_ms` and `total_ms` count from the end of the segment; a reply prepared during a pause can
    /// be ready at 0. `stt_ms` is the transcription time.
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
    hearing: OnceLock<Arc<AtomicBool>>,
}

impl Shared {
    fn set(&self, s: State) {
        self.state.store(s as u8, Ordering::Relaxed);
    }
}

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
            if let Err(e) = converse(&cfg, &sh, &tx) {
                let _ = tx.send(AgentEvent::Failed(e.to_string()));
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
        self.shared.hearing.get().is_some_and(|h| h.load(Ordering::Relaxed))
    }

    pub fn poll(&self) -> Option<AgentEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

const LIVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Extra silence Pal waits for after a transcript that doesn't sound finished.
const UNFINISHED_WAIT: Duration = Duration::from_millis(700);

/// No closing punctuation, a trailing ellipsis or comma, or a last word such as "and".
fn sounds_unfinished(text: &str, language: Language) -> bool {
    let text = text.trim().trim_end_matches(['"', '\'', '“', '”', '»', '«', ')']);
    if text.ends_with("...") || text.ends_with('…') || !text.ends_with(['.', '!', '?']) {
        return true;
    }
    let last = text.trim_end_matches(['.', '!', '?']).rsplit(' ').next().unwrap_or("");
    language.profile().continues.contains(&last.to_lowercase().as_str())
}

/// When the user stopped speaking, for a segment that arrived at `at`.
fn speech_end(at: Instant, seg: &Segment) -> Instant {
    at.checked_sub(Duration::from_millis(seg.silence_ms)).unwrap_or(at)
}

fn joined<T>(loader: std::thread::JoinHandle<Result<T>>, error: fn(String) -> Error) -> Result<T> {
    loader.join().unwrap_or_else(|_| Err(error("loading panicked".into())))
}

/// Waits until the first prompt, which is slow, has left the system prompt in the LLM's cache.
fn prefilled(inbox: &Inbox) -> Result<()> {
    loop {
        match inbox.rx.recv() {
            Ok(Input::Spoken(Spoken::Prefilled)) => return Ok(()),
            Ok(Input::Spoken(Spoken::Failed { error, .. })) => return Err(Error::Llm(error)),
            Ok(_) => {}
            Err(_) => return Err(Error::Routes("reply routes stopped".into())),
        }
    }
}

enum Input {
    Event(Event),
    Spoken(Spoken),
}

/// Microphone events and route reports in arrival order, with what was set aside for later.
struct Inbox {
    rx: Receiver<Input>,
    events: VecDeque<Event>,
    spoken: VecDeque<Spoken>,
}

impl Inbox {
    /// Set-aside reports come first, then set-aside events if `events`, then new input.
    fn recv(&mut self, timeout: Duration, events: bool) -> Option<Input> {
        if let Some(s) = self.spoken.pop_front() {
            return Some(Input::Spoken(s));
        }
        let waiting = if events { self.events.pop_front() } else { None };
        waiting.map(Input::Event).or_else(|| self.rx.recv_timeout(timeout).ok())
    }

    /// True if the user speaks again within `UNFINISHED_WAIT`; other events are dropped.
    fn goes_on(&mut self) -> bool {
        let until = Instant::now() + UNFINISHED_WAIT;
        let mut kept = Vec::new();
        let mut speaks = false;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let next = self.events.pop_front().map(Input::Event).or_else(|| self.rx.recv_timeout(left).ok());
            match next {
                Some(Input::Event(Event::SpeechStart { .. })) => {
                    speaks = true;
                    break;
                }
                Some(Input::Spoken(s)) => kept.push(s),
                Some(_) => {}
                None => break,
            }
        }
        self.spoken.extend(kept);
        speaks
    }
}

/// `PAL_RECORD_DIR`: each segment as 16 kHz WAV plus the recognised text, as STT test data.
fn record(dir: &Path, pcm: &[f32], text: &str) -> std::result::Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let name = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis().to_string();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(dir.join(format!("{name}.wav")), spec)?;
    for s in pcm {
        wav.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    wav.finalize()?;
    std::fs::write(dir.join(format!("{name}.stt.txt")), text)?;
    Ok(())
}

fn converse(cfg: &AgentConfig, shared: &Shared, tx: &Sender<AgentEvent>) -> Result<()> {
    let send = |e| drop(tx.send(e));
    let record_dir = std::env::var_os("PAL_RECORD_DIR").map(PathBuf::from);
    // Speech recognition, voice and LLM load side by side; the first transcription is slow, so it runs here too.
    let t0 = Instant::now();
    let took = move |what: &str| eprintln!("llm_pal: {what} after {} ms", t0.elapsed().as_millis());
    let stt = std::thread::spawn({
        let model = cfg.stt_model.clone();
        move || {
            let stt = Recognizer::new(&model).and_then(|mut stt| stt.transcribe(&[0.0; 16_000]).map(|_| stt));
            took("speech recognition ready");
            stt
        }
    });
    let pocket = std::thread::spawn({
        let (voice, language) = (cfg.voice.clone(), cfg.language);
        move || {
            let speaker = PocketSpeaker::new(&voice, language);
            took("voice ready");
            speaker
        }
    });
    let local = || LocalLlm::new(&cfg.llm_model, LlmConfig::default());
    let brain = match &cfg.remote {
        None => Brain::Local(local()?),
        Some(remote) => {
            let fallback = if cfg.fallback { Some(local()?) } else { None };
            Brain::Remote { server: RemoteLlm::new(remote.clone()), fallback }
        }
    };
    took("llm loaded");
    let pcfg = ProcessorConfig {
        vad_model: cfg.vad_model.clone(),
        segmenter: SegmenterConfig::default(),
        device: cfg.device.clone(),
        drop_echo: !cfg.barge_in,
        turn_model: cfg.turn_model.clone(),
    };
    let mut persona = Persona::kitt(cfg.language, &cfg.user_name);
    let (input, rx) = channel();
    let report = input.clone();
    let report = move |s| drop(report.send(Input::Spoken(s)));
    let (speaker_tx, speaker_rx) = channel();
    let routes = ReplyRoutes::start(brain, persona.fixed_messages(), speaker_rx, cfg.language, report)?;
    let mut inbox = Inbox { rx, events: VecDeque::new(), spoken: VecDeque::new() };
    routes.prefill(&persona.context())?;
    let mut stt = joined(stt, Error::Stt)?;
    let mut speaker = joined(pocket, Error::Tts)?;
    let _ = speaker_tx.send(speaker.clone());
    let _ = shared.output.set(speaker.output());
    prefilled(&inbox)?;
    took("llm prefilled");
    let echo = cfg.barge_in.then(|| speaker.output().take_echo_reference()).flatten();
    let (processor, events) = Processor::start(&pcfg, echo)?;
    std::thread::spawn(move || {
        for e in events {
            if input.send(Input::Event(e)).is_err() {
                break;
            }
        }
    });
    let active = processor.output_active();
    let _ = shared.hearing.set(processor.in_speech());
    if !processor.wait_live(LIVE_TIMEOUT) {
        eprintln!("llm_pal: no sound from '{}' yet", processor.device_name());
    }
    send(AgentEvent::Ready { device: processor.device_name().to_string() });

    let mut tags = ProsodyTags::default();
    // Speech of an unfinished sentence, put before the next segment.
    let mut carry = None::<Vec<f32>>;
    let mut turns = 0u64;
    while !shared.quit.load(Ordering::SeqCst) {
        let speaking = speaker.speaking();
        active.store(speaking, Ordering::Relaxed);
        shared.set(if speaking { State::Speaking } else { State::Listening });
        // Reports of earlier turns are stale here.
        let Some(Input::Event(event)) = inbox.recv(Duration::from_millis(20), true) else { continue };
        // A reply to a pause is prepared but held back until the segment really ends.
        let (seg, early) = match event {
            Event::SpeechStart { .. } | Event::Resume => continue,
            Event::BargeIn { .. } => {
                if cfg.barge_in {
                    speaker.stop();
                    send(AgentEvent::Interrupted);
                }
                continue;
            }
            Event::Pause(seg) => (seg, true),
            Event::Segment(seg) => (seg, false),
        };
        let mut seg = seg;
        if let Some(before) = &carry {
            seg.pcm = [before.as_slice(), &seg.pcm].concat();
        }
        // Without barge-in, speech that overlaps Pal's output is ignored: it may be Pal's own voice.
        if seg.during_output && !cfg.barge_in {
            if !early {
                send(AgentEvent::Ignored);
            }
            continue;
        }

        shared.set(State::Thinking);
        let t0 = Instant::now();
        let tags_before = tags.clone();
        let text = match stt.transcribe(&seg.pcm) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("llm_pal: {e}");
                continue;
            }
        };
        let stt_ms = t0.elapsed().as_millis();
        if let Some(dir) = &record_dir
            && let Err(e) = record(dir, &seg.pcm, &text)
        {
            eprintln!("llm_pal: recording to {}: {e}", dir.display());
        }
        if text.is_empty() {
            continue;
        }
        if !early {
            if !seg.turn_complete && sounds_unfinished(&text, cfg.language) && inbox.goes_on() {
                carry = Some(seg.pcm);
                continue;
            }
            carry = None;
        }
        let line = Persona::user_line(&text, &prosody_tags::format(&tags.tag(&seg.features), cfg.language));

        turns += 1;
        let turn = format!("turn-{turns}");
        if !early {
            send(AgentEvent::Heard(line.clone()));
        }
        let canned = persona.joke(&text).or_else(|| persona.quip(&text)).or_else(|| persona.pooled(&text));
        let started = match canned {
            Some(canned) => routes.canned(&turn, &canned, early),
            None => routes.reply(&turn, &persona.messages(&enrich::prompt_line(&text, &line, cfg.language)), early),
        };
        if let Err(e) = started {
            eprintln!("llm_pal: {e}");
            tags = tags_before;
            continue;
        }

        let mut ended = (!early).then(|| speech_end(t0, &seg));
        let mut held = early;
        let mut spoken = Vec::new();
        let mut first_sentence_ms = None;
        let mut finished = false;
        while !shared.quit.load(Ordering::SeqCst) {
            // A held reply settles on the segment's end (speak it) or the user going on (drop it); other events
            // wait until the turn is over.
            let Some(input) = inbox.recv(Duration::from_millis(20), held) else { continue };
            match input {
                Input::Spoken(Spoken::Said { turn: of, text: said, at }) if of == turn => {
                    first_sentence_ms.get_or_insert(ended.map_or(0, |e| at.saturating_duration_since(e).as_millis()));
                    active.store(true, Ordering::Relaxed);
                    shared.set(State::Speaking);
                    send(AgentEvent::Said(said.clone()));
                    spoken.push(said);
                }
                Input::Spoken(Spoken::Done { turn: of }) if of == turn => {
                    finished = true;
                    break;
                }
                Input::Spoken(Spoken::Failed { turn: of, error }) if of == turn => {
                    eprintln!("llm_pal: {error}");
                    send(AgentEvent::ReplyFailed(error));
                    break;
                }
                Input::Spoken(_) => {}
                Input::Event(Event::BargeIn { .. }) if !held && cfg.barge_in => {
                    if let Err(e) = routes.interrupt(&turn) {
                        eprintln!("llm_pal: {e}");
                    }
                    speaker.stop();
                    send(AgentEvent::Interrupted);
                    finished = true;
                    break;
                }
                Input::Event(event) if !held => inbox.events.push_back(event),
                Input::Event(Event::Resume) => break,
                Input::Event(Event::Segment(end)) => {
                    if !end.turn_complete && sounds_unfinished(&text, cfg.language) && inbox.goes_on() {
                        carry = Some([carry.take().unwrap_or_default(), end.pcm].concat());
                        break;
                    }
                    carry = None;
                    send(AgentEvent::Heard(line.clone()));
                    ended = Some(speech_end(Instant::now(), &end));
                    held = false;
                    if let Err(e) = routes.release(&turn) {
                        eprintln!("llm_pal: {e}");
                        break;
                    }
                }
                Input::Event(_) => {}
            }
        }
        if !finished {
            if let Err(e) = routes.discard(&turn) {
                eprintln!("llm_pal: {e}");
            }
            tags = tags_before;
            continue;
        }
        let reply = spoken.join(" ");
        send(AgentEvent::TurnDone {
            reply: reply.clone(),
            stt_ms,
            first_sentence_ms,
            total_ms: ended.unwrap_or(t0).elapsed().as_millis(),
        });
        persona.remember(line, reply);
        if let Err(e) = routes.prefill(&persona.context()) {
            eprintln!("llm_pal: {e}");
        }
    }
    speaker.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_endings_sound_unfinished() {
        let en = Language::English;
        for t in ["I wanted to ask whether", "I'm going today and", "I don't know...", "So I thought,", "Hello"] {
            assert!(sounds_unfinished(t, en), "{t}");
        }
        for t in ["What do you think?", "Turn on the light.", "Great!", "I like it, and."] {
            assert_eq!(sounds_unfinished(t, en), t.ends_with("and."), "{t}");
        }
        assert!(sounds_unfinished("Ich gehe heute und.", Language::German));
        for t in ["He said \"stop.\"", "Great (really)", "Sie sagte „Halt.“"] {
            assert_eq!(sounds_unfinished(t, en), t.starts_with("Great"), "{t}");
        }
        assert!(!sounds_unfinished("Ich gehe heute und.", en));
    }
}
