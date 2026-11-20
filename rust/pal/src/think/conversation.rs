//! The conversation on its own thread: microphone event → transcript → reply → speech, turn by turn.
//! `Agent::start` loads everything (`load`), then `Conversation::run` answers until `stop`. See AGENTS.md for the
//! whole flow. Readers such as a UI poll `state()` and `level()` lock-free and drain `poll()` for texts.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::downloader;
use crate::hear::event::{Event, Segment};
use crate::hear::mic::{MicConfig, MicLink};
use crate::speak::playback::PlaybackState;
use crate::speak::tts::{self, PocketSpeaker};
use crate::think::brain::Brain;
use crate::think::enrich;
use crate::think::language::Language;
use crate::think::party::Party;
use crate::think::persona::Persona;
use crate::think::prosody_tags::{self, ProsodyTags};
use crate::think::remote::RemoteConfig;
use crate::transport::{self, Input, Spoken, Transport};
use crate::{Error, Result};

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
            let send = |e| drop(tx.send(e));
            match Conversation::load(&cfg, &sh, &send).and_then(|mut c| c.run()) {
                Ok(()) | Err(Error::Stopped) => {}
                Err(e) => send(AgentEvent::Failed(e.to_string())),
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
    fn drop(&mut self) {
        self.stop();
    }
}

const LIVE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);
/// Extra silence Pal waits for after a transcript that doesn't sound finished.
const UNFINISHED_WAIT: Duration = Duration::from_millis(700);
/// Quiet after a barge-in with no words heard, after which Pal's paused voice goes on. Longer than the segmenter's
/// `end_silence_ms`, so a segment of real speech arrives first.
const RESUME_AFTER: Duration = Duration::from_millis(1200);

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

