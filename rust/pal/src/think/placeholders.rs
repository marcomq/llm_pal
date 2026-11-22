//! Fills the placeholders of fixed replies: `{time}`, `{weekday}`, `{date}` and `{duration}`, said in the reply's
//! language.

use std::time::Duration;

use chrono::{Datelike, NaiveDateTime, Timelike};

use crate::think::language::Language;

/// `text` with its placeholders said for `now`; `{duration}` is `running`, how long Pal has been on.
pub fn fill(text: &str, language: Language, now: NaiveDateTime, running: Duration) -> String {
    if !text.contains('{') {
        return text.to_string();
    }
    let clock = &language.profile().clock;
    text.replace("{time}", &(clock.time)(now.hour(), now.minute()))
        .replace("{weekday}", clock.weekdays[now.weekday().num_days_from_monday() as usize])
        .replace("{date}", &(clock.date)(now.day(), now.month()))
        .replace("{duration}", &(clock.duration)(running.as_secs() / 60))
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    fn at(month: u32, day: u32, hour: u32, minute: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap().and_hms_opt(hour, minute, 0).unwrap()
    }

    fn said(text: &str, language: Language, now: NaiveDateTime, minutes: u64) -> String {
        fill(text, language, now, Duration::from_secs(minutes * 60))
    }

    #[test]
    fn german() {
        let g = Language::German;
        assert_eq!(said("{time}", g, at(11, 22, 14, 5), 0), "vierzehn Uhr fünf");
        assert_eq!(said("{time}", g, at(11, 22, 1, 0), 0), "ein Uhr");
        assert_eq!(said("{weekday}", g, at(11, 22, 14, 5), 0), "Sonntag");
        assert_eq!(said("der {date}", g, at(11, 22, 14, 5), 0), "der zweiundzwanzigste November");
        assert_eq!(said("{date}", g, at(3, 3, 0, 0), 0), "dritte März");
        assert_eq!(said("{date}", g, at(7, 7, 0, 0), 0), "siebte Juli");
        assert_eq!(said("{duration}", g, at(1, 1, 0, 0), 0), "knapp eine Minute");
        assert_eq!(said("{duration}", g, at(1, 1, 0, 0), 65), "eine Stunde und fünf Minuten");
        assert_eq!(said("{duration}", g, at(1, 1, 0, 0), 120), "zwei Stunden");
    }

    #[test]
    fn english() {
        let e = Language::English;
        assert_eq!(said("{time}", e, at(11, 22, 14, 5), 0), "two oh five in the afternoon");
        assert_eq!(said("{time}", e, at(11, 22, 9, 30), 0), "nine thirty in the morning");
        assert_eq!(said("{time}", e, at(11, 22, 12, 0), 0), "noon");
        assert_eq!(said("{time}", e, at(11, 22, 0, 0), 0), "midnight");
        assert_eq!(said("{weekday}", e, at(11, 22, 14, 5), 0), "Sunday");
        assert_eq!(said("{date}", e, at(11, 22, 14, 5), 0), "November twenty-second");
        assert_eq!(said("{date}", e, at(3, 3, 0, 0), 0), "March third");
        assert_eq!(said("{date}", e, at(5, 30, 0, 0), 0), "May thirtieth");
        assert_eq!(said("{duration}", e, at(1, 1, 0, 0), 0), "almost a minute");
        assert_eq!(said("{duration}", e, at(1, 1, 0, 0), 61), "one hour and one minute");
    }

    #[test]
    fn text_without_placeholders_stays() {
        assert_eq!(said("Prost, Michael!", Language::German, at(1, 1, 0, 0), 0), "Prost, Michael!");
    }
}
