//! Resample → VAD → segmenter → prosody, live on a worker thread or offline over a buffer.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::capture::{self, Capture};
use crate::echo::{EchoCanceller, EchoReference};
use crate::event::{Event, Segment};
use crate::resample::{Resampler16k, TARGET_RATE};
use crate::segmenter::{SegOut, Segmenter, SegmenterConfig};
use crate::turn::{self, SmartTurn};
use crate::vad::{self, SileroVad};
use crate::{Error, prosody};

#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    pub vad_model: PathBuf,
    pub segmenter: SegmenterConfig,
    /// Part of the input device name; `None` takes the default input.
    pub device: Option<String>,
    /// When the output stops, speech that began during it is dropped: without barge-in it is Pal's own echo, and
    /// the user's next words would otherwise join it.
    pub drop_echo: bool,
    /// Smart Turn model: at a pause, a finished turn ends the segment at once instead of after `end_silence_ms`.
    pub turn_model: Option<PathBuf>,
}

/// Pal's voice still echoes in the room this long after the output stopped.
const ECHO_MS: u64 = 300;

fn ms(samples: u64) -> u64 {
    samples * 1000 / TARGET_RATE as u64
}

struct Core {
    resampler: Resampler16k,
    echo: Option<EchoCanceller>,
    resampled: Vec<f32>,
    vad: SileroVad,
    turn: Option<SmartTurn>,
    /// The next `Done` comes from Smart Turn.
    turn_ended: bool,
    segmenter: Segmenter,
    pending: Vec<f32>,
    outs: Vec<SegOut>,
    next_id: u64,
    output_active: Arc<AtomicBool>,
    in_speech: Arc<AtomicBool>,
    drop_echo: bool,
    /// Sample position (16 kHz) until which input counts as overlapping the output.
    output_until: u64,
    pos: u64,
    during_output: bool,
}

impl Core {
    fn new(
        cfg: &ProcessorConfig,
        rate: u32,
        output_active: Arc<AtomicBool>,
        in_speech: Arc<AtomicBool>,
        echo: Option<EchoReference>,
    ) -> Result<Self, Error> {
        Ok(Self {
            resampler: Resampler16k::new(rate)?,
            echo: echo.map(EchoCanceller::new).transpose()?,
            resampled: Vec::new(),
            vad: SileroVad::new(&cfg.vad_model)?,
            turn: cfg.turn_model.as_deref().map(SmartTurn::new).transpose()?,
            turn_ended: false,
            segmenter: Segmenter::new(cfg.segmenter.clone()),
            pending: Vec::with_capacity(TARGET_RATE as usize),
            outs: Vec::new(),
            next_id: 1,
            output_active,
            in_speech,
            drop_echo: cfg.drop_echo,
            output_until: 0,
            pos: 0,
            during_output: false,
        })
    }

    /// Mono samples at the input rate.
    fn feed(&mut self, input: &[f32], emit: &mut impl FnMut(Event)) -> Result<(), Error> {
        match &mut self.echo {
            Some(echo) => {
                self.resampler.push(input, &mut self.resampled)?;
                echo.process(&self.resampled, &mut self.pending)?;
                self.resampled.clear();
            }
            None => self.resampler.push(input, &mut self.pending)?,
        }
        self.run_chunks(emit)
    }

    fn finish(&mut self, emit: &mut impl FnMut(Event)) -> Result<(), Error> {
        self.resampler.flush(&mut self.pending)?;
        if !self.pending.is_empty() {
            let padded = self.pending.len().next_multiple_of(vad::CHUNK);
            self.pending.resize(padded, 0.0);
        }
        self.run_chunks(emit)?;
        self.segmenter.finish(&mut self.outs);
        self.drain_outs(emit);
        Ok(())
    }

    fn run_chunks(&mut self, emit: &mut impl FnMut(Event)) -> Result<(), Error> {
        let whole = self.pending.len() / vad::CHUNK * vad::CHUNK;
        for start in (0..whole).step_by(vad::CHUNK) {
            self.pos += vad::CHUNK as u64;
            let output = self.output();
            if self.drop_echo && !output && self.during_output && self.segmenter.in_speech() {
                self.segmenter.discard();
                self.during_output = false;
            }
            let chunk = &self.pending[start..start + vad::CHUNK];
            let prob = self.vad.process(chunk)?;
            self.segmenter.push(chunk, prob, &mut self.outs);
            self.drain_outs(emit);
            self.during_output |= self.segmenter.in_speech() && output;
            self.in_speech.store(self.segmenter.in_speech(), Ordering::Relaxed);
        }
        self.pending.drain(..whole);
        Ok(())
    }

    fn drain_outs(&mut self, emit: &mut impl FnMut(Event)) {
        let mut outs = std::mem::take(&mut self.outs);
        // A finished turn at a pause adds a `Done` to `self.outs`, handled in the next round.
        while !outs.is_empty() {
            for out in outs.drain(..) {
                self.handle(out, emit);
            }
            std::mem::swap(&mut outs, &mut self.outs);
        }
        self.outs = outs;
    }

