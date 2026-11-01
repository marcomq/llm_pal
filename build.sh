#!/bin/sh
# Usage: ./build.sh setup | test | godot | android | install | push-models
# android: debug APK in build/; install: also installs it; push-models: copies models/ to the phone (adb).
set -eu
cd "$(dirname "$0")"
GODOT="${GODOT:-godot}"
NDK="${ANDROID_NDK_HOME:-/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865}"
APK=build/llm_pal.apk
PACKAGE=io.github.marcomq.llmpal

# Godot extension for macOS, then start the app.
run_godot() {
    (cd rust && cargo build --release -p pal-godot)
    mkdir -p godot/bin/macos
    # A new file, not an overwrite: macOS kills a process that maps a signed library changed in place.
    rm -f godot/bin/macos/libpal_godot.dylib
    cp rust/target/release/libpal_godot.dylib godot/bin/macos/
    (cd godot && "$GODOT" --headless --import)
    "$GODOT" --path godot
}

# Godot extension for arm64 Android, then the APK. Godot's template already ships libc++_shared.so.
build_android() {
    # llama-cpp-sys-2 reads ANDROID_NDK, cargo-ndk ANDROID_NDK_HOME.
    export ANDROID_NDK_HOME="$NDK" ANDROID_NDK="$NDK"
    # ggml's int8 dot-product kernels (all arm64 phones since ~2018); without them the LLM runs at ~1 token/s.
    export GGML_CPU_ARM_ARCH="${GGML_CPU_ARM_ARCH:-armv8.2-a+dotprod+fp16}"
    (cd rust && cargo ndk -t arm64-v8a --platform 26 build --release -p pal-godot)
    mkdir -p godot/bin/android build
    out=rust/target/aarch64-linux-android/release
    cp "$out/libpal_godot.so" godot/bin/android/
    (cd godot && "$GODOT" --headless --import && "$GODOT" --headless --export-debug Android)
}

# The models stay out of the APK (about 2 GB); the app reads them from its own folder.
# The app must create that folder (start it once): files in a folder made by adb stay unreadable for the app.
push_models() {
    dir="/sdcard/Android/data/$PACKAGE/files/models"
    if ! adb shell test -d "$dir"; then
        echo "start the app once first, it creates $dir" >&2
        exit 1
    fi
    for f in silero_vad.onnx smart-turn-v3.2-cpu.onnx Qwen3.5-2B-Q4_K_M.gguf parakeet-tdt-0.6b-v3; do
        adb push "models/$f" "$dir/"
    done
    push_pocket "$dir/pocket"
}

# Pocket TTS files from the Hugging Face cache (Pal on the Mac downloads them), at the revisions in
# rust/pal-tts/config and defaults.rs. A language whose files are missing is skipped.
push_pocket() {
    repo=kyutai/pocket-tts-without-voice-cloning
    snap="${HF_HOME:-$HOME/.cache/huggingface}/hub/models--kyutai--pocket-tts-without-voice-cloning/snapshots"
    voices="${POCKET_VOICES:-juergen stuart_bell michael}"
    for spec in german:1e08e6a23401048648a9fdcfde2f89348215c2a7:4e1e0a3e611c51c0b4ed8174fc10f32a54644303 \
        english:a7056f505a4a8ca72d8b5fb1440071d6f1a35739:00eac05ed3d16bdc3f6b5d598874019c34a89214; do
        lang=${spec%%:*} rest=${spec#*:}
        model=${rest%%:*} tok=${rest#*:}
        src="languages/$lang"
        m="$snap/$model/$src/model.safetensors"
        t="$snap/$tok/$src/tokenizer.json"
        if [ ! -e "$m" ] || [ ! -e "$t" ]; then
            echo "skipping Pocket $lang: not in the cache (start Pal on the Mac in that language once)" >&2
            continue
        fi
        adb shell mkdir -p "$1/$repo/$src/embeddings"
        adb push "$m" "$t" "$1/$repo/$src/"
        for voice in $voices; do
            v="$snap/a3055dcb533cf2ec90458008a01e7cef550bae2a/$src/embeddings/$voice.safetensors"
            if [ -e "$v" ]; then
                adb push "$v" "$1/$repo/$src/embeddings/"
            else
                echo "skipping Pocket voice $voice ($lang): not in the cache (pick it on the Mac once)" >&2
            fi
        done
    done
}

case "${1:-}" in
    setup) sh scripts/fetch-models.sh && sh scripts/make-test-audio.sh ;;
    test) (cd rust && cargo test --workspace -- --include-ignored --skip speaker) ;;
    godot) run_godot ;;
    android) build_android ;;
    install) build_android && adb install -r "$APK" ;;
    push-models) push_models ;;
    *) echo "usage: $0 setup | test | godot | android | install | push-models" >&2; exit 1 ;;
esac
