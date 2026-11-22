//! The commentator's system prompt, sample turns and a short memory of the last turns.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::path::Path;
use std::time::Instant;

use chrono::Local;

use crate::think::character::Character;
use crate::think::language::Language;
use crate::think::local_llm::{Message, Role};
use crate::think::placeholders;

/// Longer lines are about more than a quip's trigger and go to the model.
const QUIP_MAX_WORDS: usize = 10;

/// Longer lines are about more than the pool turn they share words with and go to the model.
const POOL_MAX_WORDS: usize = 20;

/// The history may grow this many turns past `max_turns` before it is cut back to `max_turns`. Each cut changes the
/// prompt's start, and the model, which can't drop its oldest tokens, reads the whole history again.
const HISTORY_SLACK: usize = 2;

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
    examples: Vec<(String, String)>,
    /// All jokes in a random order; asked for one, Pal tells the next of them instead of letting the model retell it.
    jokes: Vec<String>,
    next_joke: usize,
    joke_intros: Vec<String>,
    joke_words: Vec<String>,
    call_words: Vec<String>,
    silence_prompt: String,
    quips: Vec<Quip>,
    /// Per quip, how many of its answers were given, starting at a random count.
    quips_given: Vec<usize>,
    /// Fixed turns: a line saying every content word of one gets its reply word for word.
    pool: Vec<(String, String)>,
    filler: &'static [&'static str],
    suggestions: &'static [&'static str],
    history: VecDeque<(String, String)>,
    max_turns: usize,
    language: Language,
    started: Instant,
}

impl Persona {
    pub fn kitt(language: Language, name: &str) -> Self {
        Self::with(Character::built_in(language), language, name)
    }

    /// The character in `file`, or the built-in one if there is none or it can't be read.
    pub fn load(file: Option<&Path>, language: Language, name: &str) -> Self {
        let character = file.map(|f| Character::from_file(f, language)).transpose().unwrap_or_else(|e| {
            eprintln!("llm_pal: persona: {e}");
            None
        });
        Self::with(character.unwrap_or_else(|| Character::built_in(language)), language, name)
    }

    fn with(c: Character, language: Language, name: &str) -> Self {
        let (quips, pool) = parse_pool(&c.replies);
        Self {
            examples: c.examples,
            jokes: shuffled(c.jokes.lines()),
            joke_intros: c.joke_intros,
            joke_words: c.joke_words,
            call_words: c.call_words,
            silence_prompt: c.silence,
            quips_given: (0..quips.len()).map(|i| RandomState::new().hash_one(i) as usize).collect(),
            quips,
            pool,
            filler: language.profile().filler,
            suggestions: language.profile().suggestions,
            name: name.to_string(),
            language,
            ..Self::new(c.system.replace(DEFAULT_NAME, name))
        }
    }

    /// What calls Pal in party mode.
    pub fn call_words(&self) -> &[String] {
        &self.call_words
    }

    /// Party mode's prompt when the room goes quiet; `{talk}` is the quoted talk before.
    pub fn silence_prompt(&self) -> &str {
        &self.silence_prompt
    }

    /// A joke with a short intro if `transcript` asks for one, so it comes out word for word.
    pub fn joke(&mut self, transcript: &str) -> Option<String> {
        let asked = transcript.to_lowercase();
        let mut words = asked.split(|c: char| !c.is_alphanumeric());
        if self.jokes.is_empty() || !words.any(|w| self.joke_words.iter().any(|j| j == w)) {
            return None;
        }
        let i = self.next_joke;
        self.next_joke += 1;
        let joke = &self.jokes[i % self.jokes.len()];
        let told = match self.joke_intros.is_empty() {
            true => joke.clone(),
            false => format!("{} {joke}", self.joke_intros[i % self.joke_intros.len()]),
        };
        Some(self.named(&told))
    }

