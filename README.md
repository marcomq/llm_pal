# llm_pal

Pal is a local voice companion written in Rust and running in Godot on macOS and Android. You talk, and Pal answers in
character as KITT, with a voice. Everything runs on the device: no cloud and no account.

Microphone → 16 kHz mono → Silero VAD → Smart Turn (end of turn) → speech segments and prosody → Parakeet TDT (STT, via mq-bridge-onnx) → Qwen3.5 2B (llama.cpp) →
sentence streaming → Kyutai Pocket TTS.

```
rust/pal         the app as one crate (GDExtension `PalNode` in godot.rs)
  hear/          capture, resampling, echo cancellation, VAD, segmenter, Smart Turn, prosody, STT
  think/         the conversation, persona, party mode, LLM (local or a server);
                 think/language/ holds every text, word list and number word per language (de.rs, en.rs)
  speak/         sentence splitting, numbers as words, Pocket TTS, playback
  transport.rs   connects the parts over mq-bridge
  downloader.rs  fetches missing models (sha256-checked), the same on macOS and Android
  platform/      the few things Android needs (desktop.rs is empty)
rust/pal-tts     Kyutai Pocket TTS in-process (model code from kyutai-labs/pocket-tts, MIT)
godot/           UI: KITT's voice box or HAL's eye (space or a tap on the label switches); language toggle top right
scripts/         make-test-audio.sh (macOS `say`), make-hal-panel.py
```

```sh
./build.sh setup         # models and a test recording
./build.sh test          # all tests, including the ones that need the models
./build.sh godot         # Pal with the Godot display (models from models/ or $PAL_MODELS; $PAL_LLM picks the GGUF)
./build.sh install       # build the arm64 Android APK and install it with adb
```

Pal downloads missing models into its model folder on start, on the Mac and on Android alike (about 2 GB the first
time).

The UI starts in German; English is one tap away.

## Voices of one's own

Besides the built-in voices (e.g. `juergen`), Pal loads voices of one's own from

```
<models folder>/pocket/voices/<de|en>/<name>.safetensors
```

They are never downloaded; `./build.sh install` copies them to the phone once Pal has started there (it makes the
folder). The voice then shows up in the settings (key S or "…"). Such a file is made from a 10–30 s recording with the separate tool `pal-voice` (Pocket TTS's
voice cloning model, see its README).

**Only clone your own voice or the voice of a person who has expressly agreed.** Never use someone else's voice
without their consent: not from films, series, radio plays, podcasts or other recordings, and not a voice someone
else has already cloned. Kyutai's terms for the cloning model require consent, and a person's voice is protected by
their personality rights. To get a character's style, have a consenting person speak in that style. Don't pass on
recordings or voices (no repository, APK or download; `models/` is git-ignored), and don't use a cloned voice so
that others take it for the real person. This is not legal advice; when in doubt, use only your own voice.

License: MIT OR Apache-2.0. Silero VAD: MIT. Smart Turn: BSD-2-Clause. Parakeet TDT: CC-BY-4.0. Qwen3.5: Apache-2.0. Pocket TTS code: MIT (check the Kyutai model card for the weights). No GPL.
