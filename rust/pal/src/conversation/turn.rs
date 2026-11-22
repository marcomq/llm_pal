//! One turn: `cue` decides whether an input asks for an answer, `turn` answers it, `follow.rs` passes the
//! reply on until `done`.
//!
//! ```text
//! cue          Pause → reply prepared but held, Segment → answered, BargeIn → voice paused (barge_in.rs)
//! turn         transcribe; an unfinished sentence waits first                       (unfinished.rs)
//!   answer     canned reply or prompt line, or silence in party mode               (answer.rs)
//!   start      transport.reply → Phase::Replying
//! follow       one report or event while the reply runs                            (follow.rs)
//! done         timing, history, prefill for the next turn
//! ```

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::answer::{Prepared, Reply};
use super::unfinished::{UNFINISHED_WAIT, waits_for_more};
use super::{AgentEvent, Conversation, Phase, State};
use crate::hear::event::{Event, Segment};
use crate::think::prosody_tags::ProsodyTags;
use crate::transport::Input;

impl Conversation {
    /// What the input asks Pal to answer, if anything. A reply to a pause is prepared but held back until the
    /// segment really ends.
    pub(super) fn cue(&mut self, input: Option<Input>) -> Option<Cue> {
        let speaking = self.speaking();
        if let Some(party) = &mut self.party
            && input.is_none()
            && !speaking
            && !self.mic.in_speech.load(Ordering::Relaxed)
        {
            let talk = party.quiet()?;
            return Some(Cue::Silence { prompt: party.prompt(&talk), talk });
        }
        let (mut seg, early) = match input? {
            Input::Event(Event::SpeechStart { .. }) => {
                if let Some(party) = &mut self.party {
                    party.speech();
                }
                return None;
            }
            Input::Event(Event::BargeIn { .. }) => {
                if self.cfg.barge_in {
                    self.pause_for_barge_in();
                }
                return None;
            }
            Input::Event(Event::Pause(seg)) => (seg, true),
            Input::Event(Event::Segment(seg)) => (seg, false),
            Input::Event(Event::Resume | Event::Lost) | Input::Spoken(_) => return None,
        };
        if let Some(before) = &self.carry {
            seg.pcm = [before.as_slice(), &seg.pcm].concat();
        }
        // Without barge-in, speech that overlaps Pal's output is ignored: it may be Pal's own voice.
        if seg.during_output && !self.cfg.barge_in {
            if !early {
                self.send(AgentEvent::Ignored);
            }
            return None;
        }
        Some(Cue::Speech(seg, early))
    }

    pub(super) fn turn(&mut self, cue: Cue) -> Phase {
        self.shared.set(State::Thinking);
        let t0 = Instant::now();
        if let Some(t) = self.timing.take() {
            t.finish(None, &|e| self.send(e));
        }
        let (seg, early) = match cue {
            Cue::Silence { talk, prompt } => {
                let tags_before = self.tags.clone();
                let p = self.silence(talk, prompt, t0);
                return self.start(p, t0, tags_before);
            }
            Cue::Speech(seg, early) => (seg, early),
        };
        let Some((text, text_at)) = self.transcribe(&seg, early) else { return Phase::Listening };
        self.understood(seg, early, text, t0, text_at)
    }

    /// The transcript is there: an unfinished sentence waits for more, anything else is answered.
    pub(super) fn understood(
        &mut self,
        seg: Segment,
        early: bool,
        text: String,
        t0: Instant,
        text_at: Instant,
    ) -> Phase {
        let stt_ms = text_at.duration_since(t0).as_millis();
        if !early && waits_for_more(&text, seg.turn_complete, self.cfg.language) {
            return Phase::Unfinished { until: text_at + UNFINISHED_WAIT, seg, text, t0, stt_ms, text_at };
        }
        self.answer(seg, early, text, t0, stt_ms, text_at)
    }

    /// Sends the reply into the reply routes; a reply to a pause stays held until the speech ends.
    pub(super) fn start(&mut self, p: Prepared, t0: Instant, tags_before: ProsodyTags) -> Phase {
        self.turns += 1;
        let id = format!("turn-{}", self.turns);
        self.last_reply = None;
        let started = match &p.reply {
            Reply::Canned(canned) => self.transport.canned(&id, canned, p.early),
            Reply::Model(prompt) => self.transport.reply(&id, &self.persona.messages(prompt), p.early),
        };
        if let Err(e) = started {
            eprintln!("llm_pal: {e}");
            self.tags = tags_before;
            return Phase::Listening;
        }
        Phase::Replying(Turn {
            id,
            held: p.early,
            p,
            ending: None,
            t0,
            sent_at: Instant::now(),
            tags_before,
            spoken: Vec::new(),
            first_piece: None,
            first_sentence: None,
            first_audio: None,
        })
    }

