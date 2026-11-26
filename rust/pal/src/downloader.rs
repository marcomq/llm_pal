//! Downloads missing model files into the model folder, the same way on every platform: over HTTPS with the
//! built-in Mozilla roots, into a `.part` file that is renamed once its sha256 matches.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::think::language::Language;
use crate::{Error, Result};

const HF: &str = "https://huggingface.co";
const POCKET: &str = "pocket/kyutai/pocket-tts-without-voice-cloning";
/// A server that sends nothing this long ends the download; a slow but steady one doesn't.
const STALL: Duration = Duration::from_secs(30);

/// Bytes of the running download, both 0 without one. `total` stays 0 if a server doesn't tell a file's size.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

/// Path in the model folder, URL, sha256.
type Entry = (&'static str, &'static str, &'static str);

const SILERO: Entry = (
    "silero_vad.onnx",
    "https://github.com/snakers4/silero-vad/raw/bfdc0193023f121ea5b3cc7b176dbed570a68a59/src/silero_vad/data/silero_vad.onnx",
    "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
);

/// Every file Pal can use. Licenses: Silero VAD MIT, Smart Turn BSD-2, Parakeet CC-BY-4.0,
/// SmolLM3, Gemma 4 and Granite 4.0 Apache-2.0, Pocket TTS CC-BY-4.0.
const CATALOG: &[Entry] = &[
    SILERO,
    (
        "smart-turn-v3.2-cpu.onnx",
        "pipecat-ai/smart-turn-v3/resolve/f766f81d3cfdf7737ac64aad813d91bbfd56bf93/smart-turn-v3.2-cpu.onnx",
        "2bb026316b14a660486a75b1733cd3fbab8c2fd0314dc9af7be49f8cca967e4f",
    ),
    (
        "parakeet-tdt-0.6b-v3/encoder-model.int8.onnx",
        "istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/encoder-model.int8.onnx",
        "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
    ),
    (
        "parakeet-tdt-0.6b-v3/decoder_joint-model.int8.onnx",
        "istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/decoder_joint-model.int8.onnx",
        "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
    ),
    (
        "parakeet-tdt-0.6b-v3/vocab.txt",
        "istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/vocab.txt",
        "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
    ),
    (
        "SmolLM3-Q4_K_M.gguf",
        "ggml-org/SmolLM3-3B-GGUF/resolve/4965cb60b150737b68a0408c36aeefb65078f894/SmolLM3-Q4_K_M.gguf",
        "8334b850b7bd46238c16b0c550df2138f0889bf433809008cc17a8b05761863e",
    ),
    (
        "gemma-4-E2B-it-Q4_K_M.gguf",
        "unsloth/gemma-4-E2B-it-GGUF/resolve/0314792d7f1f7e229411f620751375812bb9faf2/gemma-4-E2B-it-Q4_K_M.gguf",
        "740185b21d22ceb83a11c3aa62ad5842ef32c70f6096d756bbee85a1e4ec34b8",
    ),
    (
        "granite-4.0-h-1b-Q4_0.gguf",
        "ibm-granite/granite-4.0-h-1b-GGUF/resolve/408580999f394d67b425c1e99ae9fe4a540c9672/granite-4.0-h-1b-Q4_0.gguf",
        "23b20864c9544bd98ac6d5b8c53dec61992cc7add09fdd5f6b77472a4679a148",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/german/model.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/1e08e6a23401048648a9fdcfde2f89348215c2a7/languages/german/model.safetensors",
        "9fe42605604832349e3f495177bde56b77ae8fee73b2ed4b63236092ccde7621",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/german/tokenizer.json",
        "kyutai/pocket-tts-without-voice-cloning/resolve/4e1e0a3e611c51c0b4ed8174fc10f32a54644303/languages/german/tokenizer.json",
        "2d77849811e3d1b6ae80e5b11b203f57e1847901104d8a0573b34f46f8dddb53",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/english/model.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a7056f505a4a8ca72d8b5fb1440071d6f1a35739/languages/english/model.safetensors",
        "4c56cbd171c63e8a26ea61f5d48b8a0cd8f3c51f4652c0ab9a19bfcced8c931d",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/english/tokenizer.json",
        "kyutai/pocket-tts-without-voice-cloning/resolve/00eac05ed3d16bdc3f6b5d598874019c34a89214/languages/english/tokenizer.json",
        "f498428e1eafee50492f7be13dc9bfafcfc12e508cd0eb1b01c92ecd5d8c6687",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/german/embeddings/juergen.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/german/embeddings/juergen.safetensors",
        "65104a25b2aa797c50b2cee555cb75deb6abba10169168b95857227a054126d1",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/german/embeddings/stuart_bell.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/german/embeddings/stuart_bell.safetensors",
        "60062fc7b9f52bcdf7cfdfc574abb84e476595979283bdadfcd53cf6b5b3e384",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/german/embeddings/michael.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/german/embeddings/michael.safetensors",
        "cf14510456e4a0047160f052ccaa35a24b012eddf260b593b64dfa9fa1fe6d73",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/english/embeddings/juergen.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/english/embeddings/juergen.safetensors",
        "1382e92abb0abf4cd531d33d1ad4e6db0c9ebd10ab1b1533421e151f14190450",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/english/embeddings/stuart_bell.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/english/embeddings/stuart_bell.safetensors",
        "6ccec2880941cd132a9997798c8b0350d3dd547c890e45024873a85b017e5b39",
    ),
    (
        "pocket/kyutai/pocket-tts-without-voice-cloning/languages/english/embeddings/michael.safetensors",
        "kyutai/pocket-tts-without-voice-cloning/resolve/a3055dcb533cf2ec90458008a01e7cef550bae2a/languages/english/embeddings/michael.safetensors",
        "188f29bf53aca645d77accd44455b494a28e17d94b1658fbaec80f679371ad13",
    ),
];

/// Further LLMs, fetched only once chosen in the settings.
const ON_REQUEST: &[&str] = &["gemma-4-E2B-it-Q4_K_M.gguf", "granite-4.0-h-1b-Q4_0.gguf"];

/// Models Pal no longer offers: never fetched again, but a copy from an earlier version can still be removed.
const RETIRED: &[&str] = &[
    "Qwen3.5-0.8B-Q4_K_M.gguf",
    "Qwen3.5-2B-Q4_K_M.gguf",
    "Qwen3.5-4B-Q4_K_M.gguf",
    "Qwen3.5-9B-Q4_K_M.gguf",
    "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
    "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
    "Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf",
];

/// The Parakeet folder's files.
pub fn stt_files(folder: &str) -> Vec<String> {
    let prefix = format!("{folder}/");
    CATALOG.iter().filter_map(|(path, ..)| path.strip_prefix(&prefix).map(|_| path.to_string())).collect()
}

/// Pocket TTS files of `language` and `voice`; the last one is the voice. A voice outside the catalog is
/// one's own, e.g. cloned with pal-tts, in `pocket/voices/<code>/<voice>.safetensors`; it is never downloaded.
pub fn pocket_files(language: Language, voice: &str) -> Vec<String> {
    let dir = format!("{POCKET}/languages/{}", language.profile().pocket_model);
    let catalog = format!("{dir}/embeddings/{voice}.safetensors");
    let voice = if CATALOG.iter().any(|(path, ..)| *path == catalog) {
        catalog
    } else {
        format!("pocket/voices/{}/{voice}.safetensors", language.code())
    };
    vec![format!("{dir}/model.safetensors"), format!("{dir}/tokenizer.json"), voice]
}

/// The catalog without the larger LLMs, e.g. for `build.sh setup`.
pub fn all_files() -> Vec<String> {
    CATALOG.iter().filter(|(path, ..)| !ON_REQUEST.contains(path)).map(|(path, ..)| path.to_string()).collect()
}

/// Folder in the model folder for GGUF files the user brought, `Settings.OWN_MODEL_DIR`; Pal never downloads into it.
const OWN_LLM_DIR: &str = "llm";

/// `llm/<file>.gguf`: one of the user's own models.
fn is_own_llm(name: &str) -> bool {
    name.strip_prefix(OWN_LLM_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|file| file.ends_with(".gguf") && !file.contains(['/', '\\']))
}

/// Deletes a catalog LLM or one of one's own, with its saved prompt states and an unfinished download or copy.
/// Refuses any other file.
pub fn remove_llm(dir: &Path, name: &str) -> Result<()> {
    let catalog = CATALOG.iter().any(|(path, ..)| *path == name) || RETIRED.contains(&name);
    if !name.ends_with(".gguf") || !(catalog || is_own_llm(name)) {
        return Err(Error::Download(format!("{name} is not a catalog model")));
    }
    let failed = |e: std::io::Error| Error::Download(format!("removing {name}: {e}"));
    let path = dir.join(name);
    let folder = path.parent().unwrap_or(dir);
    let saved = format!("{}.", name.rsplit('/').next().unwrap_or(name));
    let mut files = vec![path.clone(), path.with_extension("part")];
    for entry in std::fs::read_dir(folder).map_err(failed)? {
        let file = entry.map_err(failed)?.file_name();
        let file = file.to_string_lossy();
        if file.starts_with(&saved) && (file.ends_with(".state") || file.ends_with(".tmp")) {
            files.push(folder.join(file.as_ref()));
        }
    }
    for file in files {
        match std::fs::remove_file(file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(failed(e)),
            _ => {}
        }
    }
    Ok(())
}

/// Deletes the saved prompt states of every model; they are made again on the next start.
pub fn remove_llm_caches(dir: &Path) -> Result<()> {
    let failed = |e: std::io::Error| Error::Download(format!("removing the LLM cache: {e}"));
    let own = dir.join(OWN_LLM_DIR);
    for folder in [dir, own.as_path()] {
        if folder != dir && !folder.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(folder).map_err(failed)? {
            let entry = entry.map_err(failed)?;
            let file = entry.file_name();
            let file = file.to_string_lossy();
            if file.contains(".gguf.") && (file.ends_with(".state") || file.ends_with(".tmp")) {
                std::fs::remove_file(entry.path()).map_err(failed)?;
            }
        }
    }
    Ok(())
}

/// Downloads each of `files` (paths in `dir`) that is missing. A file not in the catalog must be there already.
/// Setting `stop` ends it within a read. `progress` counts the bytes while it runs.
pub fn ensure(dir: &Path, files: &[String], stop: &AtomicBool, progress: &Progress) -> Result<()> {
    let result = fetch_missing(dir, files, stop, progress);
    progress.done.store(0, Ordering::Relaxed);
    progress.total.store(0, Ordering::Relaxed);
    result
}

fn fetch_missing(dir: &Path, files: &[String], stop: &AtomicBool, progress: &Progress) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(STALL))
        .timeout_recv_response(Some(STALL))
        .timeout_per_read(Some(STALL))
        .build()
        .into();
    let mut missing = Vec::new();
    for name in files {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        let Some((_, url, sha256)) = CATALOG.iter().find(|(p, ..)| p == name) else {
            return Err(Error::Download(format!("{} is missing", path.display())));
        };
        let url = if url.starts_with("https://") { url.to_string() } else { format!("{HF}/{url}") };
        missing.push((name, path, url, *sha256));
    }
    let total: Option<u64> = missing.iter().map(|(_, _, url, _)| remote_size(&agent, url)).sum();
    progress.total.store(total.unwrap_or(0), Ordering::Relaxed);
    for (name, path, url, sha256) in missing {
        eprintln!("llm_pal: downloading {name}");
        if let Err(e) = download(&agent, &url, &path, sha256, stop, progress) {
            return Err(if stop.load(Ordering::Relaxed) {
                Error::Stopped
            } else {
                Error::Download(format!("{name}: {e}"))
            });
        }
        eprintln!("llm_pal: downloaded {name}");
    }
    Ok(())
}

