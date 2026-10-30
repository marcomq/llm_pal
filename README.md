# llm_pal

Pal is a local voice companion written in Rust and running in Godot on macOS and Android. You talk, and Pal answers in
character as KITT, with a voice. Everything runs on the device: no cloud and no account.

Microphone → 16 kHz mono → Silero VAD → Smart Turn (end of turn) → speech segments and prosody → Whisper (STT) → Qwen3.5 2B (llama.cpp) →
sentence streaming → Kyutai Pocket TTS.

```
rust/pal-core    capture, resampling, VAD, segmenter, prosody features, playback
rust/pal-brain   the agent: STT loader, LLM, prosody tags, persona, sentence splitting, numbers as words, TTS
                 language/ holds every text and word list per language (de.rs, en.rs)
rust/pal-stt     whisper.cpp as a shared library (its ggml version clashes with llama.cpp's otherwise)
rust/pal-tts     Kyutai Pocket TTS in-process (model code from kyutai-labs/pocket-tts, MIT)
rust/pal-godot   GDExtension: `PalNode` (agent, state, Pal's voice level, signals for texts)
godot/           UI: KITT's voice box or HAL's eye (space or a tap on the label switches); language toggle top right
scripts/         fetch-models.sh (Silero, Smart Turn, Whisper, Qwen; sha256-checked), make-test-audio.sh (macOS `say`),
                 make-hal-panel.py
```

```sh
./build.sh setup         # models and a test recording
./build.sh test          # all tests, including the ones that need the models
./build.sh godot         # Pal with the Godot display (models from models/ or $PAL_MODELS; $PAL_LLM picks the GGUF)
./build.sh install       # build the arm64 Android APK and install it with adb
./build.sh push-models   # copy the models to the phone (once, about 1.5 GB)
```

On the Mac, Pocket TTS downloads its model and voice into the Hugging Face cache on first start. Android builds have
no downloader; `push-models` copies those files from the Mac's cache.

The UI starts in German; English is one tap away.

License: MIT OR Apache-2.0. Silero VAD: MIT. Smart Turn: BSD-2-Clause. Whisper: MIT. Qwen3.5: Apache-2.0. Pocket TTS code: MIT (check the Kyutai model card for the weights). No GPL.
