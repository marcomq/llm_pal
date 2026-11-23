//! The handler of the transport's `pal_speak` route: tokens → sentences → gate → voice.

use std::time::Instant;

use mq_bridge::blocking_handler::{BlockingHandler, HandlerContext};
use mq_bridge::{CanonicalMessage, Handled, HandlerError};

use crate::speak::sentences::Sentences;
use crate::speak::tts::PocketSpeaker;
use crate::think::language::Language;
use crate::transport::{End, Report, Spoken, ToSpeaker, Token, trace_received};

/// Numbers as words, and words the voice reads wrong respelled.
fn pronounced(text: &str, language: Language) -> String {
    let text = crate::speak::numbers::spell(text, language);
    language.profile().pronunciations.iter().fold(text, |t, (word, spoken)| t.replace(word, spoken))
}

/// Cuts the tokens of the current turn into sentences.
pub struct Split {
    language: Language,
    sentences: Sentences,
    turn: String,
}

impl Split {
    pub fn new(language: Language) -> Self {
        Self { language, sentences: Sentences::new(language), turn: String::new() }
    }

    /// The sentences `token` completed. A cancelled turn ends without its last sentence, which is dropped here.
    pub fn sentences(&mut self, token: &Token, report: &Report) -> Vec<String> {
        if token.turn != self.turn {
            self.sentences = Sentences::new(self.language);
            self.turn = token.turn.clone();
            report(Spoken::FirstPiece { turn: self.turn.clone(), at: Instant::now() });
        }
        match token.end {
            Some(End::Complete) => self.sentences.finish().into_iter().collect(),
            Some(_) => Vec::new(),
            None => self.sentences.push(&token.text),
        }
    }
}

/// A reply to a pause, kept until the segment ends.
struct Held {
    turn: String,
    sentences: Vec<String>,
    done: bool,
}

/// The `pal_speak` handler.
pub fn speak(speaker: PocketSpeaker, language: Language, report: Report) -> impl BlockingHandler {
    let mut state = Speak { speaker, language, split: Split::new(language), held: None, broken: false, report };
    move |msg: CanonicalMessage, ctx: &HandlerContext| state.on_speech(msg, ctx)
}

struct Speak {
    speaker: PocketSpeaker,
    language: Language,
    split: Split,
    held: Option<Held>,
    /// The speech thread is gone: nothing is said or done any more, each sentence reports `Failed`.
    broken: bool,
    report: Report,
}

impl Speak {
    /// Runs for each message on `pal.speech`: a token to say, or a control message for the voice.
    fn on_speech(&mut self, msg: CanonicalMessage, ctx: &HandlerContext) -> Result<Handled, HandlerError> {
        trace_received("pal_speak", &msg);
        let Some((turn, what)) = ToSpeaker::from_message(&msg) else {
            return Ok(Handled::Ack);
        };
        match what {
            ToSpeaker::Hold => self.held = Some(Held { turn, sentences: Vec::new(), done: false }),
            ToSpeaker::Release => self.release(&turn),
            ToSpeaker::Discard => self.discard(&turn),
            ToSpeaker::Stop => self.speaker.stop(),
            ToSpeaker::Pause => self.speaker.pause(),
            ToSpeaker::Resume => self.speaker.resume(),
            ToSpeaker::Piece(token) => self.piece(&turn, token, ctx),
        }
        Ok(Handled::Ack)
    }

    fn holding(&self, turn: &str) -> bool {
        self.held.as_ref().is_some_and(|h| h.turn == turn)
    }

    fn release(&mut self, turn: &str) {
        if !self.holding(turn) {
            return;
        }
        let held = self.held.take().expect("holding");
        for sentence in held.sentences {
            self.say(turn, sentence);
        }
        if held.done && !self.broken {
            (self.report)(Spoken::Done { turn: held.turn });
        }
    }

    fn discard(&mut self, turn: &str) {
        if self.holding(turn) {
            self.held = None;
        }
    }

    fn piece(&mut self, turn: &str, token: Token, ctx: &HandlerContext) {
        let holding = self.holding(turn);
        let sentences = self.split.sentences(&token, &self.report);
        if !ctx.is_cancelled() {
            if holding {
                self.held.as_mut().expect("holding").sentences.extend(sentences);
            } else {
                for sentence in sentences {
                    self.say(turn, sentence);
                }
            }
        }
        match token.end {
            Some(End::Complete) if holding => self.held.as_mut().expect("holding").done = true,
            Some(End::Complete) if !self.broken => (self.report)(Spoken::Done { turn: turn.to_string() }),
            Some(End::Failed) => {
                if holding {
                    self.held = None;
                }
                (self.report)(Spoken::Failed { turn: turn.to_string(), error: token.text });
            }
            _ => {}
        }
    }

    /// `Said` goes first: the playback's `Quiet` for this sentence must reach the inbox after it.
    fn say(&mut self, turn: &str, text: String) {
        let spoken = pronounced(&text, self.language);
        (self.report)(Spoken::Said { turn: turn.to_string(), text, at: Instant::now() });
        if let Err(e) = self.speaker.say(turn, &spoken) {
            self.broken = true;
            (self.report)(Spoken::Failed { turn: turn.to_string(), error: e.to_string() });
        }
    }
}
