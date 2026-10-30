# llm_pal – Plan: Audio-Prozessor (Phase 1) + Ausblick

## Context
Neues, leeres Projekt `/Users/marcomengelkoch/bin/llm_pal`. Ziel: wiederverwendbarer lokaler Voice-Audio-Prozessor in Rust
(Desktop + später Android via Godot), darauf zwei Spaß-Apps (Kommentar-LLM, Katze). Jetzt: Crate-Struktur festlegen,
Abhängigkeiten prüfen (erledigt, s. unten), Phase 1 auf dem Desktop umsetzen. Meilenstein: Mikrofon → Segmente als
WAV + Features als JSON.

Entscheidungen (mit dir geklärt): Lizenz **MIT OR Apache-2.0, keine GPL-Abhängigkeiten** (auch nicht indirekt, z. B. espeak-ng);
VAD zuerst **nur mit `ort`** (tract später als Vergleich); **kein mq-bridge-onnx** (Lücken in `../mq-bridge-onnx/docs/AUDIO_GAPS.md`),
mq-bridge selbst ab Segment-Ebene; TTS zuerst **System-TTS**, eigene Stimme später fest eingeplant.
Architektur und Audio-Glue wie `../trippy-synth` und `../glo-mobile` (beide übernehmbar; in glo-mobile ist nur der
Glo-C/C++-Code in `firmware/` und `port/` GPL-3 und bleibt außen vor).

## Geprüfter Stand der Abhängigkeiten (crates.io, 2026-10-30)
| Zweck | Crate / Modell | Version |
|---|---|---|
| Capture | `cpal` | 0.18.2 (wie trippy-synth; Android-Feature `realtime`) |
| SPSC Audio→Worker | `rtrb` | 0.4.1 |
| Resampling | `rubato` | 5.0.1 (+ `audioadapter` 5.0.0) |
| VAD-Inferenz | `ort` | `=2.0.0-rc.15`, `download-binaries` (identisch zu mq-bridge-onnx → später kompatibel) |
| VAD-Modell | Silero VAD v6 `silero_vad.onnx` | Eingang `[1,512]` f32 + State `[2,1,128]` + `sr` i64; Ausgang Prob + `stateN`. Exakte Tensornamen/sha256 beim Download aus snakers4/silero-vad prüfen und pinnen |
| WAV | `hound` | 3.5.1 |
| Events | `crossbeam-channel` | 0.5.17 |
| JSON | `serde` / `serde_json` | 1 / 1.0.154 |
| CLI | `clap` | 4.6.7 |
| Fehler | `thiserror` (core), `anyhow` (desktop) | 2 / 1 |
| Pitch | eigene YIN-Implementierung (~80 Zeilen); `pitch-detection` 0.3 ist seit 2022 tot, `pyin` 1.2 zieht ndarray mit | – |
| Später | `godot` 0.5.5, `whisper-rs` 0.16, `llama-cpp-2` 0.1.160, `piper-rs` 0.2, `fundsp` 0.23, `tract-onnx` 0.23.9, `mq-bridge` 1.2.1, `mq-bridge-onnx` 0.1.1 | – |

## Crate-Struktur (analog trippy-synth)
```
llm_pal/
  rust/Cargo.toml          workspace: pal-core, pal-desktop (pal-godot erst Phase 2)
  rust/pal-core/           Audio-Prozessor-Bibliothek
    src/lib.rs
    src/capture.rs         cpal-Input → rtrb (Input-Teil + input_device() aus trippy-synth/rust/synth-core/src/audio.rs)
    src/resample.rs        beliebige Rate/Kanäle → 16 kHz mono (rubato, Blöcke fester Größe)
    src/vad.rs             Silero über ort; State zwischen 512-Sample-Chunks weiterreichen; reset()
    src/segmenter.rs       Hysterese-Zustandsmaschine (rein, ohne Modell testbar)
    src/prosody.rs         YIN-Pitch-Kontur, RMS-dB, Dauer, Tempo
    src/pipeline.rs        Worker-Thread: Ring → Resample → VAD → Segmenter → Prosodie → Events
    src/event.rs           Event { Segment(Segment), SpeechStart, BargeIn }, Segment, Features (serde)
  rust/pal-desktop/        Testharness (CLI)
    src/main.rs            listen | file | (später bench)
  scripts/fetch-models.sh  lädt silero_vad.onnx nach models/, prüft sha256
  build.sh                 desktop | test (android/run später wie trippy-synth/build.sh)
  models/                  (gitignored)
```
Android-Teile aus trippy-synth (`android.rs` JNI-Context, `realtime`-Feature) kommen erst mit pal-godot dazu.

