//! Runs fixed prompts, or the items of a test file, on local GGUF models through llama.cpp and logs the raw
//! replies and timings as JSONL. See `README.md`.

// For the subtest record's `json!`.
#![recursion_limit = "256"]

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

const MAX_OUTPUT_TOKENS: usize = 1_000;
const SPEED_OUTPUT_TOKENS: usize = 256;
const MAX_RUNTIME: Duration = Duration::from_secs(8 * 60 * 60);
const N_CTX: u32 = 4096;
const BATCH: usize = 512;
/// llama.cpp threads, at most as many as Pal uses on a phone.
const MAX_THREADS: usize = 4;
/// Sampling as in Pal's defaults, so replies compare with what Pal would say.
const TEMPERATURE: f32 = 0.5;
const TOP_K: i32 = 64;
const TOP_P: f32 = 0.95;
const REPEAT_PENALTY: f32 = 1.1;
const PENALTY_LAST_N: i32 = 256;
const SEED: u32 = 0x9a1;
/// A server that sends nothing this long ends the download.
const STALL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
enum BenchMode {
    Full,
    Speed,
    /// One generation per item of `PAL_BENCH_TESTS`, greedy, for automatic scoring.
    Scored,
}

impl BenchMode {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        match std::env::var("PAL_BENCH_MODE").as_deref().unwrap_or("full") {
            "full" => Ok(Self::Full),
            "speed" => Ok(Self::Speed),
            "scored" => Ok(Self::Scored),
            other => Err(format!("unsupported PAL_BENCH_MODE: {other} (use full, speed or scored)").into()),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Speed => "speed",
            Self::Scored => "scored",
        }
    }
}

struct Deadline {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    watchdog: Option<JoinHandle<()>>,
}

impl Deadline {
    fn new(started: Instant) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let watchdog_stop = Arc::clone(&stop);
        let watchdog_done = Arc::clone(&done);
        let watchdog = std::thread::spawn(move || {
            while !watchdog_done.load(Ordering::Relaxed) {
                let remaining = MAX_RUNTIME.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    watchdog_stop.store(true, Ordering::Relaxed);
                    break;
                }
                std::thread::sleep(remaining.min(Duration::from_secs(1)));
            }
        });
        Self { stop, done, watchdog: Some(watchdog) }
    }

    fn stop_flag(&self) -> &AtomicBool {
        &self.stop
    }

    fn expired(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    models: Vec<ModelSpec>,
}

#[derive(Deserialize)]
struct ModelSpec {
    name: String,
    file: String,
    parameters_b: f32,
    license: String,
    source: String,
    gguf_source: String,
    url: String,
    sha256: String,
    optimization: String,
    remove_after: bool,
    /// Put after the chat template's assistant start, e.g. an empty `<think></think>` block as the templates of
    /// hybrid reasoning models do with thinking off; llama.cpp's built-in templates don't.
    #[serde(default)]
    assistant_prefix: Option<String>,
    /// `"gemma4"` writes Gemma 4's prompt as Pal does, since llama.cpp's built-in templates don't know it.
    #[serde(default)]
    prompt_format: Option<String>,
}

impl ModelSpec {
    fn is_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && !self.license.trim().is_empty()
            && !self.optimization.trim().is_empty()
            && [&self.source, &self.gguf_source, &self.url].iter().all(|u| u.starts_with("https://"))
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && self.file.ends_with(".gguf")
            && !self.file.contains(['/', '\\'])
            && self.parameters_b > 0.0
            && self.parameters_b <= 8.0
    }
}

#[derive(Clone, Copy)]
struct TestCase {
    id: &'static str,
    language: &'static str,
    subtest: &'static str,
    system: &'static str,
    prompt: &'static str,
    max_tokens: usize,
}

