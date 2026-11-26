#!/usr/bin/env python3
"""Build shareable CSV tables and SVG charts from benchmark logs and an optional manual scorecard."""

import argparse
import csv
import html
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any


LANGUAGE_ORDER = ("de", "en", "fr", "es")
SCORE_FIELDS = {
    "knowledge_score_0_8": ("Knowledge", 8),
    "grammar_score_0_2": ("Grammar", 2),
    "naturalness_score_0_2": ("Natural reply", 2),
    "instruction_following_score_0_2": ("Instruction following", 2),
}
COLORS = ("#4e79a7", "#f28e2b", "#59a14f", "#e15759")


def read_records(paths: list[Path]) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for path in paths:
        with path.open(encoding="utf-8") as source:
            for line_number, line in enumerate(source, start=1):
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError as error:
                    raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
    return records


def median_or_blank(values: list[float]) -> float | str:
    return round(statistics.median(values), 2) if values else ""


Rows = list[dict[str, Any]]


def build_metrics(records: Rows) -> tuple[Rows, Rows, Rows, Rows, Rows]:
    completed: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    speed: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    failures: list[dict[str, Any]] = []
    memory: dict[str, dict[str, Any]] = {}
    models_by_run: dict[str, set[str]] = defaultdict(set)

    for record in records:
        run_id = record.get("run_id", "")
        model = record.get("model", "")
        if run_id and model:
            models_by_run[run_id].add(model)
        peak_rss = record.get("process_peak_rss_bytes")
        if run_id and isinstance(peak_rss, (int, float)):
            run = memory.setdefault(
                run_id,
                {"run_id": run_id, "device": record.get("device", ""), "process_peak_rss_bytes": 0},
            )
            run["process_peak_rss_bytes"] = max(run["process_peak_rss_bytes"], peak_rss)

        if record.get("record_type") == "subtest":
            if record.get("status") != "complete":
                failures.append(record)
            elif record.get("mode") == "speed":
                speed[(record.get("device", ""), model)].append(record)
            else:
                completed[(model, record["model_file"])].append(record)
        elif record.get("record_type") in {"model_error", "model_interrupted"}:
            failures.append(record)
        elif record.get("record_type") == "model_cleanup" and record.get("status") == "failed":
            failures.append(record)

    by_model_language: dict[tuple[str, str, str], list[dict[str, Any]]] = defaultdict(list)
    model_rows: list[dict[str, Any]] = []
    for (model, model_file), items in sorted(completed.items()):
        by_language: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for item in items:
            by_language[item["language"]].append(item)
            by_model_language[(model, model_file, item["language"])].append(item)

        rates = [float(item["generation_tokens_per_second"]) for item in items]
        load_by_run = {
            item["run_id"]: float(item["model_load_ms"])
            for item in items
        }
        first = items[0]
        parameters = first.get("model_parameters_b", "")
        row = {
            "model": model,
            "model_file": model_file,
            "parameters_b": round(float(parameters), 3) if parameters != "" else "",
            "model_license": first.get("model_license", ""),
            "optimization": first.get("optimization", ""),
            "model_size_bytes": first.get("model_size_bytes", ""),
            "model_size_gib": round(first.get("model_size_bytes", 0) / (1024**3), 3),
            "completed_subtests": len(items),
            "languages": ",".join(lang for lang in LANGUAGE_ORDER if lang in by_language),
            "median_tokens_per_second": round(statistics.median(rates), 2),
            "mean_tokens_per_second": round(statistics.mean(rates), 2),
            "median_model_load_ms": round(statistics.median(load_by_run.values()), 2),
            "median_prompt_read_ms": prompt_read_median(items),
            "runs": ";".join(sorted(load_by_run)),
        }
        for language in LANGUAGE_ORDER:
            lang_rates = [
                float(item["generation_tokens_per_second"])
                for item in by_language.get(language, [])
            ]
            row[f"{language}_median_tokens_per_second"] = median_or_blank(lang_rates)
        model_rows.append(row)

    language_rows: list[dict[str, Any]] = []
    for (model, model_file, language), items in sorted(by_model_language.items()):
        rates = [float(item["generation_tokens_per_second"]) for item in items]
        first = items[0]
        language_rows.append(
            {
                "model": model,
                "model_file": model_file,
                "language": language,
                "completed_subtests": len(items),
                "median_tokens_per_second": round(statistics.median(rates), 2),
                "mean_tokens_per_second": round(statistics.mean(rates), 2),
                "model_size_bytes": first.get("model_size_bytes", ""),
                "model_license": first.get("model_license", ""),
                "runs": ";".join(sorted({item["run_id"] for item in items})),
            }
        )

    speed_rows = [
        {
            "device": device,
            "model": model,
            "generations": len(items),
            "median_tokens_per_second": round(
                statistics.median(float(item["generation_tokens_per_second"]) for item in items), 2
            ),
            "median_prompt_read_ms": prompt_read_median(items),
            "runs": ";".join(sorted({item["run_id"] for item in items})),
        }
        for (device, model), items in sorted(speed.items())
    ]

    memory_rows = [
        {
            **run,
            "models_in_run": len(models_by_run[run_id]),
            "note": "Process high-water mark; not a per-model RAM measurement.",
        }
        for run_id, run in sorted(memory.items())
    ]
    return model_rows, language_rows, speed_rows, memory_rows, failures


