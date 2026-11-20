//! The handlers of the transport's `pal_split` (tokens → sentences) and `pal_speak` (gate → voice) routes.

use std::sync::mpsc::Receiver;
use std::time::Instant;

use mq_bridge::blocking_handler::{BlockingHandler, HandlerContext};
use mq_bridge::{CanonicalMessage, Handled};

use crate::speak::sentences::Sentences;
use crate::speak::tts::PocketSpeaker;
use crate::think::language::Language;
use crate::transport::{COMPLETE, CORRELATION_ID, FAILED, KIND, Report, STREAM_END, STREAM_STATUS, Spoken, TURN};
use crate::transport::{meta, turn_message};

/// Numbers as words, and words the voice reads wrong respelled.
fn pronounced(text: &str, language: Language) -> String {
    let text = crate::speak::numbers::spell(text, language);
    language.profile().pronunciations.iter().fold(text, |t, (word, spoken)| t.replace(word, spoken))
}

pub fn split(language: Language, report: Report) -> impl BlockingHandler {
    let mut sentences = Sentences::new(language);
    let mut current = String::new();
    move |msg: CanonicalMessage, ctx: &HandlerContext| {
        let turn = meta(&msg, CORRELATION_ID);
        // A cancelled turn ends without its last sentence, which is dropped here.
        if turn != current {
            sentences = Sentences::new(language);
            current = turn.to_string();
            report(Spoken::FirstPiece { turn: current.clone(), at: Instant::now() });
        }
        let text = msg.get_payload_str();
        let (out, last) = match (meta(&msg, STREAM_END), meta(&msg, STREAM_STATUS)) {
            ("true", COMPLETE) => (sentences.finish().into_iter().collect(), Some(turn_message("end", turn, ""))),
            ("true", FAILED) => (Vec::new(), Some(turn_message("error", turn, &text))),
            ("true", _) => (Vec::new(), None),
            _ => (sentences.push(&text), None),
        };
        for s in out {
            ctx.emit(turn_message("sentence", turn, &s))?;
        }
        if let Some(last) = last {
            ctx.emit(last)?;
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
