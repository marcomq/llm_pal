//! The microphone as an mq-bridge input endpoint (`pal_mic`): resample → echo cancellation → VAD → segmenter →
//! Smart Turn → prosody on a worker thread, each `Event` a message. Frames stay on that thread; only events leave.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mq_bridge::ReceivedBatch;
use mq_bridge::errors::ConsumerError;
use mq_bridge::models::Endpoint;
use mq_bridge::traits::{CustomEndpointFactory, MessageConsumer};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::Error;
use crate::hear::capture;
use crate::hear::echo::{EchoCanceller, EchoReference};
use crate::hear::event::{Event, Segment};
use crate::hear::prosody;
use crate::hear::resample::{Resampler16k, TARGET_RATE};
use crate::hear::segmenter::{SegOut, Segmenter, SegmenterConfig};
use crate::hear::turn::{self, SmartTurn};
use crate::hear::vad::{self, SileroVad};

pub const NAME: &str = "pal_mic";

/// The endpoint's config, as JSON in a route: `{"custom": {"name": "pal_mic", "config": {...}}}`.
#[derive(Debug, Clone, Deserialize)]
pub struct MicConfig {
    pub vad_model: PathBuf,
    /// Smart Turn model: at a pause, a finished turn ends the segment at once instead of after `end_silence_ms`.
    #[serde(default)]
    pub turn_model: Option<PathBuf>,
    /// Part of the input device name; `None` takes the default input.
    #[serde(default)]
    pub device: Option<String>,
    /// When the output stops, speech that began during it is dropped: without barge-in it is Pal's own echo, and
    /// the user's next words would otherwise join it.
    #[serde(default)]
    pub drop_echo: bool,
}

/// What the app and a running microphone share besides the events. Without a link given to `endpoint`, the
/// consumer gets a fresh one: no echo cancellation and nobody reading the flags.
#[derive(Default)]
pub struct MicLink {
    /// Set while the app plays audio; speech starting meanwhile also emits `Event::BargeIn`.
    pub output_active: AtomicBool,
    /// Set while the VAD hears speech.
    pub in_speech: AtomicBool,
    live: AtomicBool,
    device: OnceLock<String>,
    /// What the output plays, removed from the microphone signal.
    echo: Mutex<Option<EchoReference>>,
}

impl MicLink {
    pub fn new(echo: Option<EchoReference>) -> Arc<Self> {
        Arc::new(Self { echo: Mutex::new(echo), ..Self::default() })
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

    pub fn device(&self) -> &str {
        self.device.get().map_or("", String::as_str)
    }
}

/// The link the next `pal_mic` consumer takes.
static NEXT_LINK: Mutex<Option<Arc<MicLink>>> = Mutex::new(None);

/// Registers `pal_mic` with mq-bridge once per process.
pub fn register() -> Result<(), Error> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    let registered = REGISTERED.get_or_init(|| {
        mq_bridge::extensions::register_endpoint_factory(NAME, Arc::new(MicFactory)).map_err(|e| format!("{e:#}"))
    });
    registered.clone().map_err(Error::Audio)
}

/// A `pal_mic` endpoint whose consumer shares `link` with the caller.
pub fn endpoint(config: &MicConfig, link: Arc<MicLink>) -> Result<Endpoint, Error> {
    register()?;
    *NEXT_LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
    let config = serde_json::json!({ "custom": { "name": NAME, "config": {
        "vad_model": config.vad_model, "turn_model": config.turn_model, "device": config.device,
        "drop_echo": config.drop_echo,
    }}});
    serde_json::from_value(config).map_err(|e| Error::Audio(e.to_string()))
}

#[derive(Debug)]
struct MicFactory;

#[async_trait]
impl CustomEndpointFactory for MicFactory {
    async fn create_consumer(
        &self,
        _route_name: &str,
        config: &serde_json::Value,
    ) -> mq_bridge::anyhow::Result<Box<dyn MessageConsumer>> {
        let config: MicConfig = serde_json::from_value(config.clone())?;
        let link = NEXT_LINK.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
        Ok(Box::new(MicConsumer::start(config, link).await?))
    }

