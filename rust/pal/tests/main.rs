//! Tests with the real models over generated German speech (scripts/make-test-audio.sh). All are ignored:
//! `cargo test --release -p pal --test main -- --ignored --nocapture <name>`.

use std::path::PathBuf;

use pal::hear::event::{Event, Segment};
use pal::hear::mic::{MicConfig, process_samples};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn models() -> PathBuf {
    root().join("models")
}

fn recording(extra_silence_s: usize) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(root().join("testdata/de_48k.wav")).unwrap();
    let rate = reader.spec().sample_rate;
    let mut samples: Vec<f32> = reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    samples.extend(std::iter::repeat_n(0.0, extra_silence_s * rate as usize));
    (samples, rate)
}

fn mic_config(smart_turn: bool) -> MicConfig {
    MicConfig {
        vad_model: models().join("silero_vad.onnx"),
        turn_model: smart_turn.then(|| models().join("smart-turn-v3.2-cpu.onnx")),
        device: None,
        drop_echo: false,
    }
}

fn segments_of(events: Vec<Event>) -> Vec<Segment> {
    events
        .into_iter()
        .filter_map(|e| match e {
            Event::Segment(s) => Some(s),
            _ => None,
        })
        .collect()
}

fn segments() -> Vec<Segment> {
    let (samples, rate) = recording(0);
    segments_of(process_samples(&mic_config(false), &samples, rate).unwrap())
}

/// Fetches every model in the catalog into `models/`; `build.sh setup` runs it.
#[test]
#[ignore = "downloads several GB"]
fn download_models() {
    pal::downloader::ensure(&models(), &pal::downloader::all_files()).unwrap();
}

mod speech {
    use super::*;

    #[test]
    #[ignore = "needs models/ and testdata/"]
    fn three_sentences_give_three_segments_and_the_question_rises() {
        let (samples, rate) = recording(0);
        let events = process_samples(&mic_config(false), &samples, rate).unwrap();
        let starts = events.iter().filter(|e| matches!(e, Event::SpeechStart { .. })).count();
        assert!(!events.iter().any(|e| matches!(e, Event::BargeIn { .. })));
        let segs = segments_of(events);
        assert_eq!(segs.len(), 3);
        assert_eq!(starts, 3);
        for s in &segs {
            assert_eq!(s.pcm.len() as u64, (s.end_ms - s.start_ms) * 16, "pcm length matches timestamps");
            assert!(s.features.duration_s > 1.0 && s.features.f0_median_hz.is_some());
        }
        assert!(segs.windows(2).all(|w| w[1].start_ms > w[0].end_ms + 500), "pauses separate segments");
        let slope = |i: usize| segs[i].features.f0_slope_st_per_s.unwrap();
        assert!(slope(0) < 0.0 && slope(1) < 0.0, "statements fall");
        assert!(slope(2) > 0.0, "question rises");
    }

    #[test]
    #[ignore = "needs models/ and testdata/"]
    fn smart_turn_ends_finished_sentences_at_the_pause() {
        // The recording stops right after the last sentence; a second of silence lets it pause there too.
        let (samples, rate) = recording(1);
        let segs = segments_of(process_samples(&mic_config(true), &samples, rate).unwrap());
        assert_eq!(segs.len(), 3);
        for s in &segs {
            eprintln!(
                "{}..{} ms, turn complete {}, silence {} ms",
                s.start_ms, s.end_ms, s.turn_complete, s.silence_ms
            );
            assert!(s.turn_complete, "a finished sentence ends at the pause");
            assert!(s.silence_ms < 200, "well before end_silence_ms");
        }
    }
}

mod models {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use pal::hear::stt;
    use pal::think::language::Language;
    use pal::think::llm::{LlmConfig, LocalLlm};
    use pal::think::persona::{DEFAULT_NAME, Persona};
    use pal::think::prosody_tags::{self, ProsodyTags};

    use super::*;

    /// Transcribes on a runtime of its own, like the app does.
    struct Stt {
        rt: tokio::runtime::Runtime,
        caller: mq_bridge::Caller,
    }

    impl Stt {
        fn new() -> Self {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let caller = pal::transport::start_stt(rt.handle(), &models().join("parakeet-tdt-0.6b-v3")).unwrap();
            Self { rt, caller }
        }

        fn transcribe(&self, pcm: &[f32]) -> String {
            self.rt.block_on(stt::transcribe(&self.caller, pcm)).unwrap()
        }
    }

    #[test]
    #[ignore = "needs models/ and testdata/"]
    fn parakeet_understands_the_test_sentences() {
        let stt = Stt::new();
        let expected = [&["hallo", "teste"][..], &["zweite", "satz"], &["hörst", "mich"]];
        for (seg, words) in segments().iter().zip(expected) {
            let t = Instant::now();
            let text = stt.transcribe(&seg.pcm);
            eprintln!("{:.2} s audio -> {} ms: {text}", seg.features.duration_s, t.elapsed().as_millis());
            let lower = text.to_lowercase();
            assert!(words.iter().all(|w| lower.contains(w)), "{text:?} lacks {words:?}");
        }
        assert_eq!(stt.transcribe(&vec![0.0; 16_000]), "", "silence gives no text");
    }

