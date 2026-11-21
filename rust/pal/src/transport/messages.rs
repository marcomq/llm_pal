//! What travels on each topic, as one Rust type per topic: `[pal.inbox]` [`Input`], `[pal.llm]` [`LlmRequest`],
//! `[pal.speech]` [`ToSpeaker`].
//!
//! A turn's data (requests, tokens) carries the turn in `correlation_id`. mq-bridge's `CancelHandle`
//! cancels by that key, so a cancelled turn's waiting data is skipped in every stage. Control messages to the
//! speaker (`Hold`, `Release`, `Discard`, `Stop`, `Pause`, `Resume`) carry the turn under `turn` instead: a cancel
//! never drops them.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mq_bridge::CanonicalMessage;

use crate::hear::event::{EVENT, Event};
use crate::think::local_llm::Message;

/// mq-bridge's key for the id a message belongs to; here always the turn.
pub const CORRELATION_ID: &str = "correlation_id";
/// The turn of a control message, which a cancel must not skip.
const TURN: &str = "turn";
const KIND: &str = "kind";
const MARK: &str = "mark";
/// How long before the message was made a report's `at` was, in µs.
const AT_BEFORE: &str = "at_before_us";
/// mq-bridge's `stream_to` keys, used for every token.
const STREAM_INDEX: &str = "mqb.stream.index";
const STREAM_END: &str = "mqb.stream.end";
const STREAM_STATUS: &str = "mqb.stream.status";

fn meta<'a>(msg: &'a CanonicalMessage, key: &str) -> &'a str {
    msg.metadata.get(key).map_or("", String::as_str)
}

/// Logs a message as it reaches `route`, with its age: the time since its id was made.
pub fn trace_received(route: &str, msg: &CanonicalMessage) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }
    let either = |a: &str, b: &str| Some(meta(msg, a)).filter(|v| !v.is_empty()).unwrap_or(meta(msg, b)).to_string();
    tracing::trace!(
        route,
        turn = either(CORRELATION_ID, TURN),
        kind = either(KIND, crate::hear::event::EVENT),
        index = meta(msg, STREAM_INDEX),
        age_us = age_us(msg.message_id),
        id = %mq_bridge::canonical_message::format_message_id(msg.message_id),
        "received"
    );
}

/// Microseconds since `id` was made. mq-bridge's ids are UUID v7: 48 bits of Unix milliseconds, the version, then
/// the millisecond's fraction in sixteenths.
fn age_us(id: u128) -> i64 {
    let ms = (id >> 80) as i64;
    let sixteenths = ((id >> 72) & 0xF) as i64;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as i64);
    now - (ms * 1000 + sixteenths * 1000 / 16)
}

/// `[pal.llm]`: what the model is asked.
pub enum LlmRequest {
    /// Read these messages into the model's cache; no reply. With a `mark`, the last message only up to that byte:
    /// the user is still saying it.
    Prefill {
        messages: Vec<Message>,
        mark: Option<usize>,
    },
    Reply {
        turn: String,
        messages: Vec<Message>,
    },
}

impl LlmRequest {
    pub fn to_message(&self) -> serde_json::Result<CanonicalMessage> {
        Ok(match self {
            Self::Prefill { messages, mark } => {
                let msg = CanonicalMessage::from(serde_json::to_string(messages)?).with_metadata_kv(KIND, "prefill");
                match mark {
                    Some(mark) => msg.with_metadata_kv(MARK, mark.to_string()),
                    None => msg,
                }
            }
            Self::Reply { turn, messages } => CanonicalMessage::from(serde_json::to_string(messages)?)
                .with_metadata_kv(KIND, "reply")
                .with_metadata_kv(CORRELATION_ID, turn),
        })
    }

    pub fn from_message(msg: &CanonicalMessage) -> serde_json::Result<Self> {
        let messages = serde_json::from_slice(&msg.payload)?;
        Ok(match meta(msg, KIND) {
            "prefill" => Self::Prefill { messages, mark: meta(msg, MARK).parse().ok() },
            _ => Self::Reply { turn: meta(msg, CORRELATION_ID).to_string(), messages },
        })
    }
}

/// How a reply ended, on its last token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Complete,
    Cancelled,
    /// The token's text is the error.
    Failed,
}

/// A piece of a reply in mq-bridge's `stream_to` format, the same for the local model, a server and canned replies.
/// The last token of a reply has `end` set and no text unless it failed.
#[derive(Debug, PartialEq, Eq)]
pub struct Token {
    pub turn: String,
    pub index: usize,
    pub text: String,
    pub end: Option<End>,
}

impl Token {
    pub fn piece(turn: &str, index: usize, text: &str) -> Self {
        Self { turn: turn.into(), index, text: text.into(), end: None }
    }

