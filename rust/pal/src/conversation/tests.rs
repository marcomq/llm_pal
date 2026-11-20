//! The phases over the topics: inputs go to `handle` as they would from `[pal.inbox]`, the tests read what reaches
//! `[pal.llm]`, `[pal.pieces]` and `[pal.speech]`. STT is a route that answers with the transcript the test sets.
//! No models, no sound.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use mq_bridge::blocking_handler::{HandlerContext, ThreadedHandler};
use mq_bridge::models::Endpoint;
use mq_bridge::{Caller, CanonicalMessage, Handled, Route};
use tokio::runtime::Runtime;

use super::barge_in::RESUME_AFTER;
use super::unfinished::UNFINISHED_WAIT;
use super::*;
use crate::hear::event::Features;
use crate::think::persona::DEFAULT_NAME;
use crate::transport::{LLM_IN, LlmRequest, PIECES, SPEECH, ToSpeaker};

/// The topics are process-wide, so the tests take turns.
struct Topics {
    rt: Runtime,
    llm: Receiver<CanonicalMessage>,
    pieces: Receiver<CanonicalMessage>,
    speech: Receiver<CanonicalMessage>,
    transcript: Arc<Mutex<String>>,
}

fn topics() -> MutexGuard<'static, Topics> {
    static TOPICS: OnceLock<Mutex<Topics>> = OnceLock::new();
    let t = TOPICS
        .get_or_init(|| {
            let rt = Runtime::new().unwrap();
            let (llm, pieces, speech) = (collect(&rt, LLM_IN), collect(&rt, PIECES), collect(&rt, SPEECH));
            Mutex::new(Topics { rt, llm, pieces, speech, transcript: Arc::default() })
        })
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    for rx in [&t.llm, &t.pieces, &t.speech] {
        while rx.try_recv().is_ok() {}
    }
    t
}

fn collect(rt: &Runtime, topic: &str) -> Receiver<CanonicalMessage> {
    let (tx, rx) = channel();
    let handler = ThreadedHandler::new(move |msg: CanonicalMessage, _: &HandlerContext| {
        let _ = tx.send(msg);
        Ok(Handled::Ack)
    });
    let route = Route::new(Endpoint::new_memory(topic, 1024), Endpoint::null()).with_handler(handler);
    rt.block_on(route.with_batch_size(1).deploy(&format!("test_{topic}"))).unwrap();
    rx
}

/// A listening conversation; its STT answers with `t.transcript`.
fn pal(t: &Topics, barge_in: bool) -> (Conversation, Receiver<AgentEvent>) {
    let transcript = t.transcript.clone();
    let stt = Route::new(Endpoint::new_memory("test.stt", 16), Endpoint::new_response()).with_handler(
        move |_: CanonicalMessage| {
            let heard = serde_json::json!({ "text": *transcript.lock().unwrap() }).to_string();
            async move { Ok(Handled::Publish(CanonicalMessage::from(heard.as_str()))) }
        },
    );
    let stt = t.rt.block_on(Caller::deploy("test_stt", stt)).unwrap();
    let cfg = AgentConfig {
        model_dir: PathBuf::new(),
        smart_turn: false,
        stt_model: String::new(),
        llm_model: String::new(),
        voice: String::new(),
        barge_in,
        language: Language::German,
        user_name: DEFAULT_NAME.into(),
        remote: None,
        fallback: false,
        party: false,
    };
    let persona = Persona::kitt(cfg.language, &cfg.user_name);
    let (tx, events) = channel();
    let transport = Transport::without_routes(t.rt.handle().clone(), stt);
    let mut c = Conversation::new(cfg, Arc::default(), tx, transport, MicLink::new(None), persona, Instant::now());
    c.phase = Phase::Listening;
    (c, events)
}

fn say(t: &Topics, c: &mut Conversation, text: &str, event: fn(Segment) -> Event, complete: bool) {
    *t.transcript.lock().unwrap() = text.into();
    let seg = Segment {
        id: 1,
        start_ms: 0,
        end_ms: 1000,
        forced_split: false,
        during_output: false,
        turn_complete: complete,
        silence_ms: 400,
        features: Features::default(),
        pcm: vec![0.0; 1600],
    };
    c.handle(Some(Input::Event(event(seg)))).unwrap();
}

