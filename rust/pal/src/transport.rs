//! How Pal's parts are connected. `[name]` is an in-process mq-bridge topic, `route` a stage on its own thread,
//! `fn()` a `Transport` method the conversation calls.
//!
//! ```text
//!   microphone                                     Conversation (think::conversation)
//!       │                                              ▲            │
//!       ▼                                        next()│            │ transcribe(pcm)
//!   pal_mic endpoint ─────────── Event ────────────────┤            ▼
//!   (hear::mic, own thread)                            │       caller pal_stt ──► Parakeet (mq-bridge-onnx)
//!                                                      │            │ text
//!                                                      │            ▼
//!                                                      │       back to the conversation
//!                                                      │
//!   prefill() reply() ──► [pal.llm]                    │
//!                            │                         │
//!                            ▼                         │
//!                   route pal_llm (think::brain) ──────┤ Prefilled
//!                   asks the local model thread,       │
//!                   or a server: its tokens come back  │
//!                   on [pal.llm.tokens]                │
//!                            │ tokens                  │
//!                            ▼                         │
//!   canned() ───────────► [pal.pieces]                 │
//!                            │                         │
//!                            ▼                         │
//!                   route pal_split (speak::stages) ───┤ FirstPiece
//!                            │ sentences, end, error   │
//!                            ▼                         │
//!   release() discard() ─► [pal.speech]                │
//!   interrupt()              │                         │
//!                            ▼                         │
//!                   route pal_speak (speak::stages) ───┘ Said, Done, Failed
//!                   holds a reply until release(), then PocketSpeaker speaks it
//! ```
//!
//! - Every reply message carries its turn id. What travels on each topic is one type in [`messages`]:
//!   `[pal.llm]` [`LlmRequest`], `[pal.pieces]` [`Token`], `[pal.speech]` [`ToSpeaker`].
//! - `[pal.pieces]` carries tokens in the format of mq-bridge's `stream_to`, from the server, the local model and
//!   canned replies alike; the last token of a reply says how it ended.
//! - The reports on the right side (`Spoken`) go back over a plain channel, not over mq-bridge; `next()` returns
//!   them together with the microphone's events.
//!
//! Ending a turn:
//!
//! | Method | Generation | Held sentences | Running voice |
//! |---|---|---|---|
//! | `cancel` | stops | – | – |
//! | `discard` | stops | drops | – |
//! | `interrupt` | stops | – | cuts off |

mod messages;

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use mq_bridge::blocking_handler::{BlockingHandler, CancelHandle, ThreadedHandler};
use mq_bridge::endpoints::create_consumer_from_route;
use mq_bridge::models::Endpoint;
use mq_bridge::traits::{MessageConsumer, MessagePublisher};
use mq_bridge::{Caller, CanonicalMessage, HandlerError, Publisher, Route};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

pub use messages::{CORRELATION_ID, End, LlmRequest, Report, Spoken, ToSpeaker, Token, trace_received};

use crate::hear::event::Event;
use crate::hear::mic::{self, MicConfig, MicLink};
use crate::hear::stt;
use crate::speak::stages;
use crate::speak::tts::PocketSpeaker;
use crate::think::brain::{self, Brain, Model, ModelThread, SERVER_CANCEL, Server};
use crate::think::language::Language;
use crate::think::llm::Message;
use crate::{Error, Result};

const CAPACITY: usize = 1024;
/// Topics: chat messages, tokens, sentences.
const LLM_IN: &str = "pal.llm";
const PIECES: &str = "pal.pieces";
const SPEECH: &str = "pal.speech";
/// Routes: model, sentence split, speaker.
const ROUTE_LLM: &str = "pal_llm";
const ROUTE_SPLIT: &str = "pal_split";
const ROUTE_SPEAK: &str = "pal_speak";

/// What reaches the conversation, in arrival order.
pub enum Input {
    Event(Event),
    Spoken(Spoken),
}

/// The running connections. Dropping it stops the routes, the microphone and the model.
pub struct Transport {
    rt: Handle,
    routes: Routes,
    /// Stops a server's reply; `None` without a server.
    server_cancel: Option<Publisher>,
    reports: mpsc::UnboundedReceiver<Spoken>,
    stt: Option<Caller>,
    mic: Option<Box<dyn MessageConsumer>>,
    mic_lost: bool,
    model: Option<ModelThread>,
}

