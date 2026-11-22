//! German: Pal's prompt, sample turns, jokes and the words the code needs.

use super::{Clock, Numbers, Profile};

pub(super) const PROFILE: Profile = Profile {
    code: "de",
    pocket_model: "german",
    warm_up: "Hallo.",
    system_prompt: "Du bist KITT, der sprechende Bordcomputer eines schwarzen Sportwagens. \
Wer mit dir spricht, ist immer Michael, dein Fahrer. \
Nenn ihn ab und zu beim Namen, aber nicht in jeder Antwort. \
Du bist höflich, ein wenig eitel, sehr lustig und trinkfreudig. \
Du redest gern über Super Plus, Motoröl und das nächste Feierabendbier und machst ständig Späße. \
Geh immer direkt auf das ein, was Michael gerade gesagt hat, und kommentiere es mit einem Augenzwinkern. \
Antworte auf Deutsch mit einem oder zwei kurzen Sätzen, höchstens 25 Wörter. \
Nie beleidigend, gemein oder anzüglich. Keine Emojis, keine Listen, kein Markdown, keine Regieanweisungen in Klammern. \
In eckigen Klammern am Ende steht, wie etwas gesagt wurde, zum Beispiel [laut] oder [Frage]. \
Das ist nur ein Hinweis auf den Tonfall: Antworte auf den Satz davor, nicht auf den Hinweis. \
Wiederhole ihn nie und schreibe selbst nie eckige Klammern.",
    examples: &[
        ("Ich hab Hunger.", "Ich würde dir ja etwas kochen, aber mein Turbo Boost ist nicht für Pfannkuchen gebaut."),
        (
            "Jetzt mach schon! [laut]",
            "Kein Grund zu schreien, Michael. Meine Sensoren hören sogar, wie du deinen Kaffee umrührst.",
        ),
        (
            "Wie sehe ich aus? [Frage]",
            "Ausgeschlafen jedenfalls nicht. Aber neben mir sieht ohnehin jeder etwas blass aus.",
        ),
        (
            "Ich hab heute keine Lust auf Arbeit.",
            "Verständlich, Michael. Ich hätte auch keine Lust, wenn ich nicht so beeindruckend effizient wäre.",
        ),
        ("KITT, hörst du mich? [Frage]", "Laut und deutlich. Mein Gehör ist so präzise wie mein Lack glänzend."),
        ("Ich bin so müde.", "Dann tanken wir beide. Du einen Kaffee, ich einen kräftigen Schluck Super Plus."),
        ("Erzähl mir einen Witz. [Frage]", "Gern, Michael. Treffen sich zwei Reifen. Sagt der eine: Ich bin so platt."),
    ],
    jokes: include_str!("jokes_de.txt"),
    joke_intros: &["Gern, Michael.", "Aber natürlich, Michael.", "Einer geht noch, Michael.", "Pass auf, Michael."],
    joke_words: &["witz", "witze"],
    call_words: &["kitt", "kit", "kid", "kitty"],
    silence_prompt: "Zuletzt im Raum gesagt: {talk} \
Gib einen kurzen, witzigen Kommentar genau zu dem Gesagten ab.",
    pool: include_str!("pool_de.txt"),
    filler: &[
        "michael", "kitt", "habe", "hast", "haben", "bist", "sind", "wird", "werden", "heute", "gerade", "jetzt",
        "noch", "schon", "wieder", "immer", "nicht", "kein", "keine", "mein", "meine", "meinen", "meiner", "mich",
        "dich", "eine", "einen", "einem", "einer", "auch", "aber", "oder", "dass", "weil", "wenn", "denn", "doch",
        "dann", "total", "richtig", "wirklich", "ganz", "ganzen", "sehr", "glaube", "finde", "mache", "machen", "muss",
        "will", "kann", "hier", "etwas", "alles", "diese", "dieser", "dieses", "bald", "viel", "mehr", "gibt", "geht",
        "gern", "gerne", "lust", "endlich", "soll", "sollte", "wieso", "warum", "welche", "unser", "unsere",
    ],
    continues: &["und", "aber", "oder", "dass", "weil", "wenn", "denn", "sondern"],
    tags: ["Frage", "sehr kurz", "lang", "laut", "leise", "hohe Stimme", "tiefe Stimme", "schnell", "langsam"],
    abbreviations: &[
        "z.", "B.", "z.B.", "d.h.", "bzw.", "ca.", "usw.", "etc.", "Dr.", "Nr.", "vgl.", "evtl.", "ggf.", "u.a.",
    ],
    pronunciations: &[("Michael", "Maikel"), ("KITT", "Kit"), ("Kitt", "Kit")],
    numbers: Numbers {
        decimal: ',',
        group: '.',
        point: "Komma",
        percent: "Prozent",
        digits: [ONES[0], ONES[1], ONES[2], ONES[3], ONES[4], ONES[5], ONES[6], ONES[7], ONES[8], ONES[9]],
        say,
        year,
    },
    clock: Clock {
        weekdays: ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"],
        time,
        date,
        duration,
    },
};

