//! Turns segment prosody into short tags (German, or English via `format_in`) for the LLM prompt, relative to the speaker's running baseline.

use pal_core::Features;

use crate::language::Language;

const QUESTION_ST_PER_S: f32 = 2.0;
const LOUDNESS_DB: f32 = 6.0;
const PITCH_ST: f32 = 3.0;
const RATE_FACTOR: f32 = 1.3;
const SHORT_S: f32 = 0.8;
const LONG_S: f32 = 8.0;
/// Weight of a new segment in the baseline.
const ALPHA: f32 = 0.3;
/// Relative tags start after this many segments.
const WARMUP: u32 = 2;

#[derive(Clone, Default)]
pub struct ProsodyTags {
    seen: u32,
    rms_db: f32,
    f0_hz: Option<f32>,
    rate: f32,
}

impl ProsodyTags {
    /// Tags for this segment, then folds it into the baseline.
    pub fn tag(&mut self, f: &Features) -> Vec<&'static str> {
        let mut tags = Vec::new();
        if f.f0_slope_st_per_s.is_some_and(|s| s >= QUESTION_ST_PER_S) {
            tags.push("Frage");
        }
        if f.duration_s < SHORT_S {
            tags.push("sehr kurz");
        } else if f.duration_s > LONG_S {
            tags.push("lang");
        }
        if self.seen >= WARMUP {
            let d = f.rms_mean_db - self.rms_db;
            tags.extend((d >= LOUDNESS_DB).then_some("laut").or((d <= -LOUDNESS_DB).then_some("leise")));
            if let (Some(f0), Some(base)) = (f.f0_median_hz, self.f0_hz) {
                let st = 12.0 * (f0 / base).log2();
                tags.extend((st >= PITCH_ST).then_some("hohe Stimme").or((st <= -PITCH_ST).then_some("tiefe Stimme")));
            }
            if self.rate > 0.0 {
                let r = f.syllable_rate / self.rate;
                tags.extend((r >= RATE_FACTOR).then_some("schnell").or((r <= 1.0 / RATE_FACTOR).then_some("langsam")));
            }
        }
        self.update(f);
        tags
    }

    fn update(&mut self, f: &Features) {
        let a = if self.seen == 0 { 1.0 } else { ALPHA };
        let ema = |old: f32, new: f32| old + a * (new - old);
        self.rms_db = ema(self.rms_db, f.rms_mean_db);
        self.rate = ema(self.rate, f.syllable_rate);
        if let Some(f0) = f.f0_median_hz {
            self.f0_hz = Some(self.f0_hz.map_or(f0, |old| ema(old, f0)));
        }
        self.seen += 1;
    }
}

/// "[Frage] [laut]", empty without tags.
pub fn format(tags: &[&str]) -> String {
    tags.iter().map(|t| format!("[{t}]")).collect::<Vec<_>>().join(" ")
}

/// Like `format`, with the tags translated for `language`.
pub fn format_in(tags: &[&str], language: Language) -> String {
    match language {
        Language::German => format(tags),
        Language::English => format(&tags.iter().map(|t| english(t)).collect::<Vec<_>>()),
    }
}

fn english(tag: &str) -> &str {
    match tag {
        "Frage" => "question",
        "sehr kurz" => "very short",
        "lang" => "long",
        "laut" => "loud",
        "leise" => "quiet",
        "hohe Stimme" => "high voice",
        "tiefe Stimme" => "low voice",
        "schnell" => "fast",
        "langsam" => "slow",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feat(db: f32, f0: f32, rate: f32, slope: f32) -> Features {
        Features {
            duration_s: 2.0,
            rms_mean_db: db,
            f0_median_hz: Some(f0),
            syllable_rate: rate,
            f0_slope_st_per_s: Some(slope),
            ..Default::default()
        }
    }

    #[test]
    fn question_is_tagged_from_the_first_segment() {
        let mut t = ProsodyTags::default();
        assert_eq!(t.tag(&feat(-25.0, 150.0, 4.0, 3.8)), ["Frage"]);
        assert!(t.tag(&feat(-25.0, 150.0, 4.0, -19.0)).is_empty());
    }

    #[test]
    fn relative_tags_wait_for_the_baseline() {
        let mut t = ProsodyTags::default();
        let normal = feat(-25.0, 150.0, 4.0, -5.0);
        let excited = feat(-15.0, 200.0, 6.0, -5.0);
        assert!(t.tag(&excited).is_empty(), "first segment sets the baseline");
        for _ in 0..5 {
            t.tag(&normal);
        }
        assert_eq!(t.tag(&excited), ["laut", "hohe Stimme", "schnell"]);
        assert_eq!(t.tag(&feat(-40.0, 110.0, 2.0, -5.0)), ["leise", "tiefe Stimme", "langsam"]);
    }

    #[test]
    fn duration_and_format() {
        let mut t = ProsodyTags::default();
        let tags = t.tag(&Features { duration_s: 0.5, ..Default::default() });
        assert_eq!(format(&tags), "[sehr kurz]");
        assert_eq!(format(&[]), "");
        assert_eq!(format_in(&["Frage", "laut"], Language::English), "[question] [loud]");
    }
}
