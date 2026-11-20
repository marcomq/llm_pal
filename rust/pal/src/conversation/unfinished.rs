//! Unfinished sentences: a transcript that doesn't sound finished waits `UNFINISHED_WAIT` for more speech
//! (`Phase::Unfinished`, or `Turn::ending` for a held reply); if the user goes on, its audio is kept in `carry`
//! and put before the next segment.

use std::time::Duration;

use crate::think::language::Language;

/// Extra silence Pal waits for after a transcript that doesn't sound finished.
pub(super) const UNFINISHED_WAIT: Duration = Duration::from_millis(700);

/// No closing punctuation, a trailing ellipsis or comma, or a last word such as "and".
fn sounds_unfinished(text: &str, language: Language) -> bool {
    let text = text.trim().trim_end_matches(['"', '\'', '“', '”', '»', '«', ')']);
    if text.ends_with("...") || text.ends_with('…') || !text.ends_with(['.', '!', '?']) {
        return true;
    }
    let last = text.trim_end_matches(['.', '!', '?']).rsplit(' ').next().unwrap_or("");
    language.profile().continues.contains(&last.to_lowercase().as_str())
}

/// True if `text` ends a segment Smart Turn didn't call complete and sounds unfinished: Pal waits for more.
pub(super) fn waits_for_more(text: &str, turn_complete: bool, language: Language) -> bool {
    !turn_complete && sounds_unfinished(text, language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_endings_sound_unfinished() {
        let en = Language::English;
        for t in ["I wanted to ask whether", "I'm going today and", "I don't know...", "So I thought,", "Hello"] {
            assert!(sounds_unfinished(t, en), "{t}");
        }
        for t in ["What do you think?", "Turn on the light.", "Great!", "I like it, and."] {
            assert_eq!(sounds_unfinished(t, en), t.ends_with("and."), "{t}");
        }
        assert!(sounds_unfinished("Ich gehe heute und.", Language::German));
        for t in ["He said \"stop.\"", "Great (really)", "Sie sagte „Halt.“"] {
            assert_eq!(sounds_unfinished(t, en), t.starts_with("Great"), "{t}");
        }
        assert!(!sounds_unfinished("Ich gehe heute und.", en));
    }
}