## Datenfluss / Threads
1. **cpal-Input-Callback** (keine Allokation/Locks): Frames → Mono-Downmix → `rtrb::Producer<f32>`; Überlauf zählt nur einen Atomic-Zähler hoch.
2. **Worker-Thread** (`pipeline.rs`): liest Ring, resampelt auf 16 kHz, schneidet 512er-Chunks (32 ms),
   VAD-Prob pro Chunk → Segmenter → bei Segment-Ende Prosodie berechnen → `crossbeam_channel::Sender<Event>`.
3. **Konsument** (Desktop-Main bzw. später App): schreibt WAV/JSON bzw. füttert STT.

Öffentliche API (pal-core):
```rust
pub struct ProcessorConfig { vad_model: PathBuf, seg: SegmenterConfig, device: Option<String> }
pub struct Processor { /* Stream + Worker-Handle */ }
impl Processor {
    pub fn start(cfg) -> Result<(Self, Receiver<Event>), Error>;   // Live-Mikrofon
    pub fn output_active(&self) -> Arc<AtomicBool>;                // für Barge-in, setzt die TTS-Ausgabe
}
pub fn process_samples(cfg, samples: &[f32], rate: u32) -> Result<Vec<Event>, Error>; // offline, deterministisch
```

## Segmenter (Defaults, alle konfigurierbar)
- Sprache an bei Prob ≥ 0.5, aus bei < 0.35 (Hysterese).
- Pre-Roll 250 ms (Ringpuffer der letzten Chunks), Ende nach 600 ms Stille, Mindestlänge 250 ms (sonst verwerfen).
- Max-Länge 28 s → erzwungener Schnitt (Whisper-Fenster < 30 s), nächstes Segment beginnt direkt.
- Timestamps: Sample-Zähler seit Start → `start_ms`/`end_ms` (monoton, unabhängig von Wall-Clock).

## Prosodie-Features (pro Segment)
- Pitch: YIN, Fenster 40 ms, Hop 10 ms, f0 70–500 Hz, Schwelle 0.15; Kontur = f0 pro 10 ms (`None` = stimmlos).
- Abgeleitet: `f0_median/min/max` (Hz), `f0_slope_st_per_s` (lineare Regression in Halbtönen über die letzten ~500 ms → Frage/Aussage), `voiced_ratio`.
- Lautstärke: RMS-dB pro Frame, `rms_mean_db`, `rms_max_db`.
- `duration_s`; Tempo: `syllable_rate` = Peaks der geglätteten Energiehüllkurve pro Sekunde (Silbenkern-Näherung).
- Segment = `{ id, start_ms, end_ms, pcm: Vec<f32> (16 kHz), features }`; JSON enthält alles außer PCM.

## Barge-in
`output_active: Arc<AtomicBool>` wird von der späteren Ausgabe gesetzt. Beginnt der Segmenter Sprache, während das Flag
gesetzt ist → `Event::BargeIn` (zusätzlich zu `SpeechStart`). Phase 1: im Desktop per `--simulate-output` testbar.
Echo der eigenen Ausgabe (ohne AEC) ist bekanntes Risiko → später Kopfhörer/Schwellwert-Anhebung während Ausgabe.

## pal-desktop (Meilenstein)
- `pal-desktop listen --out out/ [--device NAME] [--simulate-output]` → pro Segment `out/seg_0001.wav` (16 kHz mono, 16-bit) + `seg_0001.json`; Konsole: eine Zeile pro Event.
- `pal-desktop file input.wav --out out/` → gleicher Output offline (für Tests/Regression).

