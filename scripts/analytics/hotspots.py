#!/usr/bin/env python3
"""Hotspots: files that are big and change often. Joins code-maat's `revisions`
analysis (CSV on stdin) with the files' current line counts.

    java -jar code-maat.jar -l git.log -c git2 -a revisions | hotspots.py [root] [N]

Score = revisions × lines, relative to the top file. A hotspot is where a file's
size costs time on every change: the first place to split.
"""

import csv
import sys
from pathlib import Path

ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
TOP = int(sys.argv[2]) if len(sys.argv) > 2 else 15

rows = []
for row in csv.DictReader(sys.stdin):
    path = ROOT / row["entity"]
    if path.suffix != ".rs" or not path.exists():
        continue  # deleted or renamed since
    lines = path.read_text().count("\n")
    rows.append((int(row["n-revs"]) * lines, int(row["n-revs"]), lines, row["entity"]))
rows.sort(reverse=True)
top = rows[0][0] if rows else 1
print(f"{'score':>5} {'changes':>7} {'lines':>6}  file")
for score, revs, lines, entity in rows[:TOP]:
    print(f"{100 * score / top:5.0f} {revs:7} {lines:6}  {entity}")
