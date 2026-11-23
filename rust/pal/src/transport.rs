//! How Pal's parts are connected; the only place that draws it, the module docs show their insides only.
//! `[name]` is an in-process mq-bridge topic, `route` a handler on its own thread, `fn()` a `Transport` method
//! the conversation calls.
//!
//! ```text
//!   pal_mic endpoint (hear::mic) ── route pal_hear ──┐
//!   static + limiter, 50/s ──────── route pal_tick ──┼──► [pal.inbox] ──► route pal_conversation
//!   reports of the routes below (Report) ────────────┤      Input         Conversation::handle, the main loop
//!   a typed line (Agent::type_line) ─────────────────┘
//!                                                                              │ transcribe(pcm)
//!                                                                              ├──► caller pal_stt ──► Parakeet
//!   prefill() read_ahead() reply() ──► [pal.llm]                               │
//!                                          ▼                                   │
//!                   route pal_llm (think::brain) ─ Prefilled, Failed ─► inbox  │
//!                   the local model thread, or a server's tokens               │
//!                   on [pal.llm.tokens]                                        │
//!                            │ tokens                                          │
//!   canned() ────────────────┤                                                 │
//!   release() discard()      ▼                                                 │
//!   interrupt() pause() ─► [pal.speech]                                        │
//!   resume()                 ▼                                                 │
//!                   route pal_speak (speak::handlers) ─── FirstPiece, Said, Done, Failed ──► inbox
//!                   tokens → sentences, holds a reply until release(), then PocketSpeaker speaks it
//!                   its playback thread ─────────────── Audible, Quiet ──────► inbox
//! ```
//!
//! - [`deploy`] starts all of it, route by route: input, the reply routes, then the conversation. The code to read
//!   next to the diagram.
//! - Every reply message carries its turn id. What travels on each topic is one type in [`messages`]:
//!   `[pal.inbox]` [`Input`], `[pal.llm]` [`LlmRequest`], `[pal.speech]` [`ToSpeaker`].
//! - `[pal.speech]` carries control messages and, in one order with them, tokens in the format of mq-bridge's
//!   `stream_to`, from the server, the local model and canned replies alike; a reply's last token says how it ended.
//! - A [`Report`] publishes to `[pal.inbox]` from inside a handler; a failed send is only logged.
//!
//! Ending a turn:
//!
//! | Method | Generation | Held sentences | Running voice |
//! |---|---|---|---|
//! | `cancel` | stops | – | – |
//! | `discard` | stops | drops | – |
//! | `interrupt` | stops | – | cuts off |

mod messages;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use mq_bridge::blocking_handler::{BlockingHandler, CancelHandle, HandlerContext, ThreadedHandler};
use mq_bridge::get_or_create_channel;
use mq_bridge::models::{Endpoint, EndpointType, LimiterMiddleware, MemoryConfig, Middleware, StaticConfig};
use mq_bridge::{Caller, CanonicalMessage, Handled, HandlerError, Publisher, Route, Topology};
use tokio::runtime::Handle;

pub use messages::{CORRELATION_ID, End, Input, LlmRequest, Report, Spoken, ToSpeaker, Token, trace_received};

use crate::hear::stt;
use crate::speak::handlers;
use crate::speak::tts::PocketSpeaker;
use crate::think::brain::{self, Model, ModelThread, SERVER_CANCEL, SERVER_TOKENS, Server};
use crate::think::language::Language;
use crate::think::local_llm::Message;
use crate::think::remote_llm::RemoteConfig;
use crate::{Error, Result};