const TESTS: &[TestCase] = &[
    TestCase {
        id: "de-knowledge",
        language: "de",
        subtest: "knowledge",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Beantworte die acht nummerierten Fragen auf Deutsch. Gib pro Frage eine kurze Antwort, ohne Erklärungen. Wenn du es nicht weißt, sage das.",
        prompt: "1. Was ist die Hauptstadt Australiens?\n\
                 2. Wie lautet die chemische Formel von Wasser?\n\
                 3. Welcher Planet ist der Sonne am nächsten?\n\
                 4. Wie heißt der größte Ozean der Erde?\n\
                 5. Auf welchem Kontinent liegt die Sahara?\n\
                 6. Wie viele Grad beträgt die Summe der Innenwinkel eines Dreiecks?\n\
                 7. Welches Gas nehmen Pflanzen bei der Photosynthese auf?\n\
                 8. Wie viele Meter hat ein Kilometer?",
    },
    TestCase {
        id: "de-language",
        language: "de",
        subtest: "language",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Bearbeite alle drei Aufgaben auf Deutsch. Halte die Antworten kurz und natürlich. Erkläre deine Änderungen nicht.",
        prompt: "1. Korrigiere nur die Grammatik: Gestern ich habe mit meine Freunde ein Film gesehen.\n\
                 2. Korrigiere den Satz: Obwohl es stark regnete, aber wir gingen spazieren.\n\
                 3. Antworte in ein oder zwei natürlichen Sätzen: Du hast den Zug knapp verpasst. Was machst du jetzt?",
    },
    TestCase {
        id: "en-knowledge",
        language: "en",
        subtest: "knowledge",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Answer the eight numbered questions in English. Give one short answer per question, without explanations. If you do not know, say so.",
        prompt: "1. What is the capital of Australia?\n\
                 2. What is the chemical formula for water?\n\
                 3. Which planet is closest to the Sun?\n\
                 4. What is Earth's largest ocean?\n\
                 5. On which continent is the Sahara Desert?\n\
                 6. What is the sum of the interior angles of a triangle, in degrees?\n\
                 7. Which gas do plants absorb during photosynthesis?\n\
                 8. How many metres are in a kilometre?",
    },
    TestCase {
        id: "en-language",
        language: "en",
        subtest: "language",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Complete all three tasks in English. Keep the answers short and natural. Do not explain your edits.",
        prompt: "1. Correct only the grammar: Yesterday I has went to the store, but it were closed.\n\
                 2. Correct the sentence: Although it was raining heavily, but we went for a walk.\n\
                 3. Reply in one or two natural sentences: You have just missed your train. What do you do now?",
    },
    TestCase {
        id: "fr-knowledge",
        language: "fr",
        subtest: "knowledge",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Réponds aux huit questions numérotées en français. Donne une réponse courte par question, sans explication. Si tu ne sais pas, dis-le.",
        prompt: "1. Quelle est la capitale de l'Australie ?\n\
                 2. Quelle est la formule chimique de l'eau ?\n\
                 3. Quelle planète est la plus proche du Soleil ?\n\
                 4. Quel est le plus grand océan de la Terre ?\n\
                 5. Sur quel continent se trouve le Sahara ?\n\
                 6. Quelle est la somme des angles intérieurs d'un triangle, en degrés ?\n\
                 7. Quel gaz les plantes absorbent-elles pendant la photosynthèse ?\n\
                 8. Combien de mètres y a-t-il dans un kilomètre ?",
    },
    TestCase {
        id: "fr-language",
        language: "fr",
        subtest: "language",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Effectue les trois tâches en français. Réponds brièvement et naturellement. N'explique pas tes corrections.",
        prompt: "1. Corrige uniquement la grammaire : Hier, je suis allé au marché, mais les magasins était fermé.\n\
                 2. Corrige la phrase : Bien qu'il ait beaucoup plu, mais nous sommes allés nous promener.\n\
                 3. Réponds en une ou deux phrases naturelles : Tu viens de rater ton train. Que fais-tu maintenant ?",
    },
    TestCase {
        id: "es-knowledge",
        language: "es",
        subtest: "knowledge",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Responde en español a las ocho preguntas numeradas. Da una respuesta breve por pregunta, sin explicaciones. Si no lo sabes, dilo.",
        prompt: "1. ¿Cuál es la capital de Australia?\n\
                 2. ¿Cuál es la fórmula química del agua?\n\
                 3. ¿Qué planeta está más cerca del Sol?\n\
                 4. ¿Cuál es el océano más grande de la Tierra?\n\
                 5. ¿En qué continente está el Sahara?\n\
                 6. ¿Cuántos grados suman los ángulos interiores de un triángulo?\n\
                 7. ¿Qué gas absorben las plantas durante la fotosíntesis?\n\
                 8. ¿Cuántos metros tiene un kilómetro?",
    },
    TestCase {
        id: "es-language",
        language: "es",
        subtest: "language",
        max_tokens: MAX_OUTPUT_TOKENS,
        system: "Completa las tres tareas en español. Responde de forma breve y natural. No expliques las correcciones.",
        prompt: "1. Corrige solo la gramática: Ayer fui al mercado, pero las tiendas estaba cerrada.\n\
                 2. Corrige la frase: Aunque llovía mucho, pero salimos a pasear.\n\
                 3. Responde en una o dos frases naturales: Acabas de perder el tren. ¿Qué haces ahora?",
    },
];

