//! Party mode: Pal listens to the room and speaks only when called by name, asked for a joke, or when the talk
//! stops.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::think::language::Language;

/// How long the room must be quiet after talk before Pal says something.
const SILENCE: Duration = Duration::from_secs(5);
/// Lines of the room's talk a remark can be about.
const KEPT_LINES: usize = 6;

pub struct Party {
    language: Language,
    /// Transcripts not meant for Pal since he last spoke.
    talk: VecDeque<String>,
    last_speech: Instant,
}

impl Party {
    pub fn new(language: Language) -> Self {
        Self { language, talk: VecDeque::new(), last_speech: Instant::now() }
    }

    /// True if `text` calls Pal by one of his names.
    pub fn called(&self, text: &str) -> bool {
        let names = self.language.profile().call_words;
        text.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| names.contains(&w))
    }

    /// Someone speaks; the silence starts again after it.
    pub fn speech(&mut self) {
        self.last_speech = Instant::now();
    }

    pub fn overhear(&mut self, text: String) {
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
        self.language.profile().silence_prompt.replace("{talk}", &quoted.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_with_kitts_name_calls_him() {
        let party = Party::new(Language::German);
        assert!(party.called("Kitt, wie spät ist es?"));
        assert!(party.called("Hey Kit."));
        assert!(!party.called("Wir brauchen noch Kittharz."));
    }

    #[test]
    fn talk_is_given_once_after_the_silence() {
        let mut party = Party::new(Language::German);
        assert_eq!(party.quiet(), None);
        party.overhear("Das Essen ist gut.".into());
        assert_eq!(party.quiet(), None);
        party.last_speech -= SILENCE;
        assert_eq!(party.quiet(), Some(vec!["Das Essen ist gut.".to_string()]));
        assert_eq!(party.quiet(), None);
    }

    #[test]
    fn answering_someone_forgets_the_talk() {
        let mut party = Party::new(Language::English);
        party.overhear("Nice music.".into());
        party.forget();
        party.last_speech -= SILENCE;
        assert_eq!(party.quiet(), None);
    }

    #[test]
    fn the_prompt_quotes_the_talk() {
        let party = Party::new(Language::English);
        let prompt = party.prompt(&["Nice music.".into(), "Who chose it?".into()]);
        assert!(prompt.contains("\"Nice music.\" \"Who chose it?\""));
    }
}
