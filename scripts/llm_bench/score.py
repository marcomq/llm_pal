#!/usr/bin/env python3
"""Scores scored-mode runs (`PAL_BENCH_MODE=scored`) against the expected answers in the test file.

Every item gets `pass`, `fail` or, for a grammar correction that matches no accepted wording, `review`. Each
instructions reply also counts as a writing item `<id>-grammar`, judged by hand: no grammar or spelling errors, and it must make sense. Write
`pass` or `fail` into the `verdict` column of the review CSV and pass it back with `--verdicts`: those rows then
count like the others. No overall score: one percentage per model, category and language.
"""

import argparse
import csv
import json
import re
import sys
import unicodedata
from collections import defaultdict
from pathlib import Path

CATEGORIES = ["knowledge", "grammar", "writing", "instructions", "json", "tools"]
FREE_REPLY_RULE = ("free reply: no grammar or spelling errors, in the asked language, understandable and fitting the "
                   "question, not only repeating it (case ignored for the lowercase task)")
LABEL = re.compile(
    r"^(korrigiert|korrektur|corrected|correction|corrigé|corrigée|correction|corregido|corregida|corrección)"
    r"[^:\n]{0,20}:\s*",
    re.IGNORECASE,
)


def strip_thinking(text):
    """Without reasoning blocks; one cut off by the token limit leaves nothing."""
    return re.sub(r"<think>.*?(</think>|$)", "", text, flags=re.DOTALL).strip()


def fold(text):
    """Lowercase, without accents, digit groups joined (1.440 → 1440)."""
    text = text.lower().replace("œ", "oe").replace("æ", "ae")
    text = "".join(c for c in unicodedata.normalize("NFKD", text) if not unicodedata.combining(c))
    return re.sub(r"(?<=\d)[.,   ](?=\d{3}\b)", "", text)


def has_word(text, word, prefix=False):
    """`word` as a whole word, or with `prefix` as the start of one (bleu in bleue)."""
    end = "" if prefix else r"(?!\w)"
    return re.search(rf"(?<!\w){re.escape(word)}{end}", text) is not None


def sentence_line(text):
    """The first line of a reply, without a label, quotes or Markdown emphasis."""
    lines = [line.strip() for line in text.replace("```", "").splitlines() if line.strip()]
    line = LABEL.sub("", lines[0]) if lines else ""
    line = line.replace("**", "").replace("’", "'").strip()
    line = line.strip("\"'«»“”„ ").strip()
    return re.sub(r"\s+", " ", line)


def check_knowledge(reply, expect):
    if expect.get("case_sensitive"):
        return "pass" if any(has_word(reply, word) for word in expect["any_word"]) else "fail"
    return "pass" if any(has_word(fold(reply), word) for word in expect["any_word"]) else "fail"


def check_grammar(reply, prompt, expect):
    got = sentence_line(reply)
    if "unchanged" in expect:
        return "pass" if got == prompt else "fail"
    if got in expect["correction"]:
        return "pass"
    return "fail" if got == prompt else "review"


def sentences(text):
    return [s for s in re.split(r"(?<=[.!?])\s+", text.strip()) if re.search(r"\w", s)]


def check_instructions(reply, expect):
    text = reply.strip()
    words = re.findall(r"\w+", text)
    ok = True
    if "sentences" in expect:
        ok &= len(sentences(text)) == expect["sentences"] and "\n-" not in text
    if "bullets" in expect:
        lines = [line.strip() for line in text.splitlines() if line.strip()]
        ok &= len(lines) == expect["bullets"] and all(line.startswith("- ") for line in lines)
    if "max_words" in expect:
        ok &= 0 < len(words) <= expect["max_words"]
    if "min_words" in expect:
        ok &= len(words) >= expect["min_words"]
    if "must_include" in expect:
        forms = [word if isinstance(word, list) else [word] for word in expect["must_include"]]
        ok &= all(any(has_word(fold(text), fold(form)) for form in words) for words in forms)
    if "one_of" in expect:
        ok &= re.sub(r"[^\w]", "", fold(text)) in expect["one_of"]
    if expect.get("lowercase"):
        ok &= bool(words) and text == text.lower()
    return "pass" if ok else "fail"


def parse_json(reply):
    """(value, strict): strict means the reply is nothing but JSON. Lenient parsing drops code fences."""
    try:
        return json.loads(reply), True
    except ValueError:
        pass
    fenced = re.search(r"```(?:json)?\s*(.*?)```", reply, re.DOTALL)
    candidate = fenced.group(1) if fenced else reply
    start = min((i for i in (candidate.find("{"), candidate.find("[")) if i >= 0), default=-1)
    if start < 0:
        return None, False
    try:
        value, _ = json.JSONDecoder().raw_decode(candidate[start:])
        return value, False
    except ValueError:
        return None, False


