//! Extra knowledge for the LLM, found from what the user said, added just before the reply starts.
//! It goes into this turn's prompt only; the history keeps the plain line. Canned replies (jokes, quips, pool
//! lines) skip the LLM and live in `persona`. Runs before every reply, so it adds to `first_sentence_ms`.

use crate::language::Language;

/// The user's line as this turn's prompt ends with: found facts first, then `line`.
pub fn prompt_line(transcript: &str, line: &str, language: Language) -> String {
    match facts(transcript, language) {
        Some(facts) => format!("{facts}\n\n{line}"),
        None => line.to_string(),
    }
}

/// What helps answer `transcript` (a lookup, a tool, a search), or `None`. The place to add enrichment.
fn facts(_transcript: &str, _language: Language) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_facts_the_line_is_unchanged() {
        assert_eq!(prompt_line("hallo", "[leise] hallo", Language::German), "[leise] hallo");
    }
}
