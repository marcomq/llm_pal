//! Prototype: the reply path LLM → sentences → speaker as three mq-bridge routes over `memory` endpoints,
//! measured against the direct path `converse()` uses today. Acceptance: `first_sentence_ms` must not rise
//! noticeably. `cargo test --release -p pal-brain --test mq_pipeline -- --ignored --nocapture`

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use mq_bridge::blocking_handler::{CancelHandle, HandlerContext, ThreadedHandler};
use mq_bridge::models::Endpoint;
use mq_bridge::traits::MessagePublisher;
use mq_bridge::{CanonicalMessage, Handled, HandlerError, Publisher, Route};
use pal_brain::language::Language;
use pal_brain::llm::{Llm, LlmConfig, LlmSession, LocalLlm};
use pal_brain::persona::{DEFAULT_NAME, Persona};
use pal_brain::sentences::Sentences;

const LANG: Language = Language::German;
const KIND: &str = "kind";
/// When the LLM produced the piece, in ns since `BASE`; a sentence carries the stamp of the piece that ended it.
const STAMP: &str = "t";
const CAPACITY: usize = 1024;
const ROUTES: [&str; 3] = ["pal_llm", "pal_split", "pal_speak"];
const QUESTIONS: &[&str] = &[
    "Wie spät ist es eigentlich?",
    "Erzähl mir etwas über Autos.",
    "Was hältst du von Regen?",
    "Kannst du mir einen Rat geben?",
    "Was machst du am liebsten?",
];

static BASE: LazyLock<Instant> = LazyLock::new(Instant::now);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// `LlmSession` holds a `LlamaContext`, which is not `Send`, so it can't move into the handler thread.
// It is created on that thread on first use instead.
thread_local! {
    static SESSION: RefCell<Option<LlmSession<'static>>> = const { RefCell::new(None) };
}

enum Report {
    /// Arrival at the speaker, the sentence, and the ms its last piece took from the LLM to here.
    Sentence(Instant, String, f64),
    End,
    FirstPiece,
    LlmDone(Instant),
    Closed,
}

fn tagged(text: &str, kind: &str, stamp: &str) -> CanonicalMessage {
    let mut msg = CanonicalMessage::from(text);
    msg.set_metadata(KIND, kind);
    msg.set_metadata(STAMP, stamp);
    msg
}

fn kind(msg: &CanonicalMessage) -> &str {
    msg.metadata.get(KIND).map_or("", String::as_str)
}

fn stamp() -> String {
    BASE.elapsed().as_nanos().to_string()
}

fn failed(e: impl std::fmt::Display) -> HandlerError {
    HandlerError::NonRetryable(mq_bridge::anyhow::anyhow!("{e}"))
}

type Stage = Box<dyn FnMut(CanonicalMessage, &HandlerContext) -> Result<Handled, HandlerError> + Send>;

fn llm_stage(model: &'static LocalLlm, reports: Sender<Report>) -> Stage {
    let persona = Persona::kitt(LANG, DEFAULT_NAME);
    Box::new(move |msg, ctx| {
        if kind(&msg) == "close" {
            SESSION.take();
            let _ = reports.send(Report::Closed);
            return Ok(Handled::Ack);
        }
        let messages = persona.messages(&msg.get_payload_str());
        SESSION.with_borrow_mut(|slot| {
            let session = match slot {
                Some(s) => s,
                None => {
                    let mut s = model.session().map_err(failed)?;
                    s.set_fixed_messages(persona.fixed_messages());
                    slot.insert(s)
                }
            };
            let stop = AtomicBool::new(false);
            let mut sent = Ok(());
            let mut first = true;
            let replied = session.reply(&messages, &stop, &mut |piece| {
                if ctx.is_cancelled() || sent.is_err() {
                    stop.store(true, Ordering::Relaxed);
                } else {
                    sent = ctx.emit(tagged(piece, "piece", &stamp()));
                    if std::mem::take(&mut first) {
                        let _ = reports.send(Report::FirstPiece);
                    }
                }
            });
            let _ = reports.send(Report::LlmDone(Instant::now()));
            replied.map_err(failed)?;
            sent?;
            ctx.emit(tagged("", "end", &stamp()))?;
            Ok(Handled::Ack)
        })
    })
}

