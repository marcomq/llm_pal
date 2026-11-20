//! The handlers of the transport's `pal_split` (tokens → sentences) and `pal_speak` (gate → voice) routes.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use mq_bridge::blocking_handler::{BlockingHandler, HandlerContext};
use mq_bridge::{CanonicalMessage, Handled};

use crate::speak::sentences::Sentences;
use crate::speak::tts::PocketSpeaker;
use crate::think::language::Language;
use crate::transport::{End, Report, Spoken, ToSpeaker, Token};

/// Numbers as words, and words the voice reads wrong respelled.
fn pronounced(text: &str, language: Language) -> String {
    let text = crate::speak::numbers::spell(text, language);
    language.profile().pronunciations.iter().fold(text, |t, (word, spoken)| t.replace(word, spoken))
}

pub fn split(language: Language, report: Report) -> impl BlockingHandler {
    let mut sentences = Sentences::new(language);
    let mut current = String::new();
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let token = Token::from_message(&msg);
        // A cancelled turn ends without its last sentence, which is dropped here.
        if token.turn != current {
            sentences = Sentences::new(language);
            current = token.turn.clone();
            report(Spoken::FirstPiece { turn: current.clone(), at: Instant::now() });
        }
        let (out, last) = match token.end {
            Some(End::Complete) => (sentences.finish().into_iter().collect(), Some(ToSpeaker::End)),
            Some(End::Failed) => (Vec::new(), Some(ToSpeaker::Error(token.text))),
            Some(End::Cancelled) => (Vec::new(), None),
            None => (sentences.push(&token.text), None),
        };
        for s in out {
            ctx.emit(ToSpeaker::Sentence(s).to_message(&current))?;
        }
        if let Some(last) = last {
            ctx.emit(last.to_message(&current))?;
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

/// `speaker` may arrive after the routes start; the stage waits for it on its first message.
pub fn speak(speaker: Receiver<PocketSpeaker>, language: Language, report: Report) -> impl BlockingHandler {
    let mut held: Option<Held> = None;
    let mut ready: Option<PocketSpeaker> = None;
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let Some((turn, what)) = ToSpeaker::from_message(&msg) else {
            return Ok(Handled::Ack);
        };
        let turn = turn.as_str();
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
        match what {
            ToSpeaker::Hold => held = Some(Held { turn: turn.to_string(), sentences: Vec::new(), done: false }),
            ToSpeaker::Release if holding => {
                let h = held.take().expect("holding");
                h.sentences.into_iter().for_each(&mut say);
                if h.done {
                    report(Spoken::Done { turn: h.turn });
                }
            }
            ToSpeaker::Discard if holding => held = None,
            ToSpeaker::Release | ToSpeaker::Discard => {}
            ToSpeaker::Stop => speaker.stop(),
            ToSpeaker::Sentence(_) if ctx.is_cancelled() => {}
            ToSpeaker::Sentence(text) if holding => held.as_mut().expect("holding").sentences.push(text),
            ToSpeaker::Sentence(text) => say(text),
            ToSpeaker::End if holding => held.as_mut().expect("holding").done = true,
            ToSpeaker::End => report(Spoken::Done { turn: turn.to_string() }),
            ToSpeaker::Error(error) => {
                if holding {
                    held = None;
                }
                report(Spoken::Failed { turn: turn.to_string(), error });
            }
        }
        Ok(Handled::Ack)
    }
}
