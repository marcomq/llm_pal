//! What travels on each topic, as one Rust type per topic, and what the stages report back.
//!
//! A turn's data (requests, tokens, sentences) carries the turn in `correlation_id`. mq-bridge's `CancelHandle`
//! cancels by that key, so a cancelled turn's waiting data is skipped in every stage. Control messages to the
//! speaker (`Hold`, `Release`, `Discard`, `Stop`) carry the turn under `turn` instead: a cancel never drops them.

use std::sync::Arc;
use std::time::Instant;

use mq_bridge::CanonicalMessage;

use crate::think::llm::Message;

/// mq-bridge's key for the id a message belongs to; here always the turn.
pub const CORRELATION_ID: &str = "correlation_id";
/// The turn of a control message, which a cancel must not skip.
const TURN: &str = "turn";
const KIND: &str = "kind";
/// mq-bridge's `stream_to` keys, used for every token.
const STREAM_INDEX: &str = "mqb.stream.index";
const STREAM_END: &str = "mqb.stream.end";
const STREAM_STATUS: &str = "mqb.stream.status";

fn meta<'a>(msg: &'a CanonicalMessage, key: &str) -> &'a str {
    msg.metadata.get(key).map_or("", String::as_str)
}

/// `[pal.llm]`: what the model is asked.
pub enum LlmRequest {
    /// Read these messages into the model's cache; no reply.
    Prefill(Vec<Message>),
    Reply {
        turn: String,
        messages: Vec<Message>,
    },
}

impl LlmRequest {
    pub fn to_message(&self) -> serde_json::Result<CanonicalMessage> {
        Ok(match self {
            Self::Prefill(messages) => {
                CanonicalMessage::from(serde_json::to_string(messages)?).with_metadata_kv(KIND, "prefill")
            }
            Self::Reply { turn, messages } => CanonicalMessage::from(serde_json::to_string(messages)?)
                .with_metadata_kv(KIND, "reply")
                .with_metadata_kv(CORRELATION_ID, turn),
        })
    }

    pub fn from_message(msg: &CanonicalMessage) -> serde_json::Result<Self> {
        let messages = serde_json::from_slice(&msg.payload)?;
        Ok(match meta(msg, KIND) {
            "prefill" => Self::Prefill(messages),
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

/// `[pal.pieces]`: a piece of a reply in mq-bridge's `stream_to` format, the same for the local model, a server
/// and canned replies. The last token of a reply has `end` set and no text unless it failed.
#[derive(Debug)]
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
    /// Data: a sentence to speak.
    Sentence(String),
    /// Data: the turn's last sentence was sent.
    End,
    /// Data: the reply failed.
    Error(String),
}

impl ToSpeaker {
    pub fn to_message(&self, turn: &str) -> CanonicalMessage {
        let (kind, text) = match self {
            Self::Hold => ("hold", ""),
            Self::Release => ("release", ""),
            Self::Discard => ("discard", ""),
            Self::Stop => ("stop", ""),
            Self::Sentence(text) => ("sentence", text.as_str()),
            Self::End => ("end", ""),
            Self::Error(text) => ("error", text.as_str()),
        };
        let msg = CanonicalMessage::from(text).with_metadata_kv(KIND, kind);
        match self {
            Self::Sentence(_) | Self::End | Self::Error(_) => msg.with_metadata_kv(CORRELATION_ID, turn),
            Self::Hold | Self::Release | Self::Discard | Self::Stop => msg.with_metadata_kv(TURN, turn),
        }
    }

    /// The turn and what to do; `None` for an unknown kind.
    pub fn from_message(msg: &CanonicalMessage) -> Option<(String, Self)> {
        let text = || msg.get_payload_str().into_owned();
        let what = match meta(msg, KIND) {
            "hold" => Self::Hold,
            "release" => Self::Release,
            "discard" => Self::Discard,
            "stop" => Self::Stop,
            "sentence" => Self::Sentence(text()),
            "end" => Self::End,
            "error" => Self::Error(text()),
            _ => return None,
        };
        let turn = match meta(msg, CORRELATION_ID) {
            "" => meta(msg, TURN),
            turn => turn,
        };
        Some((turn.to_string(), what))
    }
}

/// What the reply stages report back to the conversation, over a plain channel.
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
}

pub type Report = Arc<dyn Fn(Spoken) + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_the_wire() {
        let request = LlmRequest::Reply { turn: "t1".into(), messages: Vec::new() };
        let LlmRequest::Reply { turn, .. } = LlmRequest::from_message(&request.to_message().unwrap()).unwrap() else {
            panic!("a reply");
        };
        assert_eq!(turn, "t1");

        let token = Token::from_message(&Token::last("t1", 3, End::Failed, "boom").to_message());
        assert_eq!(
            (token.turn.as_str(), token.index, token.text.as_str(), token.end),
            ("t1", 3, "boom", Some(End::Failed))
        );

        for what in [ToSpeaker::Release, ToSpeaker::Sentence("Hallo.".into())] {
            let msg = what.to_message("t2");
            let control = what == ToSpeaker::Release;
            assert_eq!(msg.metadata.contains_key(CORRELATION_ID), !control, "a cancel skips data only");
            assert_eq!(ToSpeaker::from_message(&msg), Some(("t2".into(), what)));
        }
    }
}
