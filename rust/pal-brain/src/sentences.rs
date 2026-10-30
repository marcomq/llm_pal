//! Cuts a token stream into whole sentences, so TTS can start on the first one while the LLM keeps generating.
//! Text in square brackets is dropped: the prompt's prosody tags, which small models like to echo. The first piece
//! may end at a comma already, so the voice starts sooner.

const ABBREVIATIONS: &[&str] = &[
    "z.", "B.", "z.B.", "d.h.", "bzw.", "ca.", "usw.", "etc.", "Dr.", "Nr.", "vgl.", "evtl.", "ggf.", "u.a.", "Mr.",
    "Mrs.", "Ms.", "e.g.", "i.e.", "vs.",
];

#[derive(Default)]
pub struct Sentences {
    buf: String,
    in_bracket: bool,
    started: bool,
}

/// Words a first piece needs before a comma may end it; "Ach, Michael," alone would sound cut off.
const FIRST_PIECE_WORDS: usize = 4;

impl Sentences {
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
            if c == ',' && next.is_whitespace() && !self.started {
                return (b[..i].split_whitespace().count() >= FIRST_PIECE_WORDS).then_some(end);
            }
            let closes = matches!(c, '.' | '!' | '?' | '…') && next.is_whitespace();
            (closes && !(c == '.' && self.is_abbreviation_or_number(end))).then_some(end)
        })
    }

    /// "z.B. " or an ordinal like "3. Oktober" does not end a sentence.
    fn is_abbreviation_or_number(&self, end: usize) -> bool {
        let word = self.buf[..end].rsplit(char::is_whitespace).next().unwrap_or("");
        let digits = &word[..word.len() - 1];
        ABBREVIATIONS.contains(&word) || (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
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

    fn split(tokens: &[&str]) -> Vec<String> {
        let mut s = Sentences::default();
        let mut out: Vec<String> = tokens.iter().flat_map(|t| s.push(t)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn splits_on_sentence_ends_across_tokens() {
        let out = split(&["Na", " super", ". Und", " jetzt", "?", " Echt", " jetzt", "!"]);
        assert_eq!(out, ["Na super.", "Und jetzt?", "Echt jetzt!"]);
    }

    #[test]
    fn first_piece_may_end_at_a_comma() {
        let out = split(&["Ach, Michael, das ist", " ja toll, wirklich", ". Noch einer, bitte."]);
        assert_eq!(out, ["Ach, Michael, das ist ja toll,", "wirklich.", "Noch einer, bitte."]);
    }

    #[test]
    fn sentence_is_released_once_whitespace_follows() {
        let mut s = Sentences::default();
        assert!(s.push("Fertig.").is_empty());
        assert_eq!(s.push(" Nächster"), ["Fertig."]);
    }

    #[test]
    fn keeps_abbreviations_numbers_and_decimals() {
        let out = split(&["Am 3. Oktober gab es z. B. 2.5 Kuchen. Toll."]);
        assert_eq!(out, ["Am 3. Oktober gab es z. B. 2.5 Kuchen.", "Toll."]);
    }

    #[test]
    fn drops_echoed_tags_across_tokens() {
        assert_eq!(split(&["Na gut [la", "ut]. [Frage] Wirklich?"]), ["Na gut.", "Wirklich?"]);
    }

    #[test]
    fn newline_ends_a_sentence_and_punctuation_only_is_dropped() {
        assert_eq!(split(&["Hallo\n... \nWelt"]), ["Hallo", "Welt"]);
    }
}