    fn handle(&mut self, out: SegOut, emit: &mut impl FnMut(Event)) {
        match out {
            SegOut::Start { at } => {
                emit(Event::SpeechStart { at_ms: ms(at) });
                self.during_output = self.output();
                if self.during_output {
                    emit(Event::BargeIn { at_ms: ms(at) });
                }
            }
            SegOut::Done { start, pcm, forced } => {
                let mut seg = self.segment(start, pcm, forced);
                seg.turn_complete = std::mem::take(&mut self.turn_ended);
                seg.silence_ms = ms(self.pos).saturating_sub(seg.end_ms);
                emit(Event::Segment(seg));
                self.next_id += 1;
            }
            SegOut::Pause { start, pcm } => {
                let over = self.turn_over(&pcm);
                emit(Event::Pause(self.segment(start, pcm, false)));
                if over {
                    self.turn_ended = true;
                    self.segmenter.finish(&mut self.outs);
                }
            }
            SegOut::Resume => emit(Event::Resume),
        }
    }

    /// Smart Turn's verdict on the speech so far; false without the model and for likely echo of the output.
    fn turn_over(&mut self, pcm: &[f32]) -> bool {
        let Some(model) = &mut self.turn else { return false };
        if self.drop_echo && self.during_output {
            return false;
        }
        match model.complete(pcm) {
            Ok(p) => p >= turn::COMPLETE,
            Err(e) => {
                eprintln!("llm_pal: {e}");
                false
            }
        }
    }
}

impl Core {
    /// True while output plays and for `ECHO_MS` after.
    fn output(&mut self) -> bool {
        if self.output_active.load(Ordering::Relaxed) {
            self.output_until = self.pos + ECHO_MS * TARGET_RATE as u64 / 1000;
        }
        self.pos < self.output_until
    }

    fn segment(&self, start: u64, pcm: Vec<f32>, forced_split: bool) -> Segment {
        Segment {
            id: self.next_id,
            start_ms: ms(start),
            end_ms: ms(start + pcm.len() as u64),
            forced_split,
            during_output: self.during_output,
            turn_complete: false,
            silence_ms: 0,
            features: prosody::analyze(&pcm),
            pcm,
        }
    }
}

/// Runs the whole chain over a recording. Deterministic; for tests and the `file` command.
pub fn process_samples(cfg: &ProcessorConfig, samples: &[f32], rate: u32) -> Result<Vec<Event>, Error> {
    let mut core = Core::new(cfg, rate, Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)), None)?;
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    core.feed(samples, &mut emit)?;
    core.finish(&mut emit)?;
    Ok(events)
}

/// Live processing of the microphone. Dropping it stops capture and the worker.
pub struct Processor {
    capture: Capture,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    output_active: Arc<AtomicBool>,
    in_speech: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
}

impl Processor {
    /// With `echo`, what the output plays is removed from the microphone signal.
    pub fn start(cfg: &ProcessorConfig, echo: Option<EchoReference>) -> Result<(Self, Receiver<Event>), Error> {
        let (capture, ring) = capture::start(cfg.device.as_deref())?;
        let output_active = Arc::new(AtomicBool::new(false));
        let in_speech = Arc::new(AtomicBool::new(false));
        let core = Core::new(cfg, capture.sample_rate, output_active.clone(), in_speech.clone(), echo)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded();
        let live = Arc::new(AtomicBool::new(false));
        let (stop_w, live_w) = (stop.clone(), live.clone());
        let worker = std::thread::Builder::new()
            .name("pal-audio".into())
            .spawn(move || worker(core, ring, tx, stop_w, live_w))
            .map_err(|e| Error::Audio(format!("worker thread: {e}")))?;
        Ok((Self { capture, stop, worker: Some(worker), output_active, in_speech, live }, rx))
    }

    /// Waits until the microphone delivers sound, not just the digital silence some devices (e.g. Bluetooth
    /// headsets switching profile) send for seconds after the start. False on timeout.
    pub fn wait_live(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while !self.live.load(Ordering::Relaxed) {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Set while the app plays audio; speech starting meanwhile also emits `Event::BargeIn`.
    pub fn output_active(&self) -> Arc<AtomicBool> {
        self.output_active.clone()
    }

    /// Set while the VAD hears speech.
    pub fn in_speech(&self) -> Arc<AtomicBool> {
        self.in_speech.clone()
    }

    pub fn sample_rate(&self) -> u32 {
        self.capture.sample_rate
    }

    pub fn device_name(&self) -> &str {
        &self.capture.device_name
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

fn worker(
    mut core: Core,
    mut ring: rtrb::Consumer<f32>,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    live: Arc<AtomicBool>,
) {
    let mut buf = Vec::with_capacity(TARGET_RATE as usize);
    let mut emit = |e| {
        let _ = tx.send(e);
    };
    while !stop.load(Ordering::Relaxed) {
        let n = ring.slots();
        if n == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let Ok(chunk) = ring.read_chunk(n) else { continue };
        let (a, b) = chunk.as_slices();
        buf.clear();
        buf.extend_from_slice(a);
        buf.extend_from_slice(b);
        chunk.commit_all();
        if let Err(e) = core.feed(&buf, &mut emit) {
            eprintln!("llm_pal: audio worker stopped: {e}");
            return;
        }
        if !live.load(Ordering::Relaxed) && buf.iter().any(|&s| s != 0.0) {
            live.store(true, Ordering::Relaxed);
        }
    }
    if let Err(e) = core.finish(&mut emit) {
        eprintln!("llm_pal: audio worker: {e}");
    }
}
