//! Kyutai Pocket TTS in-process: the same voice on every desktop platform. One thread generates, a second one
//! owns the pal-core playback (cpal streams stay on their thread), which also provides the output level.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};

use pal_core::playback::{Playback, PlaybackState};
use pal_tts::PocketTts;

use crate::language::Language;
use crate::speaker::Speaker;
use crate::{Error, Result};

enum Audio {
    Chunk(u64, Vec<f32>),
    /// All of a sentence is queued; it counts as pending until then.
    End,
}

pub struct PocketSpeaker {
    tx: Mutex<Sender<(u64, String)>>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    cut: Arc<AtomicBool>,
    output: Arc<PlaybackState>,
}

impl PocketSpeaker {
    /// `voice` is a Pocket TTS catalog name such as "juergen". Blocks until the model is loaded; the first start
    /// downloads it from Hugging Face.
    pub fn new(voice: &str, language: Language) -> Result<Self> {
        let tts = PocketTts::load(model_name(language), voice).map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let rate = tts.sample_rate();
        let warm_up = tts.synthesize("Hallo.", &AtomicBool::new(false), &mut |_| Ok(()));
        warm_up.map_err(|e| Error::Tts(format!("pocket tts: {e:#}")))?;
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let cut = Arc::new(AtomicBool::new(false));

        let (audio_tx, audio_rx) = channel::<Audio>();
        let (ready_tx, ready_rx) = sync_channel(1);
        let (g, p) = (generation.clone(), pending.clone());
        std::thread::spawn(move || {
            let mut playback = match Playback::start() {
                Ok(pb) => pb,
                Err(e) => return drop(ready_tx.send(Err(Error::Tts(e.to_string())))),
            };
            let _ = ready_tx.send(Ok(playback.state.clone()));
            for audio in audio_rx {
                match audio {
                    Audio::Chunk(gen_of_pcm, pcm) => {
                        if gen_of_pcm == g.load(Ordering::SeqCst) {
                            playback.play(&pcm, rate);
                        }
                    }
                    Audio::End => drop(p.fetch_sub(1, Ordering::SeqCst)),
                }
            }
        });
        let output = ready_rx.recv().map_err(|_| Error::Tts("playback thread died".into()))??;

        let (tx, rx) = channel::<(u64, String)>();
        let (g, c) = (generation.clone(), cut.clone());
        std::thread::spawn(move || {
            for (gen_of_text, text) in rx {
                if gen_of_text == g.load(Ordering::SeqCst) {
                    c.store(false, Ordering::SeqCst);
                    let audio_tx = audio_tx.clone();
                    let result = tts.synthesize(&text, &c, &mut |pcm| {
                        audio_tx.send(Audio::Chunk(gen_of_text, pcm.to_vec())).map_err(Into::into)
                    });
                    if let Err(e) = result {
                        eprintln!("llm_pal: pocket tts: {e:#}");
                    }
                }
                let _ = audio_tx.send(Audio::End);
            }
        });
        Ok(Self { tx: Mutex::new(tx), generation, pending, cut, output })
    }

    pub fn output(&self) -> Arc<PlaybackState> {
        self.output.clone()
    }
}

fn model_name(language: Language) -> &'static str {
    match language {
        Language::German => "german",
        Language::English => "english",
    }
}

/// Loads the model files from `dir` instead of Hugging Face, laid out as `<owner>/<repo>/<file>` (see
/// `build.sh push-models`). False, and nothing changes, if the model for `language` isn't there.
pub fn use_local_files(dir: &Path, language: Language) -> bool {
    let repo = dir.join("kyutai/pocket-tts-without-voice-cloning/languages").join(model_name(language));
    let present = repo.join("model.safetensors").exists();
    if present {
        pal_tts::use_local_files(dir.to_path_buf());
    }
    present
}

impl Speaker for PocketSpeaker {
    fn say(&mut self, text: &str) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let msg = (self.generation.load(Ordering::SeqCst), text.to_string());
        self.tx.lock().unwrap().send(msg).map_err(|_| Error::Tts("speech thread stopped".into()))
    }

    fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.cut.store(true, Ordering::SeqCst);
        self.output.stop();
    }

    fn speaking(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0 || self.output.busy()
    }

    fn level(&self) -> f32 {
        self.output.level()
    }
}
