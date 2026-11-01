//! Writes numbers out as words, since the Pocket voices can't read digits.

use crate::language::Language;

const DE_ONES: [&str; 20] = [
    "null",
    "eins",
    "zwei",
    "drei",
    "vier",
    "fünf",
    "sechs",
    "sieben",
    "acht",
    "neun",
    "zehn",
    "elf",
    "zwölf",
    "dreizehn",
    "vierzehn",
    "fünfzehn",
    "sechzehn",
    "siebzehn",
    "achtzehn",
    "neunzehn",
];
const DE_TENS: [&str; 10] =
    ["", "", "zwanzig", "dreißig", "vierzig", "fünfzig", "sechzig", "siebzig", "achtzig", "neunzig"];
const EN_ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const EN_TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

/// `text` with every number and the signs % and € as words.
pub fn spell(text: &str, language: Language) -> String {
    let (decimal, group) = match language {
        Language::German => (',', '.'),
        Language::English => ('.', ','),
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !c.is_ascii_digit() {
            match c {
                '%' => push_word(&mut out, if language == Language::German { "Prozent" } else { "percent" }),
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
        let mut words = integer(&digits, !grouped, language);
        if i + 1 < chars.len() && chars[i] == decimal && chars[i + 1].is_ascii_digit() {
            let n = run(&chars, i + 1);
            let point = if language == Language::German { "Komma" } else { "point" };
            let tail: Vec<_> = chars[i + 1..i + 1 + n].iter().map(|d| ones(language)[digit(*d)]).collect();
            words = format!("{words} {point} {}", tail.join(" "));
            i += 1 + n;
        }
        if out.ends_with(|c: char| c.is_alphabetic()) {
            out.push(' ');
        }
        out.push_str(&words);
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

fn ones(language: Language) -> &'static [&'static str; 20] {
    match language {
        Language::German => &DE_ONES,
        Language::English => &EN_ONES,
    }
}

/// Long digit strings and leading zeros are read digit by digit; 1100 to 1999 as a year unless `may_be_year` is off.
fn integer(digits: &str, may_be_year: bool, language: Language) -> String {
    let n = match digits.parse::<u64>() {
        Ok(n) if digits.len() <= 9 && (digits.len() == 1 || !digits.starts_with('0')) => n,
        _ => {
            let words: Vec<_> = digits.chars().map(|d| ones(language)[digit(d)]).collect();
            return words.join(" ");
        }
    };
    if may_be_year && digits.len() == 4 && (1100..2000).contains(&n) {
        return match language {
            Language::German => format!("{}hundert{}", de_below_100(n / 100, false), de_tail(n % 100)),
            Language::English if n % 100 == 0 => format!("{} hundred", en_below_100(n / 100)),
            Language::English if n % 100 < 10 => {
                format!("{} oh {}", en_below_100(n / 100), EN_ONES[(n % 100) as usize])
            }
            Language::English => format!("{} {}", en_below_100(n / 100), en_below_100(n % 100)),
        };
    }
    match language {
        Language::German => de(n),
        Language::English => en(n),
    }
}

fn de(n: u64) -> String {
    if n < 1000 {
        return if n < 100 { de_below_100(n, true) } else { de_below_1000(n) };
    }
    if n < 1_000_000 {
        let thousands = n / 1000;
        let head = if thousands == 1 { String::new() } else { de_below_1000_compound(thousands) };
        return format!("{head}tausend{}", de_tail(n % 1000));
    }
    let millions = n / 1_000_000;
    let head = if millions == 1 {
        "eine Million".to_string()
    } else {
        format!("{} Millionen", de_below_1000_compound(millions))
    };
    let rest = n % 1_000_000;
    if rest == 0 { head } else { format!("{head} {}", de(rest)) }
}

/// The rest after hundert or tausend; "eins" stays, as in "hunderteins".
fn de_tail(n: u64) -> String {
    match n {
        0 => String::new(),
        n if n < 100 => de_below_100(n, true),
        n => de_below_1000(n),
    }
}

fn de_below_1000(n: u64) -> String {
    let hundreds = n / 100;
    let head = if hundreds == 1 { String::new() } else { de_below_100(hundreds, false) };
    format!("{head}hundert{}", de_tail(n % 100))
}

/// A number in front of tausend or Millionen: "ein", not "eins".
fn de_below_1000_compound(n: u64) -> String {
    if n < 100 {
        de_below_100(n, false)
    } else {
        format!("{}{}", de_below_1000(n - n % 100), de_compound_tail(n % 100))
    }
}

fn de_compound_tail(n: u64) -> String {
    if n == 0 { String::new() } else { de_below_100(n, false) }
}

/// `alone` says "eins" for 1; in compounds it is "ein".
fn de_below_100(n: u64, alone: bool) -> String {
    match n {
        1 if !alone => "ein".into(),
        n if n < 20 => DE_ONES[n as usize].into(),
        n if n % 10 == 0 => DE_TENS[(n / 10) as usize].into(),
        n => format!("{}und{}", de_below_100(n % 10, false), DE_TENS[(n / 10) as usize]),
    }
}

fn en(n: u64) -> String {
    let mut parts = Vec::new();
    for (size, name) in [(1_000_000, "million"), (1000, "thousand")] {
        if n >= size && !(n / size).is_multiple_of(1000) {
            parts.push(format!("{} {name}", en_below_1000((n / size) % 1000)));
        }
    }
    if !n.is_multiple_of(1000) || n == 0 {
        parts.push(en_below_1000(n % 1000));
    }
    parts.join(" ")
}

fn en_below_1000(n: u64) -> String {
    match (n / 100, n % 100) {
        (0, rest) => en_below_100(rest),
        (h, 0) => format!("{} hundred", EN_ONES[h as usize]),
        (h, rest) => format!("{} hundred {}", EN_ONES[h as usize], en_below_100(rest)),
    }
}

fn en_below_100(n: u64) -> String {
    match n {
        n if n < 20 => EN_ONES[n as usize].into(),
        n if n % 10 == 0 => EN_TENS[(n / 10) as usize].into(),
        n => format!("{}-{}", EN_TENS[(n / 10) as usize], EN_ONES[(n % 10) as usize]),
    }
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
    fn german_numbers() {
        let cases = [
            (0, "null"),
            (1, "eins"),
            (21, "einundzwanzig"),
            (100, "hundert"),
            (101, "hunderteins"),
            (365, "dreihundertfünfundsechzig"),
            (1000, "tausend"),
            (2024, "zweitausendvierundzwanzig"),
            (21_000, "einundzwanzigtausend"),
            (101_000, "hunderteintausend"),
            (1_000_000, "eine Million"),
            (3_000_005, "drei Millionen fünf"),
        ];
        for (n, words) in cases {
            assert_eq!(de(n), words, "{n}");
        }
    }

    #[test]
    fn english_numbers() {
        assert_eq!(en(0), "zero");
        assert_eq!(en(42), "forty-two");
        assert_eq!(en(305), "three hundred five");
        assert_eq!(en(2024), "two thousand twenty-four");
        assert_eq!(en(1_000_001), "one million one");
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
