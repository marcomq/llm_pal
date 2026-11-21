//! The handler of the transport's `pal_speak` route: tokens → sentences → gate → voice.

use std::time::Instant;

use mq_bridge::blocking_handler::{BlockingHandler, HandlerContext};
use mq_bridge::{CanonicalMessage, Handled};

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

pub fn speak(mut speaker: PocketSpeaker, language: Language, report: Report) -> impl BlockingHandler {
    let mut split = Split::new(language);
    let mut held: Option<Held> = None;
    // The speech thread is gone: nothing is said or done any more, each sentence reports `Failed`.
    let mut broken = false;
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        trace_received("pal_speak", &msg);
        let Some((turn, what)) = ToSpeaker::from_message(&msg) else {
            return Ok(Handled::Ack);
        };
        let turn = turn.as_str();
        // `Said` goes first: the playback's `Quiet` for this sentence must reach the inbox after it.
        let mut say = |text: String| {
            let spoken = pronounced(&text, language);
            report(Spoken::Said { turn: turn.to_string(), text, at: Instant::now() });
            if let Err(e) = speaker.say(turn, &spoken) {
                broken = true;
                report(Spoken::Failed { turn: turn.to_string(), error: e.to_string() });
            }
        };
        let holding = held.as_ref().is_some_and(|h| h.turn == turn);
        match what {
            ToSpeaker::Hold => held = Some(Held { turn: turn.to_string(), sentences: Vec::new(), done: false }),
            ToSpeaker::Release if holding => {
                let h = held.take().expect("holding");
                h.sentences.into_iter().for_each(&mut say);
                if h.done && !broken {
                    report(Spoken::Done { turn: h.turn });
                }
            }
            ToSpeaker::Discard if holding => held = None,
            ToSpeaker::Release | ToSpeaker::Discard => {}
            ToSpeaker::Stop => speaker.stop(),
            ToSpeaker::Pause => speaker.pause(),
            ToSpeaker::Resume => speaker.resume(),
            ToSpeaker::Piece(token) => {
                let sentences = split.sentences(&token, &report);
                if !ctx.is_cancelled() {
                    match held.as_mut().filter(|h| h.turn == turn) {
                        Some(h) => h.sentences.extend(sentences),
                        None => sentences.into_iter().for_each(&mut say),
                    }
                }
                match token.end {
                    Some(End::Complete) if holding => held.as_mut().expect("holding").done = true,
                    Some(End::Complete) if !broken => report(Spoken::Done { turn: turn.to_string() }),
                    Some(End::Failed) => {
                        if holding {
                            held = None;
                        }
                        report(Spoken::Failed { turn: turn.to_string(), error: token.text });
                    }
                    _ => {}
                }
            }
        }
        Ok(Handled::Ack)
    }
}
