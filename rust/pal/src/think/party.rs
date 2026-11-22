//! Party mode: Pal listens to the room and speaks only when called by name, asked for a joke, or when the talk
//! stops.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::think::persona::Persona;

/// How long the room must be quiet after talk before Pal says something.
const SILENCE: Duration = Duration::from_secs(5);
/// Lines of the room's talk a remark can be about.
const KEPT_LINES: usize = 6;
/// How long after Pal's question speech may begin and count as the answer, without his name.
const ANSWER_WINDOW: Duration = Duration::from_secs(8);
/// Speech that begins this soon after his voice stopped may still be its echo.
const ECHO_TAIL: Duration = Duration::from_millis(150);

pub struct Party {
    call_words: Vec<String>,
    silence_prompt: String,
    /// Transcripts not meant for Pal since he last spoke.
    talk: VecDeque<String>,
    last_speech: Instant,
    /// When Pal's last reply, a question, ended.
    question: Option<Instant>,
    /// The voice of that reply has stopped; a later stop belongs to another reply.
    voice_over: bool,
    /// The current speech began in time to answer that question.
    answering: bool,
}

impl Party {
    pub fn new(persona: &Persona) -> Self {
        Self {
            call_words: persona.call_words().to_vec(),
            silence_prompt: persona.silence_prompt().to_string(),
            talk: VecDeque::new(),
            last_speech: Instant::now(),
            question: None,
            voice_over: false,
            answering: false,
        }
    }

    /// True if `text` is meant for Pal: it names him, or answers his question.
    pub fn addressed(&self, text: &str) -> bool {
        self.called(text) || self.answering
    }

    /// Pal's reply is complete; its voice may still be playing.
    pub fn replied(&mut self, reply: &str) {
        self.question = reply.trim_end().ends_with('?').then(Instant::now);
        self.voice_over = false;
        self.answering = false;
    }

    /// Pal's voice has stopped: the time to answer his question starts now, once per question.
    pub fn voice_ended(&mut self) {
        if let Some(at) = self.question.as_mut().filter(|_| !self.voice_over) {
            *at = Instant::now();
            self.voice_over = true;
        }
    }

    /// True if `text` calls Pal by one of his names.
    pub fn called(&self, text: &str) -> bool {
        text.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| self.call_words.iter().any(|n| n == w))
    }

    /// Someone speaks; the silence starts again after it.
    pub fn speech(&mut self) {
        self.last_speech = Instant::now();
        self.answering =
            self.voice_over && self.question.is_some_and(|at| (ECHO_TAIL..ANSWER_WINDOW).contains(&at.elapsed()));
    }

    pub fn overhear(&mut self, text: String) {
        self.question = None;
        self.answering = false;
        self.talk.push_back(text);
        while self.talk.len() > KEPT_LINES {
            self.talk.pop_front();
        }
        self.last_speech = Instant::now();
    }

    /// Pal answered someone; the talk before it needs no remark.
    pub fn forget(&mut self) {
        self.talk.clear();
    }

    /// The talk since Pal last spoke, once the room has been quiet for [`SILENCE`] after it.
    pub fn quiet(&mut self) -> Option<Vec<String>> {
        (!self.talk.is_empty() && self.last_speech.elapsed() >= SILENCE).then(|| self.talk.drain(..).collect())
    }

    /// The model's prompt for a remark on `talk`.
    pub fn prompt(&self, talk: &[String]) -> String {
        let quoted: Vec<String> = talk.iter().map(|line| format!("\"{line}\"")).collect();
        self.silence_prompt.replace("{talk}", &quoted.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::think::language::Language;
    use crate::think::persona::DEFAULT_NAME;

    #[test]
    fn a_line_with_kitts_name_calls_him() {
        let party = Party::new(&Persona::kitt(Language::German, DEFAULT_NAME));
        assert!(party.called("Kitt, wie spät ist es?"));
        assert!(party.called("Hey Kit."));
        assert!(!party.called("Wir brauchen noch Kittharz."));
    }

    #[test]
    fn speech_right_after_a_question_answers_it() {
        let mut party = Party::new(&Persona::kitt(Language::German, DEFAULT_NAME));
        party.replied("Soll ich das erklären?");
        party.speech();
        assert!(!party.addressed("Ja, gerne."), "over his voice");
        party.voice_ended();
        party.speech();
        assert!(!party.addressed("Ja, gerne."), "may be the echo");
        party.question = party.question.map(|at| at - ECHO_TAIL);
        party.speech();
        assert!(party.addressed("Ja, gerne."));
        party.overhear("Das Essen ist gut.".into());
        party.speech();
        assert!(!party.addressed("Ja, gerne."));
    }

    #[test]
    fn a_later_voice_stop_does_not_renew_an_old_question() {
        let mut party = Party::new(&Persona::kitt(Language::German, DEFAULT_NAME));
        party.replied("Soll ich das erklären?");
        party.voice_ended();
        party.question = party.question.map(|at| at - ANSWER_WINDOW);
        party.voice_ended();
        party.speech();
        assert!(!party.addressed("Ja, gerne."));
    }

    #[test]
    fn no_answer_without_a_question_or_after_the_window() {
        let mut party = Party::new(&Persona::kitt(Language::German, DEFAULT_NAME));
        party.replied("Das ist erledigt.");
        party.speech();
        assert!(!party.addressed("Ja, gerne."));
        party.replied("Soll ich das erklären?");
        party.voice_ended();
        party.question = party.question.map(|at| at - ANSWER_WINDOW);
        party.speech();
        assert!(!party.addressed("Ja, gerne."));
        assert!(party.addressed("Kitt, ja."));
    }

    #[test]
    fn talk_is_given_once_after_the_silence() {
        let mut party = Party::new(&Persona::kitt(Language::German, DEFAULT_NAME));
        assert_eq!(party.quiet(), None);
        party.overhear("Das Essen ist gut.".into());
        assert_eq!(party.quiet(), None);
        party.last_speech -= SILENCE;
        assert_eq!(party.quiet(), Some(vec!["Das Essen ist gut.".to_string()]));
        assert_eq!(party.quiet(), None);
    }

    #[test]
    fn answering_someone_forgets_the_talk() {
        let mut party = Party::new(&Persona::kitt(Language::English, DEFAULT_NAME));
        party.overhear("Nice music.".into());
        party.forget();
        party.last_speech -= SILENCE;
        assert_eq!(party.quiet(), None);
    }

    #[test]
    fn the_prompt_quotes_the_talk() {
        let party = Party::new(&Persona::kitt(Language::English, DEFAULT_NAME));
        let prompt = party.prompt(&["Nice music.".into(), "Who chose it?".into()]);
        assert!(prompt.contains("\"Nice music.\" \"Who chose it?\""));
    }
}
