#!/usr/bin/env python3
"""Summarise subagent runs from omp session logs into docs/agent-performance/.

Reads every subagent session (`<Name>.jsonl`) under one omp project-session folder and writes:

* ``runs.csv``    one row per agent: model, provider, reasoning effort, size and cost of the run
* ``summary.md``  runs grouped by model and reasoning effort, joined with the hand-written
                  ratings in ``ratings.csv``

``ratings.csv`` is the only hand-maintained file (agent, grade, note). Grades: A = every assigned
story done and verified, B = mostly done with minor gaps, C = partial or needed a re-run, D = quit
early, broke the build or reported unverified work, Q = cut off by a usage quota (not rated).

Usage: scripts/agent-stats.py [SESSION_DIR]
The session folder defaults to the newest folder under ~/.omp/agent/sessions/ for this project.
The script reads session logs only; it never prints message content.
"""

from __future__ import annotations

import csv
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "docs" / "agent-performance"
GRADE_POINTS = {"A": 4, "B": 3, "C": 2, "D": 1}
QUOTA = re.compile(r"usage limit|quota|rate limit|resource_exhausted", re.IGNORECASE)


def default_session_dir() -> Path:
    root = Path.home() / ".omp" / "agent" / "sessions"
    candidates = [p for p in root.glob("*ai-coding-config-studio*") if p.is_dir()]
    if not candidates:
        sys.exit("no omp session folder found; pass SESSION_DIR")
    project = max(candidates, key=lambda p: p.stat().st_mtime)
    runs = [p for p in project.iterdir() if p.is_dir()]
    return max(runs, key=lambda p: p.stat().st_mtime) if runs else project


def parse_time(text: str) -> datetime | None:
    try:
        return datetime.fromisoformat(text.replace("Z", "+00:00"))
    except (TypeError, ValueError):
        return None


def wave_of(name: str) -> str:
    match = re.match(r"W(\d+)", name)
    return match.group(1) if match else "1-3"


def summarise(path: Path) -> dict | None:
    models: list[str] = []
    levels: list[str] = []
    first = last = None
    assistant = tools = 0
    tokens_in = tokens_out = 0
    cost = 0.0
    stop = Counter()
    tail_text = ""
    for line in path.read_text(errors="ignore").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        stamp = parse_time(event.get("timestamp", ""))
        if stamp:
            first = first or stamp
            last = stamp
        kind = event.get("type")
        if kind == "model_change":
            models.append(event.get("model", ""))
        elif kind == "thinking_level_change":
            levels.append(str(event.get("thinkingLevel", "")))
        elif kind == "message":
            message = event.get("message", {})
            if message.get("role") != "assistant":
                continue
            assistant += 1
            usage = message.get("usage") or {}
            tokens_in += int(usage.get("input", 0) or 0)
            tokens_out += int(usage.get("output", 0) or 0)
            cost += float((usage.get("cost") or {}).get("total", 0) or 0)
            stop[message.get("stopReason", "")] += 1
            content = message.get("content")
            if isinstance(content, list):
                tools += sum(1 for block in content if isinstance(block, dict) and block.get("type") == "toolCall")
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "text":
                        tail_text = str(block.get("text", ""))[-400:]
            if message.get("errorMessage"):
                tail_text = str(message["errorMessage"])[-400:]
    if not models:
        return None
    minutes = round((last - first).total_seconds() / 60, 1) if first and last else 0.0
    model = models[0]
    provider, _, bare = model.partition("/")
    if not bare:
        provider, bare = "", model
    return {
        "agent": path.stem,
        "wave": wave_of(path.stem),
        "provider": provider,
        "model": bare,
        "models_used": ";".join(dict.fromkeys(models)),
        "reasoning_effort": ";".join(dict.fromkeys(levels)) or "unrecorded",
        "minutes": minutes,
        "assistant_turns": assistant,
        "tool_calls": tools,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "cost_usd": round(cost, 4),
        "quota_stop": "yes" if QUOTA.search(tail_text) else "",
    }


def read_ratings() -> dict[str, tuple[str, str]]:
    path = OUT / "ratings.csv"
    if not path.exists():
        return {}
    with path.open(newline="") as handle:
        return {row["agent"]: (row["grade"], row["note"]) for row in csv.DictReader(handle)}


def main() -> None:
    session_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_session_dir()
    rows = [row for path in sorted(session_dir.glob("*.jsonl")) if (row := summarise(path))]
    OUT.mkdir(parents=True, exist_ok=True)
    ratings = read_ratings()
    fields = list(rows[0]) + ["grade", "note"]
    with (OUT / "runs.csv").open("w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            grade, note = ratings.get(row["agent"], ("", ""))
            writer.writerow({**row, "grade": grade, "note": note})

    groups: dict[tuple[str, str, str], list[dict]] = defaultdict(list)
    for row in rows:
        groups[(row["provider"], row["model"], row["reasoning_effort"])].append(row)
    lines = [
        "# Subagent performance by model and reasoning effort",
        "",
        "Generated by `scripts/agent-stats.py` from omp session logs; grades come from "
        "`ratings.csv` (A=4 B=3 C=2 D=1, Q = cut off by a usage quota, not scored).",
        "",
        "| Provider | Model | Reasoning | Agents | Rated | Avg grade | A | B | C | D | Quota cuts | Avg min | Cost USD |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    for (provider, model, effort), members in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        grades = [ratings.get(m["agent"], ("", ""))[0] for m in members]
        scored = [GRADE_POINTS[g] for g in grades if g in GRADE_POINTS]
        average = f"{sum(scored) / len(scored):.2f}" if scored else "n/a"
        counts = Counter(grades)
        quota = sum(1 for m, g in zip(members, grades) if g == "Q" or m["quota_stop"])
        minutes = sum(m["minutes"] for m in members) / len(members)
        cost = sum(m["cost_usd"] for m in members)
        lines.append(
            f"| {provider or '-'} | {model} | {effort} | {len(members)} | {len(scored)} | {average} | "
            f"{counts['A']} | {counts['B']} | {counts['C']} | {counts['D']} | {quota} | "
            f"{minutes:.0f} | {cost:.2f} |"
        )
    lines += ["", "## Per wave", "", "| Wave | Model | Reasoning | Agents | Avg grade |", "|---|---|---|---|---|"]
    waves: dict[tuple[str, str, str], list[str]] = defaultdict(list)
    for row in rows:
        waves[(row["wave"], row["model"], row["reasoning_effort"])].append(ratings.get(row["agent"], ("", ""))[0])
    for (wave, model, effort), grades in sorted(waves.items(), key=lambda kv: (kv[0][0].zfill(4), kv[0][1])):
        scored = [GRADE_POINTS[g] for g in grades if g in GRADE_POINTS]
        average = f"{sum(scored) / len(scored):.2f}" if scored else "n/a"
        lines.append(f"| {wave} | {model} | {effort} | {len(grades)} | {average} |")
    (OUT / "summary.md").write_text("\n".join(lines) + "\n")
    print(f"{len(rows)} agents from {session_dir.name} -> {OUT}")


if __name__ == "__main__":
    main()
