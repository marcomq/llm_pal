//! The commentator's system prompt, sample turns and a short memory of the last turns.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};

use crate::language::{Language, Quip};
use crate::llm::{Message, Role};

/// Jokes per start: enough variety, few enough that a small model still follows the prompt.
const JOKES_PER_START: usize = 10;

/// Longer lines are about more than a quip's trigger and go to the model.
const QUIP_MAX_WORDS: usize = 10;

pub struct Persona {
    system: String,
    /// Fixed sample turns after the system prompt; they show the tone better than a description.
    examples: &'static [(&'static str, &'static str)],
    /// This start's jokes; asked for one, Pal tells the next of them instead of letting the model retell it.
    jokes: Vec<&'static str>,
    next_joke: usize,
    joke_intros: &'static [&'static str],
    joke_words: &'static [&'static str],
    quips: &'static [Quip],
    /// Per quip, how many of its answers were given, starting at a random count.
    quips_given: Vec<usize>,
    history: VecDeque<(String, String)>,
    max_turns: usize,
}

impl Persona {
    pub fn kitt(language: Language) -> Self {
        let p = language.profile();
        let jokes = pick(p.jokes.lines(), JOKES_PER_START);
        let list: String = jokes.iter().map(|j| format!("\n{j}")).collect();
        Self {
            examples: p.examples,
            jokes,
            joke_intros: p.joke_intros,
            joke_words: p.joke_words,
            quips: p.quips,
            quips_given: (0..p.quips.len()).map(|i| RandomState::new().hash_one(i) as usize).collect(),
            ..Self::new(format!("{}\n{}{list}", p.system_prompt, p.joke_offer))
        }
    }

    /// A joke with a short intro if `transcript` asks for one, so it comes out word for word.
    pub fn joke(&mut self, transcript: &str) -> Option<String> {
        let asked = transcript.to_lowercase();
        let mut words = asked.split(|c: char| !c.is_alphanumeric());
        if self.jokes.is_empty() || !words.any(|w| self.joke_words.contains(&w)) {
            return None;
        }
        let i = self.next_joke;
        self.next_joke += 1;
        let intro = self.joke_intros[i % self.joke_intros.len()];
        Some(format!("{intro} {}", self.jokes[i % self.jokes.len()]))
    }

    /// A fixed answer if `transcript` is a short line a quip knows; each time the next of its answers.
    pub fn quip(&mut self, transcript: &str) -> Option<String> {
        let line = transcript.to_lowercase();
        let words: Vec<&str> = line.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        if words.len() > QUIP_MAX_WORDS {
            return None;
        }
        let said = |trigger: &&str| trigger.split(' ').all(|w| words.contains(&w));
        let i = self.quips.iter().position(|q| q.triggers.iter().any(said))?;
        let answers = self.quips[i].answers;
        self.quips_given[i] += 1;
        Some(answers[self.quips_given[i] % answers.len()].to_string())
    }

    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            examples: &[],
            jokes: Vec::new(),
            next_joke: 0,
            joke_intros: &[],
            joke_words: &[],
            quips: &[],
            quips_given: Vec::new(),
            history: VecDeque::new(),
            max_turns: 3,
        }
    }

    /// The system prompt and sample turns, which start every prompt unchanged.
    pub fn fixed_messages(&self) -> usize {
        1 + 2 * self.examples.len()
    }

    /// The user's line as the model sees it: prosody tags, then the transcript.
    pub fn user_line(transcript: &str, tags: &str) -> String {
        if tags.is_empty() { transcript.to_string() } else { format!("{tags} {transcript}") }
    }

    pub fn messages(&self, user_line: &str) -> Vec<Message> {
        let mut m = self.context();
        m.push(Message::new(Role::User, user_line));
        m
    }

    /// Everything before the user's next line: system prompt, sample turns and history.
    pub fn context(&self) -> Vec<Message> {
        let mut m = vec![Message::new(Role::System, &self.system)];
        for &(u, a) in self.examples {
            m.push(Message::new(Role::User, u));
            m.push(Message::new(Role::Assistant, a));
        }
        for (u, a) in &self.history {
            m.push(Message::new(Role::User, u));
            m.push(Message::new(Role::Assistant, a));
        }
        m
    }

    /// A reply that is already in the history is not kept again, so a small model doesn't settle into repeating it.
    pub fn remember(&mut self, user_line: String, reply: String) {
        if reply.is_empty() || self.history.iter().any(|(_, a)| *a == reply) {
            return;
        }
        self.history.push_back((user_line, reply));
        while self.history.len() > self.max_turns {
            self.history.pop_front();
        }
    }
}