## Umsetzungsschritte (je mit Verifikation)
1. Workspace, Crates, `fetch-models.sh`, Lizenzdateien → `cargo check` grün.
2. `resample.rs` → Test: 48 kHz-Sinus 440 Hz → 16 kHz, Länge ±1 Block, Frequenz per Nulldurchgängen ±1 %.
3. `prosody.rs` (YIN) → Tests: Sinus 120/220/400 Hz ±1 %; Rauschen → überwiegend stimmlos; steigender Chirp → positive Slope.
4. `segmenter.rs` → Tests mit synthetischen Prob-Folgen: Pre-Roll-Länge, Ende nach Stille, Max-Länge-Split, zu kurze Bursts verworfen, Barge-in nur bei gesetztem Flag.
5. `vad.rs` (ort) → Test (ignored ohne Modell): Stille ≈ 0, Sprach-WAV > 0.5 in der Mitte; State wird weitergereicht.
6. `pipeline.rs` + `process_samples` + `file`-Kommando → Integration: deutsche Test-WAV (`say -v Anna` + `afconvert` auf 16 kHz), erwartete Segmentanzahl bei Sätzen mit Pausen.
7. `capture.rs` + `listen` → manuell: ins Mikro sprechen, WAVs anhören, JSON plausibel (Frage endet mit positiver Slope).
8. Feature `bridge`: Segmente über mq-bridge veröffentlichen (API vorher per context7/Quelle `../mq-bridge` prüfen) →
   Test mit `memory`-Endpoint: N Segmente rein → N Nachrichten mit korrekten Metadaten raus; manuell mit `file`-Endpoint.
9. `cargo clippy` einmal am Ende.

## Analyse: Wie flexibel ist mq-bridge für llm_pal, und was fehlt?
Geprüft in `../mq-bridge` (docs/EXTENDING.md, docs/REFERENCE.md, `src/traits.rs`, `src/canonical_message.rs`,
`src/endpoints/http/stream.rs`) und `../mq-bridge-onnx` (`src/tensor.rs`, `src/config.rs`, README).

### Was schon passt (ohne Änderung)
| Bedarf in llm_pal | mq-bridge heute |
|---|---|
| Segment als Nachricht (PCM + Features) | `CanonicalMessage { payload: Bytes, metadata: HashMap<String,String> }` → PCM roh als Payload, `sample_rate`/`format`/Features-JSON in Metadaten, oder JSON-Payload + PCM separat |
| In-Process-Verbindung | `memory`-Endpoint, auch Request/Reply (`request_reply: true`) |
| Großes LLM auf dem Heimserver | `http`-Endpoint; `lookup` hängt die Antwort an die Nachricht |
| Token-Streaming vom Server (für satzweise TTS) | `http` erkennt `text/event-stream` (SSE) und schreibt per `stream_response_to` in einen `stream_buffer`, je Anfrage eine eigene Partition (Korrelations-ID) |
| Hybrid-Routing | `switch` (nach Metadaten/Ausdruck), `timeout` + `retry` + `dlq` |
| Logs/Kontext in SQLite, Parquet, Dateien | `sqlx` (Lookup-Query für Gesprächskontext), `parquet`/`object-store`, `file` |
| Kontext pro Sprecher/Sitzung | `aggregate` (`single_writer` im Speicher oder mit SQLite-Store), `weak_join` |
| Eigene Bausteine | `CustomEndpointFactory` / `CustomMiddlewareFactory`, im eigenen Crate registriert, ohne mq-bridge zu ändern |

### Was fehlt bzw. unpassend ist
1. **Zustandsbehaftete n:m-Stromverarbeitung** (VAD + Segmenter: viele 32-ms-Chunks rein, 0..1 Segmente raus).
   `Handler::handle` gibt pro Nachricht genau 0 oder 1 Nachricht zurück (`Handled::Ack`/`Publish`); `aggregate` schreibt den
   State 1:1 in jede Nachricht. Ohne Core-Änderung geht es als **eigene Input-Middleware** in llm_pal (wrappt den Consumer,
   puffert Chunks, gibt fertige Segmente aus), mit Route-`concurrency: 1` für die Reihenfolge.
   *Kandidat für mq-bridge:* ein generischer „stateful window/session“-Baustein (Schlüssel, State, Emit-Bedingung, n:m).