/// One turn's milestones, logged in ms from the end of speech once its first audio played; negative for a reply
/// prepared during a pause.
struct Timing {
    turn: String,
    ended: Instant,
    marks: Vec<(&'static str, Option<Instant>)>,
    reply: String,
    stt_ms: u128,
    total_ms: u128,
}

impl Timing {
    /// Logs the milestones and reports the turn; `audio` is when its first sound played, if it did.
    fn finish(self, audio: Option<Instant>, send: &dyn Fn(AgentEvent)) {
        let ms = |at: Instant| match at.checked_duration_since(self.ended) {
            Some(after) => after.as_millis() as i64,
            None => -(self.ended.duration_since(at).as_millis() as i64),
        };
        let parts: Vec<String> = (self.marks.iter().copied().chain([("audio", audio)]))
            .map(|(name, at)| at.map_or(format!("{name} -"), |at| format!("{name} {}", ms(at))))
            .collect();
        eprintln!("llm_pal: {} ms after end of speech: {}", self.turn, parts.join(", "));
        send(AgentEvent::TurnDone {
            reply: self.reply,
            stt_ms: self.stt_ms,
            first_sentence_ms: audio.map(|at| at.saturating_duration_since(self.ended).as_millis()),
            total_ms: self.total_ms,
        });
    }
}

/// What a turn answers: speech up to a pause (`true`) or its end, or in party mode the room's talk before a
/// silence with the model's prompt about it.
enum Cue {
    Speech(Segment, bool),
    Silence { talk: Vec<String>, prompt: String },
}

enum Reply {
    Canned(String),
    /// The model answers this prompt line.
    Model(String),
}

/// A turn ready to start: what was heard and how Pal answers it.
struct Prepared {
    text: String,
    line: String,
    early: bool,
    ended: Option<Instant>,
    stt_ms: u128,
    text_at: Instant,
    reply: Reply,
}

/// How a started turn went.
struct Followed {
    spoken: Vec<String>,
    ended: Option<Instant>,
    first_piece: Option<Instant>,
    first_sentence: Option<Instant>,
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

fn joined<T>(loader: std::thread::JoinHandle<Result<T>>, error: fn(String) -> Error) -> Result<T> {
    loader.join().unwrap_or_else(|_| Err(error("loading panicked".into())))
}

struct Conversation<'a> {
    cfg: &'a AgentConfig,
    shared: &'a Shared,
    send: &'a dyn Fn(AgentEvent),
    transport: Transport,
    /// Keeps the transport's runtime alive; declared after it, so it is dropped last.
    _rt: tokio::runtime::Runtime,
    speaker: PocketSpeaker,
    mic: Arc<MicLink>,
    persona: Persona,
    party: Option<Party>,
    tags: ProsodyTags,
    /// Events and reports set aside while a turn runs or the user may go on.
    events: VecDeque<Event>,
    spoken: VecDeque<Spoken>,
    /// Speech of an unfinished sentence, put before the next segment.
    carry: Option<Vec<f32>>,
    turns: u64,
    timing: Option<Timing>,
    /// Pal's voice is paused for speech during output since then (`None` while it keeps quiet): words stop the
    /// reply, a cough or noise lets it go on.
    barge: Option<Option<Instant>>,
    /// The sentences of the last reply in the history, cut to what was heard if the user interrupts it.
    last_reply: Option<Vec<String>>,
    record_dir: Option<PathBuf>,
}

impl<'a> Conversation<'a> {
    /// Downloads what is missing, then loads speech recognition, voice and LLM side by side; the first
    /// transcription is slow, so it runs here too.
    fn load(cfg: &'a AgentConfig, shared: &'a Shared, send: &'a dyn Fn(AgentEvent)) -> Result<Self> {
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
        let rt = tokio::runtime::Runtime::new().map_err(|e| Error::Routes(e.to_string()))?;
        let stt = std::thread::spawn({
            let (rt, model) = (rt.handle().clone(), dir.join(&cfg.stt_model));
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
        let llm = dir.join(&cfg.llm_model);
        let brain = match &cfg.remote {
            None => Brain::Local(llm),
            Some(server) => Brain::Remote { server: server.clone(), fallback: cfg.fallback.then_some(llm) },
        };
        let persona = Persona::kitt(cfg.language, &cfg.user_name);
        let (speaker_tx, speaker_rx) = channel();
        let transport =
            Transport::start(rt.handle().clone(), brain, persona.fixed_messages(), speaker_rx, cfg.language).and_then(
                |t| {
                    took(if local { "llm loaded" } else { "llm routes ready" });
                    t.prefill(&persona.context())?;
                    Ok(t)
                },
            );
        // Both loaders finish before any error returns, so none outlives the runtime or overlaps a restart.
        let (stt, speaker) = (joined(stt, Error::Stt), joined(pocket, Error::Tts));
        let (mut transport, stt, speaker) = (transport?, stt?, speaker?);
        let _ = speaker_tx.send(speaker.clone());
        let _ = shared.output.set(speaker.output());
        // Waits until the first prompt, which is slow, has left the system prompt in the LLM's cache.
        loop {
            match transport.next(POLL) {
                Some(Input::Spoken(Spoken::Prefilled)) => break,
                Some(Input::Spoken(Spoken::Failed { error, .. })) => return Err(Error::Llm(error)),
                _ if shared.quit.load(Ordering::SeqCst) => return Err(Error::Stopped),
                _ => {}
            }
        }
        took("llm prefilled");

        let echo = cfg.barge_in.then(|| speaker.output().take_echo_reference()).flatten();
        let mic = MicLink::new(echo);
        let mic_config = MicConfig {
            vad_model: dir.join("silero_vad.onnx"),
            turn_model: cfg.smart_turn.then(|| dir.join("smart-turn-v3.2-cpu.onnx")),
            device: None,
            drop_echo: !cfg.barge_in,
        };
        transport.listen(stt, &mic_config, mic.clone())?;
        let _ = shared.mic.set(mic.clone());
        if !mic.wait_live(LIVE_TIMEOUT) {
            eprintln!("llm_pal: no sound from '{}' yet", mic.device());
        }
        send(AgentEvent::Ready { device: mic.device().to_string() });
        Ok(Self {
            cfg,
            shared,
            send,
            transport,
            _rt: rt,
            speaker,
            mic,
            persona,
            party: cfg.party.then(|| Party::new(cfg.language)),
            tags: ProsodyTags::default(),
            events: VecDeque::new(),
            spoken: VecDeque::new(),
            carry: None,
            turns: 0,
            timing: None,
            barge: None,
            last_reply: None,
            record_dir: std::env::var_os("PAL_RECORD_DIR").map(PathBuf::from),
        })
    }

    fn quit(&self) -> bool {
        self.shared.quit.load(Ordering::SeqCst)
    }

    fn hearing(&self) -> bool {
        self.mic.in_speech.load(Ordering::Relaxed)
    }

    fn run(&mut self) -> Result<()> {
        while !self.quit() {
            if self.transport.mic_lost() {
                return Err(Error::Audio("the microphone stopped".into()));
            }
            if self.timing.is_some()
                && let Some(audio) = self.speaker.take_audio_start()
                && let Some(t) = self.timing.take()
            {
                t.finish(Some(audio), self.send);
            }
            let speaking = self.speaker.speaking();
            self.mic.output_active.store(speaking, Ordering::Relaxed);
            self.shared.set(if speaking { State::Speaking } else { State::Listening });
            // Reports of earlier turns are stale here.
            let input = self.recv(true);
            if input.is_none() {
                self.resume_if_quiet();
            }
            if let Some(cue) = self.cue(input, speaking) {
                self.turn(cue);
            }
        }
        self.speaker.stop();
        Ok(())
    }

    /// Set-aside reports come first, then set-aside events if `events`, then new input.
    fn recv(&mut self, events: bool) -> Option<Input> {
        if let Some(s) = self.spoken.pop_front() {
            return Some(Input::Spoken(s));
        }
        let waiting = if events { self.events.pop_front() } else { None };
        waiting.map(Input::Event).or_else(|| self.transport.next(POLL))
    }

    /// True if the user speaks again within `UNFINISHED_WAIT`; other events are dropped.
    fn goes_on(&mut self) -> bool {
        let until = Instant::now() + UNFINISHED_WAIT;
        let mut kept = Vec::new();
        let mut speaks = false;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            let next = self.events.pop_front().map(Input::Event).or_else(|| self.transport.next(left));
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

    fn pause_for_barge_in(&mut self) {
        self.speaker.pause();
        self.barge = Some(None);
    }

    /// Lets Pal's paused voice go on after a barge-in with no words.
    fn resume_barged(&mut self) {
        if self.barge.take().is_some() {
            self.speaker.resume();
        }
    }

    fn resume_if_quiet(&mut self) {
        let hearing = self.hearing();
        let Some(quiet_since) = &mut self.barge else { return };
        if hearing {
            *quiet_since = None;
        } else if quiet_since.get_or_insert_with(Instant::now).elapsed() >= RESUME_AFTER {
            self.resume_barged();
            eprintln!("llm_pal: no words after the barge-in, going on");
        }
    }

    /// What the input asks Pal to answer, if anything. A reply to a pause is prepared but held back until the
    /// segment really ends.
    fn cue(&mut self, input: Option<Input>, speaking: bool) -> Option<Cue> {
        if let Some(party) = &mut self.party
            && input.is_none()
            && !speaking
            && !self.mic.in_speech.load(Ordering::Relaxed)
        {
            let talk = party.quiet()?;
            return Some(Cue::Silence { prompt: party.prompt(&talk), talk });
        }
        let (mut seg, early) = match input? {
            Input::Event(Event::SpeechStart { .. }) => {
                if let Some(party) = &mut self.party {
                    party.speech();
                }
                return None;
            }
            Input::Event(Event::BargeIn { .. }) => {
                if self.cfg.barge_in {
                    self.pause_for_barge_in();
                }
                return None;
            }
            Input::Event(Event::Pause(seg)) => (seg, true),
            Input::Event(Event::Segment(seg)) => (seg, false),
            Input::Event(Event::Resume) | Input::Spoken(_) => return None,
        };
        if let Some(before) = &self.carry {
            seg.pcm = [before.as_slice(), &seg.pcm].concat();
        }
        // Without barge-in, speech that overlaps Pal's output is ignored: it may be Pal's own voice.
        if seg.during_output && !self.cfg.barge_in {
            if !early {
                (self.send)(AgentEvent::Ignored);
            }
            return None;
        }
        Some(Cue::Speech(seg, early))
    }

    fn turn(&mut self, cue: Cue) {
        self.shared.set(State::Thinking);
        let t0 = Instant::now();
        if let Some(t) = self.timing.take() {
            t.finish(None, self.send);
        }
        self.speaker.take_audio_start();
        let tags_before = self.tags.clone();
        let prepared = match cue {
            Cue::Silence { talk, prompt } => Some(self.silence(talk, prompt, t0)),
            Cue::Speech(seg, early) => self.speech(seg, early, t0, &tags_before),
        };
        let Some(p) = prepared else { return };

        self.turns += 1;
        let turn = format!("turn-{}", self.turns);
        self.speaker.new_reply();
        self.last_reply = None;
        let started = match &p.reply {
            Reply::Canned(canned) => self.transport.canned(&turn, canned, p.early),
            Reply::Model(prompt) => self.transport.reply(&turn, &self.persona.messages(prompt), p.early),
        };
        if let Err(e) = started {
            eprintln!("llm_pal: {e}");
            self.tags = tags_before;
            return;
        }
        let sent_at = Instant::now();
        let Some(f) = self.follow(&turn, &p) else {
            if let Err(e) = self.transport.discard(&turn) {
                eprintln!("llm_pal: {e}");
            }
            self.tags = tags_before;
            return;
        };
        let ended = f.ended.unwrap_or(t0);
        self.timing = Some(Timing {
            turn,
            ended,
            marks: vec![
                ("segment", Some(t0)),
                ("text", Some(p.text_at)),
                ("prompt sent", Some(sent_at)),
                ("first token", f.first_piece),
                ("first sentence", f.first_sentence),
            ],
            reply: f.spoken.join(" "),
            stt_ms: p.stt_ms,
            total_ms: ended.elapsed().as_millis(),
        });
        if self.persona.remember(p.line, f.spoken.join(" ")) {
            self.last_reply = Some(f.spoken);
        }
        if let Err(e) = self.transport.prefill(&self.persona.context()) {
            eprintln!("llm_pal: {e}");
        }
    }

    /// Party mode: the room went quiet after talk Pal didn't answer.
    fn silence(&mut self, talk: Vec<String>, prompt: String, t0: Instant) -> Prepared {
        eprintln!("llm_pal: the room went quiet");
        let canned = talk.iter().rev().find_map(|t| self.persona.quip(t).or_else(|| self.persona.pooled(t)));
        let reply = canned.map_or_else(|| Reply::Model(prompt.clone()), Reply::Canned);
        Prepared { text: talk.join(" "), line: prompt, early: false, ended: Some(t0), stt_ms: 0, text_at: t0, reply }
    }

    /// Transcribes the speech and picks the reply; `None` if there is nothing to answer.
    fn speech(&mut self, seg: Segment, early: bool, t0: Instant, tags_before: &ProsodyTags) -> Option<Prepared> {
        // `seg.pcm` already starts with the carry; a final segment uses it up, whatever its transcript.
        if !early {
            self.carry = None;
        }
        let text = match self.transport.transcribe(&seg.pcm) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("llm_pal: {e}");
                return None;
            }
        };
        let stt_ms = t0.elapsed().as_millis();
        let text_at = Instant::now();
        if let Some(dir) = &self.record_dir
            && let Err(e) = record(dir, &seg.pcm, &text)
        {
            eprintln!("llm_pal: recording to {}: {e}", dir.display());
        }
        if text.is_empty() {
            if !early {
                self.resume_barged();
            }
            return None;
        }
        if !early {
            if !seg.turn_complete && sounds_unfinished(&text, self.cfg.language) && self.goes_on() {
                self.carry = Some(seg.pcm);
                return None;
            }
        }
        let tags = prosody_tags::format(&self.tags.tag(&seg.features), self.cfg.language);
        let line = Persona::user_line(&text, &tags);
        let joke = self.persona.joke(&text);
        if let Some(party) = &mut self.party {
            if joke.is_none() && !party.called(&text) {
                if early {
                    self.tags = tags_before.clone();
                } else {
                    (self.send)(AgentEvent::Heard(line));
                    party.overhear(text);
                    self.resume_barged();
                }
                return None;
            }
            party.forget();
        }
        if self.barge.take().is_some() {
            self.interrupt_last_reply();
        }
        if !early {
            (self.send)(AgentEvent::Heard(line.clone()));
        }
        let canned = joke.or_else(|| self.persona.quip(&text)).or_else(|| self.persona.pooled(&text));
        let reply = match canned {
            Some(canned) => Reply::Canned(canned),
            None => Reply::Model(enrich::prompt_line(&text, &line, self.cfg.language)),
        };
        let ended = (!early).then(|| speech_end(t0, &seg));
        Some(Prepared { text, line, early, ended, stt_ms, text_at, reply })
    }

    /// Words after a barge-in: the last reply stops, and the history keeps only what was heard of it.
    fn interrupt_last_reply(&mut self) {
        let said = self.last_reply.take().unwrap_or_default();
        let heard = self.speaker.heard().min(said.len());
        if let Err(e) = self.transport.interrupt(&format!("turn-{}", self.turns)) {
            eprintln!("llm_pal: {e}");
        }
        self.speaker.stop();
        if heard < said.len() {
            self.persona.cut_last_reply(said[..heard].join(" "));
        }
        (self.send)(AgentEvent::Interrupted);
    }

    /// Passes the turn's sentences on until it is done; `None` if it was dropped. A held reply settles on the
    /// segment's end (speak it) or the user going on (drop it); other events wait until the turn is over.
    fn follow(&mut self, turn: &str, p: &Prepared) -> Option<Followed> {
        let mut held = p.early;
        let mut f = Followed { spoken: Vec::new(), ended: p.ended, first_piece: None, first_sentence: None };
        while !self.quit() {
            // `run` reports the lost microphone; a held reply would wait for its segment forever.
            if self.transport.mic_lost() {
                return None;
            }
            let Some(input) = self.recv(held) else {
                self.resume_if_quiet();
                continue;
            };
            match input {
                Input::Spoken(Spoken::Said { turn: of, text, at }) if of == turn => {
                    f.first_sentence.get_or_insert(at);
                    self.mic.output_active.store(true, Ordering::Relaxed);
                    self.shared.set(State::Speaking);
                    (self.send)(AgentEvent::Said(text.clone()));
                    f.spoken.push(text);
                }
                Input::Spoken(Spoken::FirstPiece { turn: of, at }) if of == turn => {
                    f.first_piece.get_or_insert(at);
                }
                Input::Spoken(Spoken::Done { turn: of }) if of == turn => return Some(f),
                Input::Spoken(Spoken::Failed { turn: of, error }) if of == turn => {
                    eprintln!("llm_pal: {error}");
                    (self.send)(AgentEvent::ReplyFailed(error));
                    return None;
                }
                Input::Spoken(_) => {}
                Input::Event(Event::BargeIn { .. }) if !held && self.cfg.barge_in => self.pause_for_barge_in(),
                // Speech after a barge-in ends the reply; whether its paused voice goes on depends on the words.
                Input::Event(event @ (Event::Pause(_) | Event::Segment(_))) if !held && self.barge.is_some() => {
                    self.transport.cancel(turn);
                    self.events.push_back(event);
                    return Some(f);
                }
                Input::Event(event) if !held => self.events.push_back(event),
                Input::Event(Event::Resume) => return None,
                Input::Event(Event::Segment(end)) => {
                    let arrived = Instant::now();
                    if !end.turn_complete && sounds_unfinished(&p.text, self.cfg.language) && self.goes_on() {
                        self.carry = Some([self.carry.take().unwrap_or_default(), end.pcm].concat());
                        return None;
                    }
                    self.carry = None;
                    (self.send)(AgentEvent::Heard(p.line.clone()));
                    f.ended = Some(speech_end(arrived, &end));
                    held = false;
                    if let Err(e) = self.transport.release(turn) {
                        eprintln!("llm_pal: {e}");
                        return None;
                    }
                }
                Input::Event(_) => {}
            }
        }
        None
    }
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
