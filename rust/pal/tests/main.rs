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

/// llama.cpp's backend exists once per process: tests that load the model take turns.
static LLAMA: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn llama_turn() -> std::sync::MutexGuard<'static, ()> {
    LLAMA.lock().unwrap_or_else(|e| e.into_inner())
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
    pal::downloader::ensure(&models(), &pal::downloader::all_files(), &Default::default(), &Default::default())
        .unwrap();
}

/// Against a running Ollama with the model `PAL_OLLAMA_MODEL` (default `pal-test-qwen`, a thinking model):
/// `echo "FROM $PWD/models/Qwen3.5-4B-Q4_K_M.gguf" > /tmp/M && ollama create pal-test-qwen -f /tmp/M`.
mod ollama {
    use std::time::{Duration, Instant};

    use mq_bridge::models::Endpoint;
    use mq_bridge::{CanonicalMessage, Publisher, StreamCaller};
    use pal::think::local_llm::{Message, Role};
    use pal::think::remote_llm::{Provider, RemoteConfig};
    use pal::transport::{CORRELATION_ID, End, Token};
    use tokio::runtime::Runtime;

    /// Far below what a thinking model takes (minutes), far above a warm reply.
    const FIRST_WORD: Duration = Duration::from_secs(15);

    struct Chat {
        runtime: Runtime,
        config: RemoteConfig,
        call: StreamCaller,
        cancel: Publisher,
    }

    struct Reply {
        text: String,
        last: Token,
        first_word: Option<Duration>,
    }

    impl Chat {
        fn new(name: &'static str, model: String) -> Self {
            let runtime = Runtime::new().unwrap();
            let config = RemoteConfig {
                provider: Provider::Ollama,
                url: String::new(),
                model,
                api_key: String::new(),
                effort: "none".into(),
            };
            let tokens = Endpoint::new_memory(&format!("test.{name}.tokens"), 4096);
            let cancel_from = Endpoint::new_memory(&format!("test.{name}.cancel"), 16);
            let endpoint = config.endpoint(&tokens, &cancel_from).unwrap();
            let (call, cancel) = runtime.block_on(async {
                (StreamCaller::new(name, endpoint).await.unwrap(), Publisher::new(cancel_from).await.unwrap())
            });
            Self { runtime, config, call, cancel }
        }

        fn ask(&mut self, turn: &str, cancel_at_first_word: bool) -> Reply {
            let messages = [
                Message::new(Role::System, "Du bist KITT. Antworte kurz auf Deutsch."),
                Message::new(Role::User, "Wie hoch ist die Zugspitze?"),
            ];
            let request = CanonicalMessage::from(self.config.payload(&messages));
            let started = Instant::now();
            let (mut text, mut first_word) = (String::new(), None);
            let mut reply = self.runtime.block_on(self.call.start(turn, request)).unwrap();
            let last = loop {
                let message = self.runtime.block_on(reply.next()).expect("a last token").into_message();
                let token = Token::from_message(&message);
                if token.end.is_some() {
                    break token;
                }
                text.push_str(&token.text);
                if first_word.is_none() {
                    first_word = Some(started.elapsed());
                    if cancel_at_first_word {
                        let stop = CanonicalMessage::from("").with_metadata_kv(CORRELATION_ID, turn);
                        self.runtime.block_on(self.cancel.send(stop)).unwrap();
                    }
                }
            };
            Reply { text, last, first_word }
        }
    }

