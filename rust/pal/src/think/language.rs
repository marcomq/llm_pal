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
    /// ISO 639-1 code.
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
    pub joke_intros: &'static [&'static str],
    /// Lowercase words that ask for a joke.
    pub joke_words: &'static [&'static str],
    /// Lowercase words that call Pal in party mode: his name as speech recognition writes it.
    pub call_words: &'static [&'static str],
    /// Party mode's prompt when the room goes quiet; `{talk}` is the quoted talk before.
    pub silence_prompt: &'static str,
    /// Fixed replies, one per line: `= trigger; trigger | reply | reply` for short lines, `line | reply` for sample
    /// turns. `#` starts a comment line.
    pub pool: &'static str,
    /// Lowercase words too common to tell which pool turns fit a line.
    pub filler: &'static [&'static str],
    /// Lowercase words after which a sentence goes on.
    pub continues: &'static [&'static str],
    /// Prosody tag names, in the order of `prosody_tags::Tag`.
    pub tags: [&'static str; 9],
    /// Words ending in '.' that don't end a sentence.
    pub abbreviations: &'static [&'static str],
    /// Words the voice reads wrong, with a spelling it reads right.
    pub pronunciations: &'static [(&'static str, &'static str)],
    pub numbers: Numbers,
    pub clock: Clock,
}

/// How the quips' placeholders are said.
pub struct Clock {
    /// Monday first.
    pub weekdays: [&'static str; 7],
    /// Hour (0 to 23) and minute.
    pub time: fn(u32, u32) -> String,
    /// Day and month (1 to 12).
    pub date: fn(u32, u32) -> String,
    /// Whole minutes.
    pub duration: fn(u64) -> String,
}

/// How numbers are written and read aloud.
pub struct Numbers {
    pub decimal: char,
    pub group: char,
    /// Said for the decimal sign, such as "Komma".
    pub point: &'static str,
    pub percent: &'static str,
    /// The words for 0 to 9, for numbers read digit by digit.
    pub digits: [&'static str; 10],
    /// A whole number below a billion.
    pub say: fn(u64) -> String,
    /// A year from 1100 to 1999.
    pub year: fn(u64) -> String,
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
