# llm_pal

Lokaler Voice-Audio-Prozessor in Rust (Desktop, später Android über Godot) als Basis für zwei Spaß-Apps:
ein Kommentar-LLM und eine Katze, die nur miaut. Plan, Phasen und mq-bridge-Analyse: `PLAN.md`.

## Stand: Phase 1 (Desktop)
Mikrofon → 16 kHz mono → Silero VAD (ONNX Runtime) → Segmente → Prosodie-Features.

```
rust/pal-core      Bibliothek: capture, resample, vad, segmenter, prosody, pipeline
rust/pal-brain     STT-Lader, LLM (llama.cpp), Prosodie-Tags, Persona, Satz-Streaming, TTS (macOS: AVSpeechSynthesizer im Prozess, `say` als Fallback)
rust/pal-stt       whisper.cpp als Shared Library (eigene ggml-Version, kollidiert sonst mit llama.cpp)
rust/pal-desktop   Testharness: listen | file | devices | comment, optional Feature `bridge` (mq-bridge)
rust/pal-godot     GDExtension: `PalNode` (Agent, Zustand, Pegel der Pal-Stimme, Signale für Texte)
godot/             UI: KITT-Sprachanzeige oder HAL-Auge (Leertaste/Klick schaltet um)
scripts/           fetch-models.sh (Silero, Whisper, Qwen; sha256-geprüft), make-test-audio.sh (macOS `say`), make-hal-panel.py
```

```sh
./build.sh setup                      # Modell + deutsche Test-Aufnahme
./build.sh test                       # alle Tests inkl. Modell-Tests
./build.sh file testdata/de_48k.wav   # → out/seg_0001.wav + .json …
./build.sh listen                     # Mikrofon, Ctrl-C beendet
./build.sh comment                    # Phase 2: sarkastische Kommentare per Sprache (--print, --barge-in)
./build.sh godot                      # dasselbe mit Godot-Display (Modelle aus models/ oder $PAL_MODELS)
./build.sh install                    # Android-APK (arm64) bauen und per adb installieren
./build.sh push-models                # Modelle aufs Handy kopieren (einmalig, ~600 MB)
rust/target/release/pal-desktop file testdata/de_48k.wav \
    --publish '{"file": {"path": "out/segments.jsonl"}}'   # nach ./build.sh desktop
```

Jedes Segment: WAV (16 kHz, 16 bit) und JSON mit `start_ms`/`end_ms`, Pitch-Kontur (10 ms), f0 Median/Min/Max,
Tonhöhen-Steigung am Ende (Frage ≈ steigend), Lautstärke, Silbenrate.

Lizenz: MIT OR Apache-2.0. Silero VAD: MIT. Whisper: MIT. Qwen3.5: Apache-2.0. Kein GPL.
