#!/bin/sh
# Usage: ./build.sh setup | test | godot | android | install
# android: debug APK in build/; install: also installs it. The app downloads missing models itself.
set -eu
cd "$(dirname "$0")"
GODOT="${GODOT:-godot}"
NDK="${ANDROID_NDK_HOME:-/opt/homebrew/share/android-commandlinetools/ndk/29.0.14206865}"
APK=build/llm_pal.apk
PHONE_MODELS=/storage/emulated/0/Android/data/io.github.marcomq.llmpal/files/models

# Godot extension for macOS, then start the app.
run_godot() {
    (cd rust && cargo build --release -p pal)
    mkdir -p godot/bin/macos
    # A new file, not an overwrite: macOS kills a process that maps a signed library changed in place.
    rm -f godot/bin/macos/libpal.dylib
    cp rust/target/release/libpal.dylib godot/bin/macos/
    (cd godot && "$GODOT" --headless --import)
    "$GODOT" --path godot
}

# Godot extension for arm64 Android, then the APK. Godot's template already ships libc++_shared.so.
build_android() {
    # llama-cpp-sys-2 reads ANDROID_NDK, cargo-ndk ANDROID_NDK_HOME.
    export ANDROID_NDK_HOME="$NDK" ANDROID_NDK="$NDK"
    # ggml's int8 dot-product kernels (all arm64 phones since ~2018); without them the LLM runs at ~1 token/s.
    export GGML_CPU_ARM_ARCH="${GGML_CPU_ARM_ARCH:-armv8.2-a+dotprod+fp16}"
    (cd rust && cargo ndk -t arm64-v8a --platform 26 build --release -p pal)
    mkdir -p godot/bin/android build
    out=rust/target/aarch64-linux-android/release
    # The unstripped library stays in $out, for ndk-stack.
    strip=$(ls "$NDK"/toolchains/llvm/prebuilt/*/bin/llvm-strip | head -n 1)
    "$strip" --strip-debug -o godot/bin/android/libpal.so "$out/libpal.so"
    (cd godot && "$GODOT" --headless --import && "$GODOT" --headless --export-debug Android)
}

case "${1:-}" in
    setup) (cd rust && cargo test --release -p pal --test main download_models -- --ignored --exact) \
        && sh scripts/make-test-audio.sh ;;
    test) (cd rust && cargo test --release --workspace -- --include-ignored --skip reply:: --skip download_models) ;;
    godot) run_godot ;;
    android) build_android ;;
    # A standalone call, so `set -e` stops on a failed build instead of installing an old APK.
    install) build_android
        adb install -r "$APK"
        # Own voices are never downloaded, so they go onto the phone here, into the folder the app
        # made: one made by adb belongs to the shell user and the app could not write into it.
        if [ -d models/pocket/voices ]; then
            if adb shell test -d "$PHONE_MODELS/pocket"; then
                adb push models/pocket/voices "$PHONE_MODELS/pocket/"
            else
                echo "start Pal once so it creates $PHONE_MODELS/pocket, then install again for the own voices" >&2
            fi
        fi ;;
    *) echo "usage: $0 setup | test | godot | android | install" >&2; exit 1 ;;
esac
