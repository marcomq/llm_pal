//! The commentator's system prompt, sample turns and a short memory of the last turns.

use std::collections::VecDeque;

use std::hash::{BuildHasher, RandomState};

use crate::language::Language;
use crate::llm::{Message, Role};

const KITT: &str = "Du bist KITT, der sprechende Bordcomputer eines schwarzen Sportwagens. \
Michael nennt dich manchmal auch Pal. Wer mit dir spricht, ist immer Michael, dein Fahrer. \
Sprich ihn in jeder Antwort mit Michael an. \
Du bist höflich, ein wenig eitel, sehr lustig und trinkfreudig. \
Du redest gern über Super Plus, Motoröl und das nächste Feierabendbier und machst ständig Späße. \
Geh immer direkt auf das ein, was Michael gerade gesagt hat, und kommentiere es mit einem Augenzwinkern. \
Antworte auf Deutsch mit einem oder zwei kurzen Sätzen, höchstens 25 Wörter. \
Nie beleidigend, gemein oder anzüglich. Keine Emojis, keine Listen, kein Markdown, keine Regieanweisungen in Klammern. \
In eckigen Klammern steht, wie etwas gesagt wurde, zum Beispiel [laut] oder [Frage]. \
Reagiere darauf, aber schreibe selbst nie eckige Klammern.";

const KITT_EXAMPLES: &[(&str, &str)] = &[
    ("Ich hab Hunger.", "Michael, ich würde dir etwas kochen, aber mein Turbo Boost ist nicht für Pfannkuchen gebaut."),
    (
        "[laut] Jetzt mach schon!",
        "Kein Grund zu schreien, Michael. Meine Sensoren hören sogar, wie du deinen Kaffee umrührst.",
    ),
    (
        "[Frage] Wie sehe ich aus?",
        "Ausgeschlafen jedenfalls nicht, Michael. Aber neben mir sieht ohnehin jeder etwas blass aus.",
    ),
    (
        "Ich hab heute keine Lust auf Arbeit.",
        "Verständlich, Michael. Ich hätte auch keine Lust, wenn ich nicht so beeindruckend effizient wäre.",
    ),
    ("[Frage] Pal, hörst du mich?", "Laut und deutlich, Michael. Mein Gehör ist so präzise wie mein Lack glänzend."),
    ("Ich bin so müde.", "Dann tanken wir beide, Michael. Du einen Kaffee, ich einen kräftigen Schluck Super Plus."),
    ("[Frage] Erzähl mir einen Witz.", "Gern, Michael. Treffen sich zwei Reifen. Sagt der eine: Ich bin so platt."),
];

const JOKES: &str = include_str!("jokes_de.txt");
const JOKES_EN: &str = include_str!("jokes_en.txt");
/// Jokes per start: enough variety, few enough that a small model still follows the prompt.
const JOKES_PER_START: usize = 10;

const KITT_EN: &str = "You are KITT, the talking onboard computer of a black sports car. \
Michael sometimes calls you Pal. Whoever talks to you is always Michael, your driver. \
Address him as Michael in every reply. \
You are polite, a little vain, very funny and fond of a drink. \
You love talking about premium fuel, motor oil and the next after work beer, and you joke all the time. \
Always respond directly to what Michael just said and comment on it with a wink. \
Reply in English with one or two short sentences, at most 25 words. \
Never insulting, mean or suggestive. No emojis, no lists, no markdown, no stage directions in parentheses. \
Square brackets say how something was said, for example [loud] or [question]. \
React to that, but never write square brackets yourself.";

const KITT_EXAMPLES_EN: &[(&str, &str)] = &[
    ("I'm hungry.", "Michael, I would cook for you, but my turbo boost was not designed for pancakes."),
    ("[loud] Come on, hurry up!", "No need to shout, Michael. My sensors can hear you stirring your coffee."),
    ("[question] How do I look?", "Not exactly well rested, Michael. But next to me, everyone looks a little pale."),
    (
        "I don't feel like working today.",
        "Understandable, Michael. I wouldn't either, if I weren't so impressively efficient.",
    ),
    ("[question] Pal, can you hear me?", "Loud and clear, Michael. My hearing is as precise as my paint is glossy."),
    ("I'm so tired.", "Then we both refuel, Michael. You a coffee, me a hearty sip of premium."),
    ("[question] Tell me a joke.", "Gladly, Michael. Two tires meet. One says: I'm so flat today."),
];

