//! Who Pal is: the built-in character or one from a text file (`personas/<de|en>/<name>.txt` in the model folder).
//! A file has `[section]` headers; only `[system]` is required, and what it leaves out stays empty.

use std::path::Path;

use crate::think::language::Language;
use crate::think::persona::DEFAULT_NAME;

const MAX_BYTES: usize = 256 * 1024;
const SECTIONS: [&str; 8] =
    ["system", "examples", "jokes", "joke intros", "joke words", "call words", "silence", "replies"];

pub struct Character {
    pub system: String,
    pub examples: Vec<(String, String)>,
    /// One joke per line.
    pub jokes: String,
    pub joke_intros: Vec<String>,
    pub joke_words: Vec<String>,
    pub call_words: Vec<String>,
    pub silence: String,
    /// Fixed replies in the pool format, `= trigger; trigger | reply | reply` or `line | reply`.
    pub replies: String,
}

impl Character {
    pub fn built_in(language: Language) -> Self {
        Self::parse(language.profile().character, "kitt", language).expect("the built-in character is valid")
    }

    pub fn from_file(path: &Path, language: Language) -> Result<Self, String> {
        let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size > MAX_BYTES as u64 {
            return Err("the file is too large".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        Self::parse(&text, &stem, language)
    }

    /// `stem` is the file's name without extension: the default call word.
    pub fn parse(text: &str, stem: &str, language: Language) -> Result<Self, String> {
        if text.len() > MAX_BYTES {
            return Err("the file is too large".into());
        }
        let text = text.trim_start_matches('\u{feff}').replace("{name}", DEFAULT_NAME);
        let mut sections: Vec<(&str, Vec<&str>)> = SECTIONS.iter().map(|s| (*s, Vec::new())).collect();
        let mut current = None;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(head) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let head = head.trim().to_lowercase();
                current =
                    Some(SECTIONS.iter().position(|s| *s == head).ok_or_else(|| format!("unknown section [{head}]"))?);
            } else if let Some(i) = current {
                sections[i].1.push(line);
            } else {
                return Err(format!("text before the first section: {line}"));
            }
        }
        let lines = |name: &str| &sections.iter().find(|(s, _)| *s == name).expect("a known section").1;

        let p = language.profile();
        let system = lines("system").join(" ");
        if system.is_empty() {
            return Err("[system] is missing".into());
        }
        let mut examples = Vec::new();
        for line in lines("examples") {
            let turn = line.split_once('|').map(|(u, a)| (u.trim(), a.trim()));
            match turn {
                Some((u, a)) if !u.is_empty() && !a.is_empty() => examples.push((u.to_string(), a.to_string())),
                _ => return Err(format!("[examples] needs `line | reply`: {line}")),
            }
        }
        for line in lines("replies") {
            check_reply(line)?;
        }
        let silence = lines("silence").join(" ");
        if !silence.is_empty() && !silence.contains("{talk}") {
            return Err("[silence] needs {talk}".into());
        }
        let words = |name: &str| -> Vec<String> {
            let list = lines(name).iter().flat_map(|l| l.split(','));
            list.map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect()
        };
        let mut call_words = words("call words");
        if call_words.is_empty() {
            let name = stem.to_lowercase();
            call_words =
                name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
        }
        if call_words.is_empty() {
            return Err("[call words] is missing".into());
        }
        let joke_words = words("joke words");
        Ok(Self {
            system: format!("{system} {}", p.tag_rule),
            examples,
            jokes: lines("jokes").join("\n"),
            joke_intros: lines("joke intros").iter().map(|l| l.to_string()).collect(),
            joke_words: if joke_words.is_empty() { owned(p.joke_words) } else { joke_words },
            call_words,
            silence: if silence.is_empty() { p.silence_prompt.into() } else { silence },
            replies: lines("replies").join("\n"),
        })
    }

    /// The built-in character as a file to edit.
    pub fn template(language: Language) -> String {
        language.profile().character.to_string()
    }
}

fn owned(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn check_reply(line: &str) -> Result<(), String> {
    let fits = match line.strip_prefix('=') {
        Some(quip) => {
            let mut parts = quip.split('|').map(str::trim);
            let triggers = parts.next().unwrap_or_default();
            let answers: Vec<&str> = parts.collect();
            triggers.split(';').any(|t| !t.trim().is_empty())
                && !answers.is_empty()
                && answers.iter().all(|a| !a.is_empty())
        }
        None => line.split_once('|').is_some_and(|(u, a)| !u.trim().is_empty() && !a.trim().is_empty()),
    };
    if fits { Ok(()) } else { Err(format!("[replies] can't read: {line}")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_character_is_a_complete_file() {
        for language in Language::ALL {
            let c = Character::parse(&Character::template(language), "x", language).unwrap();
            assert!(c.system.ends_with(language.profile().tag_rule), "{language:?}");
            assert!(c.system.contains("Kinda Intelligent, Totally Thirsty"), "{language:?}");
            assert!(c.call_words.iter().any(|w| w == "kitt"), "{language:?}");
            let filled = [c.examples.len(), c.jokes.len(), c.joke_intros.len(), c.replies.len()];
            assert!(filled.iter().all(|n| *n > 0), "{language:?}");
        }
    }

    #[test]
    fn a_small_file_needs_only_a_system_prompt() {
        let c = Character::parse("[system]\nYou are Rosa,\nkind to {name}.", "Rosa", Language::English).unwrap();
        assert!(c.system.starts_with("You are Rosa, kind to Michael. Square brackets"));
        assert_eq!(c.call_words, ["rosa"]);
        assert!(c.examples.is_empty() && c.jokes.is_empty() && c.replies.is_empty());
    }

    #[test]
    fn a_broken_file_says_what_is_wrong() {
        for (text, wrong) in [
            ("no section", "before the first"),
            ("[examples]\nHi | Hello", "[system]"),
            ("[system]\nx\n[what]\ny", "unknown section"),
            ("[system]\nx\n[examples]\nno reply", "[examples]"),
            ("[system]\nx\n[replies]\n= trigger", "[replies]"),
            ("[system]\nx\n[replies]\n= a | b | ", "[replies]"),
            ("[system]\nx\n[silence]\nno talk", "{talk}"),
        ] {
            let e = Character::parse(text, "name", Language::German).err().unwrap();
            assert!(e.contains(wrong), "{text}: {e}");
        }
        assert!(Character::parse("[system]\nx", "---", Language::German).is_err(), "nothing to be called by");
    }
}