const SPEED_RUNS: u8 = 3;
const SPEED_TEST: TestCase = TestCase {
    id: "en-speed",
    language: "en",
    subtest: "speed",
    max_tokens: SPEED_OUTPUT_TOKENS,
    system: "Write continuous, natural English prose. Do not use a heading or stop after one sentence.",
    prompt: "Describe how a small community garden changes through the seasons in about 300 words.",
};

/// A test file for `PAL_BENCH_MODE=scored`. The expected answers in it are for `score.py`; the runner ignores them.
#[derive(Deserialize)]
struct TestFile {
    name: String,
    suites: Vec<Suite>,
}

#[derive(Deserialize)]
struct Suite {
    language: String,
    category: String,
    system: String,
    max_output_tokens: usize,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    prompt: String,
}

/// Which test file a scored run used, by name and hash.
struct TestSource {
    name: String,
    sha256: String,
}

/// What every record of a run shares.
struct Run<'a> {
    id: String,
    device: String,
    threads: usize,
    llm_threads: i32,
    backend: &'a LlamaBackend,
    mode: BenchMode,
    tests: Vec<(TestCase, u8)>,
    source: Option<TestSource>,
    deadline: &'a Deadline,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("llm_bench: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: llm_bench <model-dir> <models.json> <results.jsonl>".into());
    }

    let model_dir = PathBuf::from(&args[0]);
    fs::create_dir_all(&model_dir)?;
    let model_dir = model_dir.canonicalize()?;
    let output_arg = PathBuf::from(&args[2]);
    let output_name = output_arg.file_name().ok_or("results path must name a file")?;
    let output_parent = output_arg.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let output_path = output_parent.canonicalize()?.join(output_name);
    if output_path.starts_with(&model_dir) {
        return Err("results file must be outside the model directory".into());
    }
    let mut manifest: Manifest = serde_json::from_reader(BufReader::new(File::open(&args[1])?))?;
    if manifest.models.is_empty() {
        return Err("the model manifest must contain at least one model".into());
    }
    if let Some(invalid) = manifest.models.iter().find(|model| !model.is_valid()) {
        return Err(format!("invalid model metadata: {}", invalid.file).into());
    }
    manifest.models.iter_mut().for_each(|model| model.sha256.make_ascii_lowercase());

    let mode = BenchMode::from_env()?;
    let (tests, source): (Vec<(TestCase, u8)>, _) = match mode {
        BenchMode::Full => {
            let languages = selected_languages()?;
            (TESTS.iter().filter(|test| languages.contains(&test.language)).map(|&t| (t, 0)).collect(), None)
        }
        BenchMode::Speed => ((1..=SPEED_RUNS).map(|repetition| (SPEED_TEST, repetition)).collect(), None),
        BenchMode::Scored => {
            let (tests, source) = load_tests(&selected_languages()?)?;
            (tests.into_iter().map(|t| (t, 0)).collect(), Some(source))
        }
    };
    if tests.is_empty() {
        return Err("no test selected for these languages".into());
    }
    let output = OpenOptions::new().write(true).create_new(true).open(&output_path)?;
    let mut output = BufWriter::new(output);
    let deadline = Deadline::new(Instant::now());
    let mut backend = LlamaBackend::init()?;
    backend.void_logs();
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let run = Run {
        id: chrono::Utc::now().to_rfc3339(),
        device: std::env::var("PAL_BENCH_DEVICE").unwrap_or_else(|_| "unspecified".into()),
        threads,
        llm_threads: std::env::var("PAL_BENCH_THREADS")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(threads.min(MAX_THREADS)) as i32,
        backend: &backend,
        mode,
        tests,
        source,
        deadline: &deadline,
    };
    let mut failures = Vec::new();

    for spec in &manifest.models {
        if deadline.expired() {
            break;
        }
        let model_path = model_dir.join(&spec.file);
        if model_path.exists() && !model_path.is_file() {
            return Err(format!("model path is not a file: {}", model_path.display()).into());
        }
        // A failed fetch counts as this run's: the file was missing, and a partial download may be left.
        let fetched = fetch(&model_path, &spec.url, &spec.sha256, deadline.stop_flag());
        let downloaded = !matches!(fetched, Ok(false));
        let remove = downloaded || spec.remove_after;
        let result = fetched.and_then(|_| run_model(&run, spec, &model_path, downloaded, &mut output));

        let cleanup = if remove { remove_model(&model_path) } else { Ok(()) };
        let error_logged = match &result {
            Ok(()) => Ok(()),
            Err(error) => write_record(
                &mut output,
                &json!({
                    "record_type": "model_error",
                    "run_id": run.id,
                    "model": spec.name,
                    "model_file": spec.file,
                    "model_source": spec.source,
                    "gguf_source": spec.gguf_source,
                    "error": error.to_string(),
                    "status": if deadline.expired() { "deadline" } else { "failed" },
                }),
            ),
        };
        let cleanup_logged = write_record(
            &mut output,
            &json!({
                "record_type": "model_cleanup",
                "run_id": run.id,
                "model": spec.name,
                "model_file": spec.file,
                "downloaded_for_run": downloaded,
                "model_removed": remove && cleanup.is_ok(),
                "status": match (&cleanup, remove) {
                    (Err(_), _) => "failed",
                    (Ok(()), true) => "complete",
                    (Ok(()), false) => "kept_existing",
                },
                "error": cleanup.as_ref().err().map(ToString::to_string),
            }),
        );
        cleanup?;
        error_logged?;
        cleanup_logged?;
        match result {
            Err(_) if deadline.expired() => break,
            Err(error) => {
                eprintln!("{}: failed; continuing: {error}", spec.name);
                failures.push(format!("{}: {error}", spec.name));
            }
            Ok(()) => println!("{}: tested and {}", spec.name, if remove { "removed" } else { "kept" }),
        }
    }
    if !failures.is_empty() {
        return Err(format!("{} model(s) failed: {}", failures.len(), failures.join("; ")).into());
    }
    Ok(())
}

