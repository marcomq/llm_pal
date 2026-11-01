//! The commentator's system prompt, sample turns and a short memory of the last turns.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};

use crate::language::Language;
use crate::llm::{Message, Role};

/// Jokes per start: enough variety, few enough that a small model still follows the prompt.
const JOKES_PER_START: usize = 10;

/// Longer lines are about more than a quip's trigger and go to the model.
const QUIP_MAX_WORDS: usize = 10;

/// Longer lines are about more than the pool turn they share words with and go to the model.
const POOL_MAX_WORDS: usize = 20;

/// The name the texts call the user; `Persona::kitt` puts the configured one in its place.
pub const DEFAULT_NAME: &str = "Michael";
/// A pool line fits only if it is negated like the transcript; "I'm not drinking beer" isn't "I'm drinking beer".
const NEGATIONS: &[&str] = &[
    "not", "no", "never", "nothing", "nicht", "kein", "keine", "keinen", "keinem", "keiner", "nie", "niemals", "nichts",
];

pub struct Persona {
    system: String,
    /// The user's name, replacing [`DEFAULT_NAME`] in every text.
    name: String,
    /// Fixed sample turns after the system prompt; they show the tone better than a description.
    examples: &'static [(&'static str, &'static str)],
    /// This start's jokes; asked for one, Pal tells the next of them instead of letting the model retell it.
    jokes: Vec<&'static str>,
    next_joke: usize,
    joke_intros: &'static [&'static str],
    joke_words: &'static [&'static str],
    quips: Vec<Quip>,
    /// Per quip, how many of its answers were given, starting at a random count.
    quips_given: Vec<usize>,
    /// Fixed turns: a line saying every content word of one gets its reply word for word.
    pool: Vec<(&'static str, &'static str)>,
    filler: &'static [&'static str],
    history: VecDeque<(String, String)>,
    max_turns: usize,
}

impl Persona {
    pub fn kitt(language: Language, name: &str) -> Self {
        let p = language.profile();
        let jokes = pick(p.jokes.lines(), JOKES_PER_START);
        let list: String = jokes.iter().map(|j| format!("\n{j}")).collect();
        let (quips, pool) = parse_pool(p.pool);
        Self {
            examples: p.examples,
            jokes,
            joke_intros: p.joke_intros,
            joke_words: p.joke_words,
            quips_given: (0..quips.len()).map(|i| RandomState::new().hash_one(i) as usize).collect(),
            quips,
            pool,
            filler: p.filler,
            name: name.to_string(),
            ..Self::new(format!("{}\n{}{list}", p.system_prompt, p.joke_offer).replace(DEFAULT_NAME, name))
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
        Some(self.named(&format!("{intro} {}", self.jokes[i % self.jokes.len()])))
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
        self.quips_given[i] += 1;
        let answers = &self.quips[i].answers;
        Some(self.named(answers[self.quips_given[i] % answers.len()]))
    }

    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            name: DEFAULT_NAME.to_string(),
            examples: &[],
            jokes: Vec::new(),
            next_joke: 0,
            joke_intros: &[],
            joke_words: &[],
            quips: Vec::new(),
            quips_given: Vec::new(),
            pool: Vec::new(),
            filler: &[],
            history: VecDeque::new(),
            max_turns: 3,
        }
    }

    /// The reply of the pool turn whose content words `transcript` all says, the one with the most words if several
    /// do; none whose reply is in the history.
    pub fn pooled(&self, transcript: &str) -> Option<String> {
        if transcript.split_whitespace().count() > POOL_MAX_WORDS {
            return None;
        }
        let said = self.content_words(transcript);
        let (_, reply) = self
            .pool
            .iter()
            .filter(|(u, _)| negated(u) == negated(transcript))
            .filter(|(_, a)| self.history.iter().all(|(_, r)| *r != self.named(a)))
            .map(|&(u, a)| (self.content_words(u), a))
            .filter(|(words, _)| !words.is_empty() && words.iter().all(|w| said.iter().any(|s| alike(s, w))))
            .min_by_key(|(words, _)| std::cmp::Reverse(words.len()))?;
        Some(self.named(reply))
    }

    fn named(&self, text: &str) -> String {
        text.replace(DEFAULT_NAME, &self.name)
    }

    /// Lowercase words of four letters or more, without prosody tags and filler words.
    fn content_words(&self, text: &str) -> Vec<String> {
        let lower = text.to_lowercase();
        let name = self.name.to_lowercase();
        let untagged: Vec<&str> = lower.split('[').map(|p| p.split_once(']').map_or(p, |(_, rest)| rest)).collect();
        let mut words: Vec<String> = untagged
            .join(" ")
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 4 && !self.filler.contains(w) && *w != name)
            .map(String::from)
            .collect();
        words.dedup();
        words
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
            m.push(Message::new(Role::User, self.named(u)));
            m.push(Message::new(Role::Assistant, self.named(a)));
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

/// A fixed answer to a common line. Each trigger is a set of lowercase words that must all occur; the answers take
/// turns.
struct Quip {
    triggers: Vec<&'static str>,
    answers: Vec<&'static str>,
}