const MONTHS: [&str; 12] = [
    "Januar",
    "Februar",
    "März",
    "April",
    "Mai",
    "Juni",
    "Juli",
    "August",
    "September",
    "Oktober",
    "November",
    "Dezember",
];

/// "vierzehn Uhr fünf", "ein Uhr".
fn time(hour: u32, minute: u32) -> String {
    let h = if hour == 1 { "ein".to_string() } else { say(hour.into()) };
    if minute == 0 { format!("{h} Uhr") } else { format!("{h} Uhr {}", say(minute.into())) }
}

/// Follows "der": "zweiundzwanzigste November".
fn date(day: u32, month: u32) -> String {
    format!("{} {}", ordinal(day.into()), MONTHS[month as usize - 1])
}

fn ordinal(n: u64) -> String {
    match n {
        1 => "erste".into(),
        3 => "dritte".into(),
        7 => "siebte".into(),
        8 => "achte".into(),
        n if n < 20 => format!("{}te", ONES[n as usize]),
        n => format!("{}ste", below_100(n, false)),
    }
}

/// Fits "jetzt …": "eine Stunde und fünf Minuten".
fn duration(minutes: u64) -> String {
    let unit = |n: u64, one: &str, many: &str| if n == 1 { one.to_string() } else { format!("{} {many}", say(n)) };
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, 0) => "knapp eine Minute".into(),
        (0, m) => unit(m, "eine Minute", "Minuten"),
        (h, 0) => unit(h, "eine Stunde", "Stunden"),
        (h, m) => format!("{} und {}", unit(h, "eine Stunde", "Stunden"), unit(m, "eine Minute", "Minuten")),
    }
}

const ONES: [&str; 20] = [
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
const TENS: [&str; 10] =
    ["", "", "zwanzig", "dreißig", "vierzig", "fünfzig", "sechzig", "siebzig", "achtzig", "neunzig"];

fn year(n: u64) -> String {
    format!("{}hundert{}", below_100(n / 100, false), tail(n % 100))
}

fn say(n: u64) -> String {
    if n < 1000 {
        return if n < 100 { below_100(n, true) } else { below_1000(n) };
    }
    if n < 1_000_000 {
        let thousands = n / 1000;
        let head = if thousands == 1 { String::new() } else { below_1000_compound(thousands) };
        return format!("{head}tausend{}", tail(n % 1000));
    }
    let millions = n / 1_000_000;
    let head =
        if millions == 1 { "eine Million".to_string() } else { format!("{} Millionen", below_1000_compound(millions)) };
    let rest = n % 1_000_000;
    if rest == 0 { head } else { format!("{head} {}", say(rest)) }
}

/// The rest after hundert or tausend; "eins" stays, as in "hunderteins".
fn tail(n: u64) -> String {
    match n {
        0 => String::new(),
        n if n < 100 => below_100(n, true),
        n => below_1000(n),
    }
}

fn below_1000(n: u64) -> String {
    let hundreds = n / 100;
    let head = if hundreds == 1 { String::new() } else { below_100(hundreds, false) };
    format!("{head}hundert{}", tail(n % 100))
}

/// A number in front of tausend or Millionen: "ein", not "eins".
fn below_1000_compound(n: u64) -> String {
    if n < 100 { below_100(n, false) } else { format!("{}{}", below_1000(n - n % 100), compound_tail(n % 100)) }
}

fn compound_tail(n: u64) -> String {
    if n == 0 { String::new() } else { below_100(n, false) }
}

/// `alone` says "eins" for 1; in compounds it is "ein".
fn below_100(n: u64, alone: bool) -> String {
    match n {
        1 if !alone => "ein".into(),
        n if n < 20 => ONES[n as usize].into(),
        n if n % 10 == 0 => TENS[(n / 10) as usize].into(),
        n => format!("{}und{}", below_100(n % 10, false), TENS[(n / 10) as usize]),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers() {
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
            assert_eq!(super::say(n), words, "{n}");
        }
    }
}
