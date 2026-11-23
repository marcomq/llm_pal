//! One input while a turn runs: passes the spoken sentences on, settles a held reply on the segment's end (speak
//! it) or the user going on (drop it), and ends the reply on words after a barge-in. Speech that doesn't
//! interrupt the reply is answered after it (`pending`).

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::answer::speech_end;
use super::turn::{Cue, Turn};
use super::unfinished::{UNFINISHED_WAIT, waits_for_more};
use super::{AgentEvent, Conversation, Phase};
use crate::hear::event::{Event, Segment};
use crate::transport::{Input, Spoken};

impl Conversation {
    /// `None` is a tick: no input for a while.
    pub(super) fn follow(&mut self, mut t: Turn, input: Option<Input>) -> Phase {
        if let Some((arrived, end)) = t.ending.take_if(|(at, _)| at.elapsed() >= UNFINISHED_WAIT)
            && !self.release(&mut t, arrived, &end)
        {
            return self.drop_turn(t);
        }
        let Some(input) = input else {
            self.resume_if_quiet();
            return Phase::Replying(t);
        };
        match input {
            Input::Spoken(Spoken::Said { turn, text, at }) if turn == t.id => {
                t.first_sentence.get_or_insert(at);
                self.mic.output_active.store(true, Ordering::Relaxed);
                self.send(AgentEvent::Said(text.clone()));
                t.spoken.push(text);
            }
            Input::Spoken(Spoken::FirstPiece { turn, at }) if turn == t.id => {
                t.first_piece.get_or_insert(at);
            }
            Input::Spoken(Spoken::Done { turn }) if turn == t.id => return self.done(t),
            Input::Spoken(Spoken::Failed { turn, error }) if turn == t.id => {
                eprintln!("llm_pal: {error}");
                // Nothing of it played, so no `Quiet` may follow the `Said` that marked the voice active.
                if t.first_audio.is_none() {
                    self.mic.output_active.store(false, Ordering::Relaxed);
                }
                self.send(AgentEvent::ReplyFailed(error));
                return self.drop_turn(t);
            }
            Input::Spoken(_) | Input::Typed(_) => {}
            // The user goes on after an unfinished sentence: the held reply was too early.
            Input::Event(event @ Event::SpeechStart { .. }) if t.ending.is_some() => {
                let (_, end) = t.ending.take().expect("the guard checked it");
                self.carry = Some([self.carry.take().unwrap_or_default(), end.pcm].concat());
                self.drop_turn(t);
                return self.listening(Some(Input::Event(event)));
            }
            Input::Event(Event::Resume) if t.held && t.ending.is_none() => return self.drop_turn(t),
            Input::Event(Event::Segment(end)) if t.held && t.ending.is_none() => {
                let arrived = Instant::now();
                if waits_for_more(&t.p.text, end.turn_complete, self.cfg.language) {
                    t.ending = Some((arrived, end));
                } else if !self.release(&mut t, arrived, &end) {
                    return self.drop_turn(t);
                }
            }
            Input::Event(_) if t.held => {}
            Input::Event(Event::BargeIn { .. }) if self.cfg.barge_in => self.pause_for_barge_in(),
            // Speech after a barge-in ends the reply; whether its paused voice goes on depends on the words.
            Input::Event(event @ (Event::Pause(_) | Event::Segment(_))) if self.barge.is_some() => {
                if t.first_audio.is_none() {
                    return self.go_on(t, event);
                }
                return self.barged(t, event);
            }
            // As in `cue`: without barge-in, speech over Pal's voice may be that voice, so it isn't merged.
            Input::Event(Event::Segment(seg)) if seg.during_output && !self.cfg.barge_in => {
                self.send(AgentEvent::Ignored);
            }
            Input::Event(Event::Segment(seg)) => {
                self.pending = Some(match self.pending.take() {
                    Some(before) => Segment { pcm: [before.pcm, seg.pcm].concat(), ..seg },
                    None => seg,
                });
            }
            Input::Event(Event::SpeechStart { .. }) => {
                if let Some(party) = &mut self.party {
                    party.speech();
                }
                // The user goes on before the reply is heard: it waits for what they say (`go_on`).
                if t.first_audio.is_none() && self.barge.is_none() {
                    self.pause_for_barge_in();
                }
            }
            Input::Event(_) => {}
        }
        Phase::Replying(t)
    }

    /// The user went on before the reply was heard. Words make one turn of both: the reply is dropped and the speech
    /// it answered goes before the new one, transcribed as a whole. No words: after a segment, the reply plays.
    fn go_on(&mut self, t: Turn, event: Event) -> Phase {
        let (Event::Pause(seg) | Event::Segment(seg)) = &event else { return Phase::Replying(t) };
        if !self.transport.transcribe(&seg.pcm).is_ok_and(|text| !text.is_empty()) {
            if matches!(event, Event::Segment(_)) {
                self.resume_barged();
            }
            return Phase::Replying(t);
        }
        if let Err(e) = self.transport.interrupt(&t.id) {
            eprintln!("llm_pal: {e}");
        }
        self.barge = None;
        self.tags = t.tags_before;
        self.carry = Some([self.carry.take().unwrap_or_default(), t.p.pcm].concat());
        self.listening(Some(Input::Event(event)))
    }

    /// Speech after a barge-in while the reply is heard. Words end the reply, which may still be generating, and are
    /// answered; no words, or room talk in party mode, let it go on.
    fn barged(&mut self, t: Turn, event: Event) -> Phase {
        let t0 = Instant::now();
        let Some(Cue::Speech(seg, early)) = self.cue(Some(Input::Event(event))) else { return Phase::Replying(t) };
        let Some((text, text_at)) = self.transcribe(&seg, early) else { return Phase::Replying(t) };
        if self.party.as_ref().is_some_and(|p| !p.called(&text)) && self.persona.joke(&text).is_none() {
            let stt_ms = text_at.duration_since(t0).as_millis();
            self.answer(seg, early, text, t0, stt_ms, text_at);
            return Phase::Replying(t);
        }
        self.transport.cancel(&t.id);
        self.done(t);
        self.understood(seg, early, text, t0, text_at)
    }

    /// The speech of a held reply ended at `end`, which arrived at `arrived`: the reply may play. `false` if the
    /// routes didn't take the release.
    fn release(&mut self, t: &mut Turn, arrived: Instant, end: &Segment) -> bool {
        self.carry = None;
        self.send(AgentEvent::Heard(t.p.line.clone()));
        t.p.ended = Some(speech_end(arrived, end));
        t.held = false;
        match self.transport.release(&t.id) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("llm_pal: {e}");
                false
            }
        }
    }
}
