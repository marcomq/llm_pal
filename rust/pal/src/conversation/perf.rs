//! Time to the first answer, measured over the topics. The test plays the microphone and the voice: speech events
//! go to `[pal.inbox]` as the microphone sends them, the conversation and the reply routes run as `deploy` connects
//! them, and the test reads `[pal.speech]` in place of the voice. With the models:
//! `cargo test --release -p pal --lib perf -- --ignored --nocapture`, with `RUST_LOG=pal=debug` also the LLM's times.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use mq_bridge::blocking_handler::{BlockingHandler, CancelHandle, HandlerContext, ThreadedHandler};
use mq_bridge::models::Endpoint;
use mq_bridge::{Caller, CanonicalMessage, Handled, Publisher, Route};
use tokio::runtime::Runtime;

use super::tests::take_turn;
use super::*;
use crate::hear::event::Features;
use crate::speak::handlers;
use crate::think::brain::{self, Model};
use crate::think::persona::DEFAULT_NAME;
use crate::transport::{End, LLM_IN, LlmRequest, PIECES, Report, SPEECH, ToSpeaker, Token, emitter};

const ROUTES: [&str; 4] = ["perf_conversation", "perf_llm", "perf_split", "perf_voice"];
const LANG: Language = Language::German;
const QUESTION: &str = "Erzähl mir etwas über Vulkane in Island.";

/// Pal from `[pal.inbox]` to `[pal.speech]`.
struct Pal {
    rt: &'static Runtime,
    inbox: Publisher,
    speech: Receiver<(Instant, String, ToSpeaker)>,
    prefilled: Receiver<()>,
    events: Receiver<AgentEvent>,
    /// The turns the LLM was asked to answer; the others got a fixed reply (joke, quip, pool).
    asked: Receiver<String>,
    llm: CancelHandle,
    /// Turns over or discarded: what still comes from them is left out.
    over: RefCell<HashSet<String>>,
    _turn: MutexGuard<'static, ()>,
}

/// One turn, counted from the moment the speech ended (the last event sent).
#[derive(Debug)]
struct Measured {
    first_sentence: Duration,
    sentence: String,
    stt_ms: u128,
    by_llm: bool,
}

impl Pal {
    fn start<H: BlockingHandler>(
        stt: impl FnOnce(&Runtime) -> Caller,
        llm: impl FnOnce(Report) -> H,
        model_dir: PathBuf,
    ) -> Self {
        let (turn, rt) = take_turn();
        let stt = stt(rt);
        let publisher = || rt.block_on(Publisher::new(transport::inbox())).unwrap();
        let (inbox, to_inbox) = (publisher(), publisher());
        let report: Report = Arc::new(move |s: Spoken| drop(rt.block_on(to_inbox.send(s.to_message()))));

        let (asked_tx, asked) = channel();
        let mut llm = llm(report.clone());
        let llm = move |msg: CanonicalMessage, ctx: &HandlerContext| {
            if let Ok(LlmRequest::Reply { turn, .. }) = LlmRequest::from_message(&msg) {
                let _ = asked_tx.send(turn);
            }
            llm.handle(msg, ctx)
        };
        let llm = ThreadedHandler::with_emitter(llm, rt.block_on(emitter(PIECES)).unwrap());
        let split = ThreadedHandler::with_emitter(handlers::split(LANG, report), rt.block_on(emitter(SPEECH)).unwrap());
        let llm_cancel = llm.cancel_handle();
        let cancels = vec![llm.cancel_handle(), split.cancel_handle()];

        let cfg = AgentConfig {
            model_dir,
            smart_turn: false,
            stt_model: String::new(),
            llm_model: String::new(),
            voice: String::new(),
            barge_in: false,
            language: LANG,
            user_name: DEFAULT_NAME.into(),
            remote: None,
            fallback: false,
            party: false,
        };
        let persona = Persona::kitt(LANG, DEFAULT_NAME);
        let (events_tx, events) = channel();
        let transport = Transport::without_routes(rt.handle().clone(), stt, cancels);
        let mut c =
            Conversation::new(cfg, Arc::default(), events_tx, transport, MicLink::new(None), persona, Instant::now());
        c.phase = Phase::Listening;
        let (prefilled_tx, prefilled) = channel();
        let conversation = move |msg: CanonicalMessage, _: &HandlerContext| {
            let input = Input::from_message(&msg);
            if matches!(input, Some(Input::Spoken(Spoken::Prefilled))) {
                let _ = prefilled_tx.send(());
            }
            c.handle(input).expect("the conversation goes on");
            Ok(Handled::Ack)
        };
        let (speech_tx, speech) = channel();
        let voice = move |msg: CanonicalMessage, _: &HandlerContext| {
            if let Some((turn, what)) = ToSpeaker::from_message(&msg) {
                let _ = speech_tx.send((Instant::now(), turn, what));
            }
            Ok(Handled::Ack)
        };
        let topic = |name| Endpoint::new_memory(name, 1024);
        let deployed = rt.block_on(async {
            let conversation = ThreadedHandler::new(conversation);
            Route::new(transport::inbox(), Endpoint::null()).with_handler(conversation).deploy(ROUTES[0]).await?;
            Route::new(topic(LLM_IN), Endpoint::null()).with_handler(llm).deploy(ROUTES[1]).await?;
            Route::new(topic(PIECES), Endpoint::null()).with_handler(split).deploy(ROUTES[2]).await?;
            let voice = ThreadedHandler::new(voice);
            Route::new(topic(SPEECH), Endpoint::null()).with_handler(voice).deploy(ROUTES[3]).await
        });
        deployed.unwrap();
        Self { rt, inbox, speech, prefilled, events, asked, llm: llm_cancel, over: RefCell::default(), _turn: turn }
    }