const CAPACITY: usize = 1024;
/// Topics: the conversation's inputs, chat messages, tokens and speaker control.
const INBOX: &str = "pal.inbox";
pub(crate) const LLM_IN: &str = "pal.llm";
pub(crate) const SPEECH: &str = "pal.speech";
/// Routes: microphone, ticker, conversation; model, speaker.
const ROUTE_HEAR: &str = "pal_hear";
const ROUTE_TICK: &str = "pal_tick";
const ROUTE_CONVERSATION: &str = "pal_conversation";
/// The ticker's rate: a tick about every 20 ms.
const TICKS_PER_S: f64 = 50.0;
const ROUTE_LLM: &str = "pal_llm";
const ROUTE_SPEAK: &str = "pal_speak";

/// What Pal is made of, loaded; [`deploy`] connects it.
pub struct Parts {
    pub mic: Endpoint,
    pub stt: Caller,
    /// The local model; `None` with a server and no fallback.
    pub model: Option<(Model, ModelThread)>,
    pub server: Option<RemoteConfig>,
    pub speaker: PocketSpeaker,
    pub language: Language,
}

/// Pal as a whole: every route, in the order the data flows (the diagram above). `conversation` gets the
/// [`Transport`], its way into the reply routes, and returns the handler of `[pal.inbox]`. The reply routes stop
/// when the `Transport` is dropped, the others with [`stop_inbox`].
pub fn deploy<H: BlockingHandler>(
    rt: &Handle,
    parts: Parts,
    conversation: impl FnOnce(Transport) -> Result<H>,
) -> Result<()> {
    clear_topics();
    let Parts { mic, stt, model, server, speaker, language } = parts;
    let (model, model_thread) = model.unzip();

    // The reply side's handlers come first: the conversation needs their cancel handles. Each reports to
    // [pal.inbox]: pal_llm `Prefilled`, `Failed`, pal_speak `FirstPiece`, `Said`, `Done`, `Failed`, the voice's
    // playback `Audible`, `Quiet`.
    let reply = rt.block_on(async {
        let to_inbox = publisher(INBOX).await?;
        let report: Report = Arc::new(move |s: Spoken| {
            if let Err(e) = to_inbox.blocking_send(s.to_message()) {
                eprintln!("llm_pal: report: {e:#}");
            }
        });
        let (server, server_cancel) = match server {
            Some(config) => (Some(Server::start(config).await?), Some(publisher(SERVER_CANCEL).await?)),
            None => (None, None),
        };
        let speech = publisher(SPEECH).await?;
        let llm_handler =
            ThreadedHandler::builder(failure_ends_turn(brain::handler(model, server, report.clone()), report.clone()))
                .emit_to("tokens", speech.clone())
                .build();
        speaker.report_to(report.clone());
        let speak_handler =
            ThreadedHandler::new(failure_ends_turn(handlers::speak(speaker, language, report.clone()), report));
        let routes = Routes {
            llm: publisher(LLM_IN).await?,
            speech,
            cancels: vec![llm_handler.cancel_handle(), speak_handler.cancel_handle()],
        };
        Ok::<_, mq_bridge::anyhow::Error>((routes, server_cancel, llm_handler, speak_handler))
    });
    let (routes, server_cancel, llm_handler, speak_handler) = reply.map_err(routes_error)?;
    let transport = Transport { rt: rt.clone(), routes, server_cancel, stt: Some(stt), model: model_thread };
    let conversation_handler = ThreadedHandler::new(conversation(transport)?);

    let topic = |name| Endpoint::new_memory(name, CAPACITY);
    // Input starts first, so nothing is lost. The conversation starts last: if it ends at once, it stops the reply
    // routes, so they must be deployed. A handler may send several messages per input, so it sends them itself.
    let topology = Topology::new("pal")
        .route(ROUTE_HEAR, Route::new(mic, inbox()))
        .route(ROUTE_TICK, Route::new(static_ticker(), inbox()).with_batch_size(1))
        .route(ROUTE_LLM, Route::handled_by(topic(LLM_IN), llm_handler))
        .route(ROUTE_SPEAK, Route::handled_by(topic(SPEECH), speak_handler))
        .route(ROUTE_CONVERSATION, Route::handled_by(inbox(), conversation_handler))
        .stop_order([ROUTE_HEAR, ROUTE_TICK, ROUTE_CONVERSATION, ROUTE_LLM, ROUTE_SPEAK]);
    rt.block_on(topology.deploy()).map(|_| ()).map_err(|e| {
        let source = format!("{:#}", e.source);
        if e.route == ROUTE_HEAR { Error::Audio(source) } else { Error::Routes(source) }
    })
}

