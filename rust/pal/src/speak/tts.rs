//! Kyutai Pocket TTS in-process: the same voice on every platform. One thread generates, a second one
//! owns the pal-core playback (cpal streams stay on their thread), which also provides the output level.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::speak::playback::{Playback, PlaybackState};
use pal_tts::PocketTts;

use crate::speak::leveler::Leveler;
use crate::speak::voice_filter::VoiceFilter;
use crate::think::language::Language;
use crate::{Error, Result};

enum Audio {
    Chunk(u64, Vec<f32>),
    /// All of a sentence is queued; it counts as pending until then.
    End(u64),
}

/// Clones share one queue and playback, so one clone can speak while another stops it.
#[derive(Clone)]
pub struct PocketSpeaker {
    tx: Sender<(u64, String)>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    cut: Arc<AtomicBool>,
    output: Arc<PlaybackState>,
    /// Playback position of each sentence of the current reply.
    starts: Arc<Mutex<Vec<u64>>>,
}

impl PocketSpeaker {
    /// `voice` is a Pocket TTS catalog name such as "juergen". Blocks until the model is loaded.
    pub fn new(voice: &str, language: Language) -> Result<Self> {
        xn::set_num_threads(crate::cpu::fast_cores());
        let tts = PocketTts::load(language.profile().pocket_model, voice)
            .map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let rate = tts.sample_rate();
        let warm_up = tts.synthesize(language.profile().warm_up, &AtomicBool::new(false), &mut |_| Ok(()));
        warm_up.map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let cut = Arc::new(AtomicBool::new(false));
        let starts = Arc::new(Mutex::new(Vec::new()));

        let (audio_tx, audio_rx) = channel::<Audio>();
        let (ready_tx, ready_rx) = sync_channel(1);
        let (g, p, started) = (generation.clone(), pending.clone(), starts.clone());
        std::thread::spawn(move || {
            let mut playback = match Playback::start() {
                Ok(pb) => pb,
                Err(e) => return drop(ready_tx.send(Err(Error::Tts(e.to_string())))),
            };
            let _ = ready_tx.send(Ok(playback.state.clone()));
            let mut fresh = true;
            let state = playback.state.clone();
            let start_here = |gen_of_text: u64, fresh: &mut bool| {
                if std::mem::take(fresh)
                    && gen_of_text == g.load(Ordering::SeqCst)
                    && let Ok(mut starts) = started.lock()
                {
                    starts.push(state.written());
                }
            };
            for audio in audio_rx {
                match audio {
                    Audio::Chunk(gen_of_pcm, pcm) => {
                        if gen_of_pcm == g.load(Ordering::SeqCst) {
                            start_here(gen_of_pcm, &mut fresh);
                            playback.play(&pcm, rate);
                        }
                    }
                    Audio::End(gen_of_text) => {
                        start_here(gen_of_text, &mut fresh);
                        fresh = true;
                        p.fetch_sub(1, Ordering::SeqCst);
                        playback.drain();
                    }
                }
            }
        });
        let output = ready_rx.recv().map_err(|_| Error::Tts("playback thread died".into()))??;

        let (tx, rx) = channel::<(u64, String)>();
        let (g, c) = (generation.clone(), cut.clone());
        std::thread::spawn(move || {
            let mut leveler = Leveler::default();
            let mut filter = VoiceFilter::new(rate);
            for (gen_of_text, text) in rx {
                if gen_of_text == g.load(Ordering::SeqCst) {
                    c.store(false, Ordering::SeqCst);
                    let audio_tx = audio_tx.clone();
                    let result = tts.synthesize(&text, &c, &mut |pcm| {
                        audio_tx.send(Audio::Chunk(gen_of_text, leveler.apply(&filter.apply(pcm)))).map_err(Into::into)
                    });
                    if let Err(e) = result {
                        eprintln!("llm_pal: pocket tts: {e:#}");
                    }
                }
                let _ = audio_tx.send(Audio::End(gen_of_text));
            }
        });
        Ok(Self { tx, generation, pending, cut, output, starts })
    }

    pub fn output(&self) -> Arc<PlaybackState> {
        self.output.clone()
    }
}

/// Pocket's files come from `model_dir/pocket` (the downloader puts them there), never from its own download.
pub fn use_local_files(model_dir: &Path) {
    pal_tts::use_local_files(model_dir.join("pocket"));
}

impl PocketSpeaker {
    /// Queues a sentence and returns at once.
    pub fn say(&mut self, text: &str) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let msg = (self.generation.load(Ordering::SeqCst), text.to_string());
        self.tx.send(msg).map_err(|_| Error::Tts("speech thread stopped".into()))
    }

    /// Drops the queue and cuts off the current sentence.
    pub fn stop(&mut self) {
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

    /// When the first sound since the previous call was played.
    pub fn take_audio_start(&self) -> Option<Instant> {
        self.output.take_start()
    }

    /// The next sentences belong to a new reply, for `heard`.
    pub fn new_reply(&self) {
        if let Ok(mut starts) = self.starts.lock() {
            starts.clear();
        }
    }

    /// How many sentences of the current reply have begun to play, the last one maybe only partly.
    pub fn heard(&self) -> usize {
        let taken = self.output.taken();
        self.starts.lock().map_or(0, |starts| starts.iter().filter(|&&at| taken > at).count())
    }

    pub fn speaking(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0 || self.output.busy()
    }
}
