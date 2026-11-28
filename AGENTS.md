# AGENTS.md

Knowledge for working on Pal. The app is called **Pal**; the assistant calls itself **KITT** (persona) and addresses
the user by name (default "Michael", spoken "Maikel"). Rust workspace in `rust/`, Godot app in `godot/`. See
`README.md` for build commands and licenses.

## Crates

| Path | Role |
|---|---|
| `rust/pal` | The app, one crate. `godot.rs` `PalNode` (entry), `conversation/` the main loop, `hear/` microphone → text, `think/` persona and LLM, `speak/` text → sound, `transport.rs` the mq-bridge connections, `downloader.rs` models, `platform/` Android vs. desktop |
| `rust/pal-tts` | Kyutai Pocket TTS, copied from kyutai-labs/pocket-tts. Don't reformat or edit except for marked "pal-tts change" spots |
| `scripts/llm_bench` | Separate crate `llm-bench`, not in the workspace or the APK, no Pal dependency: benchmark of local GGUF models through llama.cpp. Manifests, results and report scripts in the same folder |
| `godot/` | UI (`main.gd`, `main.tscn`), settings form (`settings.gd`), KITT shader |
| `generate_voice` | Separate crate `pal-voice`, not in the workspace or the APK: makes a voice of one's own from a recording. Needs Kyutai's gated cloning model |

Paths below are relative to `rust/pal/src`. `platform/desktop.rs` is empty; `platform/android.rs` has the same three
functions (`init_audio`, `init_log`, `background_thread`). No `cfg(target_os)` outside `platform/`.

## The flow: microphone → reply

`PalNode::start` (`godot.rs`) → `Agent::start` spawns the agent's thread (`run`): `load` loads the parts, then
`transport::deploy` connects them: input routes, reply routes, the conversation last (read it beside the diagram
in `transport.rs`). The handler of `pal_conversation` is `Conversation::handle`, the main loop: one `Input` from
`pal.inbox` at a time (`None` = tick), a `match` over the `Phase` (`Prefilling`, `Listening`, `Unfinished`,
`Replying`). An error from `handle` (`stop`, or `Event::Lost` from a lost microphone) ends it: the handler drops the
conversation and with it the reply routes, the agent's thread stops the input side (`stop_inbox`). One turn (`cue` → `turn` → `answer` → `start`, then `follow` per input until `done`) is in
`conversation/turn.rs`. No function waits for input of its own.

0. **Models**: `downloader::ensure` fetches what is missing into the model folder (sha256-checked), on every
   platform. Pocket's files go to `pocket/`, read through `pal_tts::use_local_files`.
1. **Microphone**: the mq-bridge custom input endpoint `pal_mic` (`hear/mic.rs`, config `MicConfig`, shared flags and
   echo reference in `MicLink`). `capture.rs` opens the device (cpal callback only
   downmixes into a lock-free ring). The `pal-audio` worker thread runs resample → echo cancellation (`echo.rs`) →
   Silero VAD (`vad.rs`) → segmenter (`segmenter.rs`) → Smart Turn (`turn.rs`) → prosody (`prosody.rs`).
2. **Events** (`hear/event.rs`): `SpeechStart`, `Pause(Segment)`, `Segment(Segment)`, `Resume`, `BargeIn`, `Lost`, as
   mq-bridge messages. With the reply routes' `Spoken` reports they reach the conversation as `transport::Input`.
3. **Pause vs. segment**: `Pause` means a short pause; the reply is prepared but held (`early`). `Segment` means the
   user finished. A held reply is released on the segment's end or discarded if the user goes on; then the local
   model reads its line up to the last word ahead (`drop_turn` → `Transport::read_ahead`), so the reply to the next
   pause reads only from there on (`LlmSession::read_ahead`). The segmenter's pause is 300 ms, where Smart Turn
   may also end the turn. If the user speaks again before the reply is heard, its voice waits (`go_on`): words
   drop the reply, and its speech (`Prepared.pcm`) goes into `carry`, so one reply answers both; no words: it plays.