    /// Speech that was missed is gone; nothing is redelivered.
    fn acknowledges(&self, _config: &serde_json::Value) -> bool {
        false
    }
}

/// Live processing of the microphone. Dropping it stops capture and the worker.
struct MicConsumer {
    events: mpsc::UnboundedReceiver<Event>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl MicConsumer {
    /// The cpal stream must stay on the thread that opened it, so the worker opens it and reports back.
    async fn start(config: MicConfig, link: Arc<MicLink>) -> Result<Self, Error> {
        let (ready_tx, ready) = oneshot::channel();
        let (tx, events) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let worker = std::thread::Builder::new()
            .name("pal-audio".into())
            .spawn(move || {
                let opened = capture::start(config.device.as_deref()).and_then(|(capture, ring)| {
                    let echo = link.echo.lock().unwrap_or_else(|e| e.into_inner()).take();
                    let core = Core::new(&config, capture.sample_rate, link.clone(), echo)?;
                    let _ = link.device.set(capture.device_name.clone());
                    Ok((capture, ring, core))
                });
                match opened {
                    Ok((_capture, ring, core)) => {
                        let _ = ready_tx.send(Ok(()));
                        worker(core, ring, tx, &stop_w, &link);
                    }
                    Err(e) => drop(ready_tx.send(Err(e))),
                }
            })
            .map_err(|e| Error::Audio(format!("worker thread: {e}")))?;
        ready.await.map_err(|_| Error::Audio("the audio worker died".into()))??;
        Ok(Self { events, stop, worker: Some(worker) })
    }
}

impl Drop for MicConsumer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[async_trait]
impl MessageConsumer for MicConsumer {
    async fn receive_batch(&mut self, _max_messages: usize) -> Result<ReceivedBatch, ConsumerError> {
        let event = self.events.recv().await.ok_or(ConsumerError::EndOfStream)?;
        Ok(ReceivedBatch { messages: vec![event.to_message()], commit: Box::new(|_| Box::pin(async { Ok(()) })) })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn worker(
    mut core: Core,
    mut ring: rtrb::Consumer<f32>,
    tx: mpsc::UnboundedSender<Event>,
    stop: &AtomicBool,
    link: &MicLink,
) {
    let mut buf = Vec::with_capacity(TARGET_RATE as usize);
    let mut emit = |e| drop(tx.send(e));
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
        if !link.live.load(Ordering::Relaxed) && buf.iter().any(|&s| s != 0.0) {
            link.live.store(true, Ordering::Relaxed);
        }
    }
    if let Err(e) = core.finish(&mut emit) {
        eprintln!("llm_pal: audio worker: {e}");
    }
}

/// Runs the whole chain over a recording. Deterministic; for tests.
pub fn process_samples(config: &MicConfig, samples: &[f32], rate: u32) -> Result<Vec<Event>, Error> {
    let mut core = Core::new(config, rate, Arc::default(), None)?;
    let mut events = Vec::new();
    let mut emit = |e| events.push(e);
    core.feed(samples, &mut emit)?;
    core.finish(&mut emit)?;
    Ok(events)
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
    link: Arc<MicLink>,
    drop_echo: bool,
    /// Sample position (16 kHz) until which input counts as overlapping the output.
    output_until: u64,
    pos: u64,
    during_output: bool,
}

impl Core {
    fn new(cfg: &MicConfig, rate: u32, link: Arc<MicLink>, echo: Option<EchoReference>) -> Result<Self, Error> {
        Ok(Self {
            resampler: Resampler16k::new(rate)?,
            echo: echo.map(EchoCanceller::new).transpose()?,
            resampled: Vec::new(),
            vad: SileroVad::new(&cfg.vad_model)?,
            turn: cfg.turn_model.as_deref().map(SmartTurn::new).transpose()?,
            turn_ended: false,
            segmenter: Segmenter::new(SegmenterConfig::default()),
            pending: Vec::with_capacity(TARGET_RATE as usize),
            outs: Vec::new(),
            next_id: 1,
            link,
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
            self.link.in_speech.store(self.segmenter.in_speech(), Ordering::Relaxed);
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
        if self.link.output_active.load(Ordering::Relaxed) {
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