/// Reports a handler's error or panic as `Spoken::Failed`: the route would only drop the message, and the
/// conversation would wait for the turn's end forever.
fn failure_ends_turn(mut handler: impl BlockingHandler, report: Report) -> impl BlockingHandler {
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let turn = messages::turn_of(&msg);
        let handled = catch_unwind(AssertUnwindSafe(|| handler.handle(msg, ctx)))
            .unwrap_or_else(|_| Err(failed("the handler panicked")));
        Ok(handled.unwrap_or_else(|e| {
            eprintln!("llm_pal: {e}");
            report(Spoken::Failed { turn, error: e.to_string() });
            Handled::Ack
        }))
    }
}

/// A tick on `[pal.inbox]` about every 20 ms; one per batch, as a static source fills the whole batch at once.
fn static_ticker() -> Endpoint {
    let mut ticker = Endpoint::from(EndpointType::Static(StaticConfig {
        metadata: [(messages::TICK.0.to_string(), messages::TICK.1.to_string())].into(),
        ..StaticConfig::default()
    }));
    ticker.middlewares.push(Middleware::Limiter(LimiterMiddleware { messages_per_second: TICKS_PER_S }));
    ticker
}

/// The conversation's way into the reply routes. Dropping it stops them, speech recognition and the model.
pub struct Transport {
    rt: Handle,
    routes: Routes,
    /// Stops a server's reply; `None` without a server.
    server_cancel: Option<Publisher>,
    stt: Option<Caller>,
    model: Option<ModelThread>,
}

