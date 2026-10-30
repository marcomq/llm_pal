//! Speech output. `SayTts` queues sentences for the macOS `say` command; `PrintTts` only prints them;
//! `ExternalTts` leaves speaking to the host app (e.g. Godot on Android).

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{Error, Result};

pub trait Speaker {
    /// Queues a sentence and returns at once.
    fn say(&mut self, text: &str) -> Result<()>;
    /// Drops the queue and cuts off the current sentence.
    fn stop(&mut self);
    fn speaking(&self) -> bool;
    /// Output level 0..1 for visuals; 0 if unknown.
    fn level(&self) -> f32 {
        0.0
    }
}

pub struct PrintTts;

impl Speaker for PrintTts {
    fn say(&mut self, text: &str) -> Result<()> {
        println!("  > {text}");
        Ok(())
    }
    fn stop(&mut self) {}
    fn speaking(&self) -> bool {
        false
    }
}

pub struct SayTts {
    tx: Sender<(u64, String)>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    child: Arc<Mutex<Option<Child>>>,
}

impl SayTts {
    /// `voice` as listed by `say -v '?'`, e.g. "Anna"; `rate` in words per minute.
    pub fn new(voice: &str, rate: Option<u32>) -> Result<Self> {
        Command::new("say").arg("-v").arg("?").output().map_err(|e| Error::Tts(format!("say: {e}")))?;
        let (tx, rx) = channel::<(u64, String)>();
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let child: Arc<Mutex<Option<Child>>> = Arc::default();
        let (g, p, c, voice) = (generation.clone(), pending.clone(), child.clone(), voice.to_string());
        std::thread::spawn(move || {
            for (gen_of_text, text) in rx {
                if gen_of_text == g.load(Ordering::SeqCst) {
                    let mut cmd = Command::new("say");
                    cmd.arg("-v").arg(&voice);
                    if let Some(r) = rate {
                        cmd.arg("-r").arg(r.to_string());
                    }
                    match cmd.arg("--").arg(&text).spawn() {
                        Ok(ch) => {
                            *c.lock().unwrap() = Some(ch);
                            wait(&c);
                        }
                        Err(e) => eprintln!("say: {e}"),
                    }
                }
                p.fetch_sub(1, Ordering::SeqCst);
            }
        });
        Ok(Self { tx, generation, pending, child })
    }
}

/// Polls instead of blocking in `wait`, so `stop` can take the lock and kill the child.
fn wait(child: &Mutex<Option<Child>>) {
    loop {
        {
            let mut guard = child.lock().unwrap();
            match guard.as_mut().map(Child::try_wait) {
                Some(Ok(None)) => {}
                _ => {
                    *guard = None;
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Speaker for SayTts {
    fn say(&mut self, text: &str) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx
            .send((self.generation.load(Ordering::SeqCst), text.to_string()))
            .map_err(|_| Error::Tts("say worker stopped".into()))
    }

    fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(ch) = self.child.lock().unwrap().as_mut() {
            let _ = ch.kill();
        }
    }

    fn speaking(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }
}

/// Shared between the agent and a host that speaks the `Said` sentences itself.
#[derive(Default)]
pub struct ExternalSpeech {
    speaking: AtomicBool,
    stop: AtomicBool,
    last_say: Mutex<Option<Instant>>,
}

/// The host has this long to start speaking a sentence before the agent stops counting it as output.
const HOST_START_GRACE: Duration = Duration::from_millis(800);

impl ExternalSpeech {
    /// The host reports whether it is speaking, e.g. once per frame.
    pub fn set_speaking(&self, on: bool) {
        self.speaking.store(on, Ordering::Relaxed);
    }

    /// True once after the agent asked to cut off the speech (barge-in).
    pub fn take_stop(&self) -> bool {
        self.stop.swap(false, Ordering::Relaxed)
    }

    pub fn speaking(&self) -> bool {
        self.speaking.load(Ordering::Relaxed)
            || self.last_say.lock().unwrap().is_some_and(|t| t.elapsed() < HOST_START_GRACE)
    }
}

pub struct ExternalTts(pub Arc<ExternalSpeech>);

impl Speaker for ExternalTts {
    fn say(&mut self, _text: &str) -> Result<()> {
        *self.0.last_say.lock().unwrap() = Some(Instant::now());
        Ok(())
    }

    fn stop(&mut self) {
        *self.0.last_say.lock().unwrap() = None;
        self.0.speaking.store(false, Ordering::Relaxed);
        self.0.stop.store(true, Ordering::Relaxed);
    }

    fn speaking(&self) -> bool {
        self.0.speaking()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_speech_counts_until_the_host_reports_and_passes_stop_on() {
        let speech = Arc::new(ExternalSpeech::default());
        let mut tts = ExternalTts(speech.clone());
        assert!(!tts.speaking());
        tts.say("Hallo.").unwrap();
        assert!(tts.speaking(), "busy before the host starts speaking");
        speech.set_speaking(true);
        tts.stop();
        assert!(!tts.speaking());
        assert!(speech.take_stop());
        assert!(!speech.take_stop());
    }

    #[test]
    #[ignore = "plays audio through macOS say"]
    fn stop_cuts_off_and_drops_the_queue() {
        let mut tts = SayTts::new("Anna", None).unwrap();
        tts.say("Das ist ein ziemlich langer Satz, der hoffentlich gleich unterbrochen wird.").unwrap();
        tts.say("Dieser Satz sollte nie zu hören sein.").unwrap();
        assert!(tts.speaking());
        std::thread::sleep(Duration::from_millis(700));
        let t = Instant::now();
        tts.stop();
        while tts.speaking() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(t.elapsed() < Duration::from_millis(300), "stopped after {:?}", t.elapsed());
    }
}
