//! How Pal's parts are connected; the only place that draws it, the module docs show their insides only.
//! `[name]` is an in-process mq-bridge topic, `route` a handler on its own thread, `fn()` a `Transport` method
//! the conversation calls.
//!
//! ```text
//!   pal_mic endpoint (hear::mic) ── route pal_hear ──┐
//!   static + limiter, 50/s ──────── route pal_tick ──┼──► [pal.inbox] ──► route pal_conversation
//!   reports of the routes below (Report) ────────────┘      Input         Conversation::handle, the main loop
//!                                                                              │ transcribe(pcm)
//!                                                                              ├──► caller pal_stt ──► Parakeet
//!   prefill() read_ahead() reply() ──► [pal.llm]                               │
//!                                          ▼                                   │
//!                   route pal_llm (think::brain) ─── Prefilled ──► inbox       │
//!                   the local model thread, or a server's tokens               │
//!                   on [pal.llm.tokens]                                        │
//!   canned() ───────────► [pal.pieces]                                         │
//!                            ▼                                                 │
//!                   route pal_split (speak::handlers) ─── FirstPiece ──► inbox │
//!   release() discard() ─► [pal.speech]                                        │
//!   interrupt() pause()      ▼                                                 │
//!   resume()        route pal_speak (speak::handlers) ─── Said, Done, Failed ──► inbox
//!                   holds a reply until release(), then PocketSpeaker speaks it
//!                   its playback thread ─────────────── Audible, Quiet ──────► inbox
//! ```
//!
//! - [`deploy`] starts all of it, route by route: input, the reply routes, then the conversation. The code to read
//!   next to the diagram.
//! - Every reply message carries its turn id. What travels on each topic is one type in [`messages`]:
//!   `[pal.inbox]` [`Input`], `[pal.llm]` [`LlmRequest`], `[pal.pieces]` [`Token`], `[pal.speech]` [`ToSpeaker`].
//! - `[pal.pieces]` carries tokens in the format of mq-bridge's `stream_to`, from the server, the local model and
//!   canned replies alike; the last token of a reply says how it ended.
//! - A [`Report`] publishes to `[pal.inbox]` from inside a handler, as mq-bridge 1.6 has no `emit_to` (AGENTS.md, upstream candidates).
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

use mq_bridge::blocking_handler::{BlockingHandler, CancelHandle, ThreadedHandler};
use mq_bridge::endpoints::create_consumer_from_route;
use mq_bridge::get_or_create_channel;
use mq_bridge::models::{Endpoint, EndpointType, LimiterMiddleware, MemoryConfig, Middleware, StaticConfig};
use mq_bridge::traits::{MessageConsumer, MessageDisposition, MessagePublisher};
use mq_bridge::{Caller, CanonicalMessage, HandlerError, Publisher, Route};
use tokio::runtime::Handle;

pub use messages::{CORRELATION_ID, End, Input, LlmRequest, Report, Spoken, ToSpeaker, Token, trace_received};

use crate::hear::stt;
use crate::speak::handlers;
use crate::speak::tts::PocketSpeaker;
use crate::think::brain::{self, Model, ModelThread, SERVER_CANCEL, Server};
use crate::think::language::Language;
use crate::think::local_llm::Message;
use crate::think::remote_llm::RemoteConfig;
use crate::{Error, Result};