impl Transport {
    pub fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        let stt = self.stt.as_ref().ok_or_else(|| Error::Stt("not loaded".into()))?;
        self.rt.block_on(stt::transcribe(stt, pcm))
    }

    /// Reads `messages` into the model's cache; `Spoken::Prefilled` follows.
    pub fn prefill(&self, messages: &[Message]) -> Result<()> {
        self.send_llm(LlmRequest::Prefill { messages: messages.to_vec(), mark: None })
    }

    /// Reads `messages` with the last one up to byte `mark` into the local model's cache, while the user goes on
    /// saying it; `Spoken::Prefilled` follows.
    pub fn read_ahead(&self, messages: &[Message], mark: usize) -> Result<()> {
        self.send_llm(LlmRequest::Prefill { messages: messages.to_vec(), mark: Some(mark) })
    }

    /// Starts the model's reply. A `held` reply waits in the gate for `release` or `discard`.
    pub fn reply(&self, turn: &str, messages: &[Message], held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send_llm(LlmRequest::Reply { turn: turn.into(), messages: messages.to_vec() })
    }

    /// Speaks a fixed text as the reply, through the same sentence split and gate.
    pub fn canned(&self, turn: &str, text: &str, held: bool) -> Result<()> {
        self.hold(turn, held)?;
        self.send(&self.routes.speech, Token::piece(turn, 0, text).to_message())?;
        self.send(&self.routes.speech, Token::last(turn, 1, End::Complete, "").to_message())
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

    /// Holds Pal's voice where it is, until `resume`.
    pub fn pause(&self) -> Result<()> {
        self.to_speaker("", ToSpeaker::Pause)
    }

    pub fn resume(&self) -> Result<()> {
        self.to_speaker("", ToSpeaker::Resume)
    }

    /// Stops the turn in every route: running work ends, waiting data is skipped. Control messages to the
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

#[cfg(test)]
impl Transport {
    /// The topics without their routes, for tests that read what the conversation sends or run routes of their own
    /// (`cancels` are theirs).
    pub(crate) fn without_routes(rt: Handle, stt: Caller, cancels: Vec<CancelHandle>) -> Self {
        let routes = rt.block_on(async {
            let (llm, speech) = (publisher(LLM_IN).await?, publisher(SPEECH).await?);
            Ok::<_, mq_bridge::anyhow::Error>(Routes { llm, speech, cancels })
        });
        Self { rt, routes: routes.expect("memory topics"), server_cancel: None, stt: Some(stt), model: None }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.rt.block_on(async {
            stop_routes().await;
            if let Some(stt) = self.stt.take() {
                stt.stop().await;
            }
        });
        self.model = None;
    }
}

/// Stops the reply routes; one that isn't deployed is skipped.
async fn stop_routes() {
    Route::stop(ROUTE_LLM).await;
    Route::stop(ROUTE_SPEAK).await;
}

/// Stops the input side; once the conversation has ended, as its route waits for the running input.
pub fn stop_inbox(rt: &Handle) {
    rt.block_on(async {
        Route::stop(ROUTE_HEAR).await;
        Route::stop(ROUTE_TICK).await;
        Route::stop(ROUTE_CONVERSATION).await;
    });
}

/// The topics live as long as the process: what a stopped agent left in them must not reach the next one.
pub(crate) fn clear_topics() {
    for topic in [INBOX, LLM_IN, SPEECH, SERVER_TOKENS, SERVER_CANCEL] {
        get_or_create_channel(&MemoryConfig::new(topic, Some(CAPACITY))).drain_messages();
    }
}

/// A way into `[pal.inbox]` from outside the routes.
pub fn inbox_publisher(rt: &Handle) -> Result<Publisher> {
    rt.block_on(publisher(INBOX)).map_err(routes_error)
}

/// `[pal.inbox]`: everything the conversation reacts to, as `Input`.
pub fn inbox() -> Endpoint {
    Endpoint::new_memory(INBOX, CAPACITY)
}

/// The deployed reply routes: where to send, and how to cancel a turn in them.
struct Routes {
    llm: Publisher,
    speech: Publisher,
    cancels: Vec<CancelHandle>,
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

pub(crate) async fn publisher(topic: &str) -> mq_bridge::anyhow::Result<Publisher> {
    Publisher::new(Endpoint::new_memory(topic, CAPACITY)).await
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mq_bridge::traits::Handler;

    use super::*;

    fn failures_of(handler: impl BlockingHandler, msg: CanonicalMessage) -> Vec<String> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let report: Report = Arc::new(move |s| {
            if let Spoken::Failed { turn, .. } = s {
                log.lock().unwrap().push(turn);
            }
        });
        let handler = ThreadedHandler::new(failure_ends_turn(handler, report));
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(matches!(rt.block_on(handler.handle(msg)), Ok(Handled::Ack)));
        seen.lock().unwrap().clone()
    }

    #[test]
    fn a_handler_error_or_panic_fails_its_turn() {
        let token = || Token::piece("turn-1", 0, "x").to_message();
        let error = |_: CanonicalMessage, _: &HandlerContext| Err(failed("gone"));
        let panic =
            |_: CanonicalMessage, _: &HandlerContext| -> std::result::Result<Handled, HandlerError> { panic!("boom") };
        assert_eq!(failures_of(error, token()), ["turn-1"]);
        assert_eq!(failures_of(panic, token()), ["turn-1"]);
        assert_eq!(failures_of(error, ToSpeaker::Release.to_message("turn-2")), ["turn-2"]);
    }

    #[test]
    fn a_handler_that_succeeds_reports_nothing() {
        let fine = |_: CanonicalMessage, _: &HandlerContext| Ok(Handled::Ack);
        assert!(failures_of(fine, Token::piece("turn-1", 0, "x").to_message()).is_empty());
    }
}
