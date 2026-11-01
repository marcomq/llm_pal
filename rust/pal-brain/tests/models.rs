//! Runs the real models over generated German speech (scripts/make-test-audio.sh).

use std::path::PathBuf;
use std::time::Instant;

use std::sync::atomic::{AtomicBool, Ordering};

use pal_brain::language::Language;
use pal_brain::llm::{Llm, LlmConfig, LocalLlm};
use pal_brain::persona::{DEFAULT_NAME, Persona};
use pal_brain::prosody_tags::{self, ProsodyTags};
use pal_brain::stt::Recognizer;
use pal_core::{Event, ProcessorConfig, Segment, SegmenterConfig, process_samples};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn segments() -> Vec<Segment> {
    let mut reader = hound::WavReader::open(root().join("testdata/de_48k.wav")).unwrap();
    let rate = reader.spec().sample_rate;
    let samples: Vec<f32> = reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    let cfg = ProcessorConfig {
        vad_model: root().join("models/silero_vad.onnx"),
        segmenter: SegmenterConfig::default(),
        device: None,
        drop_echo: false,
        turn_model: None,
    };
    process_samples(&cfg, &samples, rate)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e {
            Event::Segment(s) => Some(s),
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "needs models/ and testdata/"]
fn parakeet_understands_the_test_sentences() {
    let model = root().join("models/parakeet-tdt-0.6b-v3");
    let mut stt = Recognizer::new(&model).unwrap();
    let expected = [&["hallo", "teste"][..], &["zweite", "satz"], &["hörst", "mich"]];
    for (seg, words) in segments().iter().zip(expected) {
        let t = Instant::now();
        let text = stt.transcribe(&seg.pcm).unwrap();
        eprintln!("{:.2} s audio -> {} ms: {text}", seg.features.duration_s, t.elapsed().as_millis());
        let lower = text.to_lowercase();
        assert!(words.iter().all(|w| lower.contains(w)), "{text:?} lacks {words:?}");
    }
    assert_eq!(stt.transcribe(&vec![0.0; 16_000]).unwrap(), "", "silence gives no text");
}

/// Per line, what Pal said before the pool (joke, quip or the model) and what it says now. Each line on its own,
/// without history. `cargo test --release -p pal-brain --test models pool_replies -- --ignored --nocapture`
#[test]
#[ignore = "needs models/"]
fn pool_replies_against_the_model() {
    let model = LocalLlm::new(&root().join("models/Qwen3.5-2B-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
    let mut llm = model.session().unwrap();
    let mut persona = Persona::kitt(Language::German, DEFAULT_NAME);
    llm.set_fixed_messages(persona.fixed_messages());
    let never = AtomicBool::new(false);
    let mut pooled = 0;
    for line in include_str!("pool_lines_de.txt").lines().filter(|l| !l.trim().is_empty()) {
        let canned = persona.joke(line).map(|r| ("joke", r)).or_else(|| persona.quip(line).map(|r| ("quip", r)));
        let before =
            canned.unwrap_or_else(|| ("model", llm.reply(&persona.messages(line), &never, &mut |_| {}).unwrap()));
        eprintln!("\n{line}\n  before ({}): {}", before.0, before.1);
        match persona.pooled(line) {
            Some(reply) if before.0 == "model" => {
                pooled += 1;
                eprintln!("  now    (pool): {reply}");
            }
            _ => eprintln!("  now: unchanged"),
        }
    }
    eprintln!("\n{pooled} lines now get a pool reply");
}

/// STT and LLM in one process, through the whole comment chain, plus cancellation.
#[test]
#[ignore = "needs models/ and testdata/"]
fn llm_comments_on_transcripts_and_stops_when_cancelled() {
    let mut stt = Recognizer::new(&root().join("models/parakeet-tdt-0.6b-v3")).unwrap();
    let t = Instant::now();
    let model = LocalLlm::new(&root().join("models/Qwen3.5-2B-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
    let mut llm = model.session().unwrap();
    eprintln!("llm load {} ms", t.elapsed().as_millis());
    let mut persona = Persona::kitt(Language::German, DEFAULT_NAME);
    let mut tags = ProsodyTags::default();
    let never = AtomicBool::new(false);
    for (turn, seg) in segments().into_iter().enumerate() {
        let line = Persona::user_line(
            &stt.transcribe(&seg.pcm).unwrap(),
            &prosody_tags::format(&tags.tag(&seg.features), Language::German),
        );
        let t = Instant::now();
        let mut first = None;
        let reply = llm.reply(&persona.messages(&line), &never, &mut |_| {
            first.get_or_insert(t.elapsed().as_millis());
        });
        let reply = reply.unwrap();
        let (first, all, read) = (first.unwrap_or(0), t.elapsed().as_millis(), llm.last_prefill());
        eprintln!("{line}\n  -> {reply}  (first {first} ms, all {all} ms, read {read} prompt tokens)");
        assert!(!reply.is_empty() && !reply.contains("think") && reply.chars().count() < 400, "{reply:?}");
        assert!(turn == 0 || read < 80, "later turns reuse the cached prompt, but read {read} tokens");
        persona.remember(line, reply);
    }

    let cancel = AtomicBool::new(false);
    let mut pieces = 0;
    let reply = llm
        .reply(&persona.messages("Erzähl mir eine sehr lange Geschichte."), &cancel, &mut |_| {
            pieces += 1;
            if pieces == 3 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
    assert_eq!(pieces, 3, "stops right after cancel: {reply:?}");
}