2. **Fallback-Endpoint** „Server, sonst lokal“: geht heute nur mit `timeout`/`retry` + `dlq` auf das lokale Modell.
   Das funktioniert, ist aber für den Fallback zweckentfremdet und kostet bei jedem Ausfall die volle Timeout-Zeit.
   *Kandidat:* strukturelles `fallback: [primary, secondary]` mit Health-Status (Primary nach Fehler für X s überspringen).
3. **Abbruch pro Anfrage (Barge-in)**: Spricht der Nutzer dazwischen, müssen laufende LLM-/TTS-Anfragen sofort enden.
   Ich habe Abbruch nur auf Route-Ebene gefunden, nicht pro Korrelations-ID.
   *Kandidat:* Abbruch-Signal pro Korrelations-ID (bricht HTTP-Request und `stream_buffer`-Partition ab).
4. **Streaming aus eigenen Endpoints**: `stream_response_to` ist an `http` gebunden. Ein lokales LLM als Custom-Endpoint
   bräuchte denselben Mechanismus (Tokens → `stream_buffer`). *Kandidat:* öffentliche API, mit der ein Custom-Publisher
   in einen `stream_buffer` streamt (falls nicht schon über `Publisher` möglich: beim Bau prüfen).
5. **Keine lokalen KI-Endpoints**: STT (whisper), LLM (llama.cpp), TTS gibt es nicht. Gehört nicht in den Core, sondern
   als Custom-Endpoints in llm_pal (`pal-bridge`) oder später eigene Plugin-Crates wie mq-bridge-onnx.
6. **Android**: mq-bridge braucht tokio (`rt-multi-thread`, `net`, `fs`, `signal`), das läuft unter Android. Build mit
   `default-features = false` per `cargo ndk` und die Binärgröße sind ungetestet, bitte messen.

### mq-bridge-onnx: was fehlt für Audio
Heute: Task `tensor` = **ein** Modell-Eingang, Nachrichten werden zu Batches gestapelt, Antwort = gewählte Ausgänge.
Unterschiedliche freie Längen laufen in getrennten Batches (gut für ganze Segmente).
1. **Mehrere Eingänge**: `inputs: { input: <tensor aus Payload>, state: <…>, sr: <Konstante> }` statt nur `input_name`.
2. **Konstante Eingänge** (z. B. `sr` als int64-Skalar 16000).
3. **State zwischen Aufrufen**: `state: { input: "state", output: "stateN", key: "${metadata:stream_id}", init: zeros }`:
   Ausgang wird pro Schlüssel gespeichert und beim nächsten Aufruf eingesetzt; erzwingt Batch 1 und Reihenfolge je Schlüssel.
   Damit liefe Silero VAD in mq-bridge-onnx (pro Chunk eine Prob-Antwort).
4. **Audio-Decoder in der Middleware**: `decode: pcm` (f32le/i16le + `sample_rate` aus Metadaten) und `decode: wav` → Tensor `[1, N]`.
5. **Android-Build von ort** (`download-binaries`): ob es dafür fertige Bibliotheken gibt, ist ungeklärt, bitte prüfen.
Nicht empfohlen: Whisper/LLM-Generierung (Decoder-Schleife, Tokenizer, KV-Cache) in mq-bridge-onnx. Dafür whisper.cpp/llama.cpp.

Mit 1–4 wäre mq-bridge-onnx sofort nützlich für: Silero VAD, Klassifikation ganzer Segmente (Emotion, Sprecher-Embedding),
Text-Embeddings der Transkripte als Gedächtnis (`task: embeddings`, gibt es schon). Gleicher ort-Pin (`=2.0.0-rc.15`) wie pal-core.

