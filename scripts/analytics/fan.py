#!/usr/bin/env python3
"""Fan-in and fan-out per module: which modules use it, which it uses.

A module is a crate's top-level module (`assist::lexer`, `app::workspace`; files in a
folder belong to it) or the crate root (`core::(root)`, where re-exported types
land). Uses come from `use` trees (grouped ones expanded) and inline paths
(`crate::x::…`, `tangul_y::x::…`).

    fan.py [repo root]    → table on stdout, sorted by fan-in

Reading it: a module many use (fan-in) should use little itself (fan-out); both high
is a hub where changes ripple both ways. Instability = out / (in + out): near 0 for
foundations, near 1 for the edges that assemble everything.
"""

import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
CRATES = ROOT / "crates"


def crate_name(crate_dir):
    text = (crate_dir / "Cargo.toml").read_text()
    name = re.search(r'^name\s*=\s*"([^"]+)"', text, re.M).group(1)
    # The binary is `tangul`; its code is the app.
    return {"tangul": "app"}.get(name, name.removeprefix("tangul-"))


def modules_of(src):
    """Top-level module names of a crate's src folder."""
    names = set()
    for p in src.iterdir():
        if p.suffix == ".rs" and p.stem not in ("lib", "main"):
            names.add(p.stem)
        elif p.is_dir():
            names.add(p.name)
    return names


def strip(text):
    """Source without comments and string contents (they hold fake paths)."""
    text = re.sub(r"//[^\n]*", "", text)
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    return re.sub(r'"(?:\\.|[^"\\])*"', '""', text)


def expand(tree):
    """Paths of a use tree: `a::{b::c, d}` → [a::b::c, a::d]."""
    tree = tree.strip()
    i = tree.find("{")
    if i < 0:
        return [tree]
    prefix, inner = tree[:i], tree[i + 1 : tree.rindex("}")]
    parts, depth, start = [], 0, 0
    for j, c in enumerate(inner):
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(inner[start:j])
            start = j + 1
    parts.append(inner[start:])
    return [path for part in parts if part.strip() for path in expand(prefix + part.strip())]


def main():
    crates = {}  # rust ident (tangul_x) → (name, modules)
    files = []  # (crate name, module, path)
    for crate_dir in sorted(p for p in CRATES.iterdir() if (p / "Cargo.toml").exists()):
        name = crate_name(crate_dir)
        src = crate_dir / "src"
        mods = modules_of(src)
        crates["tangul_" + name] = (name, mods)
        if name == "app":
            crates["tangul"] = (name, mods)
        for f in src.rglob("*.rs"):
            rel = f.relative_to(src).parts
            module = "(root)" if rel[0] in ("lib.rs", "main.rs") else rel[0].removesuffix(".rs")
            files.append((name, mods, module, f))

    uses = defaultdict(set)  # (crate, module) → {(crate, module)}
    lines = defaultdict(int)
    for name, mods, module, f in files:
        raw = f.read_text()
        lines[(name, module)] += raw.count("\n")
        text = strip(raw)
        # Test modules use more than the code does; count the code.
        cut = text.find("#[cfg(test)]")
        if cut >= 0:
            text = text[:cut]
        paths = []
        for m in re.finditer(r"\buse\s+([^;]+);", text):
            paths.extend(expand(m.group(1)))
        paths.extend(m.group(0) for m in re.finditer(r"\b(?:crate|tangul_\w+)::\w+", text))
        me = (name, module)
        for path in paths:
            segs = [s.strip() for s in path.split("::")]
            if segs[0] == "crate":
                target_crate, target_mods = name, mods
            elif segs[0] in crates:
                target_crate, target_mods = crates[segs[0]]
            else:
                continue
            seg = segs[1] if len(segs) > 1 else ""
            target = (target_crate, seg if seg in target_mods else "(root)")
            if target != me:
                uses[me].add(target)

    users = defaultdict(set)
    for src_mod, targets in uses.items():
        for t in targets:
            users[t].add(src_mod)
    rows = []
    for key in lines:
        fan_in, fan_out = len(users[key]), len(uses[key])
        inst = fan_out / (fan_in + fan_out) if fan_in + fan_out else 0.0
        rows.append((f"{key[0]}::{key[1]}", lines[key], fan_in, fan_out, inst))
    rows.sort(key=lambda r: (-r[2], -r[3]))
    print(f"{'module':32} {'lines':>6} {'fan-in':>7} {'fan-out':>8} {'instab.':>8}  flag")
    for mod, n, fi, fo, inst in rows:
        flag = "hub" if fi >= 5 and fo >= 5 else ""
        print(f"{mod:32} {n:6} {fi:7} {fo:8} {inst:8.2f}  {flag}")


if __name__ == "__main__":
    main()