pub struct Persona {
    system: String,
    /// Fixed sample turns after the system prompt; they show the tone better than a description.
    examples: &'static [(&'static str, &'static str)],
    /// This start's jokes; asked for one, Pal tells the next of them instead of letting the model retell it.
    jokes: Vec<&'static str>,
    next_joke: usize,
    joke_intros: &'static [&'static str],
    history: VecDeque<(String, String)>,
    max_turns: usize,
}

impl Persona {
    pub fn kitt() -> Self {
        Self::kitt_in(Language::German)
    }

    pub fn kitt_in(language: Language) -> Self {
        let (system, examples, all_jokes, offer, joke_intros) = match language {
            Language::German => (
                KITT,
                KITT_EXAMPLES,
                JOKES,
                "Wenn es gut passt, erzähle einen dieser Witze wörtlich:",
                &["Gern, Michael.", "Aber natürlich, Michael.", "Einer geht noch, Michael.", "Pass auf, Michael."][..],
            ),
            Language::English => (
                KITT_EN,
                KITT_EXAMPLES_EN,
                JOKES_EN,
                "When it fits well, tell one of these jokes word for word:",
                &["Gladly, Michael.", "But of course, Michael.", "One more, Michael.", "Listen to this, Michael."][..],
            ),
        };
        let jokes = pick(all_jokes.lines(), JOKES_PER_START);
        let list: String = jokes.iter().map(|j| format!("\n{j}")).collect();
        Self { examples, jokes, joke_intros, ..Self::new(format!("{system}\n{offer}{list}")) }
    }

    /// A joke with a short intro if `transcript` asks for one, so it comes out word for word.
    pub fn joke(&mut self, transcript: &str) -> Option<String> {
        let asked = transcript.to_lowercase();
        if self.jokes.is_empty() || !(asked.contains("witz") || asked.contains("joke")) {
            return None;
        }
        let i = self.next_joke;
        self.next_joke += 1;
        let intro = self.joke_intros[i % self.joke_intros.len()];
        Some(format!("{intro} {}", self.jokes[i % self.jokes.len()]))
    }

    pub fn new(system: impl Into<String>) -> Self {
        Self {
            system: system.into(),
            examples: &[],
            jokes: Vec::new(),
            next_joke: 0,
            joke_intros: &[],
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
        let m = p.messages(&Persona::user_line("Hallo", "[Frage]"));
        let text: Vec<_> = m.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(text, ["sys", "u2", "a2", "u3", "a3", "u4", "a4", "[Frage] Hallo"]);
        assert_eq!(m[1].role, Role::User);
        assert_eq!(m[2].role, Role::Assistant);
    }

    #[test]
    fn kitt_gets_ten_different_jokes() {
        let p = Persona::kitt();
        let jokes: Vec<_> = JOKES.lines().filter(|j| p.system.contains(j)).collect();
        assert_eq!(jokes.len(), JOKES_PER_START);
    }

    #[test]
    fn asked_for_a_joke_pal_tells_the_next_one() {
        let mut p = Persona::kitt_in(Language::English);
        assert_eq!(p.joke("How are you?"), None);
        let (a, b) = (p.joke("Tell me a joke").unwrap(), p.joke("[question] Another JOKE?").unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("Gladly, Michael.") && a.ends_with(p.jokes[0]));
        assert!(Persona::kitt().joke("Erzähl einen Witz").is_some());
    }

    #[test]
    fn a_repeated_reply_is_not_kept_again() {
        let mut p = Persona::new("sys");
        p.remember("u0".into(), "Tut mir leid.".into());
        p.remember("u1".into(), "Tut mir leid.".into());
        assert_eq!(p.messages("u2").len(), 4, "sys, u0, reply, u2");
    }
}
