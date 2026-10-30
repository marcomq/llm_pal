//! The conversation languages; each one's texts and words live in its own module.

mod de;
mod en;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    German,
    English,
}

/// Everything Pal says or recognises in one language.
pub struct Profile {
    /// ISO 639-1 code, as whisper uses it.
    pub code: &'static str,
    /// Pocket TTS model, such as "german".
    pub pocket_model: &'static str,
    /// Synthesized once at start, so the first real sentence comes fast.
    pub warm_up: &'static str,
    pub system_prompt: &'static str,
    /// Fixed sample turns after the system prompt; they show the tone better than a description.
    pub examples: &'static [(&'static str, &'static str)],
    /// One joke per line.
    pub jokes: &'static str,
    /// Introduces the jokes in the system prompt.
    pub joke_offer: &'static str,
    pub joke_intros: &'static [&'static str],
    /// Lowercase words that ask for a joke.
    pub joke_words: &'static [&'static str],
    /// Fixed answers to common short lines, so they don't take room in the prompt.
    pub quips: &'static [Quip],
    /// Lowercase words after which a sentence goes on.
    pub continues: &'static [&'static str],
    /// Prosody tag names, in the order of `prosody_tags::Tag`.
    pub tags: [&'static str; 9],
    /// Words ending in '.' that don't end a sentence.
    pub abbreviations: &'static [&'static str],
    /// Words the voice reads wrong, with a spelling it reads right.
    pub pronunciations: &'static [(&'static str, &'static str)],
}

/// A fixed answer to a common line. Each trigger is a set of lowercase words that must all occur; the answers take
/// turns.
pub struct Quip {
    pub triggers: &'static [&'static str],
    pub answers: &'static [&'static str],
}

impl Language {
    pub const ALL: [Language; 2] = [Language::German, Language::English];

    pub fn profile(self) -> &'static Profile {
        match self {
            Language::German => &de::PROFILE,
            Language::English => &en::PROFILE,
        }
    }

    pub fn code(self) -> &'static str {
        self.profile().code
    }

    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.code() == code)
    }
}