4. **STT**: `Transport::transcribe` → `hear/stt.rs`: Parakeet TDT through the mq-bridge-onnx custom endpoint
   (`task: transcribe`), asked with a `Caller`. No route. It must load
   before other ONNX models: VAD and Smart Turn share its ONNX Runtime. Each `Pause` and each segment is
   transcribed whole, from the start of the speech (with `carry` before it), never only the new audio: Parakeet on
   pieces cut at short pauses gets worse (at ≤150 ms a lone "Hallo," became "Hello?", a comma at the cut a full stop;
   `conversation/perf.rs`, `transcripts_in_pieces`). What is saved when the user goes on is the LLM's reading (3.).
5. **Unfinished sentence**: `waits_for_more` → `Phase::Unfinished` (or `Turn::ending` for a held reply) for
   `UNFINISHED_WAIT`. If the user goes on, the audio is kept in `carry` and put before the next segment.
6. **User line**: `Persona::user_line(text, tags)`: transcript + prosody tags (`prosody_tags.rs`). The tags go
   last, so a line that grows keeps its start. This line goes into the history and the UI (`AgentEvent::Heard`).
7. **Canned replies** (no LLM): `persona.joke`, `persona.quip`, `persona.pooled` in `persona.rs`. Texts in
   `think/language/` (`kitt_*.txt`, `de.rs`, `en.rs`). `pooled` only matches lines negated
   like the transcript. Quips may use `{time}`, `{weekday}`, `{date}`, `{duration}` (`think/placeholders.rs`, said per
   language by `Profile.clock`).
8. **Enrichment**: `enrich::prompt_line(text, line, language)` in `think/enrich.rs`. Add lookups in
   `facts()`. The result goes only into this turn's prompt, never into the history. It runs before every LLM reply,
   so its time adds directly to `first_sentence_ms`.
9. **Prompt**: `Persona::messages(line)` = `context()` (system prompt, sample turns, history) + the user line.
10. **Reply**: `transport.reply(turn, messages, early)` → the reply routes (`transport.rs`) → sentences → TTS.
11. **After the turn**: `persona.remember(line, reply)`, then `transport.prefill(persona.context())` warms the LLM cache
    for the next turn.

**Barge-in** (`AgentConfig.barge_in`): `BargeIn` only pauses Pal's voice (`ToSpeaker::Pause` on `pal.speech`). A
running reply is cancelled once the speech yields a `Pause` or `Segment`. Words in its transcript stop the voice and
cut the last reply in the history to the sentences that began to play (counted from `Spoken::Audible`,
`Persona::cut_last_reply`). No
words, or room talk in party mode, or `RESUME_AFTER` of quiet without a segment: the voice goes on.

**Party mode** (`AgentConfig.party`, `think/party.rs`): after step 6 a line that neither calls Pal
(`call_words`, e.g. "KITT") nor asks for a joke is only kept as room talk, unless it begins within 8 s after Pal's
voice ended on a question and doesn't overlap it (`Party::addressed`). After 5 s of silence following such talk,
Pal says a quip or pool reply to one of its lines, or else the model comments on it (`silence_prompt`); once per talk.

## mq-bridge

Topics, routes and reports: the diagram at the top of `transport.rs`. What it doesn't show:

- Each route runs its handler in a `ThreadedHandler`, on a thread of its own.
- One type per topic in `transport/messages.rs`. Data carries the turn in `correlation_id` (a cancel skips it),
  control messages (`Hold`/`Release`/`Discard`/`Stop`/`Pause`/`Resume`) under `turn` (never skipped).
  `interrupt`/`discard` cancel a turn in all routes.
- The conversation reaches the voice only over `pal.speech`; the conversation's route handler (`conversation_handler`)
  keeps a `PocketSpeaker` clone only to cut it off at the end. `canned` sends a fixed text through the same path as the model.
