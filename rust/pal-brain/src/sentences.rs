//! Cuts a token stream into whole sentences, so TTS can start on the first one while the LLM keeps generating.
//! Text in square brackets is dropped: the prompt's prosody tags, which small models like to echo. The first piece
//! may end at a comma already, so the voice starts sooner.

use crate::language::Language;

pub struct Sentences {
    abbreviations: &'static [&'static str],
    buf: String,
    in_bracket: bool,
    started: bool,
}

/// Words a first piece needs before a comma may end it; "Oh, Michael," alone would sound cut off.
const FIRST_PIECE_WORDS: usize = 4;

impl Sentences {
    pub fn new(language: Language) -> Self {
        Self { abbreviations: language.profile().abbreviations, buf: String::new(), in_bracket: false, started: false }
    }

    /// Appends text and returns the sentences it completed.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        for c in text.chars() {
            match c {
                '[' => self.in_bracket = true,
                ']' => self.in_bracket = false,
                _ if !self.in_bracket => self.buf.push(c),
                _ => {}
            }
        }
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let rest = self.buf.split_off(end);
            let s = std::mem::replace(&mut self.buf, rest.trim_start().to_string());
            push_trimmed(&mut out, &s);
            self.started |= !out.is_empty();
        }
        out
    }

    /// The unfinished rest at the end of the stream.
    pub fn finish(&mut self) -> Option<String> {
        let mut out = Vec::new();
        push_trimmed(&mut out, &std::mem::take(&mut self.buf));
        out.pop()
    }

    /// Byte index after the first sentence end that is followed by whitespace.
    fn boundary(&self) -> Option<usize> {
        let b = &self.buf;
        b.char_indices().find_map(|(i, c)| {
            let end = i + c.len_utf8();
            if c == '\n' {
                return Some(end);
            }
            let next = b[end..].chars().next()?;
            // Only while the sentence is still streaming: split, the voice says each piece as a sentence of its own.
            let streaming = !b[end..].contains(['.', '!', '?', '…']);
            if c == ',' && next.is_whitespace() && !self.started && streaming {
                return (b[..i].split_whitespace().count() >= FIRST_PIECE_WORDS).then_some(end);
            }
            let closes = matches!(c, '.' | '!' | '?' | '…') && next.is_whitespace();
            (closes && !(c == '.' && self.is_abbreviation_or_number(end))).then_some(end)
        })
    }

    /// An abbreviation like "e.g. " or an ordinal like "3. " does not end a sentence.
    fn is_abbreviation_or_number(&self, end: usize) -> bool {
        let word = self.buf[..end].rsplit(char::is_whitespace).next().unwrap_or("");
        let digits = &word[..word.len() - 1];
        self.abbreviations.contains(&word) || (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
    }
}

fn push_trimmed(out: &mut Vec<String>, s: &str) {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().any(char::is_alphanumeric) {
        out.push(s.replace(" .", ".").replace(" !", "!").replace(" ?", "?").replace(" ,", ","));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_in(language: Language, tokens: &[&str]) -> Vec<String> {
        let mut s = Sentences::new(language);
        let mut out: Vec<String> = tokens.iter().flat_map(|t| s.push(t)).collect();
        out.extend(s.finish());
        out
    }

    fn split(tokens: &[&str]) -> Vec<String> {
        split_in(Language::English, tokens)
    }

    #[test]
    fn splits_on_sentence_ends_across_tokens() {
        let out = split(&["Oh", " great", ". And", " now", "?", " Really", " now", "!"]);
        assert_eq!(out, ["Oh great.", "And now?", "Really now!"]);
    }

    #[test]
    fn first_piece_may_end_at_a_comma() {
        let out = split(&["Oh, Michael, that is", " really great, honestly", ". One more, please."]);
        assert_eq!(out, ["Oh, Michael, that is really great,", "honestly.", "One more, please."]);
    }

    #[test]
    fn whole_sentences_are_not_split_at_a_comma() {
        let out = split(&["Off to bed with you, Michael. I keep the engine warm."]);
        assert_eq!(out, ["Off to bed with you, Michael.", "I keep the engine warm."]);
    }

    #[test]
    fn sentence_is_released_once_whitespace_follows() {
        let mut s = Sentences::new(Language::English);
        assert!(s.push("Done.").is_empty());
        assert_eq!(s.push(" Next"), ["Done."]);
    }

    #[test]
    fn keeps_abbreviations_numbers_and_decimals() {
        let out = split(&["On the 3. day there was e.g. 2.5 cake. Great."]);
        assert_eq!(out, ["On the 3. day there was e.g. 2.5 cake.", "Great."]);
        let out = split_in(Language::German, &["Es gab z. B. Kuchen. Toll."]);
        assert_eq!(out, ["Es gab z. B. Kuchen.", "Toll."]);
    }

    #[test]
    fn drops_echoed_tags_across_tokens() {
        assert_eq!(split(&["Fine [lo", "ud]. [question] Really?"]), ["Fine.", "Really?"]);
    }

    #[test]
    fn newline_ends_a_sentence_and_punctuation_only_is_dropped() {
        assert_eq!(split(&["Hello\n... \nWorld"]), ["Hello", "World"]);
    }
}