    pub fn last(turn: &str, index: usize, end: End, text: &str) -> Self {
        Self { turn: turn.into(), index, text: text.into(), end: Some(end) }
    }

    pub fn to_message(&self) -> CanonicalMessage {
        let msg = CanonicalMessage::from(self.text.as_str())
            .with_metadata_kv(CORRELATION_ID, &self.turn)
            .with_metadata_kv(STREAM_INDEX, self.index.to_string())
            .with_metadata_kv(STREAM_END, self.end.is_some().to_string());
        match self.end {
            Some(End::Complete) => msg.with_metadata_kv(STREAM_STATUS, "complete"),
            Some(End::Cancelled) => msg.with_metadata_kv(STREAM_STATUS, "cancelled"),
            Some(End::Failed) => msg.with_metadata_kv(STREAM_STATUS, "error"),
            None => msg,
        }
    }

    pub fn from_message(msg: &CanonicalMessage) -> Self {
        let end = (meta(msg, STREAM_END) == "true").then(|| match meta(msg, STREAM_STATUS) {
            "complete" => End::Complete,
            "error" => End::Failed,
            _ => End::Cancelled,
        });
        Self {
            turn: meta(msg, CORRELATION_ID).to_string(),
            index: meta(msg, STREAM_INDEX).parse().unwrap_or(0),
            text: msg.get_payload_str().into_owned(),
            end,
        }
    }
}

/// `[pal.speech]`: what the speaker stage gets for a turn.
#[derive(Debug, PartialEq, Eq)]
pub enum ToSpeaker {
    /// Control: keep this turn's sentences until `Release` or `Discard`.
    Hold,
    /// Control: speak the held sentences, and the rest as they come.
    Release,
    /// Control: drop the held sentences.
    Discard,
    /// Control: cut off the voice now.
    Stop,
    /// Control: hold the voice where it is, e.g. while it is unclear whether the user interrupts.
    Pause,
    Resume,
    /// Data: a token of the reply. Tokens have no `KIND`, as a server's `stream_to` sends them.
    Piece(Token),
}

impl ToSpeaker {
    pub fn to_message(&self, turn: &str) -> CanonicalMessage {
        let kind = match self {
            Self::Hold => "hold",
            Self::Release => "release",
            Self::Discard => "discard",
            Self::Stop => "stop",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Piece(token) => return token.to_message(),
        };
        CanonicalMessage::from("").with_metadata_kv(KIND, kind).with_metadata_kv(TURN, turn)
    }

    /// The turn and what to do; `None` for an unknown kind.
    pub fn from_message(msg: &CanonicalMessage) -> Option<(String, Self)> {
        let what = match meta(msg, KIND) {
            "hold" => Self::Hold,
            "release" => Self::Release,
            "discard" => Self::Discard,
            "stop" => Self::Stop,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "" => {
                let token = Token::from_message(msg);
                return Some((token.turn.clone(), Self::Piece(token)));
            }
            _ => return None,
        };
        Some((meta(msg, TURN).to_string(), what))
    }
}

/// `[pal.inbox]`: everything the conversation reacts to, in arrival order. A tick (`KIND` `tick`, about every
/// 20 ms) decodes to `None`: the conversation checks its waiting times.
pub enum Input {
    Event(Event),
    Spoken(Spoken),
}

impl Input {
    pub fn from_message(msg: &CanonicalMessage) -> Option<Self> {
        if msg.metadata.contains_key(EVENT) {
            return Event::from_message(msg).map(Input::Event);
        }
        let turn = || meta(msg, TURN).to_string();
        let text = || msg.get_payload_str().into_owned();
        let before = meta(msg, AT_BEFORE).parse().unwrap_or(0);
        let at = || Instant::now().checked_sub(Duration::from_micros(age_us(msg.message_id).max(0) as u64 + before));
        Some(Input::Spoken(match meta(msg, KIND) {
            "said" => Spoken::Said { turn: turn(), text: text(), at: at().unwrap_or_else(Instant::now) },
            "done" => Spoken::Done { turn: turn() },
            "failed" => Spoken::Failed { turn: turn(), error: text() },
            "prefilled" => Spoken::Prefilled,
            "first_piece" => Spoken::FirstPiece { turn: turn(), at: at().unwrap_or_else(Instant::now) },
            "audible" => Spoken::Audible { turn: turn(), at: at().unwrap_or_else(Instant::now) },
            "quiet" => Spoken::Quiet,
            _ => return None,
        }))
    }
}

/// The ticks' `KIND`, as the ticker's static metadata.
pub const TICK: (&str, &str) = (KIND, "tick");