### Empfehlung
- Phase 1: Echtzeit-Pfad mit rtrb + Thread (wie geplant), dazu Segmente über mq-bridge veröffentlichen (Feature `bridge`, unten).
- Ab Phase 2: mq-bridge als Pipeline ab Segment-Ebene (STT → LLM lokal/Server → TTS; Logs in SQLite/Parquet).
  Lücken 2–4 zuerst als Custom-Bausteine in llm_pal bauen; was sich bewährt, nach mq-bridge übernehmen.
- mq-bridge-onnx-Erweiterungen 1–4 als eigenes Vorhaben in `../mq-bridge-onnx`; danach den VAD-Pfad dagegen messen.

**In Phase 1 konkret**: Feature `bridge` in pal-desktop. Jedes Segment wird als mq-bridge-Nachricht veröffentlicht
(Payload = Feature-JSON, PCM als Datei-Pfad oder base64; Metadaten = id/start/end). Ausgang per Endpoint-JSON konfigurierbar,
z. B. `{"file": {...}}` als Ersatz für das direkte WAV/JSON-Schreiben oder `memory` für Tests. mq-bridge mit
`default-features = false` + nur benötigten Features (Android-tauglich halten). Danach kurze Notiz: Aufwand, Latenz, Build-Größe.

## Ausblick (nicht Teil dieses Schritts)
- **tract-Vergleich**: zweites VAD-Backend hinter Feature, Bench-Kommando; entscheidet auch, ob Android ohne ORT-Binary geht (ort hat fertige Android-Pakete, siehe „Android: geprüfte Fakten“).
- **Phase 2**: whisper-rs (whisper.cpp, MIT), llama-cpp-2 (MIT), TTS. **Lizenzrisiko**: Piper phonemisiert mit espeak-ng (GPL-3) → vor Phase 2 GPL-freie Alternative prüfen (piper-rs-Phonemizer, andere TTS).
- **Phase 3**: llama.cpp GBNF-Grammar für `{stimmung, laute[], kontur}`, Katzen-Synth mit fundsp, pal-godot + Godot-Szene.

## Hinweise
- Kein git-Repo vorhanden; ich committe nichts.
- ort lädt beim ersten Build ONNX Runtime von pykes CDN (wie mq-bridge-onnx).

## Ergebnis Phase 1 (2026-10-30)
- Alle Schritte 1–9 umgesetzt; 16 Unit-Tests + Ende-zu-Ende-Test (deutsche `say`-Aufnahme, 48 kHz) + Bridge-Test grün, clippy sauber.
- Ende-zu-Ende: 3 Sätze → 3 Segmente; Aussagen mit fallender (−19/−26 st/s), Frage mit steigender Tonhöhe (+3,8 st/s).
- Silero v6 braucht 64 Samples Kontext vor jedem 512er-Chunk (Eingang 576); in `vad.rs` umgesetzt.
- mq-bridge (Feature `bridge`, nur `file`): Release-Binary 29,2 MB → 37,0 MB (+7,9 MB, ungestrippt; ONNX Runtime
  statisch ist der Großteil der 29 MB). Einbau: ein Modul mit ~30 Zeilen (`Publisher::from_config` + `send`).
  Publish-Latenz nicht gemessen. Android-Build noch offen.

## Phase 2: Kommentar-LLM (Plan)
Modelle (Lizenzen geprüft 2026-10-30):
- STT: whisper.cpp über `whisper-rs` 0.16 (Unlicense / whisper.cpp MIT), `ggml-base-q8_0.bin` (MIT), Option `small`.
- LLM: llama.cpp über `llama-cpp-2` 0.1.160 (MIT/Apache), `Qwen3.5-0.8B-Q4_K_M.gguf` (Apache-2.0). Vergleich: Gemma 4 E2B (Apache-2.0).
- TTS: System-TTS. Desktop: AVSpeechSynthesizer im Prozess (erster Ton ~0.3 s nach dem Satz; `say` brauchte 2–5 s pro Satz), Ausgabe über pal-core `playback` (liefert den Pegel fürs UI); `--say` als Fallback. Der Haupt-Thread muss die macOS-Run-Loop laufen lassen (die Puffer kommen über die Main-Queue). Android: Godot `DisplayServer.tts_speak`.
  Ausgeschlossen: Piper/`piper-rs` (bindet espeak-ng, GPL-3).

