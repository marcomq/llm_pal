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

# Whisper small, q5_1 (MIT), speech to text.
fetch https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin \
    models/ggml-small-q5_1.bin ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb

# Qwen3.5 0.8B, Q4_K_M (Apache-2.0), commentary LLM.
fetch https://huggingface.co/unsloth/Qwen3.5-0.8B-GGUF/resolve/main/Qwen3.5-0.8B-Q4_K_M.gguf \
    models/Qwen3.5-0.8B-Q4_K_M.gguf bd258782e35f7f458f8aced1adc053e6e92e89bc735ba3be89d38a06121dc517

# Optional: Qwen3.5 2B, Q4_K_M (Apache-2.0), a larger LLM to compare on the desktop.
if [ "${QWEN_2B:-0}" = 1 ]; then
    fetch https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/resolve/main/Qwen3.5-2B-Q4_K_M.gguf \
        models/Qwen3.5-2B-Q4_K_M.gguf aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223
fi
