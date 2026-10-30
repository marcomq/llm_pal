//! Prosody of a 16 kHz mono segment: YIN pitch, loudness, duration and syllable rate.

use crate::event::Features;
use crate::resample::TARGET_RATE;

const RATE: f32 = TARGET_RATE as f32;
const FRAME: usize = 640;
const HOP: usize = 160;
const F0_MIN: f32 = 70.0;
const F0_MAX: f32 = 500.0;
const YIN_THRESHOLD: f32 = 0.15;
const SILENCE_DB: f32 = -50.0;
const SLOPE_WINDOW_S: f32 = 0.5;
const HOP_S: f32 = HOP as f32 / RATE;

pub fn analyze(pcm: &[f32]) -> Features {
    let rms_contour_db = rms_contour(pcm);
    let pitch_contour_hz: Vec<Option<f32>> =
        frames(pcm).zip(&rms_contour_db).map(|(frame, &db)| if db < SILENCE_DB { None } else { yin(&frame) }).collect();

    let mut voiced: Vec<f32> = pitch_contour_hz.iter().flatten().copied().collect();
    voiced.sort_by(f32::total_cmp);
    let pct = |p: f32| (!voiced.is_empty()).then(|| voiced[((voiced.len() - 1) as f32 * p).round() as usize]);
    let f0_median_hz = pct(0.5);

    let loud: Vec<f32> = rms_contour_db.iter().copied().filter(|&d| d >= SILENCE_DB).collect();
    let rms_mean_db = if loud.is_empty() { SILENCE_DB } else { loud.iter().sum::<f32>() / loud.len() as f32 };
    let duration_s = pcm.len() as f32 / RATE;

    Features {
        duration_s,
        f0_median_hz,
        f0_min_hz: pct(0.05),
        f0_max_hz: pct(0.95),
        f0_slope_st_per_s: f0_median_hz.and_then(|m| final_slope(&pitch_contour_hz, m)),
        voiced_ratio: if pitch_contour_hz.is_empty() {
            0.0
        } else {
            voiced.len() as f32 / pitch_contour_hz.len() as f32
        },
        rms_mean_db,
        rms_max_db: rms_contour_db.iter().copied().fold(SILENCE_DB, f32::max),
        syllable_rate: if duration_s > 0.0 { syllable_peaks(&rms_contour_db) as f32 / duration_s } else { 0.0 },
        pitch_contour_hz,
        rms_contour_db,
    }
}

/// Frames of `FRAME` samples every `HOP`; the last ones are zero padded so every hop has a frame.
fn frames(pcm: &[f32]) -> impl Iterator<Item = Vec<f32>> + '_ {
    (0..pcm.len().div_ceil(HOP)).map(move |i| {
        let start = i * HOP;
        let mut f = vec![0.0; FRAME];
        let end = (start + FRAME).min(pcm.len());
        f[..end - start].copy_from_slice(&pcm[start..end]);
        f
    })
}

fn rms_contour(pcm: &[f32]) -> Vec<f32> {
    frames(pcm)
        .map(|f| {
            let ms = f.iter().map(|x| x * x).sum::<f32>() / FRAME as f32;
            (10.0 * ms.max(1e-12).log10()).max(-120.0)
        })
        .collect()
}

/// YIN (de Cheveigné & Kawahara 2002) with cumulative mean normalisation and parabolic refinement.
fn yin(frame: &[f32]) -> Option<f32> {
    let tau_min = (RATE / F0_MAX) as usize;
    let tau_max = (RATE / F0_MIN) as usize + 1;
    let w = frame.len() - tau_max;
    let mut d = vec![0.0f32; tau_max + 1];
    for (tau, slot) in d.iter_mut().enumerate().skip(1) {
        *slot = (0..w).map(|j| (frame[j] - frame[j + tau]).powi(2)).sum();
    }
    let mut cmnd = vec![1.0f32; tau_max + 1];
    let mut running = 0.0;
    for tau in 1..=tau_max {
        running += d[tau];
        cmnd[tau] = if running > 0.0 { d[tau] * tau as f32 / running } else { 1.0 };
    }
    let mut tau = (tau_min..tau_max).find(|&t| cmnd[t] < YIN_THRESHOLD)?;
    while tau + 1 < tau_max && cmnd[tau + 1] < cmnd[tau] {
        tau += 1;
    }
    let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-9 { 0.5 * (a - c) / denom } else { 0.0 };
    let f0 = RATE / (tau as f32 + shift.clamp(-1.0, 1.0));
    (F0_MIN..=F0_MAX).contains(&f0).then_some(f0)
}