Crate `rust/pal-brain`:
- `stt.rs`: Segment-PCM (16 kHz) → Text, Sprache fest `de`.
- `llm.rs`: Trait `Llm { fn reply(&mut self, prompt, cancel, on_token) }`; `LocalLlm` (llama.cpp, Token-Streaming, Abbruch per AtomicBool);
  `RemoteLlm` über mq-bridge `http` (OpenAI-kompatibler Server), Fallback auf lokal bei Fehler/Timeout.
- `prosody_tags.rs`: Features relativ zur laufenden Sprecher-Baseline → Tags wie `[laut] [schnell] [Frage] [hohe Stimme]`.
- `persona.rs`: System-Prompt (kurz, schräg, sarkastisch, harmlos), kurzer Gesprächsverlauf.
- `sentences.rs`: Token-Strom → ganze Sätze für satzweise TTS.
- `speaker.rs`: Trait `Speaker` (sprechen, abbrechen, spricht gerade?); `SayTts` (macOS), `PrintTts` (Fallback).

pal-desktop `comment`: Mikrofon → Segment → STT → LLM → Sätze → TTS. `output_active` während der Ausgabe.
Standard halbduplex (Segmente, die während der Ausgabe beginnen, werden verworfen, sonst hört sich das LLM selbst);
mit `--barge-in` (Kopfhörer): Nutzer unterbricht → Generierung und TTS werden abgebrochen.
Optional `--log '<mq-bridge Endpoint>'`: jede Runde (Transkript, Tags, Antwort, Latenzen) z. B. nach SQLite/Datei.

Schritte (je mit Verifikation):
1. `fetch-models.sh` um Whisper + Qwen erweitern (sha256 gepinnt).
2. `sentences.rs`, `prosody_tags.rs` → Unit-Tests.
3. `stt.rs` → Test: `testdata/de_48k.wav`-Segmente werden zu erkennbarem deutschem Text (Schlüsselwörter).
4. `llm.rs` lokal → Test: kurze deutsche Antwort, Abbruch stoppt innerhalb weniger Tokens; Latenz messen.
5. `speaker.rs` + `comment`-Befehl → manuell; Latenz Segment-Ende → erster Ton protokollieren.
6. `RemoteLlm` über mq-bridge + Fallback → Test mit lokalem Fake-HTTP-Server bzw. nicht erreichbarer URL.
7. Später: pal-godot + Android (cargo-ndk für whisper.cpp/llama.cpp), Speicher auf 6-GB-Gerät messen.

### Stand Phase 2 (2026-10-30): Schritte 1–5 erledigt
- Modelle in `fetch-models.sh` (sha256 gepinnt): `ggml-base-q8_0.bin`, `Qwen3.5-0.8B-Q4_K_M.gguf` (unsloth; ggml-org hat kein Q4_K_M), optional `WHISPER_SMALL=1`.
- **ggml-Konflikt**: whisper-rs-sys 0.15 bündelt ggml 0.9.5, llama-cpp-sys-2 0.1.160 ggml 0.26.0. Statisch in einem Binary
  nimmt der Linker whisper's Symbole, llama.cpp stürzt ab (SIGABRT in `llama_new_context_with_model`).
  Lösung: Crate `pal-stt` als `cdylib` (exportiert nur `pal_stt_new/transcribe/free/free_string`, ggml bleibt intern),
  `pal-brain::stt` lädt sie per `libloading` (ISC). Auf Android: `libpal_stt.so` mit ins APK. Kann entfallen, sobald
  beide Crates dieselbe ggml-Version bündeln.