const CAPACITY: usize = 1024;
/// Topics: the conversation's inputs, chat messages, tokens, sentences.
const INBOX: &str = "pal.inbox";
pub(crate) const LLM_IN: &str = "pal.llm";
pub(crate) const PIECES: &str = "pal.pieces";
pub(crate) const SPEECH: &str = "pal.speech";
/// Routes: microphone, ticker, conversation; model, sentence split, speaker.
const ROUTE_HEAR: &str = "pal_hear";
const ROUTE_TICK: &str = "pal_tick";
const ROUTE_CONVERSATION: &str = "pal_conversation";
/// The ticker's rate: a tick about every 20 ms.
const TICKS_PER_S: f64 = 50.0;
const ROUTE_LLM: &str = "pal_llm";
const ROUTE_SPLIT: &str = "pal_split";
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
    // [pal.inbox]: pal_llm `Prefilled`, pal_split `FirstPiece`, pal_speak `Said`, `Done`, `Failed`, the voice's
    // playback `Audible`, `Quiet`.
    let reply = rt.block_on(async {
        let inbox = publisher(INBOX).await?;
        let to_inbox = rt.clone();
        let report: Report = Arc::new(move |s: Spoken| {
            if let Err(e) = to_inbox.block_on(inbox.send(s.to_message())) {
                eprintln!("llm_pal: report: {e:#}");
            }
        });
        let (server, server_cancel) = match server {
            Some(config) => (Some(Server::start(config).await?), Some(publisher(SERVER_CANCEL).await?)),
            None => (None, None),
        };
        let llm = ThreadedHandler::with_emitter(brain::handler(model, server, report.clone()), emitter(PIECES).await?);
        let split = ThreadedHandler::with_emitter(handlers::split(language, report.clone()), emitter(SPEECH).await?);
        speaker.report_to(report.clone());
        let speak = ThreadedHandler::new(handlers::speak(speaker, language, report));
        let routes = Routes {
            llm: publisher(LLM_IN).await?,
            pieces: publisher(PIECES).await?,
            speech: publisher(SPEECH).await?,
            cancels: vec![llm.cancel_handle(), split.cancel_handle(), speak.cancel_handle()],
        };
        Ok::<_, mq_bridge::anyhow::Error>((routes, server_cancel, [llm, split, speak]))
    });
    let (routes, server_cancel, [llm, split, speak]) = reply.map_err(routes_error)?;
    let transport = Transport { rt: rt.clone(), routes, server_cancel, stt: Some(stt), model: model_thread };
    let conversation = ThreadedHandler::new(conversation(transport)?);

    let topic = |name| Endpoint::new_memory(name, CAPACITY);
    let deployed = rt.block_on(async {
        // 1. Input: the microphone and a ticker write to [pal.inbox]; they start first, so nothing is lost.
        let hear = Route::new(mic, inbox()).deploy(ROUTE_HEAR).await;
        hear.map_err(|e| Error::Audio(format!("{e:#}")))?;
        Route::new(static_ticker(), inbox()).with_batch_size(1).deploy(ROUTE_TICK).await.map_err(routes_error)?;
        // 2. Reply: [pal.llm] → model or server → [pal.pieces] → sentences → [pal.speech] → gate and voice.
        // A handler may send several messages per input, so it writes to the next topic itself.
        Route::new(topic(LLM_IN), Endpoint::null()).with_handler(llm).deploy(ROUTE_LLM).await.map_err(routes_error)?;
        Route::new(topic(PIECES), Endpoint::null())
            .with_handler(split)
            .deploy(ROUTE_SPLIT)
            .await
            .map_err(routes_error)?;
        Route::new(topic(SPEECH), Endpoint::null())
            .with_handler(speak)
            .deploy(ROUTE_SPEAK)
            .await
            .map_err(routes_error)?;
        // 3. Control, last: a conversation that ends at once stops the reply routes, so they must be deployed.
        let control = Route::new(inbox(), Endpoint::null()).with_handler(conversation);
        control.deploy(ROUTE_CONVERSATION).await.map_err(routes_error)
    });
    if deployed.is_err() {
        stop_inbox(rt);
        rt.block_on(stop_routes());
    }
    deployed.map(|_| ())
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

    /// Holds Pal's voice where it is, until `resume`.
    pub fn pause(&self) -> Result<()> {
        self.to_speaker("", ToSpeaker::Pause)
    }

    pub fn resume(&self) -> Result<()> {
        self.to_speaker("", ToSpeaker::Resume)
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

#[cfg(test)]
impl Transport {
    /// The topics without their routes, for tests that read what the conversation sends or run routes of their own
    /// (`cancels` are theirs).
    pub(crate) fn without_routes(rt: Handle, stt: Caller, cancels: Vec<CancelHandle>) -> Self {
        let routes = rt.block_on(async {
            let (llm, pieces, speech) = (publisher(LLM_IN).await?, publisher(PIECES).await?, publisher(SPEECH).await?);
            Ok::<_, mq_bridge::anyhow::Error>(Routes { llm, pieces, speech, cancels })
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
    Route::stop(ROUTE_SPLIT).await;
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
    for topic in [INBOX, LLM_IN, PIECES, SPEECH] {
        get_or_create_channel(&MemoryConfig::new(topic, Some(CAPACITY))).drain_messages();
    }
}

/// `[pal.inbox]`: everything the conversation reacts to, as `Input`.
pub fn inbox() -> Endpoint {
    Endpoint::new_memory(INBOX, CAPACITY)
}

/// The deployed reply routes: where to send, and how to cancel a turn in them.
struct Routes {
    llm: Publisher,
    pieces: Publisher,
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

/// A request whose reply streams back on another topic (`stream_to`), the way a `Caller` waits for one answer.
pub struct StreamCall {
    name: &'static str,
    requests: Publisher,
    replies: Box<dyn MessageConsumer>,
    runtime: Handle,
}

impl StreamCall {
    /// `endpoint` answers requests and streams their tokens to `replies`.
    pub async fn new(name: &'static str, endpoint: Endpoint, replies: &Endpoint) -> mq_bridge::anyhow::Result<Self> {
        Ok(Self {
            name,
            requests: Publisher::new(endpoint).await?,
            replies: create_consumer_from_route(name, replies).await?,
            runtime: Handle::current(),
        })
    }

    /// Sends `request` as `turn` and passes each token of its reply to `piece`; returns the last token, which says
    /// how the reply ended. A request that dies before its last token ends as `End::Failed`.
    pub fn call(
        &mut self,
        turn: &str,
        request: CanonicalMessage,
        mut piece: impl FnMut(CanonicalMessage) -> std::result::Result<(), HandlerError>,
    ) -> std::result::Result<Token, HandlerError> {
        let request = request.with_metadata_kv(CORRELATION_ID, turn);
        let requests = self.requests.clone();
        // The answer is the whole text again; the tokens already carry it. It only tells of a request that died.
        let mut answer = self.runtime.spawn(async move { requests.send(request).await });
        let mut index = 0;
        loop {
            let received = self.runtime.block_on(async {
                tokio::select! {
                    biased;
                    received = self.replies.receive() => received.map(Ok).map_err(failed),
                    answered = &mut answer => Ok(Err(match answered {
                        Ok(Err(e)) => format!("{e:#}"),
                        Err(e) => e.to_string(),
                        Ok(Ok(_)) => "the request ended without its last token".into(),
                    })),
                }
            })?;
            let received = match received {
                Ok(received) => received,
                Err(error) => return Ok(Token::last(turn, index, End::Failed, &error)),
            };
            let _ = self.runtime.block_on((received.commit)(MessageDisposition::Ack));
            trace_received(self.name, &received.message);
            let token = Token::from_message(&received.message);
            // The rest of an earlier reply whose forwarding failed.
            if token.turn != turn {
                continue;
            }
            if token.end.is_some() {
                return Ok(token);
            }
            piece(received.message)?;
            index += 1;
        }
    }
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

pub(crate) async fn emitter(topic: &str) -> mq_bridge::anyhow::Result<Arc<dyn MessagePublisher>> {
    Ok(publisher(topic).await?.inner())
}
