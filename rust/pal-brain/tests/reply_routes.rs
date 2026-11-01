//! `ReplyRoutes` with the real model and voice: plain, held, discarded, canned and interrupted turns.
//! Speaks through the speakers. `cargo test --release -p pal-brain --test reply_routes -- --ignored --nocapture`

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use pal_brain::language::Language;
use pal_brain::llm::{LlmConfig, LocalLlm};
use pal_brain::persona::{DEFAULT_NAME, Persona};
use pal_brain::routes::{ReplyRoutes, Spoken};
use pal_brain::tts::PocketSpeaker;

const LANG: Language = Language::German;
const LONG: Duration = Duration::from_secs(60);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Reports of `turn` until its `Done`; panics on `Failed` or a timeout.
fn until_done(rx: &Receiver<Spoken>, turn: &str) -> Vec<(Instant, String)> {
    let mut said = Vec::new();
    loop {
        match rx.recv_timeout(LONG).expect("turn finishes") {
            Spoken::Said { turn: of, text, at } if of == turn => said.push((at, text)),
            Spoken::Done { turn: of } if of == turn => return said,
            Spoken::Failed { turn: of, error } if of == turn => panic!("{error}"),
            other => assert!(!matches!(other, Spoken::Said { .. }), "stale {other:?}"),
        }
    }
}

fn quiet(rx: &Receiver<Spoken>, wait: Duration) -> Vec<Spoken> {
    let until = Instant::now() + wait;
    std::iter::from_fn(|| rx.recv_timeout(until.saturating_duration_since(Instant::now())).ok()).collect()
}

#[test]
#[ignore = "needs models/ and a speaker"]
fn reply_routes_handle_every_kind_of_turn() {
    let model = LocalLlm::new(&root().join("models/Qwen3.5-2B-Q4_K_M.gguf"), LlmConfig::default()).unwrap();
    let mut speaker = PocketSpeaker::new("juergen", LANG).unwrap();
    let persona = Persona::kitt(LANG, DEFAULT_NAME);
    let (tx, rx) = channel();
    let (speaker_tx, speaker_rx) = channel();
    let routes =
        ReplyRoutes::start(model, persona.fixed_messages(), speaker_rx, LANG, move |s| drop(tx.send(s))).unwrap();
    speaker_tx.send(speaker.clone()).unwrap();
    let ask = |q: &str| persona.messages(&Persona::user_line(q, ""));

    routes.prefill(&persona.context()).unwrap();
    assert!(matches!(rx.recv_timeout(LONG).unwrap(), Spoken::Prefilled));

    let t0 = Instant::now();
    routes.reply("t1", &ask("Was hältst du von Regen?"), false).unwrap();
    let said = until_done(&rx, "t1");
    assert!(!said.is_empty());
    println!("t1 first sentence after {} ms: {:?}", (said[0].0 - t0).as_millis(), said[0].1);

    routes.reply("t2", &ask("Erzähl mir etwas über Autos."), true).unwrap();
    let early = quiet(&rx, Duration::from_millis(1500));
    assert!(early.iter().all(|s| !matches!(s, Spoken::Said { .. } | Spoken::Done { .. })), "held: {early:?}");
    let released = Instant::now();
    routes.release("t2").unwrap();
    let said = until_done(&rx, "t2");
    println!("t2 released, first sentence after {} ms", (said[0].0 - released).as_millis());

    routes.reply("t3", &ask("Kannst du mir einen Rat geben?"), true).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    routes.discard("t3").unwrap();
    routes.canned("t4", "Das war ein Witz. Ein guter sogar!", false).unwrap();
    let said = until_done(&rx, "t4");
    assert_eq!(said.len(), 2, "{said:?}");
    let late = quiet(&rx, Duration::from_millis(500));
    assert!(late.is_empty(), "after discard: {late:?}");

    routes.reply("t5", &ask("Was machst du am liebsten? Erzähl ausführlich."), false).unwrap();
    loop {
        if let Spoken::Said { turn, .. } = rx.recv_timeout(LONG).unwrap()
            && turn == "t5"
        {
            break;
        }
    }
    let cut = Instant::now();
    routes.interrupt("t5").unwrap();
    let late = quiet(&rx, Duration::from_millis(1500));
    assert!(late.iter().all(|s| !matches!(s, Spoken::Done { .. })), "after interrupt: {late:?}");
    let said_late = late.iter().filter(|s| matches!(s, Spoken::Said { .. })).count();
    println!("t5 interrupted; {said_late} sentence(s) slipped through, quiet after {} ms", cut.elapsed().as_millis());
    assert_eq!(said_late, 0);

    speaker.stop();
    drop(routes);
}