- Messwerte M1 (llama.cpp nutzt hier Metal, Android wird CPU): Whisper base 0.4–0.7 s pro Segment; Qwen erstes Token
  ~370 ms (erster Aufruf ~2 s Warmlauf), Antwort 0.6–0.7 s; live erster Satz ~0.9–1.0 s nach Segment-Ende (+600 ms Stille).
- Beobachtungen: Qwen wiederholt die Prosodie-Tags → `Sentences` verwirft Text in `[...]`. Whisper halluziniert bei
  Stille ("* Musik *", "Ich bin hier.") → Pegel-Gate (−50 dBFS), `suppress_nst`, Anmerkungen in `*…*`/`[…]`/`(…)` entfernt.
- Offen: leere Antworten (nur Tags) abfangen; ggml/Metal-Logs auf stderr; Barge-in bricht die laufende Generierung
  noch nicht ab (nur TTS), da sie im Hauptthread läuft (0.7 s).
- **Idee für später – schon während des Sprechens verarbeiten**: Heute starten STT und LLM erst nach 600 ms Stille. Menschen
  verstehen schon beim Zuhören und warten die Pause nur ab, um selbst zu reden. Ansatz: Whisper läuft fortlaufend auf dem
  wachsenden Segment (z. B. alle ~1 s, nur der neue Teil plus etwas Kontext); bei einer kurzen Pause (~250 ms) startet
  das LLM spekulativ. Die Antwort wird erst nach der vollen Stille ausgegeben und verworfen, wenn noch weitergesprochen
  wird. Spart grob STT-Zeit + Teil der Stille-Wartezeit (~0.5–0.8 s).

### UI (2026-10-30): Godot, Desktop
- `pal-brain::agent`: Gesprächsschleife im eigenen Thread; Zustand (lädt/hört zu/denkt/spricht) und Pegel lock-frei,
  Texte als Ereignisse. `comment` und `pal-godot` nutzen beide den Agent.
- `pal-godot` (`PalNode`): `start(model_dir, stt_library)`, `state()`, `level()`, Signale `heard`/`said`/`turn_done`/`failed`.
  libpal_stt ist als GDExtension-Abhängigkeit eingetragen.
- `godot/`: KITT (drei Balkensäulen der Mittelkonsole, leuchten von der Mitte nach oben und unten; Glühlampen-Nachglühen
  pro Segment in `main.gd`) und HAL (Auge im glänzenden 70er-Stil auf Schwarz, Bild aus `scripts/make-hal-panel.py`; der Shader zeichnet
  nur das rote Licht). Retro-Look: warmes Glühlampenlicht, Vignette, Filmkorn. Reagiert nur auf Pals Stimme.
- Später: Mikrofon-Licht, Untertitel-Terminal (Signale sind schon da, heute nur `print`), Android (Mikrofon-Freigabe wie
  in `../glo-mobile/godot/main.gd`, TTS über Godot).

### mq-bridge-onnx (0.1.2 geprüft, 2026-10-30)
Deckt die Audio-Gaps ab (mehrere Eingänge, Konstanten, State über den Aufrufer, `decode: pcm`/`wav`). Entscheidung:
**nicht verwenden.** Für den VAD-Takt (32 ms) wäre es langsamer und schwerer (JSON-State pro Chunk, tokio, tokenizers,
image) als das direkte ort in `pal-core`. Höchstens später für Segment-Klassifikation oder Embeddings, falls die
Kette ohnehin über mq-bridge läuft.

### Android: geprüfte Fakten (ort-sys 2.0.0-rc.15, Quelle gelesen)
- Fertige ONNX Runtime 1.31 gibt es für `aarch64-linux-android` und `x86_64-linux-android`, nur als Variante `nnapi`.
  Das Feature `nnapi` muss nicht an sein: ort nimmt ein Paket, das alle aktivierten Features enthält (auch mehr).
  `pal-core/Cargo.toml` bleibt also voraussichtlich unverändert. Ausweg bei Problemen: `ORT_LIB_LOCATION`.
- ort, whisper.cpp und llama.cpp linken auf Android `c++_shared`. Godots Export-Vorlage bringt `libc++_shared.so` schon
  mit (eine eigene Kopie gibt einen doppelten ZIP-Eintrag, das Signieren scheitert); sie hat alle nötigen Symbole.
