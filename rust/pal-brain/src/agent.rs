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
use crate::party::Party;
use crate::persona::Persona;
use crate::prosody_tags::{self, ProsodyTags};
use crate::remote::RemoteConfig;
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
/// Quiet after a barge-in with no words heard, after which Pal's paused voice goes on. Longer than the segmenter's
/// `end_silence_ms`, so a segment of real speech arrives first.
const RESUME_AFTER: Duration = Duration::from_millis(1200);

/// Pal's voice is paused for speech during output: words in it stop the reply, a cough or noise lets it go on.
#[derive(Default)]
struct Barge {
    quiet_since: Option<Instant>,
}

impl Barge {
    fn over(&mut self, hearing: bool) -> bool {
        if hearing {
            self.quiet_since = None;
            return false;
        }
        self.quiet_since.get_or_insert_with(Instant::now).elapsed() >= RESUME_AFTER
    }
}

fn resume_if_quiet(barge: &mut Option<Barge>, hearing: bool, speaker: &PocketSpeaker) {
    if barge.as_mut().is_some_and(|b| b.over(hearing)) {
        *barge = None;
        speaker.resume();
        eprintln!("llm_pal: no words after the barge-in, going on");
    }
}

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
            Brain::Remote { server: remote.clone(), fallback }
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
    let in_speech = processor.in_speech();
    let _ = shared.hearing.set(in_speech.clone());
    if !processor.wait_live(LIVE_TIMEOUT) {
        eprintln!("llm_pal: no sound from '{}' yet", processor.device_name());
    }
    send(AgentEvent::Ready { device: processor.device_name().to_string() });

    let mut tags = ProsodyTags::default();
    // Speech of an unfinished sentence, put before the next segment.
    let mut carry = None::<Vec<f32>>;
    let mut turns = 0u64;
    let mut timing = None::<Timing>;
    let mut party = cfg.party.then(|| Party::new(cfg.language));
    let mut barge = None::<Barge>;
    // The sentences of the last reply in the history, cut to what was heard if the user interrupts it.
    let mut last_reply = None::<Vec<String>>;
    while !shared.quit.load(Ordering::SeqCst) {
        if timing.is_some()
            && let Some(audio) = speaker.take_audio_start()
            && let Some(t) = timing.take()
        {
            t.finish(Some(audio), &send);
        }
        let speaking = speaker.speaking();
        active.store(speaking, Ordering::Relaxed);
        shared.set(if speaking { State::Speaking } else { State::Listening });
        // Reports of earlier turns are stale here.
        let input = inbox.recv(Duration::from_millis(20), true);
        if input.is_none() {
            resume_if_quiet(&mut barge, in_speech.load(Ordering::Relaxed), &speaker);
        }
        let quiet = match &mut party {
            Some(party) if input.is_none() && !speaking && !in_speech.load(Ordering::Relaxed) => {
                party.quiet().map(|talk| Cue::Silence { prompt: party.prompt(&talk), talk })
            }
            _ => None,
        };
        // A reply to a pause is prepared but held back until the segment really ends.
        let mut cue = match (input, quiet) {
            (_, Some(silence)) => silence,
            (Some(Input::Event(event)), None) => match event {
                Event::SpeechStart { .. } => {
                    if let Some(party) = &mut party {
                        party.speech();
                    }
                    continue;
                }
                Event::Resume => continue,
                Event::BargeIn { .. } => {
                    if cfg.barge_in {
                        speaker.pause();
                        barge = Some(Barge::default());
                    }
                    continue;
                }
                Event::Pause(seg) => Cue::Speech(seg, true),
                Event::Segment(seg) => Cue::Speech(seg, false),
            },
            _ => continue,
        };
        if let Cue::Speech(seg, early) = &mut cue {
            if let Some(before) = &carry {
                seg.pcm = [before.as_slice(), &seg.pcm].concat();
            }
            // Without barge-in, speech that overlaps Pal's output is ignored: it may be Pal's own voice.
            if seg.during_output && !cfg.barge_in {
                if !*early {
                    send(AgentEvent::Ignored);
                }
                continue;
            }
        }

        shared.set(State::Thinking);
        let t0 = Instant::now();
        if let Some(t) = timing.take() {
            t.finish(None, &send);
        }
        speaker.take_audio_start();
        let tags_before = tags.clone();
        let (text, line, early, mut ended, stt_ms, text_at, reply) = match cue {
            Cue::Silence { talk, prompt } => {
                eprintln!("llm_pal: the room went quiet");
                let canned = talk.iter().rev().find_map(|t| persona.quip(t).or_else(|| persona.pooled(t)));
                let reply = canned.map_or_else(|| Reply::Model(prompt.clone()), Reply::Canned);
                (talk.join(" "), prompt, false, Some(t0), 0, t0, reply)
            }
            Cue::Speech(seg, early) => {
                let text = match stt.transcribe(&seg.pcm) {
                    Ok(text) => text,
                    Err(e) => {
                        eprintln!("llm_pal: {e}");
                        continue;
                    }
                };
                let stt_ms = t0.elapsed().as_millis();
                let text_at = Instant::now();
                if let Some(dir) = &record_dir
                    && let Err(e) = record(dir, &seg.pcm, &text)
                {
                    eprintln!("llm_pal: recording to {}: {e}", dir.display());
                }
                if text.is_empty() {
                    if !early && barge.take().is_some() {
                        speaker.resume();
                    }
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
                let joke = persona.joke(&text);
                if let Some(party) = &mut party {
                    if joke.is_none() && !party.called(&text) {
                        if early {
                            tags = tags_before;
                        } else {
                            send(AgentEvent::Heard(line));
                            party.overhear(text);
                            if barge.take().is_some() {
                                speaker.resume();
                            }
                        }
                        continue;
                    }
                    party.forget();
                }
                if barge.take().is_some() {
                    let said = last_reply.take().unwrap_or_default();
                    let heard = speaker.heard().min(said.len());
                    if let Err(e) = routes.interrupt(&format!("turn-{turns}")) {
                        eprintln!("llm_pal: {e}");
                    }
                    speaker.stop();
                    if heard < said.len() {
                        persona.cut_last_reply(said[..heard].join(" "));
                    }
                    send(AgentEvent::Interrupted);
                }
                if !early {
                    send(AgentEvent::Heard(line.clone()));
                }
                let reply = match joke.or_else(|| persona.quip(&text)).or_else(|| persona.pooled(&text)) {
                    Some(canned) => Reply::Canned(canned),
                    None => Reply::Model(enrich::prompt_line(&text, &line, cfg.language)),
                };
                let ended = (!early).then(|| speech_end(t0, &seg));
                (text, line, early, ended, stt_ms, text_at, reply)
            }
        };

        turns += 1;
        let turn = format!("turn-{turns}");
        speaker.new_reply();
        last_reply = None;
        let started = match reply {
            Reply::Canned(canned) => routes.canned(&turn, &canned, early),
            Reply::Model(prompt) => routes.reply(&turn, &persona.messages(&prompt), early),
        };
        if let Err(e) = started {
            eprintln!("llm_pal: {e}");
            tags = tags_before;
            continue;
        }
        let sent_at = Instant::now();

        let mut held = early;
        let mut spoken = Vec::new();
        let (mut first_piece, mut first_sentence) = (None, None);
        let mut finished = false;
        while !shared.quit.load(Ordering::SeqCst) {
            // A held reply settles on the segment's end (speak it) or the user going on (drop it); other events
            // wait until the turn is over.
            let Some(input) = inbox.recv(Duration::from_millis(20), held) else {
                resume_if_quiet(&mut barge, in_speech.load(Ordering::Relaxed), &speaker);
                continue;
            };
            match input {
                Input::Spoken(Spoken::Said { turn: of, text: said, at }) if of == turn => {
                    first_sentence.get_or_insert(at);
                    active.store(true, Ordering::Relaxed);
                    shared.set(State::Speaking);
                    send(AgentEvent::Said(said.clone()));
                    spoken.push(said);
                }
                Input::Spoken(Spoken::FirstPiece { turn: of, at }) if of == turn => {
                    first_piece.get_or_insert(at);
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
                    speaker.pause();
                    barge = Some(Barge::default());
                }
                // Speech after a barge-in ends the reply; whether its paused voice goes on depends on the words.
                Input::Event(event @ (Event::Pause(_) | Event::Segment(_))) if !held && barge.is_some() => {
                    routes.cancel(&turn);
                    inbox.events.push_back(event);
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
        timing = Some(Timing {
            turn: turn.clone(),
            ended: ended.unwrap_or(t0),
            marks: vec![
                ("segment", Some(t0)),
                ("text", Some(text_at)),
                ("prompt sent", Some(sent_at)),
                ("first token", first_piece),
                ("first sentence", first_sentence),
            ],
            reply: spoken.join(" "),
            stt_ms,
            total_ms: ended.unwrap_or(t0).elapsed().as_millis(),
        });
        if persona.remember(line, spoken.join(" ")) {
            last_reply = Some(spoken);
        }
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
