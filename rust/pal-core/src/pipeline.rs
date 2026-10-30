//! Resample → VAD → segmenter → prosody, live on a worker thread or offline over a buffer.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::capture::{self, Capture};
use crate::event::{Event, Segment};
use crate::resample::{Resampler16k, TARGET_RATE};
use crate::segmenter::{SegOut, Segmenter, SegmenterConfig};
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
}

/// Pal's voice still echoes in the room this long after the output stopped.
const ECHO_MS: u64 = 300;

fn ms(samples: u64) -> u64 {
    samples * 1000 / TARGET_RATE as u64
}

struct Core {
    resampler: Resampler16k,
    vad: SileroVad,
    segmenter: Segmenter,
    pending: Vec<f32>,
    outs: Vec<SegOut>,
    next_id: u64,
    output_active: Arc<AtomicBool>,
    drop_echo: bool,
    /// Sample position (16 kHz) until which input counts as overlapping the output.
    output_until: u64,
    pos: u64,
    during_output: bool,
}

impl Core {
    fn new(cfg: &ProcessorConfig, rate: u32, output_active: Arc<AtomicBool>) -> Result<Self, Error> {
        Ok(Self {
            resampler: Resampler16k::new(rate)?,
            vad: SileroVad::new(&cfg.vad_model)?,
            segmenter: Segmenter::new(cfg.segmenter.clone()),
            pending: Vec::with_capacity(TARGET_RATE as usize),
            outs: Vec::new(),
            next_id: 1,
            output_active,
            drop_echo: cfg.drop_echo,
            output_until: 0,
            pos: 0,
            during_output: false,
        })
    }

    /// Mono samples at the input rate.
    fn feed(&mut self, input: &[f32], emit: &mut impl FnMut(Event)) -> Result<(), Error> {
        self.resampler.push(input, &mut self.pending)?;
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
        }
        self.pending.drain(..whole);
        Ok(())
    }

    fn drain_outs(&mut self, emit: &mut impl FnMut(Event)) {
        let mut outs = std::mem::take(&mut self.outs);
        for out in outs.drain(..) {
            match out {
                SegOut::Start { at } => {
                    emit(Event::SpeechStart { at_ms: ms(at) });
                    self.during_output = self.output();
                    if self.during_output {
                        emit(Event::BargeIn { at_ms: ms(at) });
                    }
                }
                SegOut::Done { start, pcm, forced } => {
                    emit(Event::Segment(self.segment(start, pcm, forced)));
                    self.next_id += 1;
                }
                SegOut::Pause { start, pcm } => emit(Event::Pause(self.segment(start, pcm, false))),
                SegOut::Resume => emit(Event::Resume),
            }
        }
        self.outs = outs;
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
            features: prosody::analyze(&pcm),
            pcm,
        }
    }
}

/// Runs the whole chain over a recording. Deterministic; for tests and the `file` command.
pub fn process_samples(cfg: &ProcessorConfig, samples: &[f32], rate: u32) -> Result<Vec<Event>, Error> {
    let mut core = Core::new(cfg, rate, Arc::new(AtomicBool::new(false)))?;
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
    live: Arc<AtomicBool>,
}

impl Processor {
    pub fn start(cfg: &ProcessorConfig) -> Result<(Self, Receiver<Event>), Error> {
        let (capture, ring) = capture::start(cfg.device.as_deref())?;
        let output_active = Arc::new(AtomicBool::new(false));
        let core = Core::new(cfg, capture.sample_rate, output_active.clone())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = crossbeam_channel::unbounded();
        let live = Arc::new(AtomicBool::new(false));
        let (stop_w, live_w) = (stop.clone(), live.clone());
        let worker = std::thread::Builder::new()
            .name("pal-audio".into())
            .spawn(move || worker(core, ring, tx, stop_w, live_w))
            .map_err(|e| Error::Audio(format!("worker thread: {e}")))?;
        Ok((Self { capture, stop, worker: Some(worker), output_active, live }, rx))
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

    pub fn sample_rate(&self) -> u32 {
        self.capture.sample_rate
    }

    pub fn device_name(&self) -> &str {
        &self.capture.device_name
    }

    pub fn overruns(&self) -> usize {
        self.capture.overruns.load(Ordering::Relaxed)
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

fn worker(mut core: Core, mut ring: rtrb::Consumer<f32>, tx: Sender<Event>, stop: Arc<AtomicBool>, live: Arc<AtomicBool>) {
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