/// The file's size, asked uncompressed as it is counted while downloading.
fn remote_size(agent: &ureq::Agent, url: &str) -> Option<u64> {
    let response = agent.head(url).header("Accept-Encoding", "identity").call().ok()?;
    response.headers().get("content-length")?.to_str().ok()?.parse().ok()
}

fn download(
    agent: &ureq::Agent,
    url: &str,
    path: &Path,
    sha256: &str,
    stop: &AtomicBool,
    progress: &Progress,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = path.with_extension("part");
    let mut body = agent.get(url).call()?.into_body().into_reader();
    let mut file = File::create(&part)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        if stop.load(Ordering::Relaxed) {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err("stopped".into());
        }
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        progress.done.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.sync_all()?;
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got != sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("sha256 {got}, expected {sha256}").into());
    }
    std::fs::rename(&part, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_match_the_catalog() {
        assert_eq!(stt_files("parakeet-tdt-0.6b-v3").len(), 3);
        for language in Language::ALL {
            for voice in ["juergen", "stuart_bell", "michael"] {
                for file in pocket_files(language, voice) {
                    assert!(CATALOG.iter().any(|(p, ..)| *p == file), "{file}");
                }
            }
        }
        let own = format!("pocket/voices/{}/my_voice.safetensors", Language::ALL[0].code());
        assert_eq!(pocket_files(Language::ALL[0], "my_voice").last(), Some(&own));
        assert_eq!(SILERO.0, all_files()[0]);
    }

    #[test]
    fn larger_models_are_not_part_of_the_setup() {
        let files = all_files();
        assert!(files.iter().any(|f| f == "SmolLM3-Q4_K_M.gguf"));
        for model in ON_REQUEST {
            assert!(CATALOG.iter().any(|(p, ..)| p == model), "{model}");
            assert!(!files.iter().any(|f| f == model), "{model}");
        }
    }

    #[test]
    fn removing_the_caches_keeps_the_models() {
        let dir = std::env::temp_dir().join(format!("pal-remove-caches-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(dir.join(OWN_LLM_DIR)).unwrap();
        let gone = ["a.gguf.01.state", "b.gguf.02.state", "b.gguf.03.tmp", "llm/c.gguf.04.state"];
        let kept = ["a.gguf", "b.gguf", "b.part", "notes.state", "llm/c.gguf"];
        for file in gone.iter().chain(&kept) {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        remove_llm_caches(&dir).unwrap();
        assert!(gone.iter().all(|f| !dir.join(f).exists()));
        assert!(kept.iter().all(|f| dir.join(f).exists()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_a_model_takes_only_its_own_files() {
        let dir = std::env::temp_dir().join(format!("pal-remove-llm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gone = [
            "Qwen3.5-4B-Q4_K_M.gguf",
            "Qwen3.5-4B-Q4_K_M.part",
            "Qwen3.5-4B-Q4_K_M.gguf.0123.state",
            "Qwen3.5-4B-Q4_K_M.gguf.0123.tmp",
        ];
        let kept = ["Qwen3.5-2B-Q4_K_M.gguf", "Qwen3.5-2B-Q4_K_M.gguf.0123.state", "mine.gguf"];
        for file in gone.iter().chain(&kept) {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        remove_llm(&dir, gone[0]).unwrap();
        assert!(gone.iter().all(|f| !dir.join(f).exists()));
        assert!(kept.iter().all(|f| dir.join(f).exists()));
        assert!(remove_llm(&dir, "mine.gguf").is_err());
        assert!(remove_llm(&dir, "../mine.gguf").is_err());
        assert!(remove_llm(&dir, "llm/../mine.gguf").is_err());
        assert!(dir.join("mine.gguf").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_an_own_model_takes_only_its_own_files() {
        let dir = std::env::temp_dir().join(format!("pal-remove-own-llm-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(OWN_LLM_DIR)).unwrap();
        let gone = ["llm/mine.gguf", "llm/mine.part", "llm/mine.gguf.0123.state"];
        let kept = ["llm/other.gguf", "llm/other.gguf.0123.state", "mine.gguf.0123.state"];
        for file in gone.iter().chain(&kept) {
            std::fs::write(dir.join(file), b"x").unwrap();
        }
        remove_llm(&dir, gone[0]).unwrap();
        assert!(gone.iter().all(|f| !dir.join(f).exists()));
        assert!(kept.iter().all(|f| dir.join(f).exists()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