    /// The reply is through: logs its timing, keeps it in the history and warms the LLM for the next turn.
    pub(super) fn done(&mut self, t: Turn) -> Phase {
        let ended = t.p.ended.unwrap_or(t.t0);
        let reply = t.spoken.join(" ");
        let timing = Timing {
            turn: t.id,
            ended,
            marks: vec![
                ("segment", Some(t.t0)),
                ("text", Some(t.p.text_at)),
                ("prompt sent", Some(t.sent_at)),
                ("first token", t.first_piece),
                ("first sentence", t.first_sentence),
            ],
            reply: reply.clone(),
            stt_ms: t.p.stt_ms,
            total_ms: ended.elapsed().as_millis(),
        };
        match t.first_audio {
            Some(audio) => timing.finish(Some(audio), &|e| self.send(e)),
            None => self.timing = Some(timing),
        }
        if let Some(party) = &mut self.party {
            party.replied(&reply);
        }
        if self.persona.remember(t.p.line, reply) {
            self.last_reply = Some(t.spoken);
        }
        if let Err(e) = self.transport.prefill(&self.persona.context()) {
            eprintln!("llm_pal: {e}");
        }
        Phase::Listening
    }

    /// A sentence of `turn` began to play at `at`: counted for a barge-in, the first one ends the turn's timing.
    pub(super) fn audible(&mut self, turn: &str, at: Instant) {
        self.mic.output_active.store(true, Ordering::Relaxed);
        if self.heard.0 == turn {
            self.heard.1 += 1;
            return;
        }
        self.heard = (turn.to_string(), 1);
        if let Phase::Replying(t) = &mut self.phase
            && t.id == turn
        {
            t.first_audio = Some(at);
        } else if self.timing.as_ref().is_some_and(|t| t.turn == turn)
            && let Some(t) = self.timing.take()
        {
            t.finish(Some(at), &|e| self.send(e));
        }
    }

    /// The turn is dropped unheard: the routes discard it and the prosody tags forget it. If it answered a pause, the
    /// model reads its line ahead up to the last word, which may change as the user goes on; the rest likely stays.
    pub(super) fn drop_turn(&mut self, t: Turn) -> Phase {
        if let Err(e) = self.transport.discard(&t.id) {
            eprintln!("llm_pal: {e}");
        }
        if let (true, Reply::Model(prompt)) = (t.p.early, &t.p.reply) {
            let mark = prompt.len() - t.p.line.len() + t.p.text.trim_end().rfind(' ').unwrap_or(0);
            if let Err(e) = self.transport.read_ahead(&self.persona.messages(prompt), mark) {
                eprintln!("llm_pal: {e}");
            }
        }
        self.tags = t.tags_before;
        Phase::Listening
    }
}

/// A started turn, until its reply is done.
pub(super) struct Turn {
    pub(super) id: String,
    pub(super) p: Prepared,
    /// Held back until the speech really ends; starts as `p.early`.
    pub(super) held: bool,
    /// A held reply's final segment that sounds unfinished, and when it arrived: the reply is released
    /// `UNFINISHED_WAIT` later unless the user goes on.
    pub(super) ending: Option<(Instant, Segment)>,
    t0: Instant,
    sent_at: Instant,
    pub(super) tags_before: ProsodyTags,
    pub(super) spoken: Vec<String>,
    pub(super) first_piece: Option<Instant>,
    pub(super) first_sentence: Option<Instant>,
    /// When its first sentence began to play.
    pub(super) first_audio: Option<Instant>,
}

/// What a turn answers: speech up to a pause (`true`) or its end, or in party mode the room's talk before a
/// silence with the model's prompt about it.
pub(super) enum Cue {
    Speech(Segment, bool),
    Silence { talk: Vec<String>, prompt: String },
}

/// One turn's milestones, logged in ms from the end of speech once its first audio played; negative for a reply
/// prepared during a pause.
pub(super) struct Timing {
    turn: String,
    ended: Instant,
    marks: Vec<(&'static str, Option<Instant>)>,
    reply: String,
    stt_ms: u128,
    total_ms: u128,
}

impl Timing {
    /// Logs the milestones and reports the turn; `audio` is when its first sound played, if it did.
    pub(super) fn finish(self, audio: Option<Instant>, send: &dyn Fn(AgentEvent)) {
        let ms = |at: Instant| match at.checked_duration_since(self.ended) {
            Some(after) => after.as_millis() as i64,
            None => -(self.ended.duration_since(at).as_millis() as i64),
        };
        let parts: Vec<String> = (self.marks.iter().copied().chain([("audio", audio)]))
            .map(|(name, at)| at.map_or(format!("{name} -"), |at| format!("{name} {}", ms(at))))
            .collect();
        eprintln!("llm_pal: {} ms after end of speech: {}", self.turn, parts.join(", "));
        send(AgentEvent::TurnDone {
            reply: self.reply,
            stt_ms: self.stt_ms,
            first_sentence_ms: audio.map(|at| at.saturating_duration_since(self.ended).as_millis()),
            total_ms: self.total_ms,
        });
    }
}