    fn send(&self, msg: CanonicalMessage) {
        self.rt.block_on(self.inbox.send(msg)).unwrap();
    }

    /// Sends `events` with the gaps the microphone had between them, then plays the voice like `pal_speak` until
    /// the first sentence is heard: a held reply waits for its release. Then the rest of the reply is cancelled and
    /// the voice reports it done. Returns once the LLM has read the turn back in (`Prefilled`).
    fn turn(&self, events: &[(Duration, Event)]) -> Measured {
        let mut ended = Instant::now();
        for (gap, event) in events {
            std::thread::sleep(*gap);
            ended = Instant::now();
            self.send(event.to_message());
        }
        let (mut held, mut waiting) = (None::<String>, Vec::new());
        let (at, turn, sentence) = loop {
            let (at, turn, what) = self.speech.recv_timeout(Duration::from_secs(120)).expect("a reply");
            if self.over.borrow().contains(&turn) {
                continue;
            }
            let holding = held.as_ref() == Some(&turn);
            match what {
                ToSpeaker::Hold => (held, waiting) = (Some(turn), Vec::new()),
                ToSpeaker::Release if holding => {
                    held = None;
                    if !waiting.is_empty() {
                        break (at, turn, waiting.remove(0));
                    }
                }
                ToSpeaker::Discard if holding => {
                    held = None;
                    self.over.borrow_mut().insert(turn);
                }
                ToSpeaker::Sentence(text) if holding => waiting.push(text),
                ToSpeaker::Sentence(text) => break (at, turn, text),
                ToSpeaker::Error(e) => panic!("the reply failed: {e}"),
                _ => {}
            }
        };
        self.llm.cancel(&turn);
        self.over.borrow_mut().insert(turn.clone());
        // Read-aheads after a dropped reply, which came before this reply.
        self.prefilled.try_iter().for_each(drop);
        self.send(Spoken::Said { turn: turn.clone(), text: sentence.clone(), at }.to_message());
        self.send(Spoken::Audible { turn: turn.clone(), at }.to_message());
        self.send(Spoken::Done { turn: turn.clone() }.to_message());
        self.send(Spoken::Quiet.to_message());
        self.prefilled.recv_timeout(Duration::from_secs(120)).expect("the LLM reads the turn back in");
        let stt_ms = self.events.try_iter().find_map(|e| match e {
            AgentEvent::TurnDone { stt_ms, .. } => Some(stt_ms),
            _ => None,
        });
        Measured {
            first_sentence: at.saturating_duration_since(ended),
            sentence,
            stt_ms: stt_ms.expect("the turn is done"),
            by_llm: self.asked.try_iter().any(|t| t == turn),
        }
    }
}

impl Drop for Pal {
    fn drop(&mut self) {
        self.rt.block_on(async {
            for route in ROUTES {
                Route::stop(route).await;
            }
        });
    }
}

fn segment(text_ms: u64) -> Segment {
    Segment {
        id: 1,
        start_ms: 0,
        end_ms: text_ms,
        forced_split: false,
        during_output: false,
        turn_complete: true,
        silence_ms: 400,
        features: Features::default(),
        pcm: vec![0.0; 16 * text_ms as usize],
    }
}

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    sorted[(sorted.len() - 1) * p / 100]
}