fn run_model(
    run: &Run,
    spec: &ModelSpec,
    model_path: &Path,
    downloaded_for_run: bool,
    output: &mut BufWriter<File>,
) -> Result<(), Box<dyn Error>> {
    let model_size = fs::metadata(model_path)?.len();
    let model_hash = sha256(model_path, run.deadline.stop_flag())?;
    if model_hash != spec.sha256 {
        return Err(format!("sha256 {model_hash}, expected {}", spec.sha256).into());
    }
    if run.deadline.expired() {
        return Err("deadline reached before loading the model".into());
    }
    let loading = Instant::now();
    let model = LlamaModel::load_from_file(run.backend, model_path, &LlamaModelParams::default())?;
    let load_ms = loading.elapsed().as_millis();

    if run.deadline.expired() {
        write_record(
            output,
            &json!({
                "record_type": "model_interrupted",
                "run_id": run.id,
                "model": spec.name,
                "model_file": spec.file,
                "status": "deadline_during_model_load",
            }),
        )?;
        return Ok(());
    }

    for &(test, repetition) in &run.tests {
        if run.deadline.expired() {
            return Ok(());
        }

        let stats = generate(run, &model, &test, spec, u32::from(repetition))?;
        let deadline_reached = stats.deadline_reached;
        let tokens_per_second = stats.output_tokens as f64 / stats.elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

        let row = json!({
            "record_type": "subtest",
            "schema_version": 3,
            "engine": match spec.prompt_format { Some(_) => "llama.cpp, Pal's prompt format", None => "llama.cpp, the GGUF's chat template" },
            "run_id": run.id,
            "recorded_at": chrono::Utc::now().to_rfc3339(),
            "device": run.device,
            "mode": run.mode.name(),
            "sampling": match run.mode { BenchMode::Scored => "greedy", _ => "pal" },
            "tests_name": run.source.as_ref().map(|s| &s.name),
            "tests_sha256": run.source.as_ref().map(|s| &s.sha256),
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "available_threads": run.threads,
            "llm_threads": run.llm_threads,
            "model": spec.name,
            "model_file": spec.file,
            "downloaded_for_run": downloaded_for_run,
            "model_parameters_b": spec.parameters_b,
            "model_license": spec.license,
            "model_source": spec.source,
            "gguf_source": spec.gguf_source,
            "optimization": spec.optimization,
            "model_size_bytes": model_size,
            "process_peak_rss_bytes": process_peak_rss_bytes()?,
            "model_sha256": model_hash,
            "model_load_ms": load_ms,
            "language": test.language,
            "subtest": test.subtest,
            "test_id": test.id,
            "repetition": repetition,
            "system_prompt": test.system,
            "assistant_prefix": spec.assistant_prefix,
            "prompt_format": spec.prompt_format,
            "prompt": test.prompt,
            "response": stats.text,
            "max_output_tokens": test.max_tokens,
            "prompt_tokens": stats.prompt_tokens,
            "prompt_read_ms": stats.prompt_read.as_millis(),
            "output_tokens": stats.output_tokens,
            "generation_ms": stats.elapsed.as_millis(),
            "generation_tokens_per_second": tokens_per_second,
            "status": if deadline_reached { "deadline_partial" } else { "complete" },
            "scoring": "not_evaluated"
        });
        write_record(output, &row)?;
        println!(
            "{} {} #{}: {} tokens, {:.1} tok/s",
            spec.name, test.id, repetition, stats.output_tokens, tokens_per_second
        );

        if run.deadline.expired() {
            return Ok(());
        }
    }
    Ok(())
}