fn sentence_stage() -> Stage {
    let mut sentences = Sentences::new(LANG);
    Box::new(move |msg, ctx| {
        let end = kind(&msg) == "end";
        let out = if end { sentences.finish().into_iter().collect() } else { sentences.push(&msg.get_payload_str()) };
        let t = msg.metadata.get(STAMP).map_or("", String::as_str);
        for s in out {
            ctx.emit(tagged(&s, "sentence", t))?;
        }
        if end {
            ctx.emit(tagged("", "end", t))?;
        }
        Ok(Handled::Ack)
    })
}

/// Where `PocketSpeaker::say` would go; `say` only queues, so the time it is called is `first_sentence_ms`.
fn speak_stage(reports: Sender<Report>) -> Stage {
    Box::new(move |msg, _| {
        let report = if kind(&msg) == "end" {
            Report::End
        } else {
            let made: u128 = msg.metadata.get(STAMP).and_then(|t| t.parse().ok()).unwrap_or(0);
            let hops = (BASE.elapsed().as_nanos() - made) as f64 / 1e6;
            Report::Sentence(Instant::now(), msg.get_payload_str().into_owned(), hops)
        };
        let _ = reports.send(report);
        Ok(Handled::Ack)
    })
}

async fn emitter(topic: &str) -> Arc<dyn MessagePublisher> {
    Publisher::new(Endpoint::new_memory(topic, CAPACITY)).await.unwrap().inner()
}

struct MqPipeline {
    rt: tokio::runtime::Runtime,
    turns: Publisher,
    cancels: Vec<CancelHandle>,
    reports: Receiver<Report>,
}

impl MqPipeline {
    fn start(model: &'static LocalLlm) -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, reports) = channel();
        let (turns, cancels) = rt.block_on(async {
            let llm = ThreadedHandler::with_emitter(llm_stage(model, tx.clone()), emitter("pal.pieces").await);
            let split = ThreadedHandler::with_emitter(sentence_stage(), emitter("pal.sentences").await);
            let speak = ThreadedHandler::new(speak_stage(tx));
            let cancels = vec![llm.cancel_handle(), split.cancel_handle(), speak.cancel_handle()];
            let inputs = ["pal.turns", "pal.pieces", "pal.sentences"];
            for ((name, input), handler) in ROUTES.into_iter().zip(inputs).zip([llm, split, speak]) {
                let route = Route::new(Endpoint::new_memory(input, CAPACITY), Endpoint::null()).with_handler(handler);
                route.deploy(name).await.unwrap();
            }
            (Publisher::new(Endpoint::new_memory("pal.turns", CAPACITY)).await.unwrap(), cancels)
        });
        Self { rt, turns, cancels, reports }
    }

    fn send(&self, mut msg: CanonicalMessage, turn: &str) {
        msg.set_metadata("correlation_id", turn);
        self.rt.block_on(self.turns.send(msg)).unwrap();
    }

    fn next(&self) -> Report {
        self.reports.recv_timeout(Duration::from_secs(60)).expect("pipeline stalled")
    }

    /// Sentences with arrival time and hop time, until the turn's end marker.
    fn reply(&self, line: &str, turn: &str) -> Vec<(Instant, String, f64)> {
        self.send(CanonicalMessage::from(line), turn);
        let mut out = Vec::new();
        loop {
            match self.next() {
                Report::Sentence(t, s, hops) => out.push((t, s, hops)),
                Report::End => return out,
                _ => {}
            }
        }
    }

    /// Drops the LLM session on its thread and stops the routes, so the model can be freed.
    fn close(self) {
        self.send(tagged("", "close", ""), "close");
        while !matches!(self.next(), Report::Closed) {}
        self.rt.block_on(async {
            for name in ROUTES {
                Route::stop(name).await;
            }
        });
    }
}

