//! The reply path as mq-bridge routes over in-process `memory` endpoints:
//! `pal.llm` (model) → `pal.pieces` (sentence split) → `pal.speech` (gate and speaker).
//! Each stage runs on its own thread (`ThreadedHandler`); every message carries its turn id, and a turn is
//! cancelled by that id in all stages. The gate holds a reply to a pause until it is released or discarded.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use mq_bridge::blocking_handler::{BlockingHandler, CancelHandle, HandlerContext, ThreadedHandler};
use mq_bridge::models::Endpoint;
use mq_bridge::traits::MessagePublisher;
use mq_bridge::{CanonicalMessage, Handled, HandlerError, Publisher, Route};
use serde::{Deserialize, Serialize};

use crate::language::Language;
use crate::llm::{Llm, LlmSession, LocalLlm, Message};
use crate::pocket_speaker::PocketSpeaker;
use crate::remote::RemoteLlm;
use crate::sentences::Sentences;
use crate::{Error, Result};

const KIND: &str = "kind";
const TURN: &str = "turn";
const CORRELATION_ID: &str = "correlation_id";
const CAPACITY: usize = 1024;
const LLM_IN: &str = "pal.llm";
const PIECES: &str = "pal.pieces";
const SPEECH: &str = "pal.speech";
const ROUTES: [&str; 3] = ["pal_llm", "pal_split", "pal_speak"];
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the routes report back to the conversation loop.
#[derive(Debug)]
pub enum Spoken {
    /// A sentence was queued for speech at `at`.
    Said {
        turn: String,
        text: String,
        at: Instant,
    },
    /// Every sentence of the turn is queued; for a held turn, only after its release.
    Done {
        turn: String,
    },
    Failed {
        turn: String,
        error: String,
    },
    /// The model read the messages of a `prefill`.
    Prefilled,
}

/// Where replies come from.
pub enum Brain {
    Local(LocalLlm),
    /// A server; with `fallback`, the local model answers a turn the server fails on before its first word.
    Remote {
        server: RemoteLlm,
        fallback: Option<LocalLlm>,
    },
}

impl From<LocalLlm> for Brain {
    fn from(model: LocalLlm) -> Self {
        Self::Local(model)
    }
}

/// The request body for the model, in the OpenAI chat format.
#[derive(Serialize, Deserialize)]
struct Chat {
    messages: Vec<Message>,
}

// `LlmSession` holds a `LlamaContext`, which is not `Send`, so it can't move into the handler thread. It is
// created on that thread on first use instead.
thread_local! {
    static SESSION: RefCell<Option<LlmSession<'static>>> = const { RefCell::new(None) };
}

fn message(kind: &str, turn: &str, text: &str) -> CanonicalMessage {
    let mut msg = CanonicalMessage::from(text);
    msg.set_metadata(KIND, kind);
    msg.set_metadata(TURN, turn);
    msg
}

/// A message of a turn's data stream; cancelling the turn skips it in every stage.
fn turn_message(kind: &str, turn: &str, text: &str) -> CanonicalMessage {
    let mut msg = message(kind, turn, text);
    msg.set_metadata(CORRELATION_ID, turn);
    msg
}

fn meta<'a>(msg: &'a CanonicalMessage, key: &str) -> &'a str {
    msg.metadata.get(key).map_or("", String::as_str)
}

fn failed(e: impl std::fmt::Display) -> HandlerError {
    HandlerError::NonRetryable(mq_bridge::anyhow::anyhow!("{e}"))
}

fn routes_error(e: impl std::fmt::Display) -> Error {
    Error::Routes(e.to_string())
}

/// Numbers as words, and words the voice reads wrong respelled.
fn pronounced(text: &str, language: Language) -> String {
    let text = crate::numbers::spell(text, language);
    language.profile().pronunciations.iter().fold(text, |t, (word, spoken)| t.replace(word, spoken))
}

