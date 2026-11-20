//! One input while a turn runs: passes the spoken sentences on, settles a held reply on the segment's end (speak
//! it) or the user going on (drop it), and ends the reply on speech after a barge-in. Speech that doesn't
//! interrupt the reply is answered after it (`pending`).

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::answer::speech_end;
use super::turn::Turn;
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
                self.send(AgentEvent::ReplyFailed(error));
                return self.drop_turn(t);
            }
            Input::Spoken(_) => {}
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
                self.transport.cancel(&t.id);
                self.done(t);
                return self.listening(Some(Input::Event(event)));
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
            }
            Input::Event(_) => {}
        }
        Phase::Replying(t)
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