- **Tokens** on `pal.speech` use mq-bridge's `stream_to` format for every source (server, local model, canned):
  `correlation_id` = turn, `mqb.stream.index`, `mqb.stream.end`; the last token's `mqb.stream.status` is
  `complete`, `error` (payload = error text) or `cancelled`.
- `pal_tick` needs batch size 1, else `static` fills the whole batch at once.
- `transport::deploy` declares all routes as one `Topology`: they start in declaration order and stop in the explicit
  `stop_order` after a failed start. The normal end stops them by name (`Transport::drop`, `stop_inbox`).
- Reports reach `pal.inbox` through a `Report` closure over a `Publisher` (`blocking_send`). A failed send is only
  logged, so a report never fails a handler.
- The memory topics live as long as the process: `transport::deploy` empties them, so a restarted agent doesn't read
  what the stopped one left.
- STT is a `Caller` on an endpoint, not a route; `pal_mic` is a custom input endpoint, usable in any route.
- mq-bridge `1.7.0` from crates.io (any 1.x ≥ 1.7.0; also used by mq-bridge-onnx). Never a local path or git
  checkout. Don't change mq-bridge itself. Ideas for later (deferred): `lookup` middleware for
  enrichment, `switch`, embeddings + qdrant.
- **Upstream candidates** (gaps worked around here; propose to mq-bridge, then simplify Pal):
  an `interval` endpoint (instead of `static` + `limiter` for `pal_tick`); `Route::stop` that drains queued
  messages (`conversation_handler` then needs no `PocketSpeaker` clone to cut the voice off).

## LLM ("brain")

- `transport::Parts`: `model` (local, loaded when there is no server or `AgentConfig.fallback`) and `server`
  (`AgentConfig.remote`), filled in `conversation::load`; without a server, the model reads the persona's context
  while loading. The local model and its session live on their own thread (`brain::Model`).
- `local_llm.rs`: `LocalLlm` via llama.cpp (default model by RAM: Gemma 4 E2B from ~6 GB, else Granite 3.3 2B; `Settings.default_model`), `LlmSession::reply`. The prompt format
  (`Format`: ChatML, Llama 3, SmolLM3, Gemma 3, Gemma 4, Granite, Phi, Falcon) follows from the model's architecture and vocabulary. The model's thread runs
  at nice 10 on Android (`platform::background_thread`); llama.cpp's workers inherit it, so TTS and audio come first.
  Models with recurrent layers (such as Qwen3.5, no longer offered): the cache goes back only to checkpoints (after the fixed messages, the last prefill,
  the last two read-aheads or prompts). A checkpoint inside a prompt would split its reading into two decodes,
  ~80 ms more on Metal, so read-aheads make them outside the reply's time.