    /// A fixed answer if `transcript` is a short line a quip knows; each time the next of its answers.
    pub fn quip(&mut self, transcript: &str) -> Option<String> {
        let line = transcript.to_lowercase();
        let words: Vec<&str> = line.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        if words.len() > QUIP_MAX_WORDS {
            return None;
        }
        let name = self.name.to_lowercase();
        let addressing = |w: &str| w == name || self.call_words.iter().any(|c| c == w);
        let said = |trigger: &String| {
            let whole = trigger.strip_prefix('!');
            let wanted: Vec<&str> = whole.unwrap_or(trigger).split(' ').collect();
            wanted.iter().all(|w| words.contains(w))
                && (whole.is_none() || words.iter().all(|w| wanted.contains(w) || addressing(w)))
        };
        let i = self.quips.iter().position(|q| q.triggers.iter().any(said))?;
        self.quips_given[i] += 1;
        let answers = &self.quips[i].answers;
        let answer = self.named(&answers[self.quips_given[i] % answers.len()]);
        Some(placeholders::fill(&answer, self.language, Local::now().naive_local(), self.started.elapsed()))
    }

    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            name: DEFAULT_NAME.to_string(),
            examples: Vec::new(),
            jokes: Vec::new(),
            next_joke: 0,
            joke_intros: Vec::new(),
            joke_words: Vec::new(),
            call_words: Vec::new(),
            silence_prompt: String::new(),
            quips: Vec::new(),
            quips_given: Vec::new(),
            pool: Vec::new(),
            filler: &[],
            suggestions: &[],
            history: VecDeque::new(),
            max_turns: 3,
            language: Language::German,
            started: Instant::now(),
        }
    }

    /// The reply of the pool turn whose content words `transcript` all says, the one with the most words if several
    /// do; none whose reply is in the history.
    pub fn pooled(&self, transcript: &str) -> Option<String> {
        if transcript.split_whitespace().count() > POOL_MAX_WORDS {
            return None;
        }
        let clauses = self.clauses(transcript);
        let denies = clauses.iter().any(|(_, no)| *no);
        let affirmed: Vec<String> =
            clauses.iter().filter(|(_, no)| !no).flat_map(|(c, _)| self.content_words(c)).collect();
        let all = self.content_words(transcript);
        let (_, _, reply) = self
            .pool
            .iter()
            .filter(|(_, a)| self.history.iter().all(|(_, r)| *r != self.named(a)))
            .map(|(u, a)| (negated(u), self.content_words(u), a))
            .filter(|(no, _, _)| !no || denies)
            .filter(|(no, words, _)| {
                let said = if *no { &all } else { &affirmed };
                !words.is_empty() && words.iter().all(|w| said.iter().any(|s| alike(s, w)))
            })
            .min_by_key(|(_, words, _)| std::cmp::Reverse(words.len()))?;
        Some(self.named(reply))
    }

    /// The line's sentences, each with whether it denies something. A comma only splits before a suggestion, as in
    /// "I don't want the fridge empty" the negation reaches past it.
    fn clauses(&self, text: &str) -> Vec<(String, bool)> {
        let lower = text.to_lowercase().replace('’', "'");
        let mut clauses = Vec::new();
        for sentence in lower.split(['.', '!', '?']) {
            let mut current = String::new();
            for part in sentence.split(',') {
                if let Some(proposed) = self.suggestion(part) {
                    clauses.push(std::mem::take(&mut current));
                    clauses.push(proposed.to_string());
                } else {
                    current.push_str(part);
                    current.push(',');
                }
            }
            clauses.push(current);
        }
        clauses
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|c| {
                let no = negated(&c);
                (c, no)
            })
            .collect()
    }

    /// What a question proposing something asks, without its opening.
    fn suggestion<'a>(&self, clause: &'a str) -> Option<&'a str> {
        let clause = clause.trim_start();
        self.suggestions
            .iter()
            .find_map(|s| clause.strip_prefix(s).filter(|rest| !rest.starts_with(char::is_alphanumeric)))
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
        if tags.is_empty() { transcript.to_string() } else { format!("{transcript} {tags}") }
    }

    pub fn messages(&self, user_line: &str) -> Vec<Message> {
        let mut m = self.context();
        m.push(Message::new(Role::User, user_line));
        m
    }

    /// Everything before the user's next line: system prompt, sample turns and history.
    pub fn context(&self) -> Vec<Message> {
        let mut m = vec![Message::new(Role::System, &self.system)];
        for (u, a) in &self.examples {
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
    /// False if the turn was not kept.
    pub fn remember(&mut self, user_line: String, reply: String) -> bool {
        if reply.is_empty() || self.history.iter().any(|(_, a)| *a == reply) {
            return false;
        }
        self.history.push_back((user_line, reply));
        if self.history.len() > self.max_turns + HISTORY_SLACK {
            self.history.drain(..self.history.len() - self.max_turns);
        }
        true
    }

    /// The last reply was cut off: keep only what the user heard of it, or drop the turn if nothing.
    pub fn cut_last_reply(&mut self, heard: String) {
        if heard.is_empty() {
            self.history.pop_back();
        } else if let Some((_, reply)) = self.history.back_mut() {
            *reply = heard;
        }
    }
}

/// A fixed answer to a common line. Each trigger is a set of lowercase words that must all occur; the answers take
/// turns.
struct Quip {
    triggers: Vec<String>,
    answers: Vec<String>,
}

/// Splits a pool file into its quips (`= trigger; trigger | reply | reply`) and sample turns (`line | reply`).
fn parse_pool(text: &str) -> (Vec<Quip>, Vec<(String, String)>) {
    let (mut quips, mut turns) = (Vec::new(), Vec::new());
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(quip) = line.strip_prefix('=') {
            let mut parts = quip.split('|').map(str::trim);
            let triggers = parts.next().unwrap_or_default().split(';').map(str::trim);
            let triggers = triggers.filter(|t| !t.is_empty()).map(str::to_lowercase).collect();
            quips.push(Quip { triggers, answers: parts.map(String::from).collect() });
        } else if let Some((u, a)) = line.split_once('|') {
            turns.push((u.trim().to_string(), a.trim().to_string()));
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
fn shuffled<'a>(items: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut items: Vec<String> = items.filter(|l| !l.trim().is_empty()).map(String::from).collect();
    let mut seed = RandomState::new().hash_one(0u8) | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_last_turns_in_order_and_drops_old_ones_in_blocks() {
        let mut p = Persona::new("sys");
        for i in 0..5 {
            p.remember(format!("u{i}"), format!("a{i}"));
        }
        assert_eq!(p.context().len(), 1 + 2 * 5, "the history grows past max_turns by the slack");
        p.remember("u5".into(), "a5".into());
        p.remember("ignored".into(), String::new());
        let m = p.messages(&Persona::user_line("Hello", "[question]"));
        let text: Vec<_> = m.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(text, ["sys", "u3", "a3", "u4", "a4", "u5", "a5", "Hello [question]"]);
        assert_eq!(m[1].role, Role::User);
        assert_eq!(m[2].role, Role::Assistant);
    }

    #[test]
    fn kitt_keeps_jokes_out_of_the_unchanging_prompt() {
        for language in Language::ALL {
            let (a, b) = (Persona::kitt(language, DEFAULT_NAME), Persona::kitt(language, DEFAULT_NAME));
            assert_eq!(a.context().len(), a.fixed_messages(), "{language:?}");
            assert_eq!(a.context()[0].content, b.context()[0].content, "{language:?}");
            assert!(a.jokes.iter().all(|j| !a.system.contains(j)), "{language:?}");
            let lines = Character::built_in(language).jokes.lines().count();
            assert_eq!(a.jokes.len(), lines, "{language:?}");
        }
    }

    #[test]
    fn asked_for_a_joke_pal_tells_the_next_one() {
        let mut p = Persona::kitt(Language::English, DEFAULT_NAME);
        assert_eq!(p.joke("How are you?"), None);
        let (a, b) = (p.joke("Tell me a joke").unwrap(), p.joke("[question] Another JOKE?").unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("Gladly, Michael.") && a.ends_with(p.jokes[0].as_str()));
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
        assert!(p.quip("Ruf bitte meine Mutter an").unwrap().contains("Michael"), "any name between ruf and an");
        assert!(p.quip("Fahr los, KITT!").is_some());
        assert!(p.quip("Ich hole beim Edeka ein halbes Hähnchen").unwrap().contains("Hähnchen"));
        assert!(Persona::kitt(Language::English, DEFAULT_NAME).quip("Let's go, KITT!").is_some());
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
    fn quips_only_use_known_placeholders_and_say_them() {
        for language in Language::ALL {
            let p = Persona::kitt(language, DEFAULT_NAME);
            for answer in p.quips.iter().flat_map(|q| &q.answers) {
                let filled = placeholders::fill(answer, language, Local::now().naive_local(), p.started.elapsed());
                assert!(!filled.contains(['{', '}']), "{answer}");
            }
        }
        let mut p = Persona::kitt(Language::German, DEFAULT_NAME);
        for line in ["Welcher Wochentag ist heute?", "Der Wievielte ist heute?", "Wie lange reden wir schon?"] {
            assert!(!p.quip(line).unwrap().contains('{'), "{line}");
        }
        let mut p = Persona::kitt(Language::English, DEFAULT_NAME);
        for line in ["What day is it?", "What's the date?", "How long have we been talking?"] {
            assert!(!p.quip(line).unwrap().contains('{'), "{line}");
        }
    }

    #[test]
    fn only_questions_about_now_get_the_clock() {
        let mut p = Persona::kitt(Language::German, DEFAULT_NAME);
        for line in [
            "Zum wievielten Mal sagst du das?",
            "Zu welcher Uhrzeit fährt der Zug?",
            "An welchem Wochentag ist Weihnachten?",
        ] {
            assert_eq!(p.quip(line), None, "{line}");
        }
        for line in ["Wie spät ist es?", "Wieviel Uhr ist es?", "Den Wievielten haben wir?"] {
            assert!(p.quip(line).is_some(), "{line}");
        }
        let mut p = Persona::kitt(Language::English, DEFAULT_NAME);
        for line in ["What time does the train leave?", "What date is the meeting?", "What's the date of the meeting?"]
        {
            assert_eq!(p.quip(line), None, "{line}");
        }
        for line in ["What time is it?", "What's the time?", "What's the date?", "What's the date today, KITT?"] {
            assert!(p.quip(line).is_some(), "{line}");
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
        assert!(p.pooled("Der Build ist schon wieder kaputt.").unwrap().contains("Motor"));
        assert!(en.pooled("Well, it works on my machine.").unwrap().contains("ship"));
        let beer = en.pooled("I'm drinking beer today.");
        assert!(beer.as_ref().is_none_or(|r| !r.contains("without beer")), "negation must match: {beer:?}");
        assert!(en.pooled("I'm not drinking beer today.").unwrap().contains("without beer"));
    }

    #[test]
    fn a_negation_only_counts_in_its_own_sentence() {
        let p = Persona::kitt(Language::German, DEFAULT_NAME);
        let fridge = |line: &str| p.pooled(line).is_some_and(|r| r.contains("Notfall"));
        assert!(fridge("Der Kühlschrank ist leer. Sollen wir nicht einkaufen fahren?"));
        assert!(fridge("Der Kühlschrank ist leer, sollen wir nicht einkaufen fahren?"));
        assert!(fridge("Der Kühlschrank ist leer. Ich habe keine Zeit."), "the other sentence denies");
        assert!(!fridge("Der Kühlschrank ist nicht leer."));
        assert!(!fridge("Ich will nicht, dass der Kühlschrank leer ist."), "a comma alone doesn't end the negation");
        assert!(!fridge("Sollen wir nichts in den leeren Kühlschrank tun?"), "\"nichts\" is no suggestion");
        assert!(p.pooled("Sollen wir nicht einkaufen fahren?").unwrap().contains("Tüten"));
        assert_eq!(p.pooled("Ich muss heute nicht einkaufen."), None);
        let en = Persona::kitt(Language::English, DEFAULT_NAME);
        assert!(en.pooled("I'm stuck in traffic. Shouldn't we turn around?").is_some());
        assert!(en.pooled("I'm not drinking beer today. It's late.").unwrap().contains("without beer"));
    }

    #[test]
    fn every_pool_line_has_a_turn() {
        for language in Language::ALL {
            let p = Persona::kitt(language, DEFAULT_NAME);
            let lines = Character::built_in(language).replies.lines().filter(|l| !l.starts_with('#')).count();
            assert_eq!(p.pool.len() + p.quips.len(), lines, "{language:?}");
            assert!(p.pool.iter().all(|(u, a)| !u.is_empty() && !a.is_empty()), "{language:?}");
        }
    }

    #[test]
    fn kitt_says_what_the_name_stands_for() {
        for (language, asked) in
            [(Language::German, "Wofür steht KITT?"), (Language::English, "What does KITT stand for?")]
        {
            let reply = Persona::kitt(language, "Sam").quip(asked).unwrap();
            assert!(reply.starts_with("Kinda Intelligent, Totally Thirsty, Sam."), "{reply}");
        }
    }

    #[test]
    fn a_character_from_a_file_replaces_the_built_in_one() {
        let dir = std::env::temp_dir().join(format!("pal-persona-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rosa.txt");
        let text = "[system]\nYou are Rosa, kind to {name}.\n[jokes]\nA short joke.\n[replies]\n= hello | Hi, {name}.";
        std::fs::write(&file, text).unwrap();
        let mut p = Persona::load(Some(&file), Language::English, "Sam");
        assert!(p.context()[0].content.starts_with("You are Rosa, kind to Sam."));
        assert_eq!(p.joke("Tell me a joke").as_deref(), Some("A short joke."), "no intro in the file");
        assert_eq!(p.quip("Hello").as_deref(), Some("Hi, Sam."));
        assert_eq!(p.quip("What's the weather?"), None, "none of the built-in replies");
        assert_eq!(p.call_words(), ["rosa"]);

        std::fs::write(&file, "broken").unwrap();
        let fallback = Persona::load(Some(&file), Language::English, "Sam");
        assert!(fallback.context()[0].content.starts_with("You are KITT"));
        assert!(Persona::load(Some(&dir.join("gone.txt")), Language::English, "Sam").call_words().len() > 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repeated_reply_is_not_kept_again() {
        let mut p = Persona::new("sys");
        p.remember("u0".into(), "Sorry.".into());
        p.remember("u1".into(), "Sorry.".into());
        assert_eq!(p.messages("u2").len(), 4, "sys, u0, reply, u2");
    }

    #[test]
    fn a_cut_reply_keeps_only_what_was_heard() {
        let mut p = Persona::new("sys");
        assert!(p.remember("u0".into(), "One. Two.".into()));
        assert!(p.remember("u1".into(), "Three. Four.".into()));
        p.cut_last_reply("Three.".into());
        let text: Vec<_> = p.context().iter().map(|m| m.content.clone()).collect();
        assert_eq!(text, ["sys", "u0", "One. Two.", "u1", "Three."]);
        p.cut_last_reply(String::new());
        assert_eq!(p.context().len(), 3, "sys, u0, reply");
    }
}
