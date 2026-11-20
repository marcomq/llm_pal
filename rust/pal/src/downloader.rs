//! Downloads missing model files into the model folder, the same way on every platform: over HTTPS with the
//! built-in Mozilla roots, into a `.part` file that is renamed once its sha256 matches.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::{Error, Result};

const HF: &str = "https://huggingface.co";
const POCKET: &str = "pocket/kyutai/pocket-tts-without-voice-cloning";

/// Path in the model folder, URL, sha256.
type Entry = (&'static str, &'static str, &'static str);

const SILERO: Entry = (
    "silero_vad.onnx",
    "https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx",
    "1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3",
);

/// Every file Pal can use. Licenses: Silero VAD MIT, Smart Turn BSD-2, Parakeet CC-BY-4.0, Qwen Apache-2.0,
/// Pocket TTS CC-BY-4.0.
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
        "Qwen3.5-2B-Q4_K_M.gguf",
        "unsloth/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf",
        "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223",
    ),
    (
        "Qwen3.5-0.8B-Q4_K_M.gguf",
        "unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf",
        "bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517",
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

/// The Parakeet folder's files.
pub fn stt_files(folder: &str) -> Vec<String> {
    let prefix = format!("{folder}/");
    CATALOG.iter().filter_map(|(path, ..)| path.strip_prefix(&prefix).map(|_| path.to_string())).collect()
}

/// Pocket TTS files of a language (`Profile::pocket_model`) and voice.
pub fn pocket_files(language: &str, voice: &str) -> Vec<String> {
    let dir = format!("{POCKET}/languages/{language}");
    vec![
        format!("{dir}/model.safetensors"),
        format!("{dir}/tokenizer.json"),
        format!("{dir}/embeddings/{voice}.safetensors"),
    ]
}

/// Every file in the catalog, e.g. for `build.sh setup`.
pub fn all_files() -> Vec<String> {
    CATALOG.iter().map(|(path, ..)| path.to_string()).collect()
}

/// Downloads each of `files` (paths in `dir`) that is missing. A file not in the catalog must be there already.
pub fn ensure(dir: &Path, files: &[String]) -> Result<()> {
    for name in files {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        let Some((_, url, sha256)) = CATALOG.iter().find(|(p, ..)| p == name) else {
            return Err(Error::Download(format!("{} is missing", path.display())));
        };
        let url = if url.starts_with("https://") { url.to_string() } else { format!("{HF}/{url}") };
        eprintln!("llm_pal: downloading {name}");
        download(&url, &path, sha256).map_err(|e| Error::Download(format!("{name}: {e}")))?;
        eprintln!("llm_pal: downloaded {name}");
    }
    Ok(())
}

fn download(url: &str, path: &Path, sha256: &str) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = path.with_extension("part");
    let mut body = ureq::get(url).call()?.into_body().into_reader();
    let mut file = File::create(&part)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])?;
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
        for language in ["german", "english"] {
            for voice in ["juergen", "stuart_bell", "michael"] {
                for file in pocket_files(language, voice) {
                    assert!(CATALOG.iter().any(|(p, ..)| *p == file), "{file}");
                }
            }
        }
        assert_eq!(SILERO.0, all_files()[0]);
    }
}
