#!/usr/bin/env python3
"""Export benchmark JSONL into a human-review CSV without assigning scores."""

import argparse
import csv
import json
import sys
from pathlib import Path
from typing import TextIO


LANGUAGE_ORDER = {"de": 0, "en": 1, "fr": 2, "es": 3}
SUBTESTS = ("knowledge", "language")
FIELDS = (
    "run_id",
    "device",
    "model",
    "language",
    "repetition",
    "model_license",
    "optimization",
    "model_size_bytes",
    "process_peak_rss_bytes",
    "model_load_ms",
    "knowledge_status",
    "knowledge_output_tokens",
    "knowledge_tokens_per_second",
    "knowledge_response",
    "language_status",
    "language_output_tokens",
    "language_tokens_per_second",
    "language_response",
    "knowledge_score_0_8",
    "grammar_score_0_2",
    "naturalness_score_0_2",
    "instruction_following_score_0_2",
    "notes",
)


def export_rows(paths: list[Path]) -> list[dict[str, object]]:
    grouped: dict[tuple[str, str, str, int], dict[str, object]] = {}
    for path in paths:
        with path.open(encoding="utf-8") as source:
            for line_number, line in enumerate(source, start=1):
                try:
                    record = json.loads(line)
                except json.JSONDecodeError as error:
                    raise ValueError(f"{path}:{line_number}: invalid JSON: {error}") from error
                if record.get("record_type") != "subtest" or record.get("subtest") not in SUBTESTS:
                    continue

                key = (record["run_id"], record["model"], record["language"], record.get("repetition", 0))
                row = grouped.setdefault(
                    key,
                    {
                        "run_id": record["run_id"],
                        "device": record.get("device", ""),
                        "model": record["model"],
                        "language": record["language"],
                        "repetition": record.get("repetition", 0),
                        "model_license": record.get("model_license", ""),
                        "optimization": record.get("optimization", ""),
                        "model_size_bytes": record.get("model_size_bytes", ""),
                        "process_peak_rss_bytes": record.get("process_peak_rss_bytes", ""),
                        "model_load_ms": record.get("model_load_ms", ""),
                    },
                )
                subtest = record["subtest"]
                if f"{subtest}_status" in row:
                    raise ValueError(f"{path}:{line_number}: duplicate {subtest} record for {key}")
                row[f"{subtest}_status"] = record.get("status", "")
                row[f"{subtest}_output_tokens"] = record.get("output_tokens", "")
                row[f"{subtest}_tokens_per_second"] = record.get("generation_tokens_per_second", "")
                row[f"{subtest}_response"] = record.get("response", "")

    return sorted(
        grouped.values(),
        key=lambda row: (
            str(row["run_id"]),
            str(row["model"]),
            LANGUAGE_ORDER.get(str(row["language"]), len(LANGUAGE_ORDER)),
            int(row["repetition"]),
        ),
    )


def write_csv(output: TextIO, rows: list[dict[str, object]]) -> None:
    writer = csv.DictWriter(output, fieldnames=FIELDS, extrasaction="ignore")
    writer.writeheader()
    writer.writerows(rows)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jsonl", type=Path, nargs="+", help="one or more benchmark JSONL result files")
    parser.add_argument("-o", "--output", type=Path, help="new CSV path (defaults to stdout)")
    args = parser.parse_args()
    rows = export_rows(args.jsonl)
    if args.output is None:
        write_csv(sys.stdout, rows)
    else:
        with args.output.open("x", encoding="utf-8", newline="") as output:
            write_csv(output, rows)


if __name__ == "__main__":
    try:
        main()
    except ValueError as error:
        raise SystemExit(str(error)) from error