/// What the reply routes report back to the conversation, on `[pal.inbox]`.
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
    /// The first piece of a turn's reply reached the sentence split at `at`.
    FirstPiece {
        turn: String,
        at: Instant,
    },
    /// A sentence of the turn began to play at `at`.
    Audible {
        turn: String,
        at: Instant,
    },
    /// Every queued sentence has played; Pal's voice is silent.
    Quiet,
}

impl Spoken {
    /// `at` travels as the message's age (its id holds when it was made) plus how long before that it was.
    pub fn to_message(&self) -> CanonicalMessage {
        let (kind, turn, text) = match self {
            Self::Said { turn, text, .. } => ("said", turn.as_str(), text.as_str()),
            Self::Done { turn } => ("done", turn.as_str(), ""),
            Self::Failed { turn, error } => ("failed", turn.as_str(), error.as_str()),
            Self::Prefilled => ("prefilled", "", ""),
            Self::FirstPiece { turn, .. } => ("first_piece", turn.as_str(), ""),
            Self::Audible { turn, .. } => ("audible", turn.as_str(), ""),
            Self::Quiet => ("quiet", "", ""),
        };
        let msg = CanonicalMessage::from(text).with_metadata_kv(KIND, kind).with_metadata_kv(TURN, turn);
        match self {
            Self::Said { at, .. } | Self::FirstPiece { at, .. } | Self::Audible { at, .. } => {
                msg.with_metadata_kv(AT_BEFORE, at.elapsed().as_micros().to_string())
            }
            _ => msg,
        }
    }
}

/// Sends a report to `[pal.inbox]`.
pub type Report = Arc<dyn Fn(Spoken) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_and_events_reach_the_inbox_as_inputs() {
        let said = Spoken::Said { turn: "turn-1".into(), text: "Hallo.".into(), at: Instant::now() };
        let Some(Input::Spoken(Spoken::Said { turn, text, at })) = Input::from_message(&said.to_message()) else {
            panic!("not a sentence")
        };
        assert_eq!((turn.as_str(), text.as_str()), ("turn-1", "Hallo."));
        assert!(at.elapsed() < Duration::from_millis(50));
        let earlier = Instant::now() - Duration::from_millis(15);
        let audible = Spoken::Audible { turn: "turn-1".into(), at: earlier }.to_message();
        let Some(Input::Spoken(Spoken::Audible { at, .. })) = Input::from_message(&audible) else { panic!("audible") };
        assert!(at.duration_since(earlier) < Duration::from_millis(2), "{:?}", at.duration_since(earlier));
        let failed = Spoken::Failed { turn: "turn-2".into(), error: "no server".into() }.to_message();
        assert!(
            matches!(Input::from_message(&failed), Some(Input::Spoken(Spoken::Failed { error, .. })) if error == "no server")
        );
        assert!(matches!(Input::from_message(&Event::Lost.to_message()), Some(Input::Event(Event::Lost))));
        let tick = CanonicalMessage::from("").with_metadata_kv(TICK.0, TICK.1);
        assert!(Input::from_message(&tick).is_none());
    }

    #[test]
    fn a_new_message_is_younger_than_a_millisecond_and_ages() {
        let id = CanonicalMessage::from("x").message_id;
        assert!((-100..1000).contains(&age_us(id)), "{}", age_us(id));
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!((4000..100_000).contains(&age_us(id)), "{}", age_us(id));
    }

    #[test]
    fn messages_survive_the_wire() {
        let request = LlmRequest::Reply { turn: "t1".into(), messages: Vec::new() };
        let LlmRequest::Reply { turn, .. } = LlmRequest::from_message(&request.to_message().unwrap()).unwrap() else {
            panic!("a reply");
        };
        assert_eq!(turn, "t1");
        for mark in [None, Some(7)] {
            let request = LlmRequest::Prefill { messages: Vec::new(), mark };
            let read = LlmRequest::from_message(&request.to_message().unwrap()).unwrap();
            assert!(matches!(read, LlmRequest::Prefill { mark: m, .. } if m == mark));
        }

        let token = Token::from_message(&Token::last("t1", 3, End::Failed, "boom").to_message());
        assert_eq!(
            (token.turn.as_str(), token.index, token.text.as_str(), token.end),
            ("t1", 3, "boom", Some(End::Failed))
        );

        for what in [ToSpeaker::Release, ToSpeaker::Piece(Token::piece("t2", 0, "Hallo."))] {
            let msg = what.to_message("t2");
            let control = what == ToSpeaker::Release;
            assert_eq!(msg.metadata.contains_key(CORRELATION_ID), !control, "a cancel skips data only");
            assert_eq!(ToSpeaker::from_message(&msg), Some(("t2".into(), what)));
        }
    }
}
