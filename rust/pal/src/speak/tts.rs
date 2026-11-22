//! Kyutai Pocket TTS in-process: the same voice on every platform. One thread generates, a second one
//! owns the pal-core playback (cpal streams stay on their thread), which also provides the output level.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::speak::playback::{Playback, PlaybackState};
use pal_tts::PocketTts;

use crate::speak::fish::{Fish, FishConfig};
use crate::speak::leveler::Leveler;
use crate::speak::voice_filter::VoiceFilter;
use crate::think::language::Language;
use crate::transport::{Report, Spoken};
use crate::{Error, Result};

/// How often the playback thread looks whether a sentence became audible or the voice went quiet.
const WATCH: Duration = Duration::from_millis(20);

enum Audio {
    /// A sentence of the turn begins; its chunks follow.
    Begin(u64, String),
    Chunk(u64, Vec<f32>),
    /// All of a sentence is queued; it counts as pending until then.
    End,
}

/// Clones share one queue and playback, so one clone can speak while another stops it. Once `report_to` is
/// called, the playback thread reports each sentence as it becomes audible (`Spoken::Audible`) and the voice
/// going quiet (`Spoken::Quiet`).
#[derive(Clone)]
pub struct PocketSpeaker {
    tx: Sender<(u64, String, String)>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    /// Sentences queued so far: one that ends within a watch still gets its `Spoken::Quiet`.
    said: Arc<AtomicU64>,
    cut: Arc<AtomicBool>,
    output: Arc<PlaybackState>,
    report: Arc<OnceLock<Report>>,
}

impl PocketSpeaker {
    /// `voice` is a Pocket TTS catalog name such as "juergen" or a `.safetensors` voice file. Blocks until the
    /// model is loaded.
    pub fn new(voice: &str, language: Language) -> Result<Self> {
        Self::with_fish(voice, language, None)
    }

    /// Like [`Self::new`]; with `fish`, Fish Audio speaks and Pocket takes over when it gives no audio.
    pub fn with_fish(voice: &str, language: Language, fish: Option<FishConfig>) -> Result<Self> {
        xn::set_num_threads(crate::cpu::fast_cores());
        let tts = PocketTts::load(language.profile().pocket_model, voice)
            .map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let rate = tts.sample_rate();
        let warm_up = tts.synthesize(language.profile().warm_up, &AtomicBool::new(false), &mut |_| Ok(()));
        warm_up.map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let said = Arc::new(AtomicU64::new(0));
        let cut = Arc::new(AtomicBool::new(false));
        let report: Arc<OnceLock<Report>> = Arc::default();

        let (audio_tx, audio_rx) = channel::<Audio>();
        let (ready_tx, ready_rx) = sync_channel(1);
        let (g, p, s, reports) = (generation.clone(), pending.clone(), said.clone(), report.clone());
        std::thread::spawn(move || {
            let mut playback = match Playback::start() {
                Ok(pb) => pb,
                Err(e) => return drop(ready_tx.send(Err(Error::Tts(e.to_string())))),
            };
            let _ = ready_tx.send(Ok(playback.state.clone()));
            let state = playback.state.clone();
            // The sentence whose first sound comes next, and where each queued sentence starts until it is heard.
            let mut next: Option<(u64, String)> = None;
            let mut starts: Vec<(u64, String, u64)> = Vec::new();
            let (mut quiet, mut seen, mut last_turn) = (true, 0, String::new());
            loop {
                match audio_rx.recv_timeout(WATCH) {
                    Ok(Audio::Begin(gen_of_text, turn)) => {
                        // A sound start latched before a new turn belongs to an earlier one.
                        if turn != last_turn {
                            state.take_start();
                            last_turn.clone_from(&turn);
                        }
                        next = Some((gen_of_text, turn));
                    }
                    Ok(Audio::Chunk(gen_of_pcm, pcm)) => {
                        if gen_of_pcm == g.load(Ordering::SeqCst) {
                            starts.extend(next.take().map(|(gen_of_text, turn)| (gen_of_text, turn, state.written())));
                            playback.play(&pcm, rate);
                        }
                    }
                    Ok(Audio::End) => {
                        starts.extend(next.take().map(|(gen_of_text, turn)| (gen_of_text, turn, state.written())));
                        p.fetch_sub(1, Ordering::SeqCst);
                        playback.drain();
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                let Some(report) = reports.get() else { continue };
                let (now, taken) = (g.load(Ordering::SeqCst), state.taken());
                starts.retain(|(gen_of_text, turn, at)| {
                    let heard = *gen_of_text == now && taken > *at;
                    if heard {
                        report(Spoken::Audible {
                            turn: turn.clone(),
                            at: state.take_start().unwrap_or_else(Instant::now),
                        });
                    }
                    *gen_of_text == now && !heard
                });
                let said = s.load(Ordering::SeqCst);
                let silent = p.load(Ordering::SeqCst) == 0 && !state.busy();
                if silent && (!quiet || said != seen) {
                    report(Spoken::Quiet);
                }
                (quiet, seen) = (silent, said);
            }
        });
        let output = ready_rx.recv().map_err(|_| Error::Tts("playback thread died".into()))??;

        let (tx, rx) = channel::<(u64, String, String)>();
        let (g, c) = (generation.clone(), cut.clone());
        std::thread::spawn(move || {
            let mut leveler = Leveler::default();
            let mut filter = VoiceFilter::new(rate);
            let mut fish = fish.map(Fish::new);
            for (gen_of_text, turn, text) in rx {
                if gen_of_text == g.load(Ordering::SeqCst) {
                    c.store(false, Ordering::SeqCst);
                    let _ = audio_tx.send(Audio::Begin(gen_of_text, turn));
                    let audio_tx = audio_tx.clone();
                    let mut play =
                        |pcm: &[f32]| audio_tx.send(Audio::Chunk(gen_of_text, leveler.apply(&filter.apply(pcm))));
                    if !fish.as_mut().is_some_and(|fish| fish.say(&text, rate, &c, &mut play)) {
                        let result = tts.synthesize(&text, &c, &mut |pcm| play(pcm).map_err(Into::into));
                        if let Err(e) = result {
                            eprintln!("llm_pal: pocket tts: {e:#}");
                        }
                    }
                }
                let _ = audio_tx.send(Audio::End);
            }
        });
        Ok(Self { tx, generation, pending, said, cut, output, report })
    }

    pub fn output(&self) -> Arc<PlaybackState> {
        self.output.clone()
    }

    /// Where the playback reports to; the first call wins.
    pub fn report_to(&self, report: Report) {
        let _ = self.report.set(report);
    }
}

/// Pocket's files come from `model_dir/pocket` (the downloader puts them there), never from its own download.
pub fn use_local_files(model_dir: &Path) {
    pal_tts::use_local_files(model_dir.join("pocket"));
}

impl PocketSpeaker {
    /// Queues a sentence of `turn` and returns at once.
    pub fn say(&mut self, turn: &str, text: &str) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.said.fetch_add(1, Ordering::SeqCst);
        let msg = (self.generation.load(Ordering::SeqCst), turn.to_string(), text.to_string());
        self.tx.send(msg).map_err(|_| Error::Tts("speech thread stopped".into()))
    }

    /// Drops the queue and cuts off the current sentence.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cut.store(true, Ordering::SeqCst);
        self.output.stop();
    }

    /// Holds the speech, e.g. while it is unclear whether the user interrupts.
    pub fn pause(&self) {
        self.output.pause();
    }

    pub fn resume(&self) {
        self.output.resume();
    }
}