/// One reply and its timings.
struct Generation {
    text: String,
    prompt_tokens: usize,
    /// Until the first token could be sampled.
    prompt_read: Duration,
    output_tokens: usize,
    elapsed: Duration,
    /// Stopped by the deadline, not by the end of the reply or the token limit.
    deadline_reached: bool,
}

/// Answers `test` in a fresh context, stopping at the token limit, the end of the reply or the deadline.
fn generate(
    run: &Run,
    model: &LlamaModel,
    test: &TestCase,
    spec: &ModelSpec,
    seed: u32,
) -> Result<Generation, Box<dyn Error>> {
    let mut text = match spec.prompt_format.as_deref() {
        Some("gemma4") => {
            format!("<|turn>system\n{}<turn|>\n<|turn>user\n{}<turn|>\n<|turn>model\n", test.system, test.prompt)
        }
        Some(other) => return Err(format!("unknown prompt_format {other:?}").into()),
        None => {
            let template = model.chat_template(None)?;
            let chat = [
                LlamaChatMessage::new("system".into(), test.system.into())?,
                LlamaChatMessage::new("user".into(), test.prompt.into())?,
            ];
            model.apply_chat_template(&template, &chat, true)?
        }
    };
    text += spec.assistant_prefix.as_deref().unwrap_or_default();
    let vocab = model.vocab();
    let bos = vocab.token_to_piece(vocab.bos(), true, None);
    let add_bos = vocab.should_add_bos() && !text.as_bytes().starts_with(&bos);
    let prompt = vocab.tokenize(text.as_bytes(), add_bos, true);
    let max_tokens = test.max_tokens;
    if max_tokens > (N_CTX as usize).saturating_sub(prompt.len()) {
        return Err(format!("prompt of {} tokens does not fit n_ctx {N_CTX}", prompt.len()).into());
    }

    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(N_CTX))
        .with_n_batch(BATCH as u32)
        .with_n_threads(run.llm_threads)
        .with_n_threads_batch(run.llm_threads);
    let mut ctx = model.new_context(run.backend, params)?;
    let mut batch = LlamaBatch::new(BATCH, 1);
    let reading = Instant::now();
    for (start, chunk) in (0..).step_by(BATCH).zip(prompt.chunks(BATCH)) {
        batch.clear();
        for (pos, &token) in (start..).zip(chunk) {
            batch.add(token, pos as i32, &[0], pos + 1 == prompt.len())?;
        }
        ctx.decode(&mut batch)?;
    }
    ctx.synchronize();
    let prompt_read = reading.elapsed();

    // Greedy without a repeat penalty, which would punish the quotes and braces JSON repeats.
    let mut sampler = match run.mode {
        BenchMode::Scored => LlamaSampler::greedy(),
        _ => LlamaSampler::chain_simple([
            LlamaSampler::penalties(model.n_vocab(), PENALTY_LAST_N, REPEAT_PENALTY, 0.0, 0.0),
            LlamaSampler::top_k(TOP_K),
            LlamaSampler::top_p(TOP_P, 1),
            LlamaSampler::temp(TEMPERATURE),
            LlamaSampler::dist(SEED.wrapping_add(seed)),
        ]),
    };
    let mut bytes = Vec::new();
    let mut output_tokens = 0;
    let generating = Instant::now();
    let mut deadline_reached = false;
    while output_tokens < max_tokens {
        if run.deadline.expired() {
            deadline_reached = true;
            break;
        }
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        if vocab.is_eog(token) {
            break;
        }
        bytes.extend(vocab.token_to_piece(token, false, None));
        batch.clear();
        batch.add(token, (prompt.len() + output_tokens) as i32, &[0], true)?;
        ctx.decode(&mut batch)?;
        output_tokens += 1;
    }
    Ok(Generation {
        text: String::from_utf8_lossy(&bytes).trim().to_string(),
        prompt_tokens: prompt.len(),
        prompt_read,
        output_tokens,
        elapsed: generating.elapsed(),
        deadline_reached,
    })
}