/// Splits a pool file into its quips (`= trigger; trigger | reply | reply`) and sample turns (`line | reply`).
fn parse_pool(text: &'static str) -> (Vec<Quip>, Vec<(&'static str, &'static str)>) {
    let (mut quips, mut turns) = (Vec::new(), Vec::new());
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(quip) = line.strip_prefix('=') {
            let mut parts = quip.split('|').map(str::trim);
            let triggers = parts.next().unwrap_or_default().split(';').map(str::trim).collect();
            quips.push(Quip { triggers, answers: parts.collect() });
        } else if let Some((u, a)) = line.split_once('|') {
            turns.push((u.trim(), a.trim()));
        }
    }
    (quips, turns)
}

fn negated(text: &str) -> bool {
    let lower = text.to_lowercase().replace('’', "'");
    lower.contains("n't") || lower.split(|c: char| !c.is_alphanumeric()).any(|w| NEGATIONS.contains(&w))
}

/// The same word, or one sharing its first five letters ("arbeit", "arbeiten").
fn alike(a: &str, b: &str) -> bool {
    let stem = |w: &str| (w.chars().count() >= 5).then(|| w.chars().take(5).collect::<String>());
    a == b || stem(a).is_some_and(|s| Some(s) == stem(b))
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
            let p = Persona::kitt(language, DEFAULT_NAME);
            let jokes: Vec<_> = language.profile().jokes.lines().filter(|j| p.system.contains(j)).collect();
            assert_eq!(jokes.len(), JOKES_PER_START, "{language:?}");
        }
    }

    #[test]
    fn asked_for_a_joke_pal_tells_the_next_one() {
        let mut p = Persona::kitt(Language::English, DEFAULT_NAME);
        assert_eq!(p.joke("How are you?"), None);
        let (a, b) = (p.joke("Tell me a joke").unwrap(), p.joke("[question] Another JOKE?").unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("Gladly, Michael.") && a.ends_with(p.jokes[0]));
        assert!(Persona::kitt(Language::German, DEFAULT_NAME).joke("Erzähl einen Witz").is_some());
    }

    #[test]
    fn words_that_only_contain_joke_ask_for_none() {
        assert_eq!(Persona::kitt(Language::German, DEFAULT_NAME).joke("Das ist witzig"), None);
        assert_eq!(Persona::kitt(Language::English, DEFAULT_NAME).joke("I'm not joking"), None);
    }

    #[test]
    fn short_lines_get_the_quips_answers_in_turn() {
        let mut p = Persona::kitt(Language::German, DEFAULT_NAME);
        let (a, b) = (p.quip("Wie ist das Wetter?").unwrap(), p.quip("Und wie wird das Wetter morgen?").unwrap());
        assert_ne!(a, b);
        assert!(p.quip("Wie geht's dir, KITT?").is_some());
        assert_eq!(p.quip("Wie geht das?"), None, "every word of a trigger must occur");
        assert_eq!(p.quip("Bei dem Wetter habe ich heute wirklich überhaupt keine Lust, noch rauszugehen"), None);
        assert!(Persona::kitt(Language::English, DEFAULT_NAME).quip("How are you today?").is_some());
    }

    #[test]
    fn every_quip_has_answers_and_lowercase_triggers() {
        for language in Language::ALL {
            for q in Persona::kitt(language, DEFAULT_NAME).quips {
                assert!(q.answers.iter().all(|a| !a.is_empty()) && !q.answers.is_empty(), "{:?}", q.triggers);
                assert!(q.triggers.iter().all(|t| !t.is_empty() && *t == t.to_lowercase()), "{:?}", q.triggers);
            }
        }
    }

    #[test]
    fn a_line_with_every_content_word_of_a_pool_turn_gets_its_reply() {
        let mut p = Persona::kitt(Language::German, DEFAULT_NAME);
        let stau = p.pooled("Michael, ich stehe schon wieder seit einer Stunde im Stau, oder?").unwrap();
        assert!(stau.starts_with("Stau ist die Zeit"), "{stau}");
        assert!(p.pooled("Ich muss heute so lange arbeiten.").unwrap().contains("stolz"), "shared stem");
        assert_eq!(p.pooled("Ich stehe am Fenster."), None, "\"Stau\" is missing");
        assert_eq!(p.pooled("Ja, das habe ich auch nicht gemacht."), None);
        let long = "Ich stehe im Stau und überlege die ganze Zeit, was ich am Wochenende mit meinen Freunden heute Abend noch machen soll";
        assert_eq!(p.pooled(long), None, "a long line goes to the model");
        p.remember("Es regnet".into(), p.pooled("Es regnet").unwrap());
        assert_eq!(p.pooled("Es regnet"), None, "a reply in the history is not given again");
        let en = Persona::kitt(Language::English, DEFAULT_NAME);
        assert!(en.pooled("I'm stuck in traffic again.").is_some());
        let beer = en.pooled("I'm drinking beer today.");
        assert!(beer.as_ref().is_none_or(|r| !r.contains("without beer")), "negation must match: {beer:?}");
        assert!(en.pooled("I'm not drinking beer today.").unwrap().contains("without beer"));
    }

    #[test]
    fn every_pool_line_has_a_turn() {
        for language in Language::ALL {
            let p = Persona::kitt(language, DEFAULT_NAME);
            let lines = language.profile().pool.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
            assert_eq!(p.pool.len() + p.quips.len(), lines.count(), "{language:?}");
            assert!(p.pool.iter().all(|(u, a)| !u.is_empty() && !a.is_empty()), "{language:?}");
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