fn report(c: &mut Conversation, spoken: Spoken) {
    c.handle(Some(Input::Spoken(spoken))).unwrap();
}

fn next(rx: &Receiver<CanonicalMessage>) -> CanonicalMessage {
    rx.recv_timeout(Duration::from_secs(2)).expect("a message")
}

fn nothing(rx: &Receiver<CanonicalMessage>) {
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no message");
}

fn reply_of(msg: CanonicalMessage) -> String {
    match LlmRequest::from_message(&msg).unwrap() {
        LlmRequest::Reply { turn, .. } => turn,
        LlmRequest::Prefill(_) => panic!("a prefill, not a reply"),
    }
}

fn to_speaker(rx: &Receiver<CanonicalMessage>) -> (String, ToSpeaker) {
    ToSpeaker::from_message(&next(rx)).expect("a speaker message")
}

const QUESTION: &str = "Erzähl mir etwas über Vulkane in Island.";

#[test]
fn a_segment_is_answered_and_its_reply_followed_until_done() {
    let t = topics();
    let (mut c, events) = pal(&t, false);
    say(&t, &mut c, QUESTION, Event::Segment, true);
    assert_eq!(reply_of(next(&t.llm)), "turn-1");
    assert!(matches!(c.phase, Phase::Replying(_)));
    assert_eq!(c.shared.state.load(Ordering::Relaxed), State::Thinking as u8);

    report(&mut c, Spoken::Said { turn: "turn-1".into(), text: "Feuer und Eis.".into(), at: Instant::now() });
    assert_eq!(c.shared.state.load(Ordering::Relaxed), State::Speaking as u8);
    report(&mut c, Spoken::Audible { turn: "turn-1".into(), at: Instant::now() });
    report(&mut c, Spoken::Done { turn: "turn-1".into() });
    assert!(matches!(c.phase, Phase::Listening));
    assert!(matches!(LlmRequest::from_message(&next(&t.llm)).unwrap(), LlmRequest::Prefill(_)));
    report(&mut c, Spoken::Quiet);
    assert_eq!(c.shared.state.load(Ordering::Relaxed), State::Listening as u8);

    let events: Vec<_> = events.try_iter().collect();
    assert!(
        matches!(&events[..], [AgentEvent::Heard(_), AgentEvent::Said(_), AgentEvent::TurnDone { .. }]),
        "{events:?}"
    );
    let AgentEvent::TurnDone { first_sentence_ms, .. } = &events[2] else { unreachable!() };
    assert!(first_sentence_ms.is_some(), "the audible sentence ends the timing");
    nothing(&t.speech);
}

#[test]
fn a_reply_to_a_pause_is_held_until_the_segment_ends() {
    let t = topics();
    let (mut c, _events) = pal(&t, false);
    say(&t, &mut c, QUESTION, Event::Pause, false);
    assert_eq!(to_speaker(&t.speech), ("turn-1".into(), ToSpeaker::Hold));
    assert_eq!(reply_of(next(&t.llm)), "turn-1");
    say(&t, &mut c, QUESTION, Event::Segment, true);
    assert_eq!(to_speaker(&t.speech), ("turn-1".into(), ToSpeaker::Release));
    nothing(&t.llm);
}

#[test]
fn a_held_reply_is_dropped_when_the_user_goes_on() {
    let t = topics();
    let (mut c, _events) = pal(&t, false);
    say(&t, &mut c, QUESTION, Event::Pause, false);
    assert_eq!(to_speaker(&t.speech), ("turn-1".into(), ToSpeaker::Hold));
    c.handle(Some(Input::Event(Event::Resume))).unwrap();
    assert_eq!(to_speaker(&t.speech), ("turn-1".into(), ToSpeaker::Discard));
    assert!(matches!(c.phase, Phase::Listening));
}

