#!/usr/bin/env python3
"""The test report of a CI run, from what the run left behind:

    test-report.py coverage.json junit.xml docker-compose.yml OUT_DIR

- coverage.json: `cargo llvm-cov report --json --summary-only`
- junit.xml: cargo-nextest's JUnit report (profile `ci`)
- docker-compose.yml: the database images the live tests ran against

Writes to OUT_DIR: summary.md (tests and coverage per crate and file), tests.json,
and three badges: tests.svg, coverage.svg (the safety and I/O crates, which aim for
TARGET) and databases.svg.
"""

import json
import os
import re
import sys
import xml.etree.ElementTree as ET
from collections import defaultdict

TARGET = 95.0
# The crates holding the safety rules and I/O (CLAUDE.md → Safety rules).
EDGE = {"core", "drivers", "engine", "assist", "tunnel", "data", "service", "llm", "ai"}


def pct(covered, count):
    return 100.0 * covered / count if count else 100.0


def crate_of_binary(name):
    """`tangul-engine::live` or `tangul-engine` → `engine`; the app is `tangul`."""
    package = name.split("::", 1)[0].split("$", 1)[0]
    if package == "tangul":
        return "app"
    return package.removeprefix("tangul-")


def tests(junit_path):
    """Per crate: passed, failed, skipped."""
    per = defaultdict(lambda: [0, 0, 0])
    if not os.path.exists(junit_path):
        return per
    for suite in ET.parse(junit_path).getroot().iter("testsuite"):
        crate = crate_of_binary(suite.get("name", ""))
        for case in suite.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                per[crate][1] += 1
            elif case.find("skipped") is not None:
                per[crate][2] += 1
            else:
                per[crate][0] += 1
    return per


def coverage(cov_path):
    """Per crate: covered and total lines; and per file."""
    crates = defaultdict(lambda: [0, 0])
    files = []
    if not os.path.exists(cov_path):
        return crates, files
    data = json.load(open(cov_path))["data"][0]
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
    return crates, files


def databases(compose_path):
    """The images of the database services: `postgres 16`, `mysql 8.4`."""
    out = []
    for image in re.findall(r"^\s*image:\s*(\S+)", open(compose_path).read(), re.M):
        name, _, tag = image.partition(":")
        if name in ("postgres", "mysql", "mariadb"):
            out.append(f"{name} {tag or 'latest'}")
    return out


def badge(label, message, color):
    """A flat badge, sized by its text (about 6.5 px per character at 11 px)."""
    lw, mw = 10 + int(6.5 * len(label)), 10 + int(6.5 * len(message))
    w = lw + mw
    colors = {"green": "#3fb950", "yellow": "#d29922", "red": "#f85149", "blue": "#58a6ff"}
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="20" role="img" aria-label="{label}: {message}">
<rect width="{lw}" height="20" fill="#555"/><rect x="{lw}" width="{mw}" height="20" fill="{colors[color]}"/>
<g fill="#fff" font-family="Verdana,DejaVu Sans,sans-serif" font-size="11" text-anchor="middle">
<text x="{lw / 2}" y="14">{label}</text><text x="{lw + mw / 2}" y="14">{message}</text></g></svg>
"""


def main(cov_path, junit_path, compose_path, out_dir):
    os.makedirs(out_dir, exist_ok=True)
    per_tests = tests(junit_path)
    crates, files = coverage(cov_path)
    dbs = databases(compose_path)

    passed = sum(t[0] for t in per_tests.values())
    failed = sum(t[1] for t in per_tests.values())
    edge_cov = sum(c[0] for k, c in crates.items() if k in EDGE)
    edge_count = sum(c[1] for k, c in crates.items() if k in EDGE)
    edge = pct(edge_cov, edge_count)
    app = pct(*crates["app"]) if "app" in crates else None
    total = pct(sum(c[0] for c in crates.values()), sum(c[1] for c in crates.values()))

    out = [
        "## Tests",
        "",
        f"**{passed} passed, {failed} failed** · live against {', '.join(dbs)} and an SSH bastion",
        "",
        "## Coverage (lines)",
        "",
        f"**Safety and I/O crates: {edge:.1f}%** (target {TARGET:.0f}%)"
        + (f" · app: {app:.1f}%" if app is not None else "")
        + f" · all: {total:.1f}%",
        "",
        "| Crate | Tests | Lines | Covered | |",
        "|---|---:|---:|---:|---|",
    ]
    for crate in sorted(set(crates) | set(per_tests), key=lambda k: (k not in EDGE, k)):
        t = per_tests.get(crate, [0, 0, 0])
        tests_cell = f"{t[0]}" + (f" ({t[1]} failed)" if t[1] else "")
        covered, count = crates.get(crate, [0, 0])
        p = pct(covered, count)
        flag = ("✅" if p >= TARGET else "⚠️") if crate in EDGE else ""
        out.append(f"| {crate} | {tests_cell} | {count} | {p:.1f}% | {flag} |")
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
    open(os.path.join(out_dir, "summary.md"), "w").write("\n".join(out))

    report = {
        "passed": passed,
        "failed": failed,
        "crates": {
            k: {"passed": v[0], "failed": v[1], "skipped": v[2]} for k, v in sorted(per_tests.items())
        },
        "coverage": {
            "safety_and_io": round(edge, 1),
            "app": round(app, 1) if app is not None else None,
            "all": round(total, 1),
        },
        "databases": dbs,
    }
    open(os.path.join(out_dir, "tests.json"), "w").write(json.dumps(report, indent=2) + "\n")

    color = "green" if edge >= TARGET else "yellow" if edge >= 80 else "red"
    for name, svg in [
        ("tests.svg", badge("tests", f"{passed} passed" if not failed else f"{failed} failed",
                            "red" if failed or not passed else "green")),
        ("coverage.svg", badge("coverage", f"{edge:.0f}%", color)),
        ("databases.svg", badge("tested on", " · ".join(dbs) or "none", "blue")),
    ]:
        open(os.path.join(out_dir, name), "w").write(svg)
    print("\n".join(out[:8]))


if __name__ == "__main__":
    main(*sys.argv[1:5])
