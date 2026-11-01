# AGENTS.md

Knowledge for working on Pal. The app is called **Pal**; the assistant calls itself **KITT** (persona) and addresses
the user by name (default "Michael", spoken "Maikel"). Rust workspace in `rust/`, Godot app in `godot/`. See
`README.md` for build commands and licenses.

## Crates

| Path | Role |
|---|---|
| `rust/pal-core` | Audio in and out: capture, resampling, echo cancellation, VAD, segmenter, Smart Turn, prosody, playback |
| `rust/pal-brain` | The agent: STT, enrichment, persona, LLM (local or server), mq-bridge reply routes, sentences, TTS |
| `rust/pal-tts` | Kyutai Pocket TTS, copied from kyutai-labs/pocket-tts. Don't reformat or edit except for marked "pal-tts change" spots |
| `rust/pal-godot` | GDExtension `PalNode`: starts the agent, exposes state, voice level and signals |
| `godot/` | UI (`main.gd`, `main.tscn`), settings form (`settings.gd`), KITT/HAL shaders |

## The flow: microphone → reply

Everything runs in `converse` in `pal-brain/src/agent.rs`, a loop on the agent's own thread.

1. **Microphone**: `pal_core::mic_pipeline::Processor::start`. `capture.rs` opens the device (cpal callback only
   downmixes into a lock-free ring). The `pal-audio` worker thread runs resample → echo cancellation (`echo.rs`) →
   Silero VAD (`vad.rs`) → segmenter (`segmenter.rs`) → Smart Turn (`turn.rs`) → prosody (`prosody.rs`).
2. **Events** (`pal-core/src/event.rs`): `SpeechStart`, `Pause(Segment)`, `Segment(Segment)`, `Resume`, `BargeIn`.
   A thread in `converse` forwards them into the `Inbox`, together with the reply routes' `Spoken` reports.
3. **Pause vs. segment**: `Pause` means a short pause; the reply is prepared but held (`early`). `Segment` means the
   user finished. A held reply is released on the segment's end or discarded if the user goes on.
4. **STT**: `stt.transcribe(&seg.pcm)` in `converse` → `pal-brain/src/stt.rs` (`Recognizer`): Parakeet TDT through
   the mq-bridge-onnx custom endpoint (`task: transcribe`), asked with `Publisher::request`. No route. It must load
   before other ONNX models: VAD and Smart Turn share its ONNX Runtime.
5. **Unfinished sentence**: `sounds_unfinished` + `Inbox::goes_on`. The audio is kept in `carry` and put before the
   next segment.
6. **User line**: `Persona::user_line(text, tags)`: prosody tags (`prosody_tags.rs`) + transcript. This line goes
   into the history and the UI (`AgentEvent::Heard`).
7. **Canned replies** (no LLM): `persona.joke`, `persona.quip`, `persona.pooled` in `persona.rs`. Texts in
   `pal-brain/src/language/` (`jokes_*.txt`, `pool_*.txt`, `de.rs`, `en.rs`). `pooled` only matches lines negated
   like the transcript.
8. **Enrichment**: `enrich::prompt_line(text, line, language)` in `pal-brain/src/enrich.rs`. Add lookups in
   `facts()`. The result goes only into this turn's prompt, never into the history. It runs before every LLM reply,
   so its time adds directly to `first_sentence_ms`.
9. **Prompt**: `Persona::messages(line)` = `context()` (system prompt, sample turns, history) + the user line.
10. **Reply**: `routes.reply(turn, messages, early)` → mq-bridge routes (below) → sentences → TTS.
11. **After the turn**: `persona.remember(line, reply)`, then `routes.prefill(persona.context())` warms the LLM cache
    for the next turn.

## mq-bridge

