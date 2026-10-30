//! End-to-end over generated German speech (scripts/make-test-audio.sh) with the real Silero model.

use std::path::PathBuf;

use pal_core::{Event, ProcessorConfig, SegmenterConfig, process_samples};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
#[ignore = "needs models/ (scripts/fetch-models.sh) and testdata/ (scripts/make-test-audio.sh)"]
fn three_sentences_give_three_segments_and_the_question_rises() {
    let mut reader = hound::WavReader::open(root().join("testdata/de_48k.wav")).unwrap();
    let rate = reader.spec().sample_rate;
    let samples: Vec<f32> = reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let cfg = ProcessorConfig {
        vad_model: root().join("models/silero_vad.onnx"),
        segmenter: SegmenterConfig::default(),
        device: None,
        drop_echo: false,
    };
    let events = process_samples(&cfg, &samples, rate).unwrap();
    let segs: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Segment(s) => Some(s),
            _ => None,
        })
        .collect();
    let starts = events.iter().filter(|e| matches!(e, Event::SpeechStart { .. })).count();
    assert_eq!(segs.len(), 3);
    assert_eq!(starts, 3);
    assert!(!events.iter().any(|e| matches!(e, Event::BargeIn { .. })));
    for s in &segs {
        assert_eq!(s.pcm.len() as u64, (s.end_ms - s.start_ms) * 16, "pcm length matches timestamps");
        assert!(s.features.duration_s > 1.0 && s.features.f0_median_hz.is_some());
    }
    assert!(segs.windows(2).all(|w| w[1].start_ms > w[0].end_ms + 500), "pauses separate segments");
    let slope = |i: usize| segs[i].features.f0_slope_st_per_s.unwrap();
    assert!(slope(0) < 0.0 && slope(1) < 0.0, "statements fall");
    assert!(slope(2) > 0.0, "question rises");
}