/// `n` random items, in random order.
fn pick<'a>(items: impl Iterator<Item = &'a str>, n: usize) -> Vec<&'a str> {
    let mut items: Vec<_> = items.filter(|l| !l.trim().is_empty()).collect();
    let mut seed = RandomState::new().hash_one(0u8) | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    items.truncate(n);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_turns_in_order() {
        let mut p = Persona::new("sys");
        for i in 0..5 {
            p.remember(format!("u{i}"), format!("a{i}"));
        }
        p.remember("ignored".into(), String::new());
        let m = p.messages(&Persona::user_line("Hello", "[question]"));
        let text: Vec<_> = m.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(text, ["sys", "u2", "a2", "u3", "a3", "u4", "a4", "[question] Hello"]);
        assert_eq!(m[1].role, Role::User);
        assert_eq!(m[2].role, Role::Assistant);
    }

    #[test]
    fn kitt_gets_ten_different_jokes() {
        for language in Language::ALL {
            let p = Persona::kitt(language);
            let jokes: Vec<_> = language.profile().jokes.lines().filter(|j| p.system.contains(j)).collect();
            assert_eq!(jokes.len(), JOKES_PER_START, "{language:?}");
        }
    }

    #[test]
    fn asked_for_a_joke_pal_tells_the_next_one() {
        let mut p = Persona::kitt(Language::English);
        assert_eq!(p.joke("How are you?"), None);
        let (a, b) = (p.joke("Tell me a joke").unwrap(), p.joke("[question] Another JOKE?").unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("Gladly, Michael.") && a.ends_with(p.jokes[0]));
        assert!(Persona::kitt(Language::German).joke("Erzähl einen Witz").is_some());
    }

    #[test]
    fn words_that_only_contain_joke_ask_for_none() {
        assert_eq!(Persona::kitt(Language::German).joke("Das ist witzig"), None);
        assert_eq!(Persona::kitt(Language::English).joke("I'm not joking"), None);
    }

    #[test]
    fn short_lines_get_the_quips_answers_in_turn() {
        let mut p = Persona::kitt(Language::German);
        let (a, b) = (p.quip("Wie ist das Wetter?").unwrap(), p.quip("Und wie wird das Wetter morgen?").unwrap());
        assert_ne!(a, b);
        assert!(p.quip("Wie geht's dir, Pal?").is_some());
        assert_eq!(p.quip("Wie geht das?"), None, "every word of a trigger must occur");
        assert_eq!(p.quip("Bei dem Wetter habe ich heute wirklich überhaupt keine Lust, noch rauszugehen"), None);
        assert!(Persona::kitt(Language::English).quip("How are you today?").is_some());
    }

    #[test]
    fn every_quip_has_answers_and_lowercase_triggers() {
        for language in Language::ALL {
            for q in language.profile().quips {
                assert!(!q.answers.is_empty() && !q.triggers.is_empty(), "{language:?}");
                assert!(q.triggers.iter().all(|t| *t == t.to_lowercase()), "{:?}", q.triggers);
            }
        }
    }

    #[test]
    fn a_repeated_reply_is_not_kept_again() {
        let mut p = Persona::new("sys");
        p.remember("u0".into(), "Sorry.".into());
        p.remember("u1".into(), "Sorry.".into());
        assert_eq!(p.messages("u2").len(), 4, "sys, u0, reply, u2");
    }
}