/// Downloads `url` to `path` unless it exists, into a `.part` file that is renamed once its sha256 matches.
/// Returns whether it downloaded the file.
fn fetch(path: &Path, url: &str, sha256: &str, stop: &AtomicBool) -> Result<bool, Box<dyn Error>> {
    if path.exists() {
        return Ok(false);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(STALL))
        .timeout_recv_response(Some(STALL))
        .timeout_per_read(Some(STALL))
        .build()
        .into();
    let part = path.with_extension("part");
    let mut body = agent.get(url).call()?.into_body().into_reader();
    let mut file = File::create(&part)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        if stop.load(Ordering::Relaxed) {
            drop(file);
            fs::remove_file(&part)?;
            return Err("download stopped by the deadline".into());
        }
        let n = body.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
    }
    file.sync_all()?;
    let got = format!("{:x}", hash.finalize());
    if got != sha256 {
        fs::remove_file(&part)?;
        return Err(format!("{}: sha256 {got}, expected {sha256}", path.display()).into());
    }
    fs::rename(&part, path)?;
    Ok(true)
}

/// Deletes a model of the run with an unfinished download of it; the runner saves no prompt states.
fn remove_model(path: &Path) -> std::io::Result<()> {
    for file in [path.to_path_buf(), path.with_extension("part")] {
        match fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

fn write_record(output: &mut BufWriter<File>, record: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *output, record)?;
    output.write_all(b"\n")?;
    output.flush()?;
    output.get_ref().sync_data()?;
    Ok(())
}

/// Reads `PAL_BENCH_TESTS`, keeping the suites in `languages`. The strings live as long as the process.
fn load_tests(languages: &[&'static str]) -> Result<(Vec<TestCase>, TestSource), Box<dyn Error>> {
    let path = std::env::var_os("PAL_BENCH_TESTS").ok_or("PAL_BENCH_MODE=scored needs PAL_BENCH_TESTS")?;
    let bytes = fs::read(&path)?;
    let file: TestFile = serde_json::from_slice(&bytes)?;
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let mut tests = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for suite in file.suites {
        let Some(&language) = languages.iter().find(|&&l| l == suite.language) else {
            if !["de", "en", "fr", "es"].contains(&suite.language.as_str()) {
                return Err(format!("unsupported language in test file: {}", suite.language).into());
            }
            continue;
        };
        let (subtest, system) = (leak(suite.category), leak(suite.system));
        for item in suite.items {
            if !ids.insert(item.id.clone()) {
                return Err(format!("duplicate test id: {}", item.id).into());
            }
            tests.push(TestCase {
                id: leak(item.id),
                language,
                subtest,
                system,
                prompt: leak(item.prompt),
                max_tokens: suite.max_output_tokens,
            });
        }
    }
    Ok((tests, TestSource { name: file.name, sha256: format!("{:x}", Sha256::digest(&bytes)) }))
}

fn selected_languages() -> Result<Vec<&'static str>, Box<dyn Error>> {
    let requested = std::env::var("PAL_BENCH_LANGUAGES").unwrap_or_else(|_| "de,en".into());
    let mut languages = Vec::new();
    for code in requested.split(',').map(str::trim).filter(|code| !code.is_empty()) {
        let language = match code {
            "de" => "de",
            "en" => "en",
            "fr" => "fr",
            "es" => "es",
            _ => return Err(format!("unsupported language code: {code}").into()),
        };
        if !languages.contains(&language) {
            languages.push(language);
        }
    }
    if languages.is_empty() {
        return Err("PAL_BENCH_LANGUAGES must select at least one of de,en,fr,es".into());
    }
    Ok(languages)
}

/// Ends early with an error once `stop` is set.
fn sha256(path: &Path, stop: &AtomicBool) -> Result<String, Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err("hashing stopped by the deadline".into());
        }
        let bytes = file.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        hash.update(&buffer[..bytes]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "android"))]
fn process_peak_rss_bytes() -> Result<u64, Box<dyn Error>> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let peak = unsafe { usage.assume_init() }.ru_maxrss;
    let peak = u64::try_from(peak)?;
    #[cfg(target_os = "macos")]
    {
        Ok(peak)
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        Ok(peak.saturating_mul(1024))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn process_peak_rss_bytes() -> Result<u64, Box<dyn Error>> {
    Err("peak RSS measurement is supported on macOS, Linux, and Android".into())
}