def prompt_read_median(items: list[dict[str, Any]]) -> float | str:
    """Time to read the prompt; logs before schema version 2 don't have it."""
    return median_or_blank([float(item["prompt_read_ms"]) for item in items if "prompt_read_ms" in item])


def read_scores(path: Path | None) -> list[dict[str, Any]]:
    if path is None:
        return []
    scores: dict[str, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    with path.open(encoding="utf-8", newline="") as source:
        for line_number, row in enumerate(csv.DictReader(source), start=2):
            for field, (_, maximum) in SCORE_FIELDS.items():
                value = row.get(field, "").strip()
                if not value:
                    continue
                try:
                    score = float(value)
                except ValueError as error:
                    raise ValueError(f"{path}:{line_number}: {field} must be a number") from error
                if not 0 <= score <= maximum:
                    raise ValueError(f"{path}:{line_number}: {field} must be between 0 and {maximum}")
                scores[row["model"]][field].append(score)

    rows = []
    for model, fields in sorted(scores.items()):
        row: dict[str, Any] = {"model": model}
        for field in SCORE_FIELDS:
            values = fields.get(field, [])
            row[field] = round(statistics.mean(values), 2) if values else ""
            row[f"{field}_reviews"] = len(values)
        rows.append(row)
    return rows


def write_csv(path: Path, fields: list[str], rows: list[dict[str, Any]]) -> None:
    with path.open("x", encoding="utf-8", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=fields, extrasaction="ignore")
        writer.writeheader()
        writer.writerows(rows)


def grouped_bar_svg(
    title: str,
    rows: list[tuple[str, dict[str, float | None]]],
    series: list[str],
    maximum: float,
    unit: str,
) -> str:
    width = 1120
    left = 250
    right = 95
    top = 72
    row_height = 30
    chart_width = width - left - right
    height = top + max(len(rows), 1) * row_height + 48
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
        '<style>text{font-family:Arial,sans-serif;fill:#222}.grid{stroke:#ddd;stroke-width:1}</style>',
        f'<text x="{left}" y="28" font-size="20" font-weight="bold">{html.escape(title)}</text>',
    ]
    for index, name in enumerate(series):
        x = left + index * 190
        color = COLORS[index % len(COLORS)]
        parts.append(f'<rect x="{x}" y="43" width="12" height="12" fill="{color}"/>')
        parts.append(f'<text x="{x + 18}" y="54" font-size="12">{html.escape(name)}</text>')

    for tick in range(5):
        value = maximum * tick / 4
        x = left + chart_width * tick / 4
        parts.append(f'<line class="grid" x1="{x:.1f}" y1="{top - 5}" x2="{x:.1f}" y2="{height - 35}"/>')
        parts.append(f'<text x="{x:.1f}" y="{height - 15}" font-size="11" text-anchor="middle">{value:.0f}</text>')
    parts.append(f'<text x="{width - 18}" y="{height - 15}" font-size="11" text-anchor="end">{html.escape(unit)}</text>')

    for row_index, (label, values) in enumerate(rows):
        group_y = top + row_index * row_height
        parts.append(
            f'<text x="{left - 10}" y="{group_y + 17}" font-size="11" text-anchor="end">{html.escape(label)}</text>'
        )
        for series_index, name in enumerate(series):
            value = values.get(name)
            if value is None:
                continue
            bar_y = group_y + series_index * 6
            bar_width = max(0.0, min(value, maximum)) / maximum * chart_width if maximum else 0
            color = COLORS[series_index % len(COLORS)]
            parts.append(
                f'<rect x="{left}" y="{bar_y}" width="{bar_width:.1f}" height="5" fill="{color}"/>'
            )
            label_x = min(left + bar_width + 4, width - right + 26)
            parts.append(
                f'<text x="{label_x:.1f}" y="{bar_y + 5}" font-size="9">{value:.1f}</text>'
            )
    parts.append("</svg>")
    return "\n".join(parts)


