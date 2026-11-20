//! Writes numbers out as words, since the Pocket voices can't read digits.

use crate::think::language::{Language, Numbers};

/// `text` with every number and the signs % and € as words.
pub fn spell(text: &str, language: Language) -> String {
    let words = &language.profile().numbers;
    let (decimal, group) = (words.decimal, words.group);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !c.is_ascii_digit() {
            match c {
                '%' => push_word(&mut out, words.percent),
                '€' => push_word(&mut out, "Euro"),
                _ => out.push(c),
            }
            i += 1;
            continue;
        }
        let mut digits = String::new();
        let mut grouped = false;
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                digits.push(chars[i]);
                i += 1;
            } else if chars[i] == group && (1..=3).contains(&run_back(&chars, i)) && run(&chars, i + 1) == 3 {
                grouped = true;
                i += 1;
            } else {
                break;
            }
        }
        let mut said = integer(&digits, !grouped, words);
        if i + 1 < chars.len() && chars[i] == decimal && chars[i + 1].is_ascii_digit() {
            let n = run(&chars, i + 1);
            let tail: Vec<_> = chars[i + 1..i + 1 + n].iter().map(|d| words.digits[digit(*d)]).collect();
            said = format!("{said} {} {}", words.point, tail.join(" "));
            i += 1 + n;
        }
        if out.ends_with(|c: char| c.is_alphabetic()) {
            out.push(' ');
        }
        out.push_str(&said);
        if i < chars.len() && chars[i].is_alphabetic() {
            out.push(' ');
        }
    }
    out
}

fn push_word(out: &mut String, word: &str) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(word);
}

/// Digits starting at `i`.
fn run(chars: &[char], i: usize) -> usize {
    chars[i.min(chars.len())..].iter().take_while(|c| c.is_ascii_digit()).count()
}

/// Digits right before `i`, back to the previous separator.
fn run_back(chars: &[char], i: usize) -> usize {
    chars[..i].iter().rev().take_while(|c| c.is_ascii_digit()).count()
}

fn digit(c: char) -> usize {
    c as usize - '0' as usize
}

/// Long digit strings and leading zeros are read digit by digit; 1100 to 1999 as a year unless `may_be_year` is off.
fn integer(digits: &str, may_be_year: bool, words: &Numbers) -> String {
    let n = match digits.parse::<u64>() {
        Ok(n) if digits.len() <= 9 && (digits.len() == 1 || !digits.starts_with('0')) => n,
        _ => {
            let said: Vec<_> = digits.chars().map(|d| words.digits[digit(d)]).collect();
            return said.join(" ");
        }
    };
    if may_be_year && digits.len() == 4 && (1100..2000).contains(&n) {
        return (words.year)(n);
    }
    (words.say)(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de_text(t: &str) -> String {
        spell(t, Language::German)
    }

    fn en_text(t: &str) -> String {
        spell(t, Language::English)
    }

    #[test]
    fn numbers_in_text() {
        assert_eq!(de_text("Es ist 12 Uhr."), "Es ist zwölf Uhr.");
        assert_eq!(
            de_text("Das kostet 3,50€ oder 1.000 Euro."),
            "Das kostet drei Komma fünf null Euro oder tausend Euro."
        );
        assert_eq!(de_text("Seit 1985 und 50% mehr."), "Seit neunzehnhundertfünfundachtzig und fünfzig Prozent mehr.");
        assert_eq!(de_text("Gleis 7b, Nummer 007."), "Gleis sieben b, Nummer null null sieben.");
        assert_eq!(
            en_text("It's 3.5 miles, 1,200 feet, since 1905."),
            "It's three point five miles, one thousand two hundred feet, since nineteen oh five."
        );
    }
}