- **Reply routes** (`pal-brain/src/routes.rs`, `ReplyRoutes`): in-process `memory` endpoints
  `pal.llm` → `pal.pieces` → `pal.speech`, routes `pal_llm` (model), `pal_split` (sentence split, `sentences.rs`),
  `pal_speak` (gate and speaker). Each stage is a `ThreadedHandler` on its own thread. Every message carries its turn
  id; `interrupt`/`discard` cancel a turn in all stages. `canned` sends a fixed text through the same path.
- **STT**: see step 4; an endpoint, not a route.
- mq-bridge 1.3.0 (crates.io). Don't change mq-bridge itself. Ideas for later (deferred): `lookup` middleware for
  enrichment, `switch`, embeddings + qdrant.

## LLM ("brain")

- `routes::Brain`: `Local(LocalLlm)` or `Remote { server: RemoteLlm, fallback: Option<LocalLlm> }`, built in
  `converse` from `AgentConfig.remote` and `AgentConfig.fallback`.
- `llm.rs`: `LocalLlm` via llama.cpp (default model Qwen3.5 2B GGUF), trait `Llm`.
- `remote.rs`: own blocking streaming client (ureq 3, rustls + webpki-roots, works on Android). Ollama and OpenAI
  use the OpenAI chat format with SSE (`/chat/completions`); Anthropic uses `/messages` with `x-api-key` and
  `anthropic-version`. The request runs on its own thread; `reply` polls cancel every 50 ms. Timeouts: connect 5 s,
  response 30 s, per read 15 s, no overall timeout (streams may be long).
- The local fallback runs only if the server failed before the first piece and the turn wasn't cancelled. Without
  fallback the local model isn't loaded. A failed reply becomes `AgentEvent::ReplyFailed` → Godot `reply_failed`.

## TTS

`pal-brain/src/tts.rs` (`PocketSpeaker`): Pocket TTS in-process, one thread generates, one owns the playback.
`numbers.rs` writes digits as words, `leveler.rs` evens out loudness, `voice_filter.rs` adds KITT's speaker sound.
Voice: `juergen`.

## Godot

- `PalNode` (`pal-godot/src/lib.rs`): vars `user_name`, `llm_provider` (`local`/`ollama`/`openai`/`anthropic`),
  `llm_url`, `llm_remote_model`, `llm_api_key`, `llm_fallback`, `smart_turn`, `barge_in`. Signals:
  `ready_to_listen`, `heard`, `said`, `turn_done`, `interrupted`, `ignored`, `failed`, `reply_failed`.
- `main.gd`: `_start()` copies settings onto `PalNode`. The "…" label or key S opens the settings form.
- `settings.gd` (`Settings`): saves to `user://settings.cfg` (sections `user`, `llm`); saving restarts Pal. An empty
  URL means the provider default (in `settings.gd` and `remote.rs`). An empty model is replaced by the provider's
  model default from `settings.gd` only; `remote.rs` has no model default and sends the model as given.
- Environment: `PAL_MODELS` (models folder), `PAL_LLM` (GGUF file), `PAL_SMART_TURN=0`, `PAL_BARGE_IN=0`,
  `PAL_RECORD_DIR` (saves each segment as WAV + transcript, for STT test data).

## Build, test, device

- `./build.sh setup | test | godot | android | install | push-models` (see `README.md`).
- Unit tests: `cargo test -p pal-brain --lib`, `cargo test -p pal-core --lib`. Tests needing models or a speaker are
  `#[ignore]`d (`reply_routes` speaks through the speakers and is timing-sensitive).
- Android test phone: Pixel 6a over adb WLAN, `192.168.20.43:5555`. The phone must be unlocked for start checks.
- Format with `cargo fmt -p pal-core -p pal-brain -p pal-godot`. A plain `cargo fmt` also reformats `pal-tts`.

## Rules for changes

- Latency: `first_sentence_ms` (end of speech → first spoken sentence) must not rise noticeably. Measure it with the
  `turn_done` signal or the `llm_pal:` log lines.
- Per-language texts and word lists belong in `pal-brain/src/language/`, not in code.
- Don't call paid APIs (OpenAI, Anthropic) in tests without asking.