def same(got, want):
    if isinstance(want, bool) or want is None:
        return got is want
    if isinstance(want, (int, float)):
        return isinstance(got, (int, float)) and not isinstance(got, bool) and abs(got - want) < 1e-9
    if isinstance(want, str):
        return isinstance(got, str) and got == want
    if isinstance(want, list):
        return isinstance(got, list) and len(got) == len(want) and all(map(same, got, want))
    return isinstance(got, dict) and got.keys() == want.keys() and all(same(got[k], want[k]) for k in want)


def check_json(reply, expect):
    value, strict = parse_json(reply)
    want = expect["json"]
    if "equals" in want:
        ok = same(value, want["equals"])
    else:
        ok = (isinstance(value, dict) and set(want["keys"]) <= value.keys()
              and all(same(value[k], v) for k, v in want["fields"].items())
              and all(isinstance(value[k], (int, float)) and not isinstance(value[k], bool) and lo <= value[k] <= hi
                      for k, (lo, hi) in want.get("ranges", {}).items()))
    return ("pass" if ok else "fail"), value is not None, strict


def same_argument(got, want, integer=False):
    if want == "*":
        return isinstance(got, str) and got.strip() != ""
    if isinstance(want, list):
        return any(same_argument(got, w) for w in want)
    if isinstance(want, str):
        return isinstance(got, str) and got.strip().lower() == want.lower()
    if integer and not isinstance(got, int):
        return False
    return same(got, want)


def same_call(got, want, integer_args):
    """Llama 3's own tool format calls the arguments `parameters`; both count."""
    args = got.get("arguments", got.get("parameters")) if isinstance(got, dict) else None
    if not isinstance(args, dict) or got.get("name") != want["name"]:
        return False
    allowed = set(want["arguments"]) | set(want.get("optional", []))
    return (set(want["arguments"]) <= args.keys() <= allowed
            and all(same_argument(args[k], v, k in integer_args) for k, v in want["arguments"].items()))


def check_tools(reply, expect, integer_args):
    value, strict = parse_json(reply)
    want = expect["calls"]
    ok = isinstance(value, list) and len(value) == len(want)
    if ok:
        left = list(value)
        for call in want:
            match = next((g for g in left if same_call(g, call, integer_args)), None)
            if match is None:
                ok = False
                break
            left.remove(match)
    return ("pass" if ok else "fail"), isinstance(value, list), strict


def integer_arguments(system):
    """Names of the tools' integer parameters, from the tool list in the system prompt."""
    match = re.search(r"^\[.*\]$", system, re.MULTILINE)
    tools = json.loads(match.group(0)) if match else []
    return {name for tool in tools for name, p in tool["parameters"].items() if p.get("type") == "integer"}