/// The LLM is ahead of speech anyway; at normal priority its threads starve speech synthesis on phones with
/// few fast cores and the voice stutters. Threads llama.cpp starts from here inherit this.
fn lower_priority() {
    // Per thread on Linux; on macOS the same call would slow the whole process.
    #[cfg(target_os = "android")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

/// Streams a reply as "piece" messages. Returns the model's result and whether a piece went out.
fn stream(
    llm: &mut dyn Llm,
    chat: &Chat,
    turn: &str,
    ctx: &HandlerContext,
) -> std::result::Result<(Result<String>, bool), HandlerError> {
    let stop = AtomicBool::new(false);
    let (mut sent, mut any) = (Ok(()), false);
    let replied = llm.reply(&chat.messages, &stop, &mut |piece| {
        if ctx.is_cancelled() || sent.is_err() {
            stop.store(true, Ordering::Relaxed);
        } else {
            any = true;
            sent = ctx.emit(turn_message("piece", turn, piece));
        }
    });
    sent?;
    Ok((replied, any))
}

fn finish(replied: Result<String>, turn: &str, ctx: &HandlerContext) -> std::result::Result<Handled, HandlerError> {
    match replied {
        Ok(_) => ctx.emit(turn_message("end", turn, ""))?,
        Err(e) => ctx.emit(turn_message("error", turn, &e.to_string()))?,
    }
    Ok(Handled::Ack)
}

fn llm_stage(
    local: Option<&'static LocalLlm>,
    mut remote: Option<RemoteLlm>,
    fixed: usize,
    report: Report,
    closed: Sender<()>,
) -> impl BlockingHandler {
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let kind = meta(&msg, KIND);
        if kind == "close" {
            SESSION.take();
            let _ = closed.send(());
            return Ok(Handled::Ack);
        }
        let turn = meta(&msg, TURN);
        let chat: Chat = serde_json::from_slice(&msg.payload).map_err(failed)?;
        if let Some(server) = remote.as_mut() {
            // The server keeps no cache; a fallback model reads everything on its first turn instead.
            if kind == "prefill" {
                report(Spoken::Prefilled);
                return Ok(Handled::Ack);
            }
            match stream(server, &chat, turn, ctx)? {
                (Err(e), false) if local.is_some() && !ctx.is_cancelled() => {
                    eprintln!("llm_pal: {e}; the local model answers");
                }
                (replied, _) => return finish(replied, turn, ctx),
            }
        }
        let Some(model) = local else { return finish(Err(Error::Llm("no model".into())), turn, ctx) };
        SESSION.with_borrow_mut(|slot| {
            if slot.is_none() {
                lower_priority();
            }
            let session = match slot {
                Some(s) => s,
                None => match model.session() {
                    Ok(mut s) => {
                        s.set_fixed_messages(fixed);
                        slot.insert(s)
                    }
                    Err(e) => return ctx.emit(turn_message("error", turn, &e.to_string())).map(|_| Handled::Ack),
                },
            };
            if kind == "prefill" {
                if let Err(e) = session.prefill(&chat.messages) {
                    eprintln!("llm_pal: {e}");
                }
                report(Spoken::Prefilled);
                return Ok(Handled::Ack);
            }
            let (replied, _) = stream(session, &chat, turn, ctx)?;
            finish(replied, turn, ctx)
        })
    }
}

fn split_stage(language: Language) -> impl BlockingHandler {
    let mut sentences = Sentences::new(language);
    let mut current = String::new();
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let turn = meta(&msg, TURN);
        // A cancelled turn never sends its end, so its unfinished sentence is dropped here.
        if turn != current {
            sentences = Sentences::new(language);
            current = turn.to_string();
        }
        let out = match meta(&msg, KIND) {
            "piece" => sentences.push(&msg.get_payload_str()),
            "end" => sentences.finish().into_iter().collect(),
            _ => Vec::new(),
        };
        for s in out {
            ctx.emit(turn_message("sentence", turn, &s))?;
        }
        match meta(&msg, KIND) {
            "end" => ctx.emit(turn_message("end", turn, ""))?,
            "error" => ctx.emit(msg)?,
            _ => {}
        }
        Ok(Handled::Ack)
    }
}

