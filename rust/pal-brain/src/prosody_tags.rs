//! Turns segment prosody into short tags for the LLM prompt, relative to the speaker's running baseline.

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

/// The order of `Profile::tags`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Question,
    VeryShort,
    Long,
    Loud,
    Quiet,
    HighVoice,
    LowVoice,
    Fast,
    Slow,
}

#[derive(Clone, Default)]
pub struct ProsodyTags {
    seen: u32,
    rms_db: f32,
    f0_hz: Option<f32>,
    rate: f32,
}

impl ProsodyTags {
    /// Tags for this segment, then folds it into the baseline.
    pub fn tag(&mut self, f: &Features) -> Vec<Tag> {
        let mut tags = Vec::new();
        if f.f0_slope_st_per_s.is_some_and(|s| s >= QUESTION_ST_PER_S) {
            tags.push(Tag::Question);
        }
        if f.duration_s < SHORT_S {
            tags.push(Tag::VeryShort);
        } else if f.duration_s > LONG_S {
            tags.push(Tag::Long);
        }
        if self.seen >= WARMUP {
            let d = f.rms_mean_db - self.rms_db;
            tags.extend((d >= LOUDNESS_DB).then_some(Tag::Loud).or((d <= -LOUDNESS_DB).then_some(Tag::Quiet)));
            if let (Some(f0), Some(base)) = (f.f0_median_hz, self.f0_hz) {
                let st = 12.0 * (f0 / base).log2();
                tags.extend((st >= PITCH_ST).then_some(Tag::HighVoice).or((st <= -PITCH_ST).then_some(Tag::LowVoice)));
            }
            if self.rate > 0.0 {
                let r = f.syllable_rate / self.rate;
                tags.extend((r >= RATE_FACTOR).then_some(Tag::Fast).or((r <= 1.0 / RATE_FACTOR).then_some(Tag::Slow)));
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

/// Such as "[question] [loud]" in `language`, empty without tags.
pub fn format(tags: &[Tag], language: Language) -> String {
    let names = &language.profile().tags;
    tags.iter().map(|&t| format!("[{}]", names[t as usize])).collect::<Vec<_>>().join(" ")
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
        assert_eq!(t.tag(&feat(-25.0, 150.0, 4.0, 3.8)), [Tag::Question]);
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
        assert_eq!(t.tag(&excited), [Tag::Loud, Tag::HighVoice, Tag::Fast]);
        assert_eq!(t.tag(&feat(-40.0, 110.0, 2.0, -5.0)), [Tag::Quiet, Tag::LowVoice, Tag::Slow]);
    }

    #[test]
    fn duration_and_format() {
        let mut t = ProsodyTags::default();
        let tags = t.tag(&Features { duration_s: 0.5, ..Default::default() });
        assert_eq!(format(&tags, Language::English), "[very short]");
        assert_eq!(format(&[], Language::English), "");
        assert_eq!(format(&[Tag::Question, Tag::Loud], Language::English), "[question] [loud]");
        assert_eq!(format(&[Tag::Question, Tag::Loud], Language::German), "[Frage] [laut]");
    }
}
