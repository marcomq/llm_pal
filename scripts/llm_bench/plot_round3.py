"""Heatmaps of the round-3 scores, needs matplotlib:

    uv run --with matplotlib python plot_round3.py scores/scores.csv results/*round3*.jsonl round3
"""

import csv
import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.colors import LinearSegmentedColormap, PowerNorm

RAMP = ["#cce3fd", "#b6d5f8", "#9dc8f7", "#84b9f3", "#6aabf1", "#519ced", "#338ced",
        "#257de0", "#2370cd", "#1b64be", "#1757ac", "#0f4a96", "#0e3d7e"]
CMAP = LinearSegmentedColormap.from_list("blue", RAMP)
INK, MUTED, GRID = "#1f1f1f", "#6b6b6b", "#ffffff"
CATEGORIES = ["knowledge", "grammar", "writing", "instructions", "json", "tools"]
LANGUAGES = ["de", "en", "fr", "es"]

scores_csv, *results, out = sys.argv[1:]
out = Path(out)
pct, count, pooled = {}, {}, {}
for row in csv.DictReader(open(scores_csv, encoding="utf-8")):
    key = (row["model"], row["category"], row["language"])
    pct[key] = float(row["pass_pct"])
    count[key] = f'{row["pass"]}/{row["items"]}'
    if row["category"] in CATEGORIES[:4] and row["language"] != "all":
        passed, items = pooled.get((row["model"], row["language"]), (0, 0))
        pooled[(row["model"], row["language"])] = (passed + int(row["pass"]), items + int(row["items"]))
for (model, language), (passed, items) in pooled.items():
    pct[(model, "language", language)] = 100 * passed / items

speed, meta, custom = defaultdict(list), {}, set()
for path in results:
    for line in open(path, encoding="utf-8"):
        r = json.loads(line)
        if r["record_type"] != "subtest":
            continue
        meta[r["model"]] = (r["model_parameters_b"], r["model_size_bytes"] / 1e9)
        if "non-OSI" in r["model_license"]:
            custom.add(r["model"])
        if r["output_tokens"] >= 50:
            speed[r["model"]].append(r["generation_tokens_per_second"])

models = sorted(meta, key=lambda m: -statistics.mean(pct[(m, c, "all")] for c in CATEGORIES))


def heatmap(columns, groups, extra, title, subtitle, note, file, fractions=False):
    data = [[pct[(m, c, l)] for c, l in columns] for m in models]
    width = 2.6 + 0.78 * len(columns) + 0.9 * len(extra[0])
    fig, ax = plt.subplots(figsize=(width, 0.42 * len(models) + 2.0), dpi=200)
    ax.imshow(data, cmap=CMAP, norm=PowerNorm(2.5, vmin=0, vmax=100), aspect="auto")
    for i, row in enumerate(data):
        for j, v in enumerate(row):
            m, (c, l) = models[i], columns[j]
            ax.text(j, i, count[(m, c, l)] if fractions else int(v + 0.5), ha="center", va="center", fontsize=9,
                    color="white" if v >= 80 else INK)
    ax.set_xticks(range(len(columns)), [l if l != "all" else c for c, l in columns], fontsize=9, color=INK)
    ax.set_yticks(range(len(models)), [m + " *" if m in custom else m for m in models], fontsize=9, color=INK)
    ax.xaxis.tick_top()
    ax.tick_params(which="both", length=0)
    ax.set_xticks([x - 0.5 for x in range(1, len(columns))], minor=True)
    ax.set_yticks([y - 0.5 for y in range(1, len(models))], minor=True)
    ax.grid(which="minor", color=GRID, linewidth=2)
    for s in ax.spines.values():
        s.set_visible(False)
    for label, start, end in groups:
        ax.text((start + end) / 2, -1.25, label, ha="center", va="bottom", fontsize=9.5, color=INK, fontweight="bold")
        if end > start:
            ax.plot([start - 0.4, end + 0.4], [-1.1, -1.1], color=MUTED, linewidth=0.8, clip_on=False)
    headers, rows = extra
    for k, header in enumerate(headers):
        x = len(columns) - 0.5 + 0.55 + 0.9 * k
        ax.text(x, -0.75, header, ha="left", va="center", fontsize=8.5, color=MUTED)
        for i, m in enumerate(models):
            ax.text(x, i, rows[m][k], ha="left", va="center", fontsize=9, color=INK)
    ax.set_xlim(-0.5, len(columns) - 0.5)
    fig.suptitle(title, x=0.02, ha="left", fontsize=13, fontweight="bold", color=INK)
    fig.text(0.02, 0.925, subtitle, ha="left", fontsize=9, color=MUTED)
    note += "\n* custom license, not OSI-approved (Gemma Terms, Llama 3.2, Falcon, Qwen Research, LFM Open)"
    fig.text(0.02, 0.015, note, ha="left", fontsize=7.5, color=MUTED)
    fig.subplots_adjust(left=0.22, right=1 - (0.9 * len(headers) + 0.3) / width, top=0.845, bottom=0.11)
    fig.savefig(out / file, facecolor="white")
    print(out / file)


extra_rows = {m: [f"{meta[m][1]:.1f} GB", f"{statistics.median(speed[m]):.0f}",
                  int(statistics.mean(pct[(m, c, 'all')] for c in CATEGORIES) + 0.5)] for m in models}
heatmap(
    [("language", l) for l in ["en", "de", "fr", "es"]] + [(c, "all") for c in CATEGORIES],
    [("by language", 0, 3), ("all 4 languages", 4, 7), ("en only", 8, 9)],
    (["size", "tok/s", "mean"], extra_rows),
    "13 small LLMs, 187 checked tasks each",
    "% passed · greedy, thinking off · llama.cpp on a MacBook Air M1, 4 threads",
    "by language: knowledge, grammar, writing and instructions in that language, 47 checks each · "
    "knowledge 80, grammar 60, writing 24, instructions 24, json 10, tools 13\n"
    "writing = the instructions replies, judged by hand: correct grammar and spelling, and they make sense · "
    "size = GGUF Q4_K_M · tok/s = median generation speed · mean = unweighted mean of the 6 categories, only for sorting",
    "categories.png",
)
LANGUAGE_NOTE = ("Grammar: correct a sentence (3 of 15 are already correct). Writing: the 6 instructions replies, free text, "
                 "judged by hand: correct grammar and spelling, and they make sense. fr/es not checked by native speakers.")
heatmap(
    [(c, l) for c in CATEGORIES[:4] for l in LANGUAGES] + [("json", "en"), ("tools", "en")],
    [("knowledge", 0, 3), ("grammar", 4, 7), ("writing", 8, 11), ("instructions", 12, 15), ("json", 16, 16),
     ("tools", 17, 17)],
    (["params"], {m: [f"{meta[m][0]:.1f}B"] for m in models}),
    "Same models, by language",
    "% passed · per language: knowledge 20, grammar 15, writing 6, instructions 6 · json 10, tools 13 (English)",
    LANGUAGE_NOTE,
    "languages.png",
)
heatmap(
    [(c, l) for c in CATEGORIES[:4] for l in LANGUAGES],
    [("knowledge", 0, 3), ("grammar", 4, 7), ("writing", 8, 11), ("instructions", 12, 15)],
    (["params"], {m: [f"{meta[m][0]:.1f}B"] for m in models}),
    "Same models, by language: tasks passed",
    "passed / tasks · color = share passed",
    LANGUAGE_NOTE,
    "languages-counts.png",
    fractions=True,
)