/// Without models: STT answers at once, the "model" streams a fixed reply. What is left is what the topics, the
/// routes and the conversation cost.
#[test]
fn the_routes_add_little_to_the_first_answer() {
    let stt = |rt: &Runtime| {
        let heard = serde_json::json!({ "text": QUESTION }).to_string();
        let route = Route::new(Endpoint::new_memory("test.stt", 16), Endpoint::new_response()).with_handler(
            move |_: CanonicalMessage| {
                let heard = CanonicalMessage::from(heard.as_str());
                async move { Ok(Handled::Publish(heard)) }
            },
        );
        rt.block_on(Caller::deploy("test_stt", route)).unwrap()
    };
    let llm = |report: Report| {
        move |msg: CanonicalMessage, ctx: &HandlerContext| {
            let LlmRequest::Reply { turn, .. } = LlmRequest::from_message(&msg).unwrap() else {
                report(Spoken::Prefilled);
                return Ok(Handled::Ack);
            };
            for (i, piece) in ["Feuer ", "und Eis. ", "Und Wind."].into_iter().enumerate() {
                ctx.emit(Token::piece(&turn, i, piece).to_message())?;
            }
            ctx.emit(Token::last(&turn, 3, End::Complete, "").to_message())?;
            Ok(Handled::Ack)
        }
    };
    let pal = Pal::start(stt, llm, PathBuf::new());
    let mut firsts: Vec<_> =
        (0..50).map(|_| pal.turn(&[(Duration::ZERO, Event::Segment(segment(1000)))]).first_sentence).collect();
    firsts.sort();
    let (p50, p90) = (percentile(&firsts, 50), percentile(&firsts, 90));
    println!("routes only, end of speech → first sentence: p50 {p50:?}, p90 {p90:?}, max {:?}", firsts.last().unwrap());
    assert!(p50 < Duration::from_millis(20), "p50 {p50:?}");
}

fn models() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models")
}

/// The microphone's events for `testdata/de_48k.wav`, one turn per final segment, each with the time the
/// microphone took since the event before.
fn spoken_turns(smart_turn: bool) -> Vec<Vec<(Duration, Event)>> {
    let mut wav = hound::WavReader::open(models().join("../testdata/de_48k.wav")).unwrap();
    let rate = wav.spec().sample_rate;
    let samples: Vec<f32> = wav.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let config = MicConfig {
        vad_model: models().join("silero_vad.onnx"),
        turn_model: smart_turn.then(|| models().join("smart-turn-v3.2-cpu.onnx")),
        device: None,
        drop_echo: false,
    };
    let (mut turns, mut turn, mut last_ms) = (Vec::new(), Vec::new(), None);
    for event in mic::process_samples(&config, &samples, rate).unwrap() {
        let at_ms = match &event {
            Event::SpeechStart { at_ms } | Event::BargeIn { at_ms } => Some(*at_ms),
            Event::Pause(seg) | Event::Segment(seg) => Some(seg.end_ms + seg.silence_ms),
            Event::Resume | Event::Lost => None,
        };
        let gap = match (turn.is_empty(), last_ms, at_ms) {
            (false, Some(before), Some(at)) => Duration::from_millis(at.saturating_sub(before)),
            _ => Duration::ZERO,
        };
        last_ms = at_ms.or(last_ms);
        let ends = matches!(event, Event::Segment(_));
        turn.push((gap, event));
        if ends {
            turns.push(std::mem::take(&mut turn));
        }
    }
    turns
}

/// `turn` with a pause after 55 % of its speech, after which the user goes on for 1.5 s.
fn mid_sentence_pause(turn: &[(Duration, Event)]) -> Vec<(Duration, Event)> {
    let Some((_, Event::Segment(whole))) = turn.last() else { panic!("a turn ends with its segment") };
    let mut part = whole.clone();
    part.pcm.truncate(whole.pcm.len() * 55 / 100);
    part.end_ms = whole.start_ms + part.pcm.len() as u64 / 16;
    part.silence_ms = 0;
    let mut events = vec![
        (Duration::ZERO, Event::SpeechStart { at_ms: whole.start_ms }),
        (Duration::from_millis(part.end_ms - whole.start_ms), Event::Pause(part)),
        (Duration::from_millis(300), Event::Resume),
    ];
    let ends = turn.iter().skip_while(|(_, e)| !matches!(e, Event::Pause(_) | Event::Segment(_)));
    events.extend(
        ends.enumerate().map(|(i, (gap, e))| (if i == 0 { Duration::from_millis(1500) } else { *gap }, e.clone())),
    );
    events
}

fn kind(event: &Event) -> String {
    match event {
        Event::SpeechStart { .. } => "SpeechStart".into(),
        Event::BargeIn { .. } => "BargeIn".into(),
        Event::Pause(s) => format!("Pause({}–{} ms, silence {})", s.start_ms, s.end_ms, s.silence_ms),
        Event::Segment(s) => format!("Segment({}–{} ms, silence {})", s.start_ms, s.end_ms, s.silence_ms),
        Event::Resume => "Resume".into(),
        Event::Lost => "Lost".into(),
    }
}

/// Pocket TTS on its own, without playback: how long until the first audio of `text`. The rest is stopped.
fn tts_times(tts: &pal_tts::PocketTts, text: &str) -> Duration {
    let (t0, mut first) = (Instant::now(), None);
    let stop = std::sync::atomic::AtomicBool::new(false);
    tts.synthesize(text, &stop, &mut |_| {
        first.get_or_insert_with(|| t0.elapsed());
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    })
    .unwrap();
    first.expect("audio")
}