def load_rows(paths):
    rows = []
    for path in paths:
        with open(path, encoding="utf-8") as file:
            for number, line in enumerate(file, 1):
                if not line.strip():
                    continue
                row = json.loads(line)
                if row.get("record_type") == "subtest" and row.get("mode") == "scored":
                    row["_source"] = f"{Path(path).name}:{number}"
                    rows.append(row)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("tests", help="test file, e.g. tests-round3.json")
    parser.add_argument("results", nargs="+", help="scored-mode JSONL files")
    parser.add_argument("--verdicts", help="review CSV with a filled-in verdict column")
    parser.add_argument("--output-dir", default="scores", help="directory for the CSV and Markdown output")
    args = parser.parse_args()

    suites = json.load(open(args.tests, encoding="utf-8"))["suites"]
    items = {item["id"]: (suite, item) for suite in suites for item in suite["items"]}
    verdicts = {}
    if args.verdicts:
        with open(args.verdicts, encoding="utf-8", newline="") as file:
            for row in csv.DictReader(file):
                if row["verdict"].strip() in ("pass", "fail"):
                    verdicts[(row["model"], row["test_id"])] = row["verdict"].strip()

    results, review, seen = [], [], set()
    for row in load_rows(args.results):
        test_id, model = row["test_id"], row["model"]
        if test_id not in items:
            sys.exit(f"{row['_source']}: unknown test id {test_id}")
        if (model, test_id) in seen:
            sys.exit(f"{row['_source']}: {model} {test_id} appears twice; pass one run per model")
        seen.add((model, test_id))
        suite, item = items[test_id]
        reply = strip_thinking(row["response"])
        valid = strict = ""
        if row["status"] != "complete":
            verdict = "fail"
        elif suite["category"] == "knowledge":
            verdict = check_knowledge(reply, item["expect"])
        elif suite["category"] == "grammar":
            verdict = check_grammar(reply, item["prompt"], item["expect"])
        elif suite["category"] == "instructions":
            verdict = check_instructions(reply, item["expect"])
        elif suite["category"] == "json":
            verdict, valid, strict = check_json(reply, item["expect"])
        else:
            verdict, valid, strict = check_tools(reply, item["expect"], integer_arguments(suite["system"]))
        if verdict == "review":
            verdict = verdicts.get((model, test_id), "review")
            if verdict == "review":
                accepted = item["expect"].get("correction", [])
                review.append({"model": model, "test_id": test_id, "prompt": item["prompt"],
                               "accepted": " | ".join(accepted), "reply": sentence_line(reply), "verdict": ""})
        results.append({"model": model, "language": row["language"], "category": suite["category"],
                        "test_id": test_id, "verdict": verdict, "parsed": valid, "strict_json": strict,
                        "status": row["status"], "output_tokens": row["output_tokens"],
                        "tokens_per_second": round(row["generation_tokens_per_second"], 2),
                        "prompt_read_ms": row["prompt_read_ms"], "response": row["response"]})
        if suite["category"] == "instructions":
            grammar_id = f"{test_id}-grammar"
            verdict = "fail" if row["status"] != "complete" or not reply.strip() else \
                verdicts.get((model, grammar_id), "review")
            if verdict == "review":
                review.append({"model": model, "test_id": grammar_id, "prompt": item["prompt"],
                               "accepted": FREE_REPLY_RULE, "reply": reply.strip(), "verdict": ""})
            results.append({**results[-1], "category": "writing", "test_id": grammar_id, "verdict": verdict})

    if not results:
        sys.exit("no scored-mode rows in the results")
    out = Path(args.output_dir)
    out.mkdir(parents=True, exist_ok=True)
    write_csv(out / "items.csv", results)
    if review:
        write_csv(out / "review-todo.csv", review)

    groups = defaultdict(lambda: defaultdict(int))
    for r in results:
        for key in ((r["model"], r["category"], r["language"]), (r["model"], r["category"], "all")):
            groups[key][r["verdict"]] += 1
            groups[key]["n"] += 1
            groups[key]["parsed"] += r["parsed"] is True
            groups[key]["strict"] += r["strict_json"] is True
    # Items of the run's languages that a model has no row for, e.g. after a deadline or a failed model.
    run_languages = {r["language"] for r in results}
    expected = defaultdict(set)
    for suite in suites:
        if suite["language"] in run_languages:
            for item in suite["items"]:
                expected[(suite["category"], suite["language"])].add(item["id"])
                if suite["category"] == "instructions":
                    expected[("writing", suite["language"])].add(f"{item['id']}-grammar")
    for model in {r["model"] for r in results}:
        for (category, language), ids in expected.items():
            got = {r["test_id"] for r in results
                   if r["model"] == model and r["category"] == category and r["language"] == language}
            for key in ((model, category, language), (model, category, "all")):
                groups[key]["missing"] += len(ids - got)
    summary = []
    for (model, category, language), g in sorted(groups.items()):
        decided = g["pass"] + g["fail"]
        summary.append({"model": model, "category": category, "language": language, "items": g["n"],
                        "pass": g["pass"], "fail": g["fail"], "review": g["review"], "missing": g["missing"],
                        "pass_pct": round(100 * g["pass"] / decided, 1) if decided else "",
                        "json_parsed": g["parsed"] if category in ("json", "tools") else "",
                        "json_strict": g["strict"] if category in ("json", "tools") else ""})
    write_csv(out / "scores.csv", summary)

    by = {(s["model"], s["category"], s["language"]): s for s in summary}
    models = sorted({s["model"] for s in summary},
                    key=lambda m: -sum(by.get((m, c, "all"), {}).get("pass_pct") or 0 for c in CATEGORIES))
    cell = lambda s: "–" if s is None else (f"{s['pass_pct']}" + (f" ({s['review']}?)" if s["review"] else "")
                                             + (f" (incomplete: {s['missing']} missing)" if s["missing"] else ""))
    lines = ["| Model | " + " | ".join(CATEGORIES) + " |", "|---|" + "---:|" * len(CATEGORIES)]
    lines += [f"| {m} | " + " | ".join(cell(by.get((m, c, "all"))) for c in CATEGORIES) + " |" for m in models]
    languages = sorted({s["language"] for s in summary if s["language"] != "all"})
    for category in ("knowledge", "grammar", "writing", "instructions"):
        lines += ["", f"**{category}** by language", "", "| Model | " + " | ".join(languages) + " |",
                  "|---|" + "---:|" * len(languages)]
        lines += [f"| {m} | " + " | ".join(cell(by.get((m, category, l))) for l in languages) + " |" for m in models]
    (out / "scores.md").write_text("Pass rate in % (unreviewed items in parentheses; incomplete groups say how many items are missing).\n\n" + "\n".join(lines) + "\n")
    print("\n".join(lines))
    if review:
        print(f"\n{len(review)} replies to review: {out / 'review-todo.csv'}")


def write_csv(path, rows):
    with open(path, "w", encoding="utf-8", newline="") as file:
        writer = csv.DictWriter(file, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)


if __name__ == "__main__":
    main()