def single_bar_svg(title: str, rows: list[tuple[str, float]], maximum: float, unit: str) -> str:
    return grouped_bar_svg(title, [(name, {unit: value}) for name, value in rows], [unit], maximum, unit)


def html_table(rows: list[dict[str, Any]], columns: list[tuple[str, str]]) -> str:
    headers = "".join(f"<th>{html.escape(label)}</th>" for _, label in columns)
    body = []
    for row in rows:
        cells = "".join(f"<td>{html.escape(str(row.get(key, '')))}</td>" for key, _ in columns)
        body.append(f"<tr>{cells}</tr>")
    return f"<table><thead><tr>{headers}</tr></thead><tbody>{''.join(body)}</tbody></table>"


def write_report(
    output_dir: Path,
    input_paths: list[Path],
    model_rows: list[dict[str, Any]],
    language_rows: list[dict[str, Any]],
    speed_mode_rows: list[dict[str, Any]],
    memory_rows: list[dict[str, Any]],
    failures: list[dict[str, Any]],
    score_rows: list[dict[str, Any]],
) -> None:
    output_dir.mkdir(parents=True, exist_ok=False)

    model_fields = [
        "model",
        "model_file",
        "parameters_b",
        "model_license",
        "optimization",
        "model_size_bytes",
        "model_size_gib",
        "completed_subtests",
        "languages",
        "median_tokens_per_second",
        "mean_tokens_per_second",
        "median_model_load_ms",
        "median_prompt_read_ms",
        "runs",
        *(f"{language}_median_tokens_per_second" for language in LANGUAGE_ORDER),
    ]
    language_fields = [
        "model",
        "model_file",
        "language",
        "completed_subtests",
        "median_tokens_per_second",
        "mean_tokens_per_second",
        "model_size_bytes",
        "model_license",
        "runs",
    ]
    speed_fields = ["device", "model", "generations", "median_tokens_per_second", "median_prompt_read_ms", "runs"]
    memory_fields = ["run_id", "device", "models_in_run", "process_peak_rss_bytes", "note"]
    failure_fields = ["record_type", "run_id", "model", "language", "subtest", "status", "error"]
    write_csv(output_dir / "model-summary.csv", model_fields, model_rows)
    write_csv(output_dir / "language-speed.csv", language_fields, language_rows)
    write_csv(output_dir / "run-memory.csv", memory_fields, memory_rows)
    if speed_mode_rows:
        write_csv(output_dir / "speed-mode.csv", speed_fields, speed_mode_rows)
    write_csv(
        output_dir / "failures.csv",
        failure_fields,
        [
            {field: record.get(field, "") for field in failure_fields}
            for record in failures
        ],
    )

    speed_rows = []
    for model in model_rows:
        speeds = {
            language: (
                float(model[f"{language}_median_tokens_per_second"])
                if model[f"{language}_median_tokens_per_second"] != ""
                else None
            )
            for language in LANGUAGE_ORDER
        }
        speed_rows.append((model["model"], speeds))
    speed_max = max(
        (value for _, values in speed_rows for value in values.values() if value is not None),
        default=1.0,
    )
    speed_max = math.ceil(speed_max / 10) * 10
    (output_dir / "speed-by-language.svg").write_text(
        grouped_bar_svg(
            "Median generation speed by language",
            speed_rows,
            list(LANGUAGE_ORDER),
            speed_max,
            "Tok/s",
        ),
        encoding="utf-8",
    )

    size_rows = [(row["model"], float(row["model_size_gib"])) for row in model_rows]
    size_max = math.ceil(max((value for _, value in size_rows), default=1.0))
    (output_dir / "model-size.svg").write_text(
        single_bar_svg("Quantized model size on disk", size_rows, max(size_max, 1), "GiB"),
        encoding="utf-8",
    )

    score_section = (
        "<p>No manual scores have been entered yet. Quality scores are deliberately not inferred from model responses.</p>"
    )
    if score_rows:
        score_fields = ["model"]
        series = []
        for field, (label, _) in SCORE_FIELDS.items():
            score_fields.extend([field, f"{field}_reviews"])
            series.append(label)
        write_csv(output_dir / "quality-scores.csv", score_fields, score_rows)
        quality_chart_rows = []
        for row in score_rows:
            values = {}
            for field, (label, maximum) in SCORE_FIELDS.items():
                score = row[field]
                values[label] = float(score) / maximum * 100 if score != "" else None
            quality_chart_rows.append((row["model"], values))
        (output_dir / "quality-scores.svg").write_text(
            grouped_bar_svg(
                "Manual quality scores (% of each dimension's maximum)",
                quality_chart_rows,
                series,
                100,
                "%",
            ),
            encoding="utf-8",
        )
        score_section = (
            '<img src="quality-scores.svg" alt="Manual quality score chart">'
            + html_table(
                score_rows,
                [("model", "Model")]
                + [(field, label) for field, (label, _) in SCORE_FIELDS.items()],
            )
            + "<p>Each dimension is shown separately and normalized to its own maximum; they are not added into a single score.</p>"
        )

    summary_table = html_table(
        model_rows,
        [
            ("model", "Model"),
            ("parameters_b", "Parameter (B)"),
            ("model_license", "License"),
            ("model_size_gib", "Disk size (GiB)"),
            ("median_tokens_per_second", "Median tok/s"),
            ("median_prompt_read_ms", "Median prompt read (ms)"),
            ("completed_subtests", "Complete subtests"),
        ],
    )
    speed_section = (
        html_table(
            speed_mode_rows,
            [
                ("device", "Device"),
                ("model", "Model"),
                ("generations", "Generations"),
                ("median_tokens_per_second", "Median tok/s"),
                ("median_prompt_read_ms", "Median prompt read (ms)"),
            ],
        )
        + '<p>Detail: <a href="speed-mode.csv">speed mode CSV</a>.</p>'
        if speed_mode_rows
        else "<p>No speed-mode runs (<code>PAL_BENCH_MODE=speed</code>) in these logs.</p>"
    )
    failure_table = html_table(
        failures,
        [
            ("record_type", "Record"),
            ("model", "Model"),
            ("language", "Language"),
            ("subtest", "Subtest"),
            ("status", "Status"),
            ("error", "Error"),
        ],
    )
    source_list = "".join(f"<li>{html.escape(path.name)}</li>" for path in input_paths)
    page = f"""<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Local LLM benchmark report</title>
<style>
body{{font:15px/1.5 system-ui,sans-serif;max-width:1200px;margin:2rem auto;padding:0 1rem;color:#222}}
table{{border-collapse:collapse;width:100%;margin:1rem 0 2rem;font-size:13px}}
th,td{{border:1px solid #ddd;padding:.4rem;text-align:left}}th{{background:#f3f5f7}}
img{{max-width:100%;height:auto;border:1px solid #eee;margin:1rem 0 2rem}}
.note{{background:#f5f7fa;padding:.8rem;border-left:4px solid #4e79a7}}
</style>
<h1>Local LLM benchmark report</h1>
<p>Objective measurements only. The raw responses are not automatically graded.</p>
<p class="note">Process peak RSS is a run-level high-water mark, not a per-model RAM measurement. See <code>run-memory.csv</code>.</p>
<h2>Model summary</h2>{summary_table}
<h2>Generation speed</h2><img src="speed-by-language.svg" alt="Median generation speed by language">
<h2>Speed mode</h2>{speed_section}
<h2>Disk size</h2><img src="model-size.svg" alt="Model file sizes">
<h2>Manual quality scores</h2>{score_section}
<h2>Failures and partial results</h2>{failure_table}
<h2>Source logs</h2><ul>{source_list}</ul>
<p>Machine-readable detail: <a href="model-summary.csv">model summary CSV</a>,
<a href="language-speed.csv">per-language speed CSV</a>,
<a href="run-memory.csv">run memory CSV</a>,
<a href="failures.csv">failures CSV</a>.</p>
</html>"""
    (output_dir / "index.html").write_text(page, encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jsonl", type=Path, nargs="+", help="one or more raw benchmark JSONL files")
    parser.add_argument("--review", type=Path, help="manually filled review CSV from review.py")
    parser.add_argument("--output-dir", type=Path, required=True, help="new directory for report files")
    args = parser.parse_args()

    records = read_records(args.jsonl)
    model_rows, language_rows, speed_rows, memory_rows, failures = build_metrics(records)
    score_rows = read_scores(args.review)
    write_report(
        args.output_dir, args.jsonl, model_rows, language_rows, speed_rows, memory_rows, failures, score_rows
    )


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        raise SystemExit(str(error)) from error
