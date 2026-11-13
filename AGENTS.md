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

**Barge-in** (`AgentConfig.barge_in`): `BargeIn` only pauses Pal's voice (`PocketSpeaker::pause`). A running reply
is cancelled once the speech yields a `Pause` or `Segment`. Words in its transcript stop the voice and cut the last
reply in the history to the sentences that began to play (`PocketSpeaker::heard`, `Persona::cut_last_reply`). No
words, or room talk in party mode, or `RESUME_AFTER` of quiet without a segment: the voice goes on.

**Party mode** (`AgentConfig.party`, `pal-brain/src/party.rs`): after step 6 a line that neither calls Pal
(`call_words`, e.g. "KITT") nor asks for a joke is only kept as room talk. After 5 s of silence following such talk,
Pal says a quip or pool reply to one of its lines, or else the model comments on it (`silence_prompt`); once per talk.

## mq-bridge

- **Reply routes** (`pal-brain/src/routes.rs`, `ReplyRoutes`): in-process `memory` endpoints
  `pal.llm` → `pal.pieces` → `pal.speech`, routes `pal_llm` (model), `pal_split` (sentence split, `sentences.rs`),
  `pal_speak` (gate and speaker). Each stage is a `ThreadedHandler` on its own thread. Every message carries its turn
  id; `interrupt`/`discard` cancel a turn in all stages. `canned` sends a fixed text through the same path.
- **Tokens** on `pal.pieces` use mq-bridge's `stream_to` format for every source (server, local model, canned):
  `correlation_id` = turn, `mqb.stream.index`, `mqb.stream.end`; the last token's `mqb.stream.status` is
  `complete`, `error` (payload = error text) or `cancelled`.
- **STT**: see step 4; an endpoint, not a route.
- mq-bridge 1.5.0 from the local checkout `../mq-bridge` (`[patch.crates-io]` in `rust/Cargo.toml`, also used by
  mq-bridge-onnx). Don't change mq-bridge itself. Ideas for later (deferred): `lookup` middleware for
  enrichment, `switch`, embeddings + qdrant.

## LLM ("brain")

- `routes::Brain`: `Local(LocalLlm)` or `Remote { server: RemoteConfig, fallback: Option<LocalLlm> }`, built in
  `converse` from `AgentConfig.remote` and `AgentConfig.fallback`.
- `llm.rs`: `LocalLlm` via llama.cpp (default model Qwen3.5 2B GGUF), trait `Llm`. The model stage's thread runs
  at nice 10 (`run_in_background`, Linux/Android); llama.cpp's workers inherit it, so TTS and audio come first.
- Thread counts for llama.cpp, Pocket TTS (xn), Parakeet and Smart Turn: `pal_core::cpu::fast_cores()`.
- `remote.rs`: builds the mq-bridge endpoint: `openai_chat` with `messages_from_payload` for Ollama and OpenAI,
  the streaming `http_bulk` recipe for Anthropic's `/messages`. Both stream to `pal.llm.tokens` (`stream_to`) and
  stop on `pal.llm.cancel` (`cancel_from`, sent by `ReplyRoutes::cancel`). The model stage passes the tokens on to
  `pal.pieces`. Timeout: 30 s per silence of the server, no overall timeout (streams may be long).
- Android: reqwest's platform verifier is not initialised there, so `remote.rs` sets `tls.ca_file = "$.bundled"`
  (mq-bridge's built-in Mozilla roots) for every server.
- The local fallback runs only if the server's last token is `error` at index 0 (no text) and the turn wasn't
  cancelled. Without fallback the local model isn't loaded. A failed reply becomes `AgentEvent::ReplyFailed` → Godot `reply_failed`.

## TTS

`pal-brain/src/tts.rs` (`PocketSpeaker`): Pocket TTS in-process, one thread generates, one owns the playback.
Weights are quantized to q8_0 at load (`pal-tts/src/lib.rs`, `type Backend`): 2.4× faster than f32 on the Mac.
`numbers.rs` writes digits as words, `leveler.rs` evens out loudness, `voice_filter.rs` adds KITT's speaker sound.
Voice per language from the settings (`Settings.VOICES`, default `juergen`); `push-models` copies the same list.

## Godot

- `PalNode` (`pal-godot/src/lib.rs`): vars `user_name`, `llm_provider` (`local`/`ollama`/`openai`/`anthropic`),
  `llm_url`, `llm_remote_model`, `llm_api_key`, `llm_fallback`, `smart_turn`, `barge_in`, `party_mode`. Signals:
  `ready_to_listen`, `heard`, `said`, `turn_done`, `interrupted`, `ignored`, `failed`, `reply_failed`.
- `main.gd`: `_start()` copies settings onto `PalNode`. The "…" label or key S opens the settings form; the P0/P1
  label or key P switches party mode (`user/party`).
- `settings.gd` (`Settings`): saves to `user://settings.cfg` (sections `user` incl. `party`, `voice`, `llm`); saving restarts Pal. An empty
  URL means the provider default (in `settings.gd` and `remote.rs`). An empty model is replaced by the provider's
  model default from `settings.gd` only; `remote.rs` has no model default and sends the model as given.
- Environment: `PAL_MODELS` (models folder), `PAL_LLM` (GGUF file), `PAL_SMART_TURN=0`, `PAL_BARGE_IN=0`,
  `PAL_RECORD_DIR` (saves each segment as WAV + transcript, for STT test data).

## Build, test, device

- `./build.sh setup | test | godot | android | install | push-models` (see `README.md`).
- Everything builds in release (one build cache with `build.sh`): `cargo check --release`, unit tests
  `cargo test --release -p pal-brain --lib`, `cargo test --release -p pal-core --lib`. Tests needing models or a
  speaker are `#[ignore]`d (`reply_routes` speaks through the speakers and is timing-sensitive).
- Android test phone: Pixel 6a over adb WLAN, `192.168.20.43:5555`. The phone must be unlocked for start checks.
- Format with `cargo fmt -p pal-core -p pal-brain -p pal-godot`. A plain `cargo fmt` also reformats `pal-tts`.

## Rules for changes

- Latency: `first_sentence_ms` (end of speech → first spoken sentence) must not rise noticeably. Measure it with the
  `turn_done` signal or the `llm_pal:` log lines.
- Per-language texts and word lists belong in `pal-brain/src/language/`, not in code.
- Don't call paid APIs (OpenAI, Anthropic) in tests without asking.
