# llm_pal

Pal is a local voice companion written in Rust and running in Godot on macOS and Android. You talk, and Pal answers in
character as KITT (**K**inda **I**ntelligent, **T**otally **T**hirsty), with a voice. By default everything runs on
the device: no cloud and no account. Only the optional servers you set up yourself (an LLM server, Fish Audio) receive data.

> **Disclaimer:** Pal is a private hobby project and not an official product. It is not affiliated with, endorsed by or
> sponsored by Fish Audio, Kyutai, or the owners of Knight Rider or KITT. All names and trademarks belong to their
> owners. The software comes without warranty; you are responsible for how you use it and for the voices you choose.
>
> **Parody:** Pal's default character is an unofficial, non-commercial parody of the talking car from 1980s television:
> vain, thirsty and unreliable. It is not affiliated with or endorsed by the rights holders of the original series, and
> it does not use its footage, music, logos or voices.

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
godot/           UI: KITT's voice box; language toggle top right
generate_voice/  makes a voice of one's own from a recording (separate crate, not part of the app)
scripts/         make-test-audio.sh (macOS `say`), make-hal-panel.py
```

```sh
./build.sh setup         # models and a test recording
./build.sh test          # all tests, including the ones that need the models
./build.sh godot         # Pal with the Godot display (models from models/ or $PAL_MODELS; $PAL_LLM picks the GGUF)
./build.sh install       # build the arm64 Android APK and install it with adb
./build.sh release       # signed release APK (keystore from GODOT_ANDROID_KEYSTORE_RELEASE_*)
```

A tag `v<version>` matching the version in `rust/Cargo.toml` makes the CI build the signed APK and publish a GitHub
release. It needs the repository secrets `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_ALIAS` and
`ANDROID_KEYSTORE_PASSWORD`.

Pal downloads missing models into its model folder on start, on the Mac and on Android alike (about 2 GB the first
time).

The UI starts in German; English is one tap away.

## Larger models

The settings offer Qwen3.5 2B (default), 4B (2.7 GB, 8 GB RAM or more) and 9B (5.7 GB, 12 GB RAM or more). A larger
model is downloaded only after you choose and save it, and models too large for the device's RAM can't be chosen. If
one doesn't start, Pal goes back to the previous one. Either way, Pal then asks whether to delete the unused model.

## Personalities

Each language has its own personality, chosen in the settings. The built-in character is always first in the list, so
you can always go back to it. **Template** writes it as a text file to edit, **Load file** adds your own. Files live in
`personas/<de|en>/<name>.txt` in the model folder. A language without a file keeps the built-in character.

Only `[system]` is required. Without `[call words]`, the words of the file name call the character. Without
`[joke words]` or `[silence]`, the language's defaults apply. Other sections you leave out stay empty. Each call word
must be a single word of letters or digits:

```text
[system]
You are Rosa, a calm gardener. Talk to {name} in one or two short sentences.

[examples]
I'm tired. | Then sit down for a moment, the tomatoes can wait.

[call words]
rosa
```

The template explains the other sections: `[jokes]`, `[joke intros]`, `[joke words]`, `[silence]` and `[replies]`.

## Fish Audio (optional)

Pal can speak through [Fish Audio](https://fish.audio) with your own account. It is off by default: choose "Fish
Audio (online)" under Voice in the settings, then enter your API key, a voice ID and a model. You can browse voices in Fish's
[voice library](https://fish.audio/app/voice-library/). Pal ships no voice ID. Each sentence Pal says is
then sent to Fish, under Fish's terms and prices, and you are responsible for having the right to use the voice you
choose. The default model is `s2.1-pro-free`; check Fish's current prices, since an unknown model is billed as a paid
one. If Fish fails on a sentence, Pocket TTS says it instead.

> **Only use voices you have the right to use.** Fish's library holds voices uploaded by third parties, and some may
> imitate real people, actors, dubbing artists or characters from film, TV, games and radio plays without consent. That a voice is available there does not
> mean you may use it. Fish Audio's terms do not allow cloning voices from its library, so do not use them as source
> material for `generate_voice`. Use `generate_voice` only with your own voice or with the speaker's explicit consent.

## Voices of one's own

Besides the built-in voices (e.g. `juergen`), Pal loads voices of one's own from

```
<models folder>/pocket/voices/<de|en>/<name>.safetensors
```

They are never downloaded. To add one, press **Load voice** in the settings (key S or "…") and pick a `.safetensors`
file; Pal asks for your confirmation the first time and refuses files that are not a Pocket TTS voice. Alternatively,
`./build.sh install` copies them to the phone once Pal has started there (it makes the folder). Such a file is made from a 10–30 s recording with the separate tool in `generate_voice/` (Pocket TTS's
voice cloning model, see its README).

**Only clone your own voice or the voice of a person who has expressly agreed.** Never use someone else's voice
without their consent: not from films, series, radio plays, podcasts or other recordings, not a voice someone
else has already cloned, and not audio generated with a voice from Fish Audio's library, which would break both
Fish's and Kyutai's terms. Kyutai's terms for the cloning model require consent, and a person's voice is protected by
their personality rights. A consenting speaker does not make a sound-alike safe: imitating a recognizable person's
voice or a protected character can still infringe their rights. Don't pass on recordings or voices (no repository, APK or download; `models/` is git-ignored), and don't use a cloned voice so
that others take it for the real person. This is not legal advice; when in doubt, use only your own voice.

License: MIT OR Apache-2.0. Silero VAD: MIT. Smart Turn: BSD-2-Clause. Parakeet TDT: CC-BY-4.0 (see below). Qwen3.5: Apache-2.0. Pocket TTS code: MIT (check the Kyutai model card for the weights). No GPL.

Speech recognition uses [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) by NVIDIA, licensed
under [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/). Pal uses the int8 ONNX conversion by istupakov
([pinned revision](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/tree/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce),
also CC-BY-4.0) without further changes. The model is not part of this repository or the APK; the app downloads it from
Hugging Face and checks its sha256.
