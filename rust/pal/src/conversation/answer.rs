//! What Pal answers to what it heard: `transcribe` turns a segment into text, `answer` picks a canned reply or a
//! prompt line for the model and starts the turn; `silence` does the same for the room's talk in party mode.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{AgentEvent, Conversation, Phase};
use crate::hear::event::Segment;
use crate::think::enrich;
use crate::think::persona::Persona;
use crate::think::prosody_tags;

impl Conversation {
    /// The transcript of the speech and when it was ready; `None` if nothing was understood.
    pub(super) fn transcribe(&mut self, seg: &Segment, early: bool) -> Option<(String, Instant)> {
        // `seg.pcm` already starts with the carry; a final segment uses it up, whatever its transcript.
        if !early {
            self.carry = None;
        }
        let text = match self.transport.transcribe(&seg.pcm) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("llm_pal: {e}");
                return None;
            }
        };
        let text_at = Instant::now();
        if let Some(dir) = &self.record_dir
            && let Err(e) = record(dir, &seg.pcm, &text)
        {
            eprintln!("llm_pal: recording to {}: {e}", dir.display());
        }
        if text.is_empty() {
            if !early {
                self.resume_barged();
            }
            return None;
        }
        Some((text, text_at))
    }

    /// Picks the reply to the transcript and starts it, unless there is nothing to answer.
    pub(super) fn answer(
        &mut self,
        seg: Segment,
        early: bool,
        text: String,
        t0: Instant,
        stt_ms: u128,
        text_at: Instant,
    ) -> Phase {
        let tags_before = self.tags.clone();
        let tags = prosody_tags::format(&self.tags.tag(&seg.features), self.cfg.language);
        let line = Persona::user_line(&text, &tags);
        let joke = self.persona.joke(&text);
        if let Some(party) = &mut self.party {
            if joke.is_none() && !party.addressed(&text) {
                if early {
                    self.tags = tags_before;
                } else {
                    party.overhear(text);
                    self.send(AgentEvent::Heard(line));
                    self.resume_barged();
                }
                return Phase::Listening;
            }
            party.forget();
        }
        if self.barge.take().is_some() {
            self.interrupt_last_reply();
        }
        if !early {
            self.send(AgentEvent::Heard(line.clone()));
        }
        let reply = self.reply_to(joke, &text, &line);
        let ended = (!early).then(|| speech_end(t0, &seg));
        self.start(Prepared { text, line, early, ended, stt_ms, text_at, reply, pcm: seg.pcm }, t0, tags_before)
    }

    /// A typed line is meant for Pal, also in party mode: it ends whatever runs and is answered without tags.
    pub(super) fn typed(&mut self, phase: Phase, text: String) -> Phase {
        let text = text.trim().to_string();
        if text.is_empty() {
            return phase;
        }
        match phase {
            Phase::Replying(t) if t.held => drop(self.drop_turn(t)),
            Phase::Replying(t) => {
                if let Err(e) = self.transport.interrupt(&t.id) {
                    eprintln!("llm_pal: {e}");
                }
                self.done(t);
            }
            _ => {}
        }
        self.carry = None;
        if self.barge.take().is_some() || self.speaking() {
            self.interrupt_last_reply();
        }
        if let Some(party) = &mut self.party {
            party.forget();
        }
        let t0 = Instant::now();
        if let Some(t) = self.timing.take() {
            t.finish(None, &|e| self.send(e));
        }
        self.send(AgentEvent::Heard(text.clone()));
        let joke = self.persona.joke(&text);
        let reply = self.reply_to(joke, &text, &text);
        let tags_before = self.tags.clone();
        let p = Prepared {
            line: text.clone(),
            text,
            early: false,
            ended: Some(t0),
            stt_ms: 0,
            text_at: t0,
            reply,
            pcm: Vec::new(),
        };
        self.start(p, t0, tags_before)
    }

    fn reply_to(&mut self, joke: Option<String>, text: &str, line: &str) -> Reply {
        let canned = joke.or_else(|| self.persona.quip(text)).or_else(|| self.persona.pooled(text));
        canned.map_or_else(|| Reply::Model(enrich::prompt_line(text, line, self.cfg.language)), Reply::Canned)
    }

    /// Party mode: the room went quiet after talk Pal didn't answer.
    pub(super) fn silence(&mut self, talk: Vec<String>, prompt: String, t0: Instant) -> Prepared {
        eprintln!("llm_pal: the room went quiet");
        let canned = talk.iter().rev().find_map(|t| self.persona.quip(t).or_else(|| self.persona.pooled(t)));
        let reply = canned.map_or_else(|| Reply::Model(prompt.clone()), Reply::Canned);
        Prepared {
            text: talk.join(" "),
            line: prompt,
            early: false,
            ended: Some(t0),
            stt_ms: 0,
            text_at: t0,
            reply,
            pcm: Vec::new(),
        }
    }
}

pub(super) enum Reply {
    Canned(String),
    /// The model answers this prompt line.
    Model(String),
}

/// A turn ready to start: what was heard and how Pal answers it.
pub(super) struct Prepared {
    pub(super) text: String,
    pub(super) line: String,
    pub(super) early: bool,
    pub(super) ended: Option<Instant>,
    pub(super) stt_ms: u128,
    pub(super) text_at: Instant,
    pub(super) reply: Reply,
    /// The speech answered, with the carry before it.
    pub(super) pcm: Vec<f32>,
}

/// When the user stopped speaking, for a segment that arrived at `at`.
pub(super) fn speech_end(at: Instant, seg: &Segment) -> Instant {
    at.checked_sub(Duration::from_millis(seg.silence_ms)).unwrap_or(at)
}

/// `PAL_RECORD_DIR`: each segment as 16 kHz WAV plus the recognised text, as STT test data.
fn record(dir: &Path, pcm: &[f32], text: &str) -> std::result::Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let name = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis().to_string();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(dir.join(format!("{name}.wav")), spec)?;
    for s in pcm {
        wav.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    wav.finalize()?;
    std::fs::write(dir.join(format!("{name}.stt.txt")), text)?;
    Ok(())
}
