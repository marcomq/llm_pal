//! Whisper (shortened window), Whisper (full 30 s window) and Parakeet TDT on recorded segments.
//! Record with `PAL_RECORD_DIR=testdata/stt`, then write the spoken words into `<name>.txt` next to each `<name>.wav`
//! (a corrected copy of `<name>.stt.txt`, empty for noise). Fetch Parakeet with `PARAKEET=1 sh scripts/fetch-models.sh`. Run:
//! `cargo build -p pal-stt && cargo test -p pal-brain --test stt_compare -- --ignored --nocapture`

use std::path::{Path, PathBuf};
use std::time::Instant;

use pal_brain::stt::Stt;
use parakeet_rs::{ExecutionConfig, ParakeetTDT, Transcriber};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Clip {
    name: String,
    pcm: Vec<f32>,
    /// None: no `<name>.txt` written yet, the output is shown but not scored.
    reference: Option<String>,
}

fn clips(dir: &Path) -> Vec<Clip> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    paths.sort();
    let mut clips: Vec<_> = paths
        .iter()
        .map(|p| {
            let mut reader = hound::WavReader::open(p).unwrap();
            assert_eq!(reader.spec().sample_rate, 16_000, "{}", p.display());
            Clip {
                name: p.file_stem().unwrap().to_string_lossy().into(),
                pcm: reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect(),
                reference: std::fs::read_to_string(p.with_extension("txt")).ok(),
            }
        })
        .collect();
    clips.push(Clip { name: "noise".into(), pcm: noise(32_000, 0.02), reference: Some(String::new()) });
    clips
}

/// Quiet white noise: Whisper tends to invent sentences for it.
fn noise(n: usize, amplitude: f32) -> Vec<f32> {
    let mut x = 0x2545_f491_u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * amplitude
        })
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

/// Word-level edit distance: substitutions, insertions and deletions.
fn word_errors(reference: &[String], hypothesis: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=hypothesis.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, h) in hypothesis.iter().enumerate() {
            let next = (diagonal + usize::from(r != h)).min(row[j] + 1).min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[hypothesis.len()]
}

type Transcribe = Box<dyn FnMut(&[f32]) -> String>;

#[derive(Default)]
struct Score {
    errors: usize,
    reference_words: usize,
    ms: u128,
    /// Words produced where the reference is empty (noise, breathing).
    invented: usize,
}

fn whisper(full_window: bool) -> Stt {
    // SAFETY: the only test in this binary, so no other thread reads the environment meanwhile.
    unsafe { std::env::set_var("PAL_STT_FULL_WINDOW", if full_window { "1" } else { "0" }) };
    let mut stt = Stt::new(&pal_brain::stt::default_library(), &root().join("models/ggml-small-q5_1.bin")).unwrap();
    stt.set_language("de").unwrap();
    stt.transcribe(&[0.0; 16_000]).unwrap();
    stt
}

#[test]
#[ignore = "needs models/, the Parakeet model and recordings in testdata/stt"]
fn compare_whisper_and_parakeet() {
    let dir = std::env::var_os("PAL_STT_DATA").map_or_else(|| root().join("testdata/stt"), PathBuf::from);
    let clips = clips(&dir);

    let env = parakeet_rs::ort::init().build().unwrap();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(4));
    let config = ExecutionConfig::new().with_environment(env).with_intra_threads(threads);
    let mut parakeet = ParakeetTDT::from_pretrained(root().join("models/parakeet-tdt-0.6b-v3"), Some(config)).unwrap();
    parakeet.transcribe_samples(vec![0.0; 16_000], 16_000, 1, None).unwrap();

    let mut short = whisper(false);
    let mut full = whisper(true);
    let mut systems: [(&str, Transcribe); 3] = [
        ("whisper", Box::new(move |pcm| short.transcribe(pcm).unwrap())),
        ("whisper-full", Box::new(move |pcm| full.transcribe(pcm).unwrap())),
        ("parakeet", Box::new(move |pcm| parakeet.transcribe_samples(pcm.to_vec(), 16_000, 1, None).unwrap().text)),
    ];
    let mut scores: [Score; 3] = Default::default();
    let mut audio_s = 0.0;

    for clip in &clips {
        audio_s += clip.pcm.len() as f32 / 16_000.0;
        eprintln!("\n{} ({:.1} s)", clip.name, clip.pcm.len() as f32 / 16_000.0);
        if let Some(r) = &clip.reference {
            eprintln!("  {:<13}{r}", "reference");
        }
        for ((name, transcribe), score) in systems.iter_mut().zip(&mut scores) {
            let t = Instant::now();
            let text = transcribe(&clip.pcm);
            let ms = t.elapsed().as_millis();
            score.ms += ms;
            let mark = clip.reference.as_ref().map_or(String::new(), |r| {
                let (r, h) = (words(r), words(&text));
                let errors = word_errors(&r, &h);
                if r.is_empty() {
                    score.invented += h.len();
                } else {
                    score.errors += errors;
                    score.reference_words += r.len();
                }
                format!("  [{errors} errors]")
            });
            eprintln!("  {name:<13}{text}  ({ms} ms){mark}");
        }
    }

    eprintln!("\n{:.0} s audio in {} clips", audio_s, clips.len());
    for ((name, _), s) in systems.iter().zip(&scores) {
        let wer = 100.0 * s.errors as f32 / s.reference_words.max(1) as f32;
        eprintln!(
            "{name:<13}WER {wer:5.1} %   invented words {:3}   {:6} ms total   {:.2} x real time",
            s.invented,
            s.ms,
            s.ms as f32 / 1000.0 / audio_s
        );
    }
}

#[test]
fn word_errors_count_edits() {
    let w = |s| words(s);
    assert_eq!(word_errors(&w("das ist gut"), &w("Das ist gut.")), 0);
    assert_eq!(word_errors(&w("das ist gut"), &w("das gut")), 1);
    assert_eq!(word_errors(&w("das ist gut"), &w("was ist gut heute")), 2);
    assert_eq!(word_errors(&w(""), &w("Andrea Andrea")), 2);
}