- Modelle (Silero, Whisper, Qwen) müssen als Dateien auf dem Gerät liegen: beim ersten Start aus den Assets kopieren
  oder herunterladen.
- whisper-rs-sys 0.15 linkt `ggml-blas`, sobald der Build-Rechner macOS ist (`cfg!` im build.rs prüft den Host).
  Für Android baut `build.sh` deshalb eine leere `libggml-blas.a`. llama-cpp-sys-2 braucht zusätzlich `ANDROID_NDK`.

### Android: Stand (2026-10-30)
- `./build.sh android` baut das Debug-APK (73 MB, ohne Modelle), `install` installiert es, `push-models` kopiert
  `models/` (~600 MB) nach `/sdcard/Android/data/io.github.marcomq.llmpal/files/models`.
- Mikrofon: JNI-Kontext für cpal aus trippy-synth (`pal-core/src/android.rs`); Freigabe in `main.gd`.
- Sprachausgabe: Godot-TTS (`Voice::External`); der Agent zählt einen Satz als Ausgabe, bis Godot meldet, dass es
  spricht (0,8 s Vorlauf). Godot liefert keinen Pegel, KITT/HAL bekommen beim Sprechen einen silbenartigen Ersatz.
- Läuft auf dem Pixel 6a (~1 GB Speicher). Offen: App-Icon, Latenz nach dem dotprod-Build messen.

### Sprache Deutsch/Englisch (2026-10-30)
- `pal-brain::language::Language`; Whisper per `pal_stt_set_language`, englischer Persona-Prompt, englische Tags,
  Stimme je Sprache. In Godot: Tippen auf DE/EN oben rechts (Taste L) schaltet um und startet Pal neu.

### Latenz (2026-10-30)
- Vorher auf dem Pixel 6a ~2,8 s vom Segment-Ende bis zum ersten Satz, dazu 0,6 s Warten auf Stille.
- LLM: `LlmSession` behält den Kontext über die Runden und liest nur den neuen Teil (Runde 2+: ~25–45 statt ~180
  Token). Qwen3.5 hat rekurrente Schichten, deren Speicher sich nicht an beliebiger Stelle kürzen lässt; deshalb
  Checkpoints (`state_seq_get`, `PARTIAL_ONLY`) nach dem System-Prompt und nach dem letzten Prompt.
- Whisper: `audio_ctx` nach Clip-Länge statt immer 30 s, am Mac ~350 → ~90 ms bei gleichem Text.
- Vorziehen: Segmenter meldet nach 250 ms Stille `Pause` (bzw. `Resume`, wenn weitergesprochen wird); der Agent
  rechnet schon los und spricht erst, wenn das Segment nach 600 ms endet. Spart ~350 ms.
- Log in Godot: „Text nach X ms, erster Satz nach Y ms“. Neue Werte auf dem Gerät noch messen.

### Einheitliche Stimme (Wunsch, noch offen)
- Die Google-Stimme auf Android klingt besser als die macOS-Stimme. Ziel: auf allen Plattformen dieselbe, eigene
  TTS-Stimme, GPL-frei (Piper und Kitten/Kokoro hängen an espeak-ng, GPL-3).
- Kandidaten (beide ONNX, also über `ort`, beide ohne espeak):
  - Supertonic 3 (Supertone): 31 Sprachen inkl. de/en, ~99 M Parameter, ohne G2P, offizielles Rust-Beispiel.
    Code MIT, Gewichte OpenRAIL-M (Nutzungsauflagen, z. B. keine Imitation, KI-Ausgabe kenntlich machen; kein Copyleft).
  - Kyutai Pocket TTS: 6 Sprachen inkl. de/en, CPU-schnell. Code MIT, Gewichte CC BY 4.0 (Namensnennung);
    ONNX-Exporte stammen von Dritten.
- Nächster Schritt: Hörproben vergleichen, dann einen Kandidaten als `Speaker` in pal-brain einbauen (Desktop zuerst).