impl Transport {
    /// Starts the reply routes; with a local model, returns once it is loaded. `fixed` is the number of leading
    /// messages that stay the same across turns (`Persona::fixed_messages`).
    pub fn start(
        rt: Handle,
        brain: Brain,
        fixed: usize,
        speaker: Receiver<PocketSpeaker>,
        language: Language,
    ) -> Result<Self> {
        let (local, server) = match brain {
            Brain::Local(path) => (Some(path), None),
            Brain::Remote { server, fallback } => (fallback, Some(server)),
        };
        let (model, model_thread) = match local {
            Some(path) => Model::start(&path, fixed).map(|(m, t)| (Some(m), Some(t)))?,
            None => (None, None),
        };
        let (report_tx, reports) = mpsc::unbounded_channel();
        let report: Report = Arc::new(move |s| drop(report_tx.send(s)));
        let started = rt.block_on(async {
            let (server, server_cancel) = match server {
                Some(config) => (Some(Server::start(config).await?), Some(publisher(SERVER_CANCEL).await?)),
                None => (None, None),
            };
            let routes = deploy_routes(brain::stage(model, server, report.clone()), speaker, language, report).await?;
            Ok::<_, mq_bridge::anyhow::Error>((routes, server_cancel))
        });
        let (routes, server_cancel) = started.map_err(routes_error)?;
        Ok(Self { rt, routes, server_cancel, reports, stt: None, mic: None, mic_lost: false, model: model_thread })
    }

    /// Opens the microphone; its events arrive through `next`.
    pub fn listen(&mut self, stt: Caller, config: &MicConfig, link: Arc<MicLink>) -> Result<()> {
        let endpoint = mic::endpoint(config, link)?;
        let consumer = self.rt.block_on(create_consumer_from_route(mic::NAME, &endpoint));
        self.mic = Some(consumer.map_err(|e| Error::Audio(format!("{e:#}")))?);
        self.stt = Some(stt);
        Ok(())
    }

    /// The next microphone event or stage report within `timeout`.
    pub fn next(&mut self, timeout: Duration) -> Option<Input> {
        let (mic, reports) = (&mut self.mic, &mut self.reports);
        let (input, mic_ended) = self.rt.block_on(async {
            let event = async {
                match mic {
                    Some(mic) => mic.receive().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(s) = reports.recv() => (Some(Input::Spoken(s)), false),
                received = event => match received {
                    Ok(received) => {
                        trace_received("conversation", &received.message);
                        (Event::from_message(&received.message).map(Input::Event), false)
                    }
                    Err(e) => {
                        eprintln!("llm_pal: microphone: {e:#}");
                        (None, true)
                    }
                },
                _ = tokio::time::sleep(timeout) => (None, false),
            }
        });
        if mic_ended {
            self.mic = None;
            self.mic_lost = true;
        }
        input
    }

    /// True once a microphone opened by `listen` delivers no more events.
    pub fn mic_lost(&self) -> bool {
        self.mic_lost
    }

    pub fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        let stt = self.stt.as_ref().ok_or_else(|| Error::Stt("not listening".into()))?;
        self.rt.block_on(stt::transcribe(stt, pcm))
    }

    /// Reads `messages` into the model's cache; `Spoken::Prefilled` follows.
    pub fn prefill(&self, messages: &[Message]) -> Result<()> {
        self.send_llm(LlmRequest::Prefill(messages.to_vec()))
    }

    /// Starts the model's reply. A `held` reply waits in the gate for `release` or `discard`.
    pub fn reply(&self, turn: &str, messages: &[Message], held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send_llm(LlmRequest::Reply { turn: turn.into(), messages: messages.to_vec() })
    }

    /// Speaks a fixed text as the reply, through the same sentence split and gate.
    pub fn canned(&self, turn: &str, text: &str, held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send(&self.routes.pieces, Token::piece(turn, 0, text).to_message())?;
        self.send(&self.routes.pieces, Token::last(turn, 1, End::Complete, "").to_message())
    }

    /// Speaks a held reply: what is ready now, the rest as it comes.
    pub fn release(&self, turn: &str) -> Result<()> {
        self.to_speaker(turn, ToSpeaker::Release)
    }

    /// Drops a held reply and stops its generation.
    pub fn discard(&self, turn: &str) -> Result<()> {
        self.cancel(turn);
        self.to_speaker(turn, ToSpeaker::Discard)
    }

    /// Stops the turn and cuts off its speech, including a sentence that slipped past the cancel.
    pub fn interrupt(&self, turn: &str) -> Result<()> {
        self.cancel(turn);
        self.to_speaker(turn, ToSpeaker::Stop)
    }