#[test]
fn an_unfinished_sentence_waits_and_its_audio_goes_before_the_next() {
    let t = topics();
    let (mut c, _events) = pal(&t, false);
    say(&t, &mut c, "Ich wollte fragen, ob", Event::Segment, false);
    assert!(matches!(c.phase, Phase::Unfinished { .. }));
    c.handle(Some(Input::Event(Event::SpeechStart { at_ms: 0 }))).unwrap();
    assert!(matches!(c.phase, Phase::Listening));
    assert_eq!(c.carry.as_ref().map(Vec::len), Some(1600));
    nothing(&t.llm);

    say(&t, &mut c, "Ich wollte fragen, ob Vulkane in Island wachsen und", Event::Segment, false);
    assert!(matches!(c.phase, Phase::Unfinished { .. }));
    std::thread::sleep(UNFINISHED_WAIT);
    c.handle(None).unwrap();
    assert_eq!(reply_of(next(&t.llm)), "turn-1");
    assert!(c.carry.is_none());
}

#[test]
fn a_barge_in_pauses_the_voice_until_quiet() {
    let t = topics();
    let (mut c, _events) = pal(&t, true);
    c.handle(Some(Input::Event(Event::BargeIn { at_ms: 0 }))).unwrap();
    assert_eq!(to_speaker(&t.speech).1, ToSpeaker::Pause);
    c.handle(None).unwrap();
    std::thread::sleep(RESUME_AFTER);
    c.handle(None).unwrap();
    assert_eq!(to_speaker(&t.speech).1, ToSpeaker::Resume);
}

#[test]
fn words_after_a_barge_in_stop_the_last_reply() {
    let t = topics();
    let (mut c, events) = pal(&t, true);
    say(&t, &mut c, QUESTION, Event::Segment, true);
    assert_eq!(reply_of(next(&t.llm)), "turn-1");
    for text in ["Feuer und Eis.", "Und viel Wind."] {
        report(&mut c, Spoken::Said { turn: "turn-1".into(), text: text.into(), at: Instant::now() });
    }
    report(&mut c, Spoken::Audible { turn: "turn-1".into(), at: Instant::now() });
    report(&mut c, Spoken::Done { turn: "turn-1".into() });
    next(&t.llm);

    c.handle(Some(Input::Event(Event::BargeIn { at_ms: 0 }))).unwrap();
    assert_eq!(to_speaker(&t.speech).1, ToSpeaker::Pause);
    say(&t, &mut c, "Moment, warte mal.", Event::Segment, true);
    assert_eq!(to_speaker(&t.speech), ("turn-1".into(), ToSpeaker::Stop));
    assert_eq!(reply_of(next(&t.llm)), "turn-2");
    assert!(events.try_iter().any(|e| matches!(e, AgentEvent::Interrupted)));
}

#[test]
fn speech_over_the_voice_does_not_spoil_a_pending_question() {
    let t = topics();
    let (mut c, events) = pal(&t, false);
    say(&t, &mut c, QUESTION, Event::Segment, true);
    assert_eq!(reply_of(next(&t.llm)), "turn-1");
    say(&t, &mut c, "Und was ist mit Hekla?", Event::Segment, true);
    let echo = |seg: Segment| Event::Segment(Segment { during_output: true, ..seg });
    say(&t, &mut c, "Feuer und Eis.", echo, true);
    assert!(events.try_iter().any(|e| matches!(e, AgentEvent::Ignored)));

    report(&mut c, Spoken::Done { turn: "turn-1".into() });
    assert!(matches!(LlmRequest::from_message(&next(&t.llm)).unwrap(), LlmRequest::Prefill(_)));
    assert_eq!(reply_of(next(&t.llm)), "turn-2");
}

#[test]
fn a_lost_microphone_or_stop_ends_the_conversation() {
    let t = topics();
    let (mut c, _events) = pal(&t, false);
    assert!(matches!(c.handle(Some(Input::Event(Event::Lost))), Err(Error::Audio(_))));
    c.shared.quit.store(true, Ordering::SeqCst);
    assert!(matches!(c.handle(None), Err(Error::Stopped)));
}

#[test]
fn while_prefilling_speech_is_dropped_and_a_failure_ends_it() {
    let t = topics();
    let (mut c, _events) = pal(&t, false);
    c.phase = Phase::Prefilling { since: Instant::now() };
    say(&t, &mut c, QUESTION, Event::Segment, true);
    assert!(matches!(c.phase, Phase::Prefilling { .. }));
    nothing(&t.llm);
    let failed = Spoken::Failed { turn: String::new(), error: "no model".into() };
    assert!(matches!(c.handle(Some(Input::Spoken(failed))), Err(Error::Llm(_))));
}