    #[test]
    #[ignore = "needs a running Ollama and its test model"]
    fn replies_stream_without_thinking_stop_on_cancel_and_report_a_missing_model() {
        let model = std::env::var("PAL_OLLAMA_MODEL").unwrap_or_else(|_| "pal-test-qwen".into());
        let mut chat = Chat::new("ollama", model);

        let loaded = chat.ask("load", false);
        assert!(loaded.last.end == Some(End::Complete), "{}", loaded.last.text);

        let reply = chat.ask("reply", false);
        assert!(reply.last.end == Some(End::Complete), "{}", reply.last.text);
        assert!(reply.text.contains("Zugspitze") || reply.text.contains("Meter"), "{}", reply.text);
        assert!(!reply.text.contains("<think>"), "{}", reply.text);
        let first_word = reply.first_word.expect("a word");
        assert!(first_word < FIRST_WORD, "first word after {first_word:?}");
        println!("first word after {first_word:?}: {}", reply.text);

        let stopped = chat.ask("stopped", true);
        assert!(stopped.last.end == Some(End::Cancelled), "{}", stopped.last.text);

        let missing = Chat::new("ollama_missing", "pal-no-such-model".into()).ask("missing", false);
        assert!(missing.last.end == Some(End::Failed), "{}", missing.last.text);
        assert_eq!(missing.last.index, 0, "the local fallback needs an error before any text");
    }
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
    use pal::think::local_llm::{LlmConfig, LocalLlm};
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
        let segments = segments();
        assert_eq!(segments.len(), expected.len());
        for (seg, words) in segments.iter().zip(expected) {
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
        let _llama = llama_turn();
        let model = LocalLlm::new(&models().join("granite-3.3-2b-instruct-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
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
        let _llama = llama_turn();
        let stt = Stt::new();
        let t = Instant::now();
        let model = LocalLlm::new(&models().join("granite-3.3-2b-instruct-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
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
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    use mq_bridge::blocking_handler::HandlerContext;
    use mq_bridge::models::Endpoint;
    use mq_bridge::{Caller, CanonicalMessage, Handled, Route};

    use pal::speak::tts::{self, PocketSpeaker};
    use pal::think::brain::Model;
    use pal::think::language::Language;
    use pal::think::persona::{DEFAULT_NAME, Persona};
    use pal::transport::{self, Input, Parts, Spoken, Transport};

    use super::*;

    const LANG: Language = Language::German;
    const LONG: Duration = Duration::from_secs(60);

    /// Pal with a silent microphone and no STT; the reports that reach `[pal.inbox]` come out in place of the
    /// conversation, which is the test itself, with the `Transport`.
    fn deploy(
        rt: &tokio::runtime::Runtime,
        speaker: &PocketSpeaker,
        persona: &Persona,
    ) -> (Transport, Receiver<Spoken>) {
        let none = Route::new(Endpoint::new_memory("test.stt", 1), Endpoint::new_response())
            .with_handler(|_: CanonicalMessage| async { Ok(Handled::Ack) });
        let parts = Parts {
            mic: Endpoint::new_memory("test.mic", 1),
            stt: rt.block_on(Caller::deploy("test_stt", none)).unwrap(),
            model: Some(
                Model::start(&models().join("granite-3.3-2b-instruct-Q4_K_M.gguf"), persona.fixed_messages()).unwrap(),
            ),
            server: None,
            speaker: speaker.clone(),
            language: LANG,
        };
        let (transport_tx, transport) = channel();
        let (tx, rx) = channel();
        transport::deploy(rt.handle(), parts, move |t| {
            transport_tx.send(t).unwrap();
            Ok(move |msg: CanonicalMessage, _: &HandlerContext| {
                match Input::from_message(&msg) {
                    // Playback, not the reply routes: this test checks what is queued.
                    Some(Input::Spoken(Spoken::Audible { .. } | Spoken::Quiet)) => {}
                    Some(Input::Spoken(s)) => drop(tx.send(s)),
                    Some(Input::Event(e)) => panic!("no microphone, but {e:?}"),
                    Some(Input::Typed(line)) => panic!("nothing typed, but {line}"),
                    None => {}
                }
                Ok(Handled::Ack)
            })
        })
        .unwrap();
        (transport.recv().unwrap(), rx)
    }

    fn spoken(t: &Receiver<Spoken>, timeout: Duration) -> Option<Spoken> {
        t.recv_timeout(timeout).ok()
    }

    /// Reports of `turn` until its `Done`; panics on `Failed` or a timeout.
    fn until_done(t: &Receiver<Spoken>, turn: &str) -> Vec<(Instant, String)> {
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

    fn quiet(t: &Receiver<Spoken>, wait: Duration) -> Vec<Spoken> {
        let until = Instant::now() + wait;
        std::iter::from_fn(|| spoken(t, until.saturating_duration_since(Instant::now()))).collect()
    }

    #[test]
    #[ignore = "needs models/ and a speaker"]
    fn reply_routes_handle_every_kind_of_turn() {
        let _llama = llama_turn();
        tts::use_local_files(&models());
        let speaker = PocketSpeaker::new("juergen", LANG).unwrap();
        let persona = Persona::kitt(LANG, DEFAULT_NAME);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (t, inbox) = deploy(&rt, &speaker, &persona);
        let ask = |q: &str| persona.messages(&Persona::user_line(q, ""));

        t.prefill(&persona.context()).unwrap();
        assert!(matches!(spoken(&inbox, LONG), Some(Spoken::Prefilled)));

        let t0 = Instant::now();
        t.reply("t1", &ask("Was hältst du von Regen?"), false).unwrap();
        let said = until_done(&inbox, "t1");
        assert!(!said.is_empty());
        println!("t1 first sentence after {} ms: {:?}", (said[0].0 - t0).as_millis(), said[0].1);

        t.reply("t2", &ask("Erzähl mir etwas über Autos."), true).unwrap();
        let early = quiet(&inbox, Duration::from_millis(1500));
        assert!(early.iter().all(|s| !matches!(s, Spoken::Said { .. } | Spoken::Done { .. })), "held: {early:?}");
        let released = Instant::now();
        t.release("t2").unwrap();
        let said = until_done(&inbox, "t2");
        println!("t2 released, first sentence after {} ms", (said[0].0 - released).as_millis());

        t.reply("t3", &ask("Kannst du mir einen Rat geben?"), true).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        t.discard("t3").unwrap();
        t.canned("t4", "Das war ein Witz. Ein guter sogar!", false).unwrap();
        let said = until_done(&inbox, "t4");
        assert_eq!(said.len(), 2, "{said:?}");
        let late = quiet(&inbox, Duration::from_millis(500));
        assert!(late.is_empty(), "after discard: {late:?}");

        t.reply("t5", &ask("Was machst du am liebsten? Erzähl ausführlich."), false).unwrap();
        let deadline = Instant::now() + LONG;
        loop {
            let left = deadline.checked_duration_since(Instant::now()).expect("t5 says a sentence in time");
            match spoken(&inbox, left) {
                Some(Spoken::Said { turn, .. }) if turn == "t5" => break,
                Some(Spoken::Done { turn } | Spoken::Failed { turn, .. }) if turn == "t5" => panic!("t5 ended unsaid"),
                _ => {}
            }
        }
        let cut = Instant::now();
        t.interrupt("t5").unwrap();
        let late = quiet(&inbox, Duration::from_millis(1500));
        assert!(late.iter().all(|s| !matches!(s, Spoken::Done { .. })), "after interrupt: {late:?}");
        let said_late = late.iter().filter(|s| matches!(s, Spoken::Said { .. })).count();
        println!(
            "t5 interrupted; {said_late} sentence(s) slipped through, quiet after {} ms",
            cut.elapsed().as_millis()
        );
        assert_eq!(said_late, 0);

        speaker.stop();
        drop(t);
        transport::stop_inbox(rt.handle());
        rt.block_on(Route::stop("test_stt"));
    }
}
