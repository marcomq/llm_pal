//! Where replies come from: the local model on its own thread, or a server behind mq-bridge with the local model
//! as an optional fallback. `handler` serves the transport's `pal_llm` route.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use mq_bridge::blocking_handler::{BlockingHandler, HandlerContext};
use mq_bridge::models::Endpoint;
use mq_bridge::{CanonicalMessage, Handled, HandlerError};

use crate::think::local_llm::{LlmConfig, LocalLlm, Message};
use crate::think::remote_llm::RemoteConfig;
use crate::transport::{End, LlmRequest, Report, Spoken, StreamCall, Token, failed, trace_received};
use crate::{Error, Result};

/// How often a running reply checks whether its turn was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(50);
const CAPACITY: usize = 1024;
/// The server's tokens, read by the model stage before they go on to `pal.pieces`.
pub const SERVER_TOKENS: &str = "pal.llm.tokens";
pub const SERVER_CANCEL: &str = "pal.llm.cancel";

enum Job {
    Close,
    Prefill(Vec<Message>, Sender<()>),
    Reply { messages: Vec<Message>, stop: Arc<AtomicBool>, pieces: Sender<Piece> },
}

enum Piece {
    Text(String),
    Done(Result<String>),
}

/// Sends work to the local model's thread.
#[derive(Clone)]
pub struct Model {
    jobs: Sender<Job>,
}

/// The local model and its session live on one thread: the session borrows the model and isn't `Send`.
/// Dropping this ends the thread and waits for it, so the model's memory is free before a new one loads.
pub struct ModelThread {
    jobs: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for ModelThread {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Close);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Model {
    /// Returns once the model is loaded. `fixed` is the number of leading messages that stay the same across
    /// turns (`Persona::fixed_messages`).
    pub fn start(path: &Path, fixed: usize) -> Result<(Self, ModelThread)> {
        let (jobs, rx) = channel::<Job>();
        let (ready_tx, ready) = channel();
        let path = path.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("pal-llm".into())
            .spawn(move || {
                let llm = match LocalLlm::new(&path, LlmConfig::default()) {
                    Ok(llm) => llm,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                // The LLM is ahead of speech anyway; at normal priority it starves speech synthesis on phones.
                crate::platform::background_thread();
                let mut session = match llm.session() {
                    Ok(session) => session,
                    Err(e) => return drop(ready_tx.send(Err(e))),
                };
                session.set_fixed_messages(fixed);
                let _ = ready_tx.send(Ok(()));
                for job in rx {
                    match job {
                        Job::Close => break,
                        Job::Prefill(messages, done) => {
                            if let Err(e) = session.prefill(&messages) {
                                eprintln!("llm_pal: {e}");
                            }
                            let _ = done.send(());
                        }
                        Job::Reply { messages, stop, pieces } => {
                            let reply = session.reply(&messages, &stop, &mut |piece| {
                                let _ = pieces.send(Piece::Text(piece.to_string()));
                            });
                            let _ = pieces.send(Piece::Done(reply));
                        }
                    }
                }
            })
            .map_err(|e| Error::Llm(e.to_string()))?;
        let thread = ModelThread { jobs: jobs.clone(), thread: Some(thread) };
        ready.recv().map_err(|_| Error::Llm("the model thread died".into()))??;
        Ok((Self { jobs }, thread))
    }

    fn send(&self, job: Job) -> std::result::Result<(), HandlerError> {
        self.jobs.send(job).map_err(|_| failed("the model thread ended"))
    }

    /// Returns once the model has read `messages`; a later reply starting with them skips that part.
    pub(crate) fn prefill(&self, messages: Vec<Message>) -> std::result::Result<(), HandlerError> {
        let (done, wait) = channel();
        self.send(Job::Prefill(messages, done))?;
        let _ = wait.recv();
        Ok(())
    }

    /// Streams the reply as tokens; a route cancel stops it within `CANCEL_POLL`.
    fn reply(
        &self,
        turn: &str,
        messages: Vec<Message>,
        ctx: &HandlerContext,
    ) -> std::result::Result<Handled, HandlerError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, pieces) = channel();
        self.send(Job::Reply { messages, stop: stop.clone(), pieces: tx })?;
        let (mut sent, mut index) = (Ok(()), 0);
        let replied = loop {
            match pieces.recv_timeout(CANCEL_POLL) {
                Ok(Piece::Text(_)) | Err(RecvTimeoutError::Timeout) if ctx.is_cancelled() || sent.is_err() => {
                    stop.store(true, Ordering::Relaxed);
                }
                Ok(Piece::Text(piece)) => {
                    sent = ctx.emit(Token::piece(turn, index, &piece).to_message());
                    index += 1;
                }
                Ok(Piece::Done(replied)) => break replied,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break Err(Error::Llm("the model thread ended".into())),
            }
        };
        sent?;
        let last = match replied {
            _ if ctx.is_cancelled() => Token::last(turn, index, End::Cancelled, ""),
            Ok(_) => Token::last(turn, index, End::Complete, ""),
            Err(e) => Token::last(turn, index, End::Failed, &e.to_string()),
        };
        ctx.emit(last.to_message())?;
        Ok(Handled::Ack)
    }
}

/// A reply server behind mq-bridge: its tokens come back on `SERVER_TOKENS`.
pub struct Server {
    config: RemoteConfig,
    call: StreamCall,
}

impl Server {
    pub async fn start(config: RemoteConfig) -> mq_bridge::anyhow::Result<Self> {
        let tokens = Endpoint::new_memory(SERVER_TOKENS, CAPACITY);
        let endpoint = config.endpoint(&tokens, &Endpoint::new_memory(SERVER_CANCEL, CAPACITY))?;
        Ok(Self { call: StreamCall::new("pal_llm_server", endpoint, &tokens).await?, config })
    }

    /// Asks for a reply and passes its text on; returns the last token, which says how the reply ended.
    fn reply(
        &mut self,
        turn: &str,
        messages: &[Message],
        ctx: &HandlerContext,
    ) -> std::result::Result<Token, HandlerError> {
        let request = CanonicalMessage::from(self.config.payload(messages));
        self.call.call(turn, request, |piece| ctx.emit(piece))
    }
}

/// The `pal_llm` stage: messages in, tokens out. Reports `Prefilled` after each prefill.
pub fn handler(model: Option<Model>, mut server: Option<Server>, report: Report) -> impl BlockingHandler {
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        trace_received("pal_llm", &msg);
        let (turn, messages) = match LlmRequest::from_message(&msg).map_err(failed)? {
            LlmRequest::Prefill(messages) => {
                // The server keeps no cache; a fallback model reads everything on its first turn instead.
                if let (None, Some(model)) = (&server, &model) {
                    model.prefill(messages)?;
                }
                report(Spoken::Prefilled);
                return Ok(Handled::Ack);
            }
            LlmRequest::Reply { turn, messages } => (turn, messages),
        };
        if let Some(server) = server.as_mut() {
            let last = server.reply(&turn, &messages, ctx)?;
            let unanswered = last.end == Some(End::Failed) && last.index == 0;
            if !(unanswered && model.is_some() && !ctx.is_cancelled()) {
                ctx.emit(last.to_message())?;
                return Ok(Handled::Ack);
            }
            eprintln!("llm_pal: {}; the local model answers", last.text);
        }
        let Some(model) = &model else {
            ctx.emit(Token::last(&turn, 0, End::Failed, "no model").to_message())?;
            return Ok(Handled::Ack);
        };
        model.reply(&turn, messages, ctx)
    }
}
