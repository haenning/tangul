#!/usr/bin/env python3
"""Turns `cargo llvm-cov report --json --summary-only` into a Markdown table per crate
and per file, plus a shields.io endpoint badge.

    coverage-summary.py cov.json summary.md badge.json

Crates holding the safety rules and I/O (CLAUDE.md → Safety rules) aim for TARGET.
"""

import json
import sys
from collections import defaultdict

TARGET = 95.0
EDGE = {"core", "drivers", "engine", "assist", "tunnel", "data", "service"}


def pct(covered, count):
    return 100.0 * covered / count if count else 100.0


def mark(p, edge):
    if not edge:
        return ""
    return "✅" if p >= TARGET else "⚠️"


def main(src, summary_path, badge_path):
    data = json.load(open(src))["data"][0]
    crates = defaultdict(lambda: [0, 0])
    files = []
    for f in data["files"]:
        path = f["filename"]
        if "/crates/" not in path:
            continue
        rel = path.split("/crates/", 1)[1]
        crate = rel.split("/", 1)[0]
        lines = f["summary"]["lines"]
        crates[crate][0] += lines["covered"]
        crates[crate][1] += lines["count"]
        files.append((rel, lines["covered"], lines["count"], crate in EDGE))

    edge_cov = sum(c[0] for k, c in crates.items() if k in EDGE)
    edge_count = sum(c[1] for k, c in crates.items() if k in EDGE)
    edge = pct(edge_cov, edge_count)
    total = data["totals"]["lines"]["percent"]

    out = [
        "## Test coverage (lines)",
        "",
        f"**Safety and I/O crates: {edge:.1f}%** (target {TARGET:.0f}%) · all crates: {total:.1f}%",
        "",
        "| Crate | Lines | Covered | |",
        "|---|---:|---:|---|",
    ]
    for crate in sorted(crates, key=lambda k: (k not in EDGE, k)):
        covered, count = crates[crate]
        p = pct(covered, count)
        out.append(f"| {crate} | {count} | {p:.1f}% | {mark(p, crate in EDGE)} |")
    out += [
        "",
        "<details><summary>Files</summary>",
        "",
        "| File | Lines | Covered |",
        "|---|---:|---:|",
    ]
    for rel, covered, count, is_edge in sorted(files):
        p = pct(covered, count)
        flag = " ⚠️" if is_edge and p < TARGET else ""
        out.append(f"| {rel} | {count} | {p:.1f}%{flag} |")
    out += ["", "</details>", ""]
    open(summary_path, "w").write("\n".join(out))

    color = "brightgreen" if edge >= TARGET else "yellow" if edge >= 80 else "red"
    badge = {
        "schemaVersion": 1,
        "label": "coverage",
        "message": f"{edge:.0f}%",
        "color": color,
    }
    open(badge_path, "w").write(json.dumps(badge))


if __name__ == "__main__":
    main(*sys.argv[1:4])