/// The real chain as the app runs it (smart turn on): the microphone's events from a recording, Parakeet, the
/// persona's fixed replies or Qwen, and the sentence split over the topics, each reply cut off after its first
/// sentence; then Pocket TTS for that sentence. Only the playback is left out.
#[test]
#[ignore = "needs models/ and testdata/"]
fn time_to_the_first_answer_with_the_models() {
    let filter = tracing_subscriber::EnvFilter::from_default_env();
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).try_init();
    let persona = Persona::kitt(LANG, DEFAULT_NAME);
    let (model, _thread) = Model::start(&models().join("Qwen3.5-2B-Q4_K_M.gguf"), persona.fixed_messages()).unwrap();
    model.prefill(persona.context(), None).unwrap();
    let stt = |rt: &Runtime| transport::start_stt(rt.handle(), &models().join("parakeet-tdt-0.6b-v3")).unwrap();
    let pal = Pal::start(stt, |report| brain::handler(Some(model), None, report), models());
    let mut turns = spoken_turns(true);
    let paused = mid_sentence_pause(&turns[1]);
    turns.push(paused);
    let measured: Vec<Measured> = turns
        .iter()
        .map(|events| {
            let line: Vec<String> =
                events.iter().map(|(gap, e)| format!("+{}ms {}", gap.as_millis(), kind(e))).collect();
            eprintln!("perf: events {}", line.join(", "));
            pal.turn(events)
        })
        .collect();
    drop(pal);

    xn::set_num_threads(crate::cpu::fast_cores());
    tts::use_local_files(&models());
    let tts = pal_tts::PocketTts::load(LANG.profile().pocket_model, "juergen").unwrap();
    tts_times(&tts, LANG.profile().warm_up);
    println!("end of speech → first sentence queued, + Pocket TTS until its first audio (no playback):");
    for m in &measured {
        let audio = tts_times(&tts, &m.sentence);
        println!(
            "  {:5} stt {:4} ms, first sentence {:5} ms, + tts {:4} ms = {:5} ms  {:?}",
            if m.by_llm { "llm" } else { "fixed" },
            m.stt_ms,
            m.first_sentence.as_millis(),
            audio.as_millis(),
            (m.first_sentence + audio).as_millis(),
            m.sentence,
        );
    }
}

/// Parakeet on a whole segment and on pieces of it, cut where the segmenter saw a pause of `pause_ms`: the pieces'
/// text should read like the whole.
#[test]
#[ignore = "needs models/ and testdata/"]
fn transcripts_in_pieces() {
    use crate::hear::resample::Resampler16k;
    use crate::hear::segmenter::{SegOut, Segmenter, SegmenterConfig};
    use crate::hear::vad::{CHUNK, SileroVad};

    let mut wav = hound::WavReader::open(models().join("../testdata/de_48k.wav")).unwrap();
    let rate = wav.spec().sample_rate;
    let samples: Vec<f32> = wav.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let mut pcm = Vec::new();
    Resampler16k::new(rate).unwrap().push(&samples, &mut pcm).unwrap();
    let rt = Runtime::new().unwrap();
    let stt = transport::start_stt(rt.handle(), &models().join("parakeet-tdt-0.6b-v3")).unwrap();
    let text = |pcm: &[f32]| rt.block_on(crate::hear::stt::transcribe(&stt, pcm)).unwrap();
    for pause_ms in [250, 150, 100] {
        let mut vad = SileroVad::new(&models().join("silero_vad.onnx")).unwrap();
        let mut segmenter = Segmenter::new(SegmenterConfig { pause_ms, ..SegmenterConfig::default() });
        let (mut outs, mut cuts) = (Vec::new(), Vec::new());
        for chunk in pcm.chunks_exact(CHUNK) {
            segmenter.push(chunk, vad.process(chunk).unwrap(), &mut outs);
            for out in outs.drain(..) {
                match out {
                    SegOut::Pause { pcm, .. } => cuts.push(pcm.len()),
                    SegOut::Done { pcm, .. } => {
                        let whole = text(&pcm);
                        let mut from = 0;
                        let pieces: Vec<String> = cuts
                            .drain(..)
                            .chain([pcm.len()])
                            .filter_map(|to| {
                                let piece = (to > from).then(|| text(&pcm[from..to]));
                                from = from.max(to);
                                piece.filter(|t| !t.is_empty())
                            })
                            .collect();
                        println!("pause {pause_ms} ms\n  whole:  {whole}\n  pieces: {}", pieces.join(" | "));
                    }
                    _ => {}
                }
            }
        }
    }
    rt.block_on(stt.stop());
}
