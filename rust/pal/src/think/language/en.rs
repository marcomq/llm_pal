//! English: Pal's prompt, sample turns, jokes and the words the code needs.

use super::{Clock, Numbers, Profile};

pub(super) const PROFILE: Profile = Profile {
    code: "en",
    pocket_model: "english",
    warm_up: "Hello.",
    tag_rule: "Square brackets at the end say how something was said, for example [loud] or [question]. \
That is only a hint about the tone: answer the sentence before it, not the hint. \
Never repeat it and never write square brackets yourself.",
    character: include_str!("kitt_en.txt"),
    filler: &[
        "michael", "kitt", "have", "just", "really", "today", "still", "again", "always", "about", "would", "could",
        "should", "there", "their", "this", "that", "what", "with", "your", "from", "been", "were", "will", "want",
        "think", "feel", "like", "some", "very", "much", "more", "right", "going", "doing", "getting", "make", "know",
        "does", "dont", "when", "then", "them", "they", "because", "need", "into", "over", "here", "only", "also",
        "even", "well", "totally", "finally", "keeps",
    ],
    joke_words: &["joke", "jokes"],
    silence_prompt: "Last said in the room: {talk} \
Make a short, witty comment on exactly what was said.",
    continues: &["and", "but", "or", "because", "if"],
    suggestions: &[
        "shouldn't we",
        "couldn't we",
        "can't we",
        "why don't we",
        "should we not",
        "could we not",
        "why not",
    ],
    tags: ["question", "very short", "long", "loud", "quiet", "high voice", "low voice", "fast", "slow"],
    abbreviations: &["Mr.", "Mrs.", "Ms.", "Dr.", "No.", "e.g.", "i.e.", "etc.", "vs."],
    pronunciations: &[("KITT", "Kit"), ("Kitt", "Kit")],
    numbers: Numbers {
        decimal: '.',
        group: ',',
        point: "point",
        percent: "percent",
        digits: [ONES[0], ONES[1], ONES[2], ONES[3], ONES[4], ONES[5], ONES[6], ONES[7], ONES[8], ONES[9]],
        say,
        year,
    },
    clock: Clock {
        weekdays: ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"],
        time,
        date,
        duration,
    },
};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// "two oh five in the afternoon", "noon".
fn time(hour: u32, minute: u32) -> String {
    match (hour, minute) {
        (0, 0) => return "midnight".into(),
        (12, 0) => return "noon".into(),
        _ => {}
    }
    let h = ONES[if hour % 12 == 0 { 12 } else { hour as usize % 12 }];
    let said = match minute {
        0 => format!("{h} o'clock"),
        m if m < 10 => format!("{h} oh {}", ONES[m as usize]),
        m => format!("{h} {}", below_100(m.into())),
    };
    let part = match hour {
        5..=11 => "in the morning",
        12..=17 => "in the afternoon",
        18..=21 => "in the evening",
        _ => "at night",
    };
    format!("{said} {part}")
}

/// "November twenty-second".
fn date(day: u32, month: u32) -> String {
    format!("{} {}", MONTHS[month as usize - 1], ordinal(day.into()))
}

fn ordinal(n: u64) -> String {
    match n {
        1 => "first".into(),
        2 => "second".into(),
        3 => "third".into(),
        5 => "fifth".into(),
        8 => "eighth".into(),
        9 => "ninth".into(),
        12 => "twelfth".into(),
        20 => "twentieth".into(),
        30 => "thirtieth".into(),
        n if n < 20 => format!("{}th", ONES[n as usize]),
        n => format!("{}-{}", TENS[(n / 10) as usize], ordinal(n % 10)),
    }
}

/// Fits "for …": "one hour and five minutes".
fn duration(minutes: u64) -> String {
    let unit = |n: u64, name: &str| if n == 1 { format!("one {name}") } else { format!("{} {name}s", say(n)) };
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, 0) => "almost a minute".into(),
        (0, m) => unit(m, "minute"),
        (h, 0) => unit(h, "hour"),
        (h, m) => format!("{} and {}", unit(h, "hour"), unit(m, "minute")),
    }
}

const ONES: [&str; 20] = [
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
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

/// "nineteen hundred", "nineteen oh five", "nineteen eighty-five".
fn year(n: u64) -> String {
    match n % 100 {
        0 => format!("{} hundred", below_100(n / 100)),
        rest if rest < 10 => format!("{} oh {}", below_100(n / 100), ONES[rest as usize]),
        rest => format!("{} {}", below_100(n / 100), below_100(rest)),
    }
}

fn say(n: u64) -> String {
    let mut parts = Vec::new();
    for (size, name) in [(1_000_000, "million"), (1000, "thousand")] {
        if n >= size && !(n / size).is_multiple_of(1000) {
            parts.push(format!("{} {name}", below_1000((n / size) % 1000)));
        }
    }
    if !n.is_multiple_of(1000) || n == 0 {
        parts.push(below_1000(n % 1000));
    }
    parts.join(" ")
}

fn below_1000(n: u64) -> String {
    match (n / 100, n % 100) {
        (0, rest) => below_100(rest),
        (h, 0) => format!("{} hundred", ONES[h as usize]),
        (h, rest) => format!("{} hundred {}", ONES[h as usize], below_100(rest)),
    }
}

fn below_100(n: u64) -> String {
    match n {
        n if n < 20 => ONES[n as usize].into(),
        n if n % 10 == 0 => TENS[(n / 10) as usize].into(),
        n => format!("{}-{}", TENS[(n / 10) as usize], ONES[(n % 10) as usize]),
    }
}

#[cfg(test)]
mod tests {
    use super::say;

    #[test]
    fn numbers() {
        assert_eq!(say(0), "zero");
        assert_eq!(say(42), "forty-two");
        assert_eq!(say(305), "three hundred five");
        assert_eq!(say(2024), "two thousand twenty-four");
        assert_eq!(say(1_000_001), "one million one");
    }
}
