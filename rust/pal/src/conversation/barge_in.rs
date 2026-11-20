//! Barge-in: speech during Pal's output pauses its voice. Words end the reply and cut it in the history to what
//! was heard; no words within `RESUME_AFTER` let the voice go on.

use std::time::{Duration, Instant};

use super::{AgentEvent, Conversation};

/// Quiet after a barge-in with no words heard, after which Pal's paused voice goes on. Longer than the segmenter's
/// `end_silence_ms`, so a segment of real speech arrives first.
pub(super) const RESUME_AFTER: Duration = Duration::from_millis(1200);

impl Conversation {
    pub(super) fn pause_for_barge_in(&mut self) {
        if let Err(e) = self.transport.pause() {
            eprintln!("llm_pal: {e}");
        }
        self.barge = Some(None);
    }

    /// Lets Pal's paused voice go on after a barge-in with no words.
    pub(super) fn resume_barged(&mut self) {
        if self.barge.take().is_some()
            && let Err(e) = self.transport.resume()
        {
            eprintln!("llm_pal: {e}");
        }
    }

    pub(super) fn resume_if_quiet(&mut self) {
        let hearing = self.hearing();
        let Some(quiet_since) = &mut self.barge else { return };
        if hearing {
            *quiet_since = None;
        } else if quiet_since.get_or_insert_with(Instant::now).elapsed() >= RESUME_AFTER {
            self.resume_barged();
            eprintln!("llm_pal: no words after the barge-in, going on");
        }
    }

    /// Words after a barge-in: the last reply stops, and the history keeps only what was heard of it.
    pub(super) fn interrupt_last_reply(&mut self) {
        let said = self.last_reply.take().unwrap_or_default();
        let turn = format!("turn-{}", self.turns);
        let heard = if self.heard.0 == turn { self.heard.1.min(said.len()) } else { 0 };
        if let Err(e) = self.transport.interrupt(&turn) {
            eprintln!("llm_pal: {e}");
        }
        if heard < said.len() {
            self.persona.cut_last_reply(said[..heard].join(" "));
        }
        self.send(AgentEvent::Interrupted);
    }
}