fn direct_reply(session: &mut LlmSession, persona: &Persona, line: &str) -> Vec<(Instant, String)> {
    let mut sentences = Sentences::new(LANG);
    let mut out = Vec::new();
    session
        .reply(&persona.messages(line), &AtomicBool::new(false), &mut |piece| {
            out.extend(sentences.push(piece).into_iter().map(|s| (Instant::now(), s)));
        })
        .unwrap();
    out.extend(sentences.finish().map(|s| (Instant::now(), s)));
    out
}

fn ms(from: Instant, to: Instant) -> f64 {
    to.duration_since(from).as_secs_f64() * 1000.0
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "needs models/"]
fn mq_pipeline_keeps_first_sentence_latency() {
    let path = root().join("models/Qwen3.5-2B-Q4_K_M.gguf");
    let model: *mut LocalLlm = Box::into_raw(Box::new(LocalLlm::new(&path, LlmConfig::default()).unwrap()));
    // SAFETY: freed at the end, after both sessions are dropped and the routes are stopped.
    let model: &'static LocalLlm = unsafe { &*model };
    let persona = Persona::kitt(LANG, DEFAULT_NAME);
    let mut direct = model.session().unwrap();
    direct.set_fixed_messages(persona.fixed_messages());
    let mq = MqPipeline::start(model);

    let warm_up = Persona::user_line("Hallo.", "");
    direct_reply(&mut direct, &persona, &warm_up);
    mq.reply(&warm_up, "warm-up");

    let (mut first_hops, mut all_hops) = (Vec::new(), Vec::new());
    for (i, q) in QUESTIONS.iter().enumerate() {
        let line = Persona::user_line(q, "");
        let t = Instant::now();
        let a = direct_reply(&mut direct, &persona, &line);
        let da = ms(t, a[0].0);
        let t = Instant::now();
        let b = mq.reply(&line, &format!("turn-{i}"));
        let db = ms(t, b[0].0);
        eprintln!("{q}\n  direct first {da:.1} ms | mq first {db:.1} ms, hops of the first sentence {:.3} ms", b[0].2);
        first_hops.push(b[0].2);
        all_hops.extend(b.iter().map(|s| s.2));
    }
    let max = all_hops.iter().copied().fold(0.0, f64::max);
    eprintln!(
        "\nLLM → speaker over 2 mq hops: first sentence median {:.3} ms; all {} sentences median {:.3} ms, max {max:.3} ms",
        median(first_hops),
        all_hops.len(),
        median(all_hops.clone()),
    );

    mq.send(CanonicalMessage::from(Persona::user_line("Erzähl mir eine sehr lange Geschichte.", "")), "turn-cancel");
    let mut cancelled_at = None;
    let mut after_cancel = 0;
    loop {
        match mq.next() {
            Report::FirstPiece => {
                mq.cancels.iter().for_each(|c| c.cancel("turn-cancel"));
                cancelled_at = Some(Instant::now());
            }
            Report::Sentence(..) => after_cancel += 1,
            Report::LlmDone(t) => {
                let at = cancelled_at.expect("the LLM produced no piece");
                eprintln!("cancel → LLM stopped {:.1} ms", ms(at, t));
                break;
            }
            Report::End => panic!("a cancelled turn must not reach its end marker"),
            Report::Closed => unreachable!(),
        }
    }
    std::thread::sleep(Duration::from_millis(200));
    after_cancel += mq.reports.try_iter().filter(|r| matches!(r, Report::Sentence(..))).count();
    eprintln!("sentences that reached the speaker after cancel: {after_cancel}");

    mq.close();
    drop(direct);
    // SAFETY: nothing borrows the model any more; the LLM handler's session was dropped on close.
    drop(unsafe { Box::from_raw(model as *const LocalLlm as *mut LocalLlm) });
    assert_eq!(after_cancel, 0);
    assert!(max < 20.0, "a hop took {max:.3} ms");
}
