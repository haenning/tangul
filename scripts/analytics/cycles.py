#!/usr/bin/env python3
"""Cycles between modules, from `cargo modules dependencies` DOT output on stdin.

`cargo modules --acyclic` also counts a type and its own methods as a cycle, so the
graph is filtered to modules first (see run.sh) and the cycles are found here:
strongly connected components of the "uses" edges, leaving out a module's edges to
its own parent and children.

    cargo modules dependencies … | cycles.py   → cycles, exit 1 if there are any
"""

import re
import sys
from collections import defaultdict

def related(a, b):
    """A module and its own parent or child: one type split over files
    (`use super::…` in the child, `pub use child::…` in the parent) is not a cycle."""
    return a.startswith(b + "::") or b.startswith(a + "::")


edges = defaultdict(set)
for a, b in re.findall(r'"([^"]+)" -> "([^"]+)" \[label="uses"', sys.stdin.read()):
    if a != b and not related(a, b):
        edges[a].add(b)

# Tarjan's strongly connected components.
index, low, on_stack, stack, sccs = {}, {}, set(), [], []


def visit(v):
    index[v] = low[v] = len(index)
    stack.append(v)
    on_stack.add(v)
    for w in edges[v]:
        if w not in index:
            visit(w)
            low[v] = min(low[v], low[w])
        elif w in on_stack:
            low[v] = min(low[v], index[w])
    if low[v] == index[v]:
        scc = []
        while True:
            w = stack.pop()
            on_stack.discard(w)
            scc.append(w)
            if w == v:
                break
        if len(scc) > 1:
            sccs.append(sorted(scc))


sys.setrecursionlimit(10_000)
for v in list(edges):
    if v not in index:
        visit(v)

if not sccs:
    print("no cycles between modules")
for scc in sccs:
    print("cycle: " + " ⇄ ".join(scc))
    for v in scc:
        for w in sorted(edges[v] & set(scc)):
            print(f"    {v} → {w}")
sys.exit(1 if sccs else 0)
