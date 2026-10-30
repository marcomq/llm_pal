#!/bin/sh
# Downloads the models into models/ and checks their sha256.
set -eu
cd "$(dirname "$0")/.."
mkdir -p models

fetch() { # url file sha256
    if [ ! -f "$2" ] || ! echo "$3  $2" | shasum -a 256 -c - >/dev/null 2>&1; then
        curl -fL -o "$2.part" "$1"
        echo "$3  $2.part" | shasum -a 256 -c -
        mv "$2.part" "$2"
    fi
    echo "ok: $2"
}

# Silero VAD v6 (MIT), 16 kHz streaming model.
fetch https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx \
    models/silero_vad.onnx 1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3

# Smart Turn v3.2, int8 CPU build (BSD-2), end of turn from the audio.
fetch https://huggingface.co/pipecat-ai/smart-turn-v3/resolve/f766f81d3cfdf7737ac64aad813d91bbfd56bf93/smart-turn-v3.2-cpu.onnx \
    models/smart-turn-v3.2-cpu.onnx 2bb026316b14a660486a75b1733cd3fbab8c2fd0314dc9af7be49f8cca967e4f

# Whisper small, q5_1 (MIT), speech to text.
fetch https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin \
    models/ggml-small-q5_1.bin ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb

# Qwen3.5 2B, Q4_K_M (Apache-2.0), commentary LLM.
fetch https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf \
    models/Qwen3.5-2B-Q4_K_M.gguf aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223

# Optional: Qwen3.5 0.8B, Q4_K_M (Apache-2.0), faster but with clumsier German (PAL_LLM picks it).
if [ "${QWEN_SMALL:-0}" = 1 ]; then
    fetch https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf \
        models/Qwen3.5-0.8B-Q4_K_M.gguf bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517
fi

# Optional: Parakeet TDT 0.6B v3, int8 (CC-BY-4.0), compared against Whisper in pal-brain/tests/stt_compare.rs.
if [ "${PARAKEET:-0}" = 1 ]; then
    mkdir -p models/parakeet-tdt-0.6b-v3
    hf=https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce
    fetch $hf/encoder-model.int8.onnx models/parakeet-tdt-0.6b-v3/encoder-model.int8.onnx \
        6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09
    fetch $hf/decoder_joint-model.int8.onnx models/parakeet-tdt-0.6b-v3/decoder_joint-model.int8.onnx \
        eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70
    fetch $hf/vocab.txt models/parakeet-tdt-0.6b-v3/vocab.txt \
        d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d
fi