    /// Stops the turn in every stage: running work ends, waiting data is skipped. Control messages to the
    /// speaker still arrive.
    pub fn cancel(&self, turn: &str) {
        self.routes.cancels.iter().for_each(|c| c.cancel(turn));
        if let Some(server) = &self.server_cancel {
            let stop = CanonicalMessage::from(turn).with_metadata_kv(CORRELATION_ID, turn);
            if let Err(e) = self.send(server, stop) {
                eprintln!("llm_pal: {e}");
            }
        }
    }

    fn hold(&self, turn: &str, held: bool) -> Result<()> {
        if held { self.to_speaker(turn, ToSpeaker::Hold) } else { Ok(()) }
    }

    fn send_llm(&self, request: LlmRequest) -> Result<()> {
        self.send(&self.routes.llm, request.to_message().map_err(routes_error)?)
    }

    fn to_speaker(&self, turn: &str, what: ToSpeaker) -> Result<()> {
        self.send(&self.routes.speech, what.to_message(turn))
    }

    fn send(&self, to: &Publisher, msg: CanonicalMessage) -> Result<()> {
        self.rt.block_on(to.send(msg)).map(|_| ()).map_err(routes_error)
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.mic = None;
        self.rt.block_on(async {
            Route::stop(ROUTE_LLM).await;
            Route::stop(ROUTE_SPLIT).await;
            Route::stop(ROUTE_SPEAK).await;
            if let Some(stt) = self.stt.take() {
                stt.stop().await;
            }
        });
        self.model = None;
    }
}

/// The deployed reply routes: where to send, and how to cancel a turn in them.
struct Routes {
    llm: Publisher,
    pieces: Publisher,
    speech: Publisher,
    cancels: Vec<CancelHandle>,
}

/// Each route reads one topic and runs its handler on a thread of its own. A handler may send several messages
/// per input (tokens → sentences), so it writes to the next topic itself (`with_emitter`); the route's own output
/// stays unused (`null`).
async fn deploy_routes(
    llm_stage: impl BlockingHandler,
    speaker: Receiver<PocketSpeaker>,
    language: Language,
    report: Report,
) -> mq_bridge::anyhow::Result<Routes> {
    // 1. pal.llm (chat messages) → local model or server → pal.pieces (tokens)
    let llm = ThreadedHandler::with_emitter(llm_stage, emitter(PIECES).await?);
    let llm_cancel = llm.cancel_handle();
    Route::new(Endpoint::new_memory(LLM_IN, CAPACITY), Endpoint::null()).with_handler(llm).deploy(ROUTE_LLM).await?;

    // 2. pal.pieces (tokens) → sentence split → pal.speech (sentences)
    let split = ThreadedHandler::with_emitter(stages::split(language, report.clone()), emitter(SPEECH).await?);
    let split_cancel = split.cancel_handle();
    Route::new(Endpoint::new_memory(PIECES, CAPACITY), Endpoint::null())
        .with_handler(split)
        .deploy(ROUTE_SPLIT)
        .await?;

    // 3. pal.speech (sentences, hold/release/stop) → Pocket TTS; the end reports back as `Spoken`
    let speak = ThreadedHandler::new(stages::speak(speaker, language, report));
    let speak_cancel = speak.cancel_handle();
    Route::new(Endpoint::new_memory(SPEECH, CAPACITY), Endpoint::null())
        .with_handler(speak)
        .deploy(ROUTE_SPEAK)
        .await?;

    Ok(Routes {
        llm: publisher(LLM_IN).await?,
        pieces: publisher(PIECES).await?,
        speech: publisher(SPEECH).await?,
        cancels: vec![llm_cancel, split_cancel, speak_cancel],
    })
}

/// Deploys the STT caller on `rt` and warms it up: the first transcription is slow.
pub fn start_stt(rt: &Handle, model: &Path) -> Result<Caller> {
    rt.block_on(async {
        let caller = stt::caller(model).await?;
        stt::transcribe(&caller, &[0.0; 16_000]).await?;
        Ok(caller)
    })
}

pub fn failed(e: impl std::fmt::Display) -> HandlerError {
    HandlerError::NonRetryable(mq_bridge::anyhow::anyhow!("{e}"))
}

fn routes_error(e: impl std::fmt::Display) -> Error {
    Error::Routes(format!("{e:#}"))
}

async fn publisher(topic: &str) -> mq_bridge::anyhow::Result<Publisher> {
    Publisher::new(Endpoint::new_memory(topic, CAPACITY)).await
}

async fn emitter(topic: &str) -> mq_bridge::anyhow::Result<Arc<dyn MessagePublisher>> {
    Ok(publisher(topic).await?.inner())
}
