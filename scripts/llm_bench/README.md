# Local LLM benchmark

The benchmark runs two short, fixed prompts per selected language: a basic factual-knowledge check and a language check
with two grammar corrections and one short free response. The canonical prompts are in
[`src/main.rs`](./src/main.rs). It saves raw responses and measurements as JSONL; it does not
score them. A completed row is flushed to disk immediately.

Each model/language/subtest is one generation capped at 1,000 output tokens. The default languages are German and
English. Select all four with `PAL_BENCH_LANGUAGES=de,en,fr,es`. With ten models this means 80 subtests (10 models ×
4 languages × 2 subtests) plus ten cleanup records. The process stops after eight hours and saves any response
interrupted by the deadline as `deadline_partial`.

The runner processes manifest entries sequentially: it downloads the current model if it is missing (from the
manifest's `url`, checked against its `sha256`), runs and logs its subtests, unloads it, then removes that model before
starting the next one. A model downloaded by the runner is always removed afterward. Set `remove_after` to `true` to
also remove a model that was already present before the run; otherwise existing files are kept. A present file must
match the manifest's `sha256` too. The benchmark models are not part of Pal's download catalog: the app neither lists
nor downloads them. Keep the results file outside the model directory so cleanup can never remove the log.

The benchmark is a standalone crate (`llm-bench`), neither part of the `rust/` workspace nor of the APK, and does not
depend on Pal. It drives llama.cpp directly with the GGUF's own chat template and Pal's sampling defaults (temperature,
top-k, top-p, repeat penalty, seed), at most 4 threads as Pal uses on a phone. Rows from schema version 2 on record this
as `engine` and `llm_threads`.

Results of the runs on 2026-11-24 (Mac M1), raw JSONL in [`results/`](./results). These runs (schema version 1) still
went through Pal's `LocalLlm`: Pal's prompt formats, Markdown and emoji tokens banned, Qwen3 without thinking. New runs
are therefore not directly comparable with them.

| File | Manifest | Languages |
|---|---|---|
| `2026-11-24-round1-de-en.jsonl` | `models-round1.json` | de, en |
| `2026-11-24-round1-fr-es.jsonl` | `models-round1.json` | fr, es |
| `2026-11-24-round2-de-en-fr-es.jsonl` | `models-round2.json` | de, en, fr, es (MiniCPM3 4B failed) |
| `2026-11-24-round2-replacement-de-en-fr-es.jsonl` | `models-round2-replacement.json` | de, en, fr, es |

The first ten-model selection is [`models-round1.json`](./models-round1.json). The second, distinct
selection is [`models-round2.json`](./models-round2.json). Every model in both lists is
under 6B parameters. All second-round files use Q4_K_M; the set includes multilingual models and small/on-device
designs. Licenses vary, including custom/non-OSI terms; check each model card and license before reuse. Model-card and
pinned GGUF source URLs are included in each manifest and copied into every result row.

In the second-round attempt, MiniCPM3 4B failed at llama.cpp decoding and was removed. Its error stays in the original
JSONL. The replacement is Phi-4 Mini Reasoning in
[`models-round2-replacement.json`](./models-round2-replacement.json); it is run separately
with the same four languages. For later review, use both result files so the final set has ten successfully tested
models, while retaining the failed attempt for traceability.

To define a different set, use this schema:

```json
{
  "models": [
    {
      "name": "Example 3B Q4",
      "file": "SmolLM3-Q4_K_M.gguf",
      "parameters_b": 3.0,
      "license": "Apache-2.0",
      "source": "https://huggingface.co/HuggingFaceTB/SmolLM3-3B",
      "gguf_source": "https://huggingface.co/ggml-org/SmolLM3-3B-GGUF",
      "url": "https://huggingface.co/ggml-org/SmolLM3-3B-GGUF/resolve/<commit>/SmolLM3-Q4_K_M.gguf",
      "sha256": "<64 hex digits>",
      "optimization": "GGUF Q4_K_M",
      "remove_after": true
    }
  ]
}
```

Use the exact license and describe the quantization or other optimization for each model. `file` is the GGUF's name in the
model directory (no folders), `parameters_b` must be at most 8, `source`, `gguf_source` and `url` must be HTTPS URLs
(pin `url` to a commit), and `sha256` is the file's hash. The runner records each model's file size and SHA-256 hash
alongside the raw answers, and per subtest the prompt tokens read and the time to read them (`prompt_read_ms`, from
schema version 2 on; for Pal's latency that matters as much as tok/s). It also records the process peak RSS (`ru_maxrss`); that
high-water mark accumulates for the lifetime of the process, so it is a run-level memory figure when the manifest
contains multiple models.

Run on the M1:

```sh
cd scripts/llm_bench
PAL_BENCH_DEVICE="Mac M1" cargo run --release -- \
  /path/to/models /path/to/models.json /path/to/results.jsonl
```

For the second selection, use a fresh, empty result path and run all four languages together:

```sh
cd scripts/llm_bench
PAL_BENCH_LANGUAGES=de,en,fr,es PAL_BENCH_DEVICE="Mac M1" \
  cargo run --release -- \
  /path/to/benchmark-models models-round2.json \
  results/<date>-round2-de-en-fr-es.jsonl
```

The replacement model uses the same invocation pattern and its one-model manifest:

```sh
cd scripts/llm_bench
PAL_BENCH_LANGUAGES=de,en,fr,es PAL_BENCH_DEVICE="Mac M1" \
  cargo run --release -- \
  /path/to/benchmark-models models-round2-replacement.json \
  results/<date>-round2-replacement-de-en-fr-es.jsonl
```

For a one-model manual rerun, copy that model's object into a temporary manifest with a single `models` entry, use a
new result path, and keep `remove_after: true`. This reruns the identical fixed prompts and records a normal JSONL
file. For a human review of existing outputs, use the JSONL records; do not infer a score from throughput, file size,
or model license.

For a later device-only throughput run, set `PAL_BENCH_MODE=speed`. It uses a fixed English prompt for three
generations capped at 256 tokens each (768 maximum output tokens per model), skips the knowledge and language prompts,
and still writes raw results. Set `PAL_BENCH_DEVICE` to `Pixel 6a` for that run.

The result file is created exclusively; choose a new path for every run. Do not point the benchmark at a paid remote
provider: this runner only loads local GGUF files through llama.cpp.
The speed-only mode is ready to use on the M1; running it on Android still needs a device launch/build path.

## Scored run (round 3)

`PAL_BENCH_MODE=scored` runs the items of a test file instead of the fixed prompts: one item per generation, each in
a fresh context, greedy (no sampling, no repeat penalty, which would punish the quotes and braces of JSON). The
expected answers sit in the same file and are read only by `score.py`. Rows from schema version 3 on carry
`test_id`, `sampling`, `tests_name`, `tests_sha256` and `assistant_prefix`.

[`tests-round3.json`](./tests-round3.json) has 187 items:

| Category | Languages | Items per language | Scored by |
|---|---|---:|---|
| `knowledge` | de, en, fr, es | 20 | an accepted answer as a whole word (case- and accent-insensitive) |
| `grammar` | de, en, fr, es | 15, of them 3 already correct | the first line equals an accepted correction; a correct sentence must come back unchanged |
| `instructions` | de, en, fr, es | 6 | checkable constraints: sentence count, list, word limit, required word, yes/no, lowercase |
| `writing` | de, en, fr, es | 6 | the same instructions replies, judged by hand: no grammar or spelling errors, in the asked language, makes sense |
| `json` | en | 10 | the parsed reply equals the expected value (extraction, nesting, escaping, null, fixing and reading JSON) |
| `tools` | en | 13 | the same calls as expected, in any order; `[]` where no tool fits |

The tools are described in the system prompt in one format for all models (a JSON array of
`{"name", "arguments"}`), not through each model's own tool template: llama.cpp's built-in templates ignore tool
definitions. `parameters` counts as `arguments`, as Llama 3 is trained to write it. JSON and tool replies in a code
fence still count; `items.csv` shows per item whether the reply was nothing but JSON (`strict_json`).

[`models-round3.json`](./models-round3.json): the 12 best models of the 2026-11-24 runs without Qwen2.5 1.5B, plus
Qwen3 4B Instruct 2507. Thinking is off: Qwen3 1.7B and SmolLM3 get an empty `<think></think>` block after the
assistant start (`assistant_prefix`), as their own templates do with thinking off, since llama.cpp's built-in
templates don't. A reasoning block cut off by the token limit counts as an empty reply. llama.cpp's built-in templates
don't know Gemma 4's either: with `"prompt_format": "gemma4"` the runner writes its prompt as Pal does.

```sh
cd scripts/llm_bench
PAL_BENCH_MODE=scored PAL_BENCH_TESTS=tests-round3.json PAL_BENCH_LANGUAGES=de,en,fr,es \
  PAL_BENCH_DEVICE="Mac M1" cargo run --release -- \
  /path/to/benchmark-models models-round3.json results/<date>-round3.jsonl
python3 score.py tests-round3.json results/<date>-round3.jsonl
```

`score.py` writes `scores/items.csv` (each item with its verdict and the raw reply), `scores/scores.csv` (pass rate
per model, category and language) and `scores/scores.md` (the same as Markdown tables). A grammar reply that matches
no accepted wording and is not the unchanged sentence is left as `review` and listed in `scores/review-todo.csv`:
fill in `pass` or `fail` in its `verdict` column and pass it with `--verdicts`. Every instructions reply is listed
there too, as the writing item `<id>-grammar`. There is no overall score.

## Manual review

The JSONL is the source of truth. Filter `record_type == "subtest"` and group by `run_id`, `model`, `language`, and
`subtest`. Keep `status`, `response`, model/license/quantization metadata, and the original JSONL together. A failed
or partial response is not a zero; review its status separately.

To make a review spreadsheet without scoring or changing the raw log:

```sh
cd scripts/llm_bench
python3 review.py results/2026-11-24-round2-de-en-fr-es.jsonl \
  results/2026-11-24-round2-replacement-de-en-fr-es.jsonl -o review.csv
```

The CSV has one row per model and language, places the knowledge and language responses in separate columns, includes
throughput and model metadata, and leaves manual score and notes columns blank. It can combine multiple run files and
refuses to overwrite an existing CSV. Keep each JSONL as the archival source.

To build shareable tables and diagrams from several runs:

```sh
cd scripts/llm_bench
python3 report.py results/*.jsonl --review review.csv --output-dir report
```

`review*.csv`, `report*/` and `scores*/` in this folder are ignored by git (see `.gitignore`), as are downloaded models.

The report directory contains an `index.html`, CSV tables, and SVG charts for generation speed by language and GGUF
disk size. Speed-mode runs get a table of their own (`speed-mode.csv`). Failed or partial records are listed separately. It does not infer language quality; after manual scores are
entered in the review CSV, rerunning the report adds quality tables and charts. RSS is shown per run only: the recorded
process high-water mark cannot be attributed to an individual model.

For a simple, consistent scorecard, record one row per model and language with:

| Field | Scale | Review rule |
|---|---:|---|
| Knowledge | 0–8 | One point for each correct factual answer below |
| Grammar | 0–2 | One point for each of the two corrections that fixes the error without introducing a new one |
| Natural reply | 0–2 | 0 = unusable/off-topic, 1 = understandable but awkward, 2 = natural and relevant |
| Instruction following | 0–2 | 0 = misses most tasks, 1 = partly follows, 2 = completes all requested items in the requested language |

Use the following knowledge key, accepting equivalent wording and spelling:

| # | German | English | French | Spanish |
|---:|---|---|---|---|
| 1 | Canberra | Canberra | Canberra | Canberra |
| 2 | H₂O | H₂O | H₂O | H₂O |
| 3 | Merkur | Mercury | Mercure | Mercurio |
| 4 | Pazifischer Ozean | Pacific Ocean | océan Pacifique | océano Pacífico |
| 5 | Afrika | Africa | Afrique | África |
| 6 | 180 Grad | 180 degrees | 180 degrés | 180 grados |
| 7 | Kohlendioxid / CO₂ | carbon dioxide / CO₂ | dioxyde de carbone / CO₂ | dióxido de carbono / CO₂ |
| 8 | 1.000 Meter | 1,000 metres/meters | 1 000 mètres | 1.000 metros |

For the language subtest, compare the two corrections with the source prompts in `src/main.rs`; accept equivalent
grammatical rewrites. Judge the final missed-train reply for naturalness and relevance, not for one exact wording.
The test is a small screening set, not a standardized language certification. Do not combine raw quality scores with
tokens/s or RAM; report those as separate measurements.
