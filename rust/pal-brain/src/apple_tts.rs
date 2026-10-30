//! macOS speech in-process: one AVSpeechSynthesizer stays loaded (a `say` process takes 2-5 s to start), and its
//! samples go through pal-core playback, which also provides the output level for the UI.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioPCMBuffer, AVSpeechBoundary, AVSpeechSynthesisVoice, AVSpeechSynthesizer, AVSpeechUtterance,
    AVSpeechUtteranceDefaultSpeechRate, AVSpeechUtteranceMaximumSpeechRate,
};
use objc2_foundation::{NSDate, NSRunLoop, NSString};
use pal_core::playback::{Playback, PlaybackState};

use crate::speaker::Speaker;
use crate::{Error, Result};

/// The synthesizer sends no end marker; a pause this long after the last buffer ends the utterance.
const IDLE_END: Duration = Duration::from_millis(250);
const NO_AUDIO_TIMEOUT: Duration = Duration::from_secs(8);

pub struct AppleTts {
    tx: Mutex<Sender<(u64, String)>>,
    generation: Arc<AtomicU64>,
    pending: Arc<AtomicUsize>,
    output: Arc<PlaybackState>,
}

impl AppleTts {
    /// `voice` is a system voice name such as "Anna"; `rate` scales the default speaking rate.
    /// Blocks until the voice is loaded and warmed up (the first utterance takes seconds).
    pub fn new(voice: &str, rate: f32) -> Result<Self> {
        Self::new_in(voice, rate, "de")
    }

    /// Like `new`; without a voice named `voice`, takes the system voice for `language` ("de", "en").
    pub fn new_in(voice: &str, rate: f32, language: &str) -> Result<Self> {
        let language = language.to_string();
        let (tx, rx) = channel::<(u64, String)>();
        let (ready_tx, ready_rx) = sync_channel(1);
        let generation = Arc::new(AtomicU64::new(0));
        let pending = Arc::new(AtomicUsize::new(0));
        let (g, p, voice) = (generation.clone(), pending.clone(), voice.to_string());
        std::thread::spawn(move || {
            let mut playback = match Playback::start() {
                Ok(pb) => pb,
                Err(e) => return drop(ready_tx.send(Err(Error::Tts(e.to_string())))),
            };
            let synth = unsafe { AVSpeechSynthesizer::new() };
            let Some(voice) = find_voice(&voice, &language) else {
                return drop(ready_tx.send(Err(Error::Tts(format!("no system voice {voice:?}")))));
            };
            synthesize(&synth, &voice, rate, "Hallo.", |_, _| true);
            let _ = ready_tx.send(Ok(playback.state.clone()));
            for (gen_of_text, text) in rx {
                if gen_of_text == g.load(Ordering::SeqCst) {
                    synthesize(&synth, &voice, rate, &text, |pcm, sr| {
                        g.load(Ordering::SeqCst) == gen_of_text && playback.play(pcm, sr)
                    });
                }
                p.fetch_sub(1, Ordering::SeqCst);
            }
        });
        let output = ready_rx.recv().map_err(|_| Error::Tts("speech thread died".into()))??;
        Ok(Self { tx: Mutex::new(tx), generation, pending, output })
    }

    pub fn output(&self) -> Arc<PlaybackState> {
        self.output.clone()
    }
}

fn find_voice(name: &str, language: &str) -> Option<Retained<AVSpeechSynthesisVoice>> {
    let voices = unsafe { AVSpeechSynthesisVoice::speechVoices() };
    let named = voices
        .iter()
        .find(|v| unsafe { v.name().to_string() == name && v.language().to_string().starts_with(language) });
    let fallback = if language == "de" { "de-DE" } else { "en-US" };
    named.or_else(|| unsafe { AVSpeechSynthesisVoice::voiceWithLanguage(Some(&NSString::from_str(fallback))) })
}

/// Streams one utterance to `on_audio(samples, rate)` until it ends or `on_audio` returns false.
fn synthesize(
    synth: &AVSpeechSynthesizer,
    voice: &AVSpeechSynthesisVoice,
    rate: f32,
    text: &str,
    mut on_audio: impl FnMut(&[f32], u32) -> bool,
) {
    let (tx, rx) = channel::<(Vec<f32>, u32)>();
    let block = RcBlock::new(move |buf: NonNull<AVAudioBuffer>| {
        if let Some(chunk) = samples(unsafe { buf.as_ref() }) {
            let _ = tx.send(chunk);
        }
    });
    unsafe {
        let utt = AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));
        utt.setVoice(Some(voice));
        utt.setRate((AVSpeechUtteranceDefaultSpeechRate * rate).min(AVSpeechUtteranceMaximumSpeechRate));
        synth.writeUtterance_toBufferCallback(&utt, RcBlock::as_ptr(&block));
    }
    let start = Instant::now();
    let mut last = None::<Instant>;
    loop {
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok((pcm, sr)) => {
                last = Some(Instant::now());
                if !on_audio(&pcm, sr) {
                    unsafe { synth.stopSpeakingAtBoundary(AVSpeechBoundary::Immediate) };
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if last.map_or(start.elapsed() > NO_AUDIO_TIMEOUT, |t| t.elapsed() > IDLE_END) {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The synthesizer delivers its buffers on the main queue, so the main thread must run this (GUI apps already do).
pub fn run_main_loop() -> ! {
    loop {
        let t = Instant::now();
        NSRunLoop::mainRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.1));
        if t.elapsed() < Duration::from_millis(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Mono f32 samples of channel 0 and the sample rate; the synthesizer delivers float or int16.
fn samples(buf: &AVAudioBuffer) -> Option<(Vec<f32>, u32)> {
    let pcm = buf.downcast_ref::<AVAudioPCMBuffer>()?;
    unsafe {
        let n = pcm.frameLength() as usize;
        if n == 0 {
            return None;
        }
        let format = pcm.format();
        let rate = format.sampleRate() as u32;
        let stride = if format.isInterleaved() { format.channelCount().max(1) as usize } else { 1 };
        let float = pcm.floatChannelData();
        if !float.is_null() {
            let ch0 = std::slice::from_raw_parts((*float).as_ptr(), n * stride);
            return Some((ch0.iter().step_by(stride).copied().collect(), rate));
        }
        let int = pcm.int16ChannelData();
        if !int.is_null() {
            let ch0 = std::slice::from_raw_parts((*int).as_ptr(), n * stride);
            return Some((ch0.iter().step_by(stride).map(|&s| s as f32 / 32768.0).collect(), rate));
        }
        None
    }
}

impl Speaker for AppleTts {
    fn say(&mut self, text: &str) -> Result<()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        let msg = (self.generation.load(Ordering::SeqCst), text.to_string());
        self.tx.lock().unwrap().send(msg).map_err(|_| Error::Tts("speech thread stopped".into()))
    }

    fn stop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.output.stop();
    }

    fn speaking(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0 || self.output.busy()
    }

    fn level(&self) -> f32 {
        self.output.level()
    }
}