    /// Per line, what Pal said before the pool (joke, quip or the model) and what it says now. Each line on its
    /// own, without history.
    #[test]
    #[ignore = "needs models/"]
    fn pool_replies_against_the_model() {
        let model = LocalLlm::new(&models().join("Qwen3.5-2B-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
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
        let stt = Stt::new();
        let t = Instant::now();
        let model = LocalLlm::new(&models().join("Qwen3.5-2B-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
        let mut llm = model.session().unwrap();
        eprintln!("llm load {} ms", t.elapsed().as_millis());
        let mut persona = Persona::kitt(Language::German, DEFAULT_NAME);
        let mut tags = ProsodyTags::default();
        let never = AtomicBool::new(false);
        for (turn, seg) in segments().into_iter().enumerate() {
            let line = Persona::user_line(
                &stt.transcribe(&seg.pcm),
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
}

/// The reply routes with the real model and voice: plain, held, discarded, canned and interrupted turns. Speaks
/// through the speakers.
mod reply {
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    use pal::speak::tts::{self, PocketSpeaker};
    use pal::think::brain::Brain;
    use pal::think::language::Language;
    use pal::think::persona::{DEFAULT_NAME, Persona};
    use pal::transport::{Input, Spoken, Transport};

    use super::*;

    const LANG: Language = Language::German;
    const LONG: Duration = Duration::from_secs(60);

    fn spoken(t: &mut Transport, timeout: Duration) -> Option<Spoken> {
        match t.next(timeout)? {
            Input::Spoken(s) => Some(s),
            Input::Event(e) => panic!("no microphone, but {e:?}"),
        }
    }

    /// Reports of `turn` until its `Done`; panics on `Failed` or a timeout.
    fn until_done(t: &mut Transport, turn: &str) -> Vec<(Instant, String)> {
        let mut said = Vec::new();
        loop {
            match spoken(t, LONG).expect("turn finishes") {
                Spoken::Said { turn: of, text, at } if of == turn => said.push((at, text)),
                Spoken::Done { turn: of } if of == turn => return said,
                Spoken::Failed { turn: of, error } if of == turn => panic!("{error}"),
                other => assert!(!matches!(other, Spoken::Said { .. }), "stale {other:?}"),
            }
        }
    }

    fn quiet(t: &mut Transport, wait: Duration) -> Vec<Spoken> {
        let until = Instant::now() + wait;
        std::iter::from_fn(|| spoken(t, until.saturating_duration_since(Instant::now()))).collect()
    }

    #[test]
    #[ignore = "needs models/ and a speaker"]
    fn reply_routes_handle_every_kind_of_turn() {
        tts::use_local_files(&models());
        let mut speaker = PocketSpeaker::new("juergen", LANG).unwrap();
        let persona = Persona::kitt(LANG, DEFAULT_NAME);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (speaker_tx, speaker_rx) = channel();
        let brain = Brain::Local(models().join("Qwen3.5-2B-Q4_K_M.gguf"));
        let mut t = Transport::start(rt.handle().clone(), brain, persona.fixed_messages(), speaker_rx, LANG).unwrap();
        speaker_tx.send(speaker.clone()).unwrap();
        let ask = |q: &str| persona.messages(&Persona::user_line(q, ""));

        t.prefill(&persona.context()).unwrap();
        assert!(matches!(spoken(&mut t, LONG), Some(Spoken::Prefilled)));

        let t0 = Instant::now();
        t.reply("t1", &ask("Was hältst du von Regen?"), false).unwrap();
        let said = until_done(&mut t, "t1");
        assert!(!said.is_empty());
        println!("t1 first sentence after {} ms: {:?}", (said[0].0 - t0).as_millis(), said[0].1);

        t.reply("t2", &ask("Erzähl mir etwas über Autos."), true).unwrap();
        let early = quiet(&mut t, Duration::from_millis(1500));
        assert!(early.iter().all(|s| !matches!(s, Spoken::Said { .. } | Spoken::Done { .. })), "held: {early:?}");
        let released = Instant::now();
        t.release("t2").unwrap();
        let said = until_done(&mut t, "t2");
        println!("t2 released, first sentence after {} ms", (said[0].0 - released).as_millis());

        t.reply("t3", &ask("Kannst du mir einen Rat geben?"), true).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        t.discard("t3").unwrap();
        t.canned("t4", "Das war ein Witz. Ein guter sogar!", false).unwrap();
        let said = until_done(&mut t, "t4");
        assert_eq!(said.len(), 2, "{said:?}");
        let late = quiet(&mut t, Duration::from_millis(500));
        assert!(late.is_empty(), "after discard: {late:?}");

        t.reply("t5", &ask("Was machst du am liebsten? Erzähl ausführlich."), false).unwrap();
        loop {
            if let Some(Spoken::Said { turn, .. }) = spoken(&mut t, LONG)
                && turn == "t5"
            {
                break;
            }
        }
        let cut = Instant::now();
        t.interrupt("t5").unwrap();
        let late = quiet(&mut t, Duration::from_millis(1500));
        assert!(late.iter().all(|s| !matches!(s, Spoken::Done { .. })), "after interrupt: {late:?}");
        let said_late = late.iter().filter(|s| matches!(s, Spoken::Said { .. })).count();
        println!(
            "t5 interrupted; {said_late} sentence(s) slipped through, quiet after {} ms",
            cut.elapsed().as_millis()
        );
        assert_eq!(said_late, 0);

        speaker.stop();
        drop(t);
    }
}