/// A reply to a pause, kept until the segment ends.
struct Held {
    turn: String,
    sentences: Vec<String>,
    done: bool,
}

fn speak_stage(speaker: Receiver<PocketSpeaker>, language: Language, report: Report) -> impl BlockingHandler {
    let mut held: Option<Held> = None;
    let mut ready: Option<PocketSpeaker> = None;
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let turn = meta(&msg, TURN);
        if ready.is_none() {
            ready = speaker.recv().ok();
        }
        let Some(speaker) = ready.as_mut() else {
            report(Spoken::Failed { turn: turn.to_string(), error: "the voice did not load".into() });
            return Ok(Handled::Ack);
        };
        let mut say = |text: String| {
            if let Err(e) = speaker.say(&pronounced(&text, language)) {
                eprintln!("llm_pal: {e}");
            }
            report(Spoken::Said { turn: turn.to_string(), text, at: Instant::now() });
        };
        let holding = held.as_ref().is_some_and(|h| h.turn == turn);
        match meta(&msg, KIND) {
            "hold" => held = Some(Held { turn: turn.to_string(), sentences: Vec::new(), done: false }),
            "release" if holding => {
                let h = held.take().expect("holding");
                h.sentences.into_iter().for_each(&mut say);
                if h.done {
                    report(Spoken::Done { turn: h.turn });
                }
            }
            "discard" if holding => held = None,
            "stop" => speaker.stop(),
            "sentence" if ctx.is_cancelled() => {}
            "sentence" if holding => held.as_mut().expect("holding").sentences.push(msg.get_payload_str().into()),
            "sentence" => say(msg.get_payload_str().into()),
            "end" if holding => held.as_mut().expect("holding").done = true,
            "end" => report(Spoken::Done { turn: turn.to_string() }),
            "error" => {
                if holding {
                    held = None;
                }
                report(Spoken::Failed { turn: turn.to_string(), error: msg.get_payload_str().into() });
            }
            _ => {}
        }
        Ok(Handled::Ack)
    }
}

type Report = Arc<dyn Fn(Spoken) + Send + Sync>;

async fn publisher(topic: &str) -> mq_bridge::anyhow::Result<Publisher> {
    Publisher::new(Endpoint::new_memory(topic, CAPACITY)).await
}

async fn emitter(topic: &str) -> mq_bridge::anyhow::Result<Arc<dyn MessagePublisher>> {
    Ok(publisher(topic).await?.inner())
}

/// The running reply routes. Dropping them closes the model session and frees the model.
pub struct ReplyRoutes {
    rt: tokio::runtime::Runtime,
    llm: Publisher,
    pieces: Publisher,
    speech: Publisher,
    cancels: Vec<CancelHandle>,
    closed: Receiver<()>,
    model: Option<*mut LocalLlm>,
}