/// Least-squares slope in semitones/s over the voiced frames in the last `SLOPE_WINDOW_S` of voicing.
fn final_slope(contour: &[Option<f32>], median: f32) -> Option<f32> {
    let last = contour.iter().rposition(Option::is_some)?;
    let first = last.saturating_sub((SLOPE_WINDOW_S / HOP_S) as usize);
    let pts: Vec<(f32, f32)> =
        (first..=last).filter_map(|i| contour[i].map(|f| (i as f32 * HOP_S, 12.0 * (f / median).log2()))).collect();
    if pts.len() < 5 {
        return None;
    }
    let n = pts.len() as f32;
    let (mx, my) = (pts.iter().map(|p| p.0).sum::<f32>() / n, pts.iter().map(|p| p.1).sum::<f32>() / n);
    let sxx: f32 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f32 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    (sxx > 0.0).then(|| sxy / sxx)
}

/// Peaks of the smoothed energy envelope at least 100 ms apart that rise 3 dB above the preceding dip.
fn syllable_peaks(rms_db: &[f32]) -> usize {
    const SMOOTH: usize = 2;
    const MIN_GAP: usize = 10;
    const PROMINENCE_DB: f32 = 3.0;
    let smooth: Vec<f32> = (0..rms_db.len())
        .map(|i| {
            let win = &rms_db[i.saturating_sub(SMOOTH)..(i + SMOOTH + 1).min(rms_db.len())];
            win.iter().sum::<f32>() / win.len() as f32
        })
        .collect();
    let floor = smooth.iter().copied().fold(f32::MIN, f32::max) - 25.0;
    let mut peaks = 0;
    let mut last_peak: Option<usize> = None;
    let mut dip = f32::MAX;
    for i in 1..smooth.len().saturating_sub(1) {
        dip = dip.min(smooth[i]);
        let is_peak = smooth[i] >= smooth[i - 1] && smooth[i] > smooth[i + 1];
        let far = last_peak.is_none_or(|p| i - p >= MIN_GAP);
        if is_peak && far && smooth[i] > floor.max(SILENCE_DB) && smooth[i] - dip >= PROMINENCE_DB {
            peaks += 1;
            last_peak = Some(i);
            dip = smooth[i];
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn tone(f: impl Fn(f32) -> f32, secs: f32) -> Vec<f32> {
        let mut phase = 0.0f32;
        (0..(RATE * secs) as usize)
            .map(|i| {
                phase += TAU * f(i as f32 / RATE) / RATE;
                phase.sin() * 0.3
            })
            .collect()
    }

    #[test]
    fn sine_pitch_within_one_percent() {
        for hz in [120.0, 220.0, 400.0] {
            let feat = analyze(&tone(|_| hz, 1.0));
            let m = feat.f0_median_hz.unwrap();
            assert!((m - hz).abs() / hz < 0.01, "{hz}: {m}");
            assert!(feat.voiced_ratio > 0.9);
        }
    }

    #[test]
    fn noise_is_mostly_unvoiced() {
        let mut s = 12345u32;
        let noise: Vec<f32> = (0..16_000)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.6
            })
            .collect();
        assert!(analyze(&noise).voiced_ratio < 0.2);
    }

    #[test]
    fn rising_and_falling_glides_have_matching_slope_sign() {
        let up = analyze(&tone(|t| 150.0 * 2f32.powf(t), 1.0));
        let down = analyze(&tone(|t| 300.0 * 2f32.powf(-t), 1.0));
        let (u, d) = (up.f0_slope_st_per_s.unwrap(), down.f0_slope_st_per_s.unwrap());
        assert!((u - 12.0).abs() < 1.5, "up {u}");
        assert!((d + 12.0).abs() < 1.5, "down {d}");
    }

    #[test]
    fn silence_has_no_pitch() {
        let feat = analyze(&vec![0.0; 8000]);
        assert_eq!(feat.f0_median_hz, None);
        assert_eq!(feat.voiced_ratio, 0.0);
        assert_eq!(feat.syllable_rate, 0.0);
    }

    #[test]
    fn syllable_rate_follows_amplitude_modulation() {
        let carrier = tone(|_| 180.0, 3.0);
        let pcm: Vec<f32> =
            carrier.iter().enumerate().map(|(i, x)| x * (0.5 - 0.5 * (TAU * 4.0 * i as f32 / RATE).cos())).collect();
        let rate = analyze(&pcm).syllable_rate;
        assert!((rate - 4.0).abs() < 0.7, "rate {rate}");
    }
}