- Thread counts for Pocket TTS (xn), Parakeet and Smart Turn: `cpu::fast_cores()`; for llama.cpp `cpu::llm_threads()`, only the biggest cores (2 on the Pixel 6a, ~1.5× faster than 4).
- `remote_llm.rs`: builds the mq-bridge endpoint: `openai_chat` with `messages_from_payload` for Ollama and OpenAI,
  the streaming `http_bulk` recipe for Anthropic's `/messages`. Both stream to `pal.llm.tokens` (`stream_to`) and
  stop on `pal.llm.cancel` (`cancel_from`, sent by `Transport::cancel`). `brain::Server` reads them back (mq-bridge's `StreamCaller`)
  and the `pal_llm` handler passes them on to `pal.speech`. Timeout: 30 s per silence of the server, no overall timeout (streams may be long).
- reqwest's platform verifier is not initialised on Android, so `remote_llm.rs` sets `tls.ca_file = "$.bundled"`
  (mq-bridge's built-in Mozilla roots) for every server.
- The local fallback runs only if the server's last token is `error` at index 0 (no text) and the turn wasn't
  cancelled. Without fallback the local model isn't loaded. A failed reply becomes `AgentEvent::ReplyFailed` → Godot `reply_failed`.

## TTS

`speak/tts.rs` (`PocketSpeaker`): Pocket TTS in-process, one thread generates, one owns the playback. The playback
thread looks every 20 ms and reports to `pal.inbox`: `Spoken::Audible { turn, at }` when a sentence begins to play
(`at` exact from the output callback; the first one ends the turn's timing) and `Spoken::Quiet` when the voice is
silent (`speaking` = `MicLink::output_active`).
Weights are quantized to q8_0 at load (`pal-tts/src/lib.rs`, `type Backend`): 2.4× faster than f32 on the Mac.
`numbers.rs` writes digits as words (number words per language in `think/language/`), `leveler.rs` evens out loudness, `voice_filter.rs` adds KITT's speaker sound.
One voice for both languages from the settings (`voice/name`, `Settings.VOICES`; default the first voice of one's own, else `juergen`), loaded from the active language's file; a voice of one's own missing in that language gives way to the default. `downloader.rs` lists the same voices.
Optional Fish Audio (`speak/fish.rs`, `AgentConfig.fish`), off until chosen as voice source (`voice/source`) in the settings: the user's own key, voice ID and model from the settings,
never an environment variable and never a prefilled voice ID. It replaces only the generation; Pocket stays loaded and
says a sentence Fish failed on (from its start, also after a break mid-sentence), and speaks alone for 60 s after a failure. Always send a model: Fish bills an
unknown or missing one as paid.
Voices of one's own: `pocket/voices/<de|en>/<name>.safetensors` in the model folder, made with the separate tool
`generate_voice/`, never downloaded (README, "Voices of one's own"). The settings import them after a one-time consent
(`voice/consent`); `speak/voice_file.rs` checks a file on import and at start (`PalNode.voice_error`), a broken one
gives way to the first catalog voice. Only own voices or voices with the speaker's
explicit consent; never add recordings or cloned voices to the repository or the APK.

## Godot

- `PalNode` (`godot.rs`): vars `user_name`, `llm_provider` (`local`/`ollama`/`openai`/`anthropic`),
  `llm_url`, `llm_remote_model`, `llm_effort`, `llm_api_key`, `llm_fallback`, `fish_api_key`, `fish_voice_id`, `fish_model`,
  `smart_turn`, `barge_in`, `party_mode`. Signals:
  `ready_to_listen`, `heard`, `said`, `turn_done`, `interrupted`, `ignored`, `failed`, `reply_failed`.
- Typed lines: the T label or key T opens a field; `PalNode.type_line` → `Agent::type_line` → `Input::Typed` on
  `pal.inbox`. No STT and no tags; it ends a running reply like words after a barge-in.
- `main.gd`: subtitles (`You` above the voice box, `Reply` below, without prosody tags), brighter red while said. While nothing moves it draws
  nothing and runs at `IDLE_FPS` (`low_processor_mode`); anything animated must count as `moving` in `_process`.
  `_start()` copies settings onto `PalNode`. The "…" label or key S opens the settings form; the P0/P1
  label or key P switches party mode (`user/party`).
- `settings.gd` (`Settings`): saves to `user://settings.cfg` (sections `user` incl. `party`, `voice`, `persona`, `fish`, `llm`); saving restarts Pal. An empty
  URL means the provider default (in `settings.gd` and `remote_llm.rs`). An empty model is replaced by the provider's
  model default from `settings.gd` only; `remote_llm.rs` has no model default and sends the model as given. The same
  goes for the effort (`llm/effort`), except that an own OpenAI model gets none, as its accepted values vary.
- Local models: `Settings.LOCAL_MODELS`, same files as `downloader.rs`, whose `ON_REQUEST` models are fetched only once
  chosen (`llm/local_model`). A new one is on trial (`llm/trying_from`): `main.gd` goes back if it fails or the app
  died before `ready_to_listen`. `llm/replaced` (the old model, or the new one if it failed) makes it ask before
  `PalNode.remove_llm` once Pal is ready. `Settings.fits_ram` disables models too large for the device.
- Own models: any GGUF the user loads in the settings is copied (on a thread, via `.part`) to `llm/` in the model
  folder and chosen as `llm/<file>.gguf`, on trial like a catalog model. Pal neither lists nor downloads models outside
  the catalog (e.g. Llama), so their licenses bind only the user. An own model whose file is gone gives way to the default.
- Characters: `think/character.rs`, files in `personas/<de|en>/<name>.txt` in the model folder, one choice for both languages
  (`persona/name`, empty = built-in), loaded from the active language's file. A file gets the language's `Profile.tag_rule` added and none of KITT's texts.
  `PalNode.persona_file`; a file that is missing in that language or can't be read falls back to the built-in character (`persona_error`). The
  built-in KITT is such a file too (`think/language/kitt_<lang>.txt`). Changing a model or character sets
  `llm/cache_stale`: the next start deletes all `.state` files (`remove_llm_caches`), a language switch doesn't.
- Environment: `PAL_MODELS` (models folder), `PAL_LLM` (GGUF file), `PAL_SMART_TURN=0`, `PAL_BARGE_IN=0`,
  `PAL_RECORD_DIR` (saves each segment as WAV + transcript, for STT test data).

## Build, test, device

- `./build.sh setup | test | godot | android | release | install | version` (see `README.md`).
- Everything builds in release (one build cache with `build.sh`): `cargo check --release`, unit tests
  `cargo test --release -p pal --lib`; the phases are tested over the topics in `conversation/tests.rs` (no models,
  no sound, STT a fake route); model tests in `pal/tests/main.rs`. Tests needing models or a
  speaker are `#[ignore]`d (`reply::` speaks through the speakers and is timing-sensitive).
- Android test phone: Pixel 6a over adb WLAN, `192.168.20.43:5555`. The phone must be unlocked for start checks.
- Manual speech checks (desktop and phone) after changes to `conversation/`, `hear/` or `speak/`: a question;
  a short pause, then the end (held reply plays); a pause, then going on (one reply to the whole); a finished
  question, then more before the reply is heard (one reply to both); a sentence
  ending in "und …", then more; barge-in with words (stops, history cut) and without (a cough: goes on after
  ~1.2 s); a joke; party talk without "KITT", silence, then with "KITT"; an unreachable server (fallback or
  `reply_failed`); the microphone gone (message, no hang).
- Format with `cargo fmt -p pal`. A plain `cargo fmt` also reformats `pal-tts`.

## Rules for changes

- Latency: `first_sentence_ms` (end of speech → first spoken sentence) must not rise noticeably. Measure it with the
  `turn_done` signal or the `llm_pal:` log lines.
- Per-language texts, word lists and number words belong in `think/language/`, not in code.
- Don't call paid APIs (OpenAI, Anthropic) in tests without asking.
- The four rules of the conversation: every part is a route handler reading one topic; everything the control
  reacts to comes as `Input` via `pal.inbox`; only `Conversation::handle` decides; plain Rust only at the edges
  (models, audio devices). No helper traits or fake types for tests: test over the topics (`conversation/tests.rs`).
- Module overviews (`lib.rs`, `conversation.rs`, `hear.rs`, `think.rs`, `speak.rs`) show only what happens
  inside the module. Topics, routes and reports between modules are drawn only in `transport.rs`; update that
  diagram with the topology, and point to it elsewhere instead of repeating it.
- Speech while a reply is heard (no barge-in) is kept as one `pending` segment and answered after it; `Pause`/`Resume`
  then are ignored. Waits (`UNFINISHED_WAIT`, `RESUME_AFTER`) are checked per input or tick, up to 20 ms late.
- `conversation_handler` keeps a `PocketSpeaker` clone only to cut the voice off at the end, as `Route::stop` doesn't
  promise to process queued messages.
