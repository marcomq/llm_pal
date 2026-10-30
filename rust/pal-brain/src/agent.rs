//! The conversation loop on its own thread: microphone -> segment -> STT -> LLM -> sentences -> speech.
//! Readers such as a UI poll `state()` and `level()` lock-free and drain `poll()` for texts.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pal_core::playback::PlaybackState;
use pal_core::{Event, Processor, ProcessorConfig, Segment, SegmenterConfig};

use crate::Result;
use crate::language::Language;
use crate::llm::{Llm, LlmConfig, LlmSession, LocalLlm};
use crate::persona::Persona;
use crate::pocket_speaker::PocketSpeaker;
use crate::prosody_tags::{self, ProsodyTags};
use crate::sentences::Sentences;
use crate::stt::Recognizer;

pub struct AgentConfig {
    pub vad_model: PathBuf,
    /// Smart Turn model; without it a turn ends after a fixed silence.
    pub turn_model: Option<PathBuf>,
    /// Part of the input device name.
    pub device: Option<String>,
    pub stt_library: PathBuf,
    /// A Whisper `.bin` file (through `stt_library`) or a Parakeet TDT folder.
    pub stt_model: PathBuf,
    pub llm_model: PathBuf,
    /// Kyutai Pocket TTS catalog voice, such as "juergen".
    pub voice: String,
    /// Speech during output stops it, with Pal's voice removed from the microphone; otherwise such speech is
    /// ignored.
    pub barge_in: bool,
    pub language: Language,
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