impl ReplyRoutes {
    /// `fixed` is the number of leading messages that stay the same across turns (`Persona::fixed_messages`).
    /// `speaker` may arrive later, e.g. while the model prefills; the speech stage waits for it on its first message.
    pub fn start(
        brain: impl Into<Brain>,
        fixed: usize,
        speaker: Receiver<PocketSpeaker>,
        language: Language,
        report: impl Fn(Spoken) + Send + Sync + 'static,
    ) -> Result<Self> {
        let report: Report = Arc::new(report);
        let (local, remote) = match brain.into() {
            Brain::Local(model) => (Some(model), None),
            Brain::Remote { server, fallback } => (fallback, Some(server)),
        };
        let model = local.map(|m| Box::into_raw(Box::new(m)));
        // SAFETY: freed in `drop`, only after the session that borrows it was dropped on the handler thread.
        let shared: Option<&'static LocalLlm> = model.map(|m| unsafe { &*m });
        let rt = tokio::runtime::Runtime::new().map_err(routes_error)?;
        let (closed_tx, closed) = channel();
        let started = rt.block_on(async {
            let llm = ThreadedHandler::with_emitter(
                llm_stage(shared, remote, fixed, report.clone(), closed_tx),
                emitter(PIECES).await?,
            );
            let split = ThreadedHandler::with_emitter(split_stage(language), emitter(SPEECH).await?);
            let speak = ThreadedHandler::new(speak_stage(speaker, language, report));
            let cancels = vec![llm.cancel_handle(), split.cancel_handle(), speak.cancel_handle()];
            for ((name, input), handler) in ROUTES.into_iter().zip([LLM_IN, PIECES, SPEECH]).zip([llm, split, speak]) {
                Route::new(Endpoint::new_memory(input, CAPACITY), Endpoint::null())
                    .with_handler(handler)
                    .deploy(name)
                    .await?;
            }
            Ok::<_, mq_bridge::anyhow::Error>((
                publisher(LLM_IN).await?,
                publisher(PIECES).await?,
                publisher(SPEECH).await?,
                cancels,
            ))
        });
        match started {
            Ok((llm, pieces, speech, cancels)) => Ok(Self { rt, llm, pieces, speech, cancels, closed, model }),
            Err(e) => {
                // SAFETY: no session was created, so nothing borrows the model.
                model.into_iter().for_each(|m| drop(unsafe { Box::from_raw(m) }));
                Err(routes_error(e))
            }
        }
    }

    fn send(&self, to: &Publisher, msg: CanonicalMessage) -> Result<()> {
        self.rt.block_on(to.send(msg)).map(|_| ()).map_err(routes_error)
    }

    fn chat(kind: &str, turn: &str, messages: &[Message]) -> Result<CanonicalMessage> {
        let body = serde_json::to_string(&Chat { messages: messages.to_vec() }).map_err(routes_error)?;
        Ok(turn_message(kind, turn, &body))
    }

    /// Reads `messages` into the model's cache; `Spoken::Prefilled` follows.
    pub fn prefill(&self, messages: &[Message]) -> Result<()> {
        self.send(&self.llm, Self::chat("prefill", "prefill", messages)?)
    }

    /// Starts the model's reply. A `held` reply waits in the gate for `release` or `discard`.
    pub fn reply(&self, turn: &str, messages: &[Message], held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send(&self.llm, Self::chat("reply", turn, messages)?)
    }

    /// Speaks a fixed text as the reply, through the same sentence split and gate.
    pub fn canned(&self, turn: &str, text: &str, held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send(&self.pieces, turn_message("piece", turn, text))?;
        self.send(&self.pieces, turn_message("end", turn, ""))
    }

    fn hold(&self, turn: &str, held: bool) -> Result<()> {
        if held { self.send(&self.speech, message("hold", turn, "")) } else { Ok(()) }
    }

    /// Speaks a held reply: what is ready now, the rest as it comes.
    pub fn release(&self, turn: &str) -> Result<()> {
        self.send(&self.speech, message("release", turn, ""))
    }

    /// Drops a held reply and stops its generation.
    pub fn discard(&self, turn: &str) -> Result<()> {
        self.cancel(turn);
        self.send(&self.speech, message("discard", turn, ""))
    }

    /// Stops the turn and cuts off its speech, including a sentence that slipped past the cancel.
    pub fn interrupt(&self, turn: &str) -> Result<()> {
        self.cancel(turn);
        self.send(&self.speech, message("stop", turn, ""))
    }

    /// Stops the turn in every stage: running work ends, waiting messages are skipped.
    pub fn cancel(&self, turn: &str) {
        self.cancels.iter().for_each(|c| c.cancel(turn));
    }
}

impl Drop for ReplyRoutes {
    fn drop(&mut self) {
        let closed = self.send(&self.llm, message("close", "close", "{}")).is_ok()
            && self.closed.recv_timeout(CLOSE_TIMEOUT).is_ok();
        self.rt.block_on(async {
            for name in ROUTES {
                Route::stop(name).await;
            }
        });
        if let (true, Some(model)) = (closed, self.model) {
            // SAFETY: the session that borrowed the model was dropped on close, and the routes are stopped.
            drop(unsafe { Box::from_raw(model) });
        }
    }
}