    /// Blocks for the next event; None once the agent thread is gone.
    pub fn next(&self) -> Option<AgentEvent> {
        self.events.recv().ok()
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Numbers as words, and words the voice reads wrong respelled.
fn pronounced(text: &str, language: Language) -> String {
    let text = crate::numbers::spell(text, language);
    language.profile().pronunciations.iter().fold(text, |t, (word, spoken)| t.replace(word, spoken))
}

fn route(sentence: String, held: &mut Option<Vec<String>>, speak: &mut dyn FnMut(String)) {
    match held {
        Some(h) => h.push(sentence),
        None => speak(sentence),
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

/// The first transcription and the first prompt are slow; this runs both before Pal listens and leaves the system
/// prompt in the LLM's cache.
fn warm_up(stt: &mut Recognizer, llm: &mut LlmSession, persona: &Persona) -> Result<()> {
    stt.transcribe(&[0.0; 16_000])?;
    llm.prefill(&persona.context())
}

/// Moves waiting events to `backlog`; true if one of them was a barge-in.
fn barged_in(mut waiting: impl FnMut() -> Option<Event>, backlog: &mut VecDeque<Event>) -> bool {
    let mut barged = false;
    while let Some(event) = waiting() {
        match event {
            Event::BargeIn { .. } => barged = true,
            event => backlog.push_back(event),
        }
    }
    barged
}

/// `PAL_RECORD_DIR`: each segment as 16 kHz WAV plus the recognised text, test data for tests/stt_compare.rs.
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
    let mut stt = Recognizer::new(&cfg.stt_library, &cfg.stt_model, cfg.language.code())?;
    let model = LocalLlm::new(&cfg.llm_model, LlmConfig::default())?;
    let mut llm = model.session()?;
    let mut speaker = PocketSpeaker::new(&cfg.voice, cfg.language)?;
    let _ = shared.output.set(speaker.output());
    let pcfg = ProcessorConfig {
        vad_model: cfg.vad_model.clone(),
        segmenter: SegmenterConfig::default(),
        device: cfg.device.clone(),
        drop_echo: !cfg.barge_in,
        turn_model: cfg.turn_model.clone(),
    };
    let mut persona = Persona::kitt(cfg.language);
    llm.set_fixed_messages(persona.fixed_messages());
    warm_up(&mut stt, &mut llm, &persona)?;
    let echo = cfg.barge_in.then(|| speaker.output().take_echo_reference()).flatten();
    let (processor, events) = Processor::start(&pcfg, echo)?;
    let active = processor.output_active();
    let _ = shared.hearing.set(processor.in_speech());
    if !processor.wait_live(LIVE_TIMEOUT) {
        eprintln!("llm_pal: no sound from '{}' yet", processor.device_name());
    }
    send(AgentEvent::Ready { device: processor.device_name().to_string() });

    let mut tags = ProsodyTags::default();
    // Events that arrived while a reply was generated; `recv` takes them first.
    let backlog = RefCell::new(VecDeque::new());
    let recv = |timeout: Duration| {
        let waiting = backlog.borrow_mut().pop_front();
        waiting.map_or_else(|| events.recv_timeout(timeout), Ok)
    };
    // Speech of an unfinished sentence, put before the next segment.
    let carry = RefCell::new(None::<Vec<f32>>);
    // True if the user speaks again within `UNFINISHED_WAIT`.
    let goes_on = || {
        let until = Instant::now() + UNFINISHED_WAIT;
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            match recv(left) {
                Ok(Event::SpeechStart { .. }) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    };
    while !shared.quit.load(Ordering::SeqCst) {
        let speaking = speaker.speaking();
        active.store(speaking, Ordering::Relaxed);
        shared.set(if speaking { State::Speaking } else { State::Listening });
        let Ok(event) = recv(Duration::from_millis(20)) else { continue };
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
        if let Some(before) = carry.borrow().as_ref() {
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
            if !seg.turn_complete && sounds_unfinished(&text, cfg.language) && goes_on() {
                *carry.borrow_mut() = Some(seg.pcm);
                continue;
            }
            carry.take();
        }
        let line = Persona::user_line(&text, &prosody_tags::format(&tags.tag(&seg.features), cfg.language));

        let ended = Cell::new(if early { None } else { Some(speech_end(t0, &seg)) });
        let mut held = early.then(Vec::new);
        let mut discarded = false;
        let stop = AtomicBool::new(false);
        let mut interrupted = false;
        let mut sentences = Sentences::new(cfg.language);
        let mut spoken = Vec::new();
        let mut first_sentence_ms = None;
        let mut speak = |s: String| {
            first_sentence_ms.get_or_insert(ended.get().map_or(0, |t| t.elapsed().as_millis()));
            if let Err(e) = speaker.say(&pronounced(&s, cfg.language)) {
                eprintln!("{e}");
            }
            active.store(true, Ordering::Relaxed);
            shared.set(State::Speaking);
            send(AgentEvent::Said(s.clone()));
            spoken.push(s);
        };
        // Settles a held reply once the segment ends (speak it) or the user goes on (drop it).
        let settle =
            |held: &mut Option<Vec<String>>, discarded: &mut bool, wait: bool, speak: &mut dyn FnMut(String)| {
                while held.is_some() && !*discarded && !shared.quit.load(Ordering::SeqCst) {
                    let event = match recv(if wait { Duration::from_millis(20) } else { Duration::ZERO }) {
                        Ok(e) => e,
                        Err(e) if wait && e.is_timeout() => continue,
                        Err(_) => break,
                    };
                    match event {
                        Event::Resume => *discarded = true,
                        Event::Segment(end) => {
                            if !end.turn_complete && sounds_unfinished(&text, cfg.language) && goes_on() {
                                let before = carry.take().unwrap_or_default();
                                *carry.borrow_mut() = Some([before, end.pcm].concat());
                                *discarded = true;
                                continue;
                            }
                            carry.take();
                            send(AgentEvent::Heard(line.clone()));
                            ended.set(Some(speech_end(Instant::now(), &end)));
                            held.take().into_iter().flatten().for_each(&mut *speak);
                        }
                        _ => {}
                    }
                }
            };
        if !early {
            send(AgentEvent::Heard(line.clone()));
        }
        if let Some(canned) = persona.joke(&text).or_else(|| persona.quip(&text)) {
            sentences.push(&canned).into_iter().for_each(|s| route(s, &mut held, &mut speak));
        } else {
            let replied = llm.reply(&persona.messages(&line), &stop, &mut |piece| {
                interrupted |=
                    cfg.barge_in && held.is_none() && barged_in(|| events.try_recv().ok(), &mut backlog.borrow_mut());
                if !interrupted {
                    sentences.push(piece).into_iter().for_each(|s| route(s, &mut held, &mut speak));
                    settle(&mut held, &mut discarded, false, &mut speak);
                }
                if interrupted || discarded || shared.quit.load(Ordering::SeqCst) {
                    stop.store(true, Ordering::Relaxed);
                }
            });
            if let Err(e) = replied {
                eprintln!("llm_pal: {e}");
                tags = tags_before;
                continue;
            }
        }
        if !interrupted {
            sentences.finish().into_iter().for_each(|s| route(s, &mut held, &mut speak));
        }
        settle(&mut held, &mut discarded, true, &mut speak);
        if interrupted {
            speaker.stop();
            send(AgentEvent::Interrupted);
        }
        if discarded || held.is_some() {
            tags = tags_before;
            continue;
        }
        let reply = spoken.join(" ");
        send(AgentEvent::TurnDone {
            reply: reply.clone(),
            stt_ms,
            first_sentence_ms,
            total_ms: ended.get().unwrap_or(t0).elapsed().as_millis(),
        });
        persona.remember(line, reply);
        if let Err(e) = llm.prefill(&persona.context()) {
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
