#!/usr/bin/env python3
"""Fields per struct and methods per type: where one type holds or does too much.

Counts named fields of `struct X { … }` and the `fn`s in every `impl X` and
`impl Trait for X` block (across files, so a type split over several files still
counts as one). Test code is left out.

    shapes.py [repo root] [top N]   → two tables on stdout
"""

import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
TOP = int(sys.argv[2]) if len(sys.argv) > 2 else 15


def strip(text):
    text = re.sub(r"//[^\n]*", "", text)
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"'(?:\\.|[^'\\\n])'", "' '", text)  # char literals like '{'
    return re.sub(r'"(?:\\.|[^"\\])*"', '""', text)


def block(text, open_at):
    """The text between the brace at `open_at` and its partner."""
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[open_at + 1 : i]
    return text[open_at + 1 :]


def main():
    fields = {}
    methods = defaultdict(int)
    where = {}
    for f in sorted((ROOT / "crates").rglob("*.rs")):
        if "/tests/" in str(f):
            continue
        text = strip(f.read_text())
        cut = text.find("#[cfg(test)]")
        if cut >= 0:
            text = text[:cut]
        crate = f.relative_to(ROOT / "crates").parts[0]
        for m in re.finditer(r"\bstruct\s+(\w+)(?:<[^>{]*>)?\s*(?:where[^{]*)?\{", text):
            body = block(text, m.end() - 1)
            # Top-level `name:` entries only (nested braces are types or defaults).
            depth, count = 0, 0
            for line in body.split("\n"):
                if depth == 0 and re.match(r"\s*(pub(\([^)]*\))?\s+)?\w+\s*:", line):
                    count += 1
                depth += line.count("{") + line.count("(") + line.count("<")
                depth -= line.count("}") + line.count(")") + line.count(">")
                depth = max(depth, 0)
            key = f"{crate}::{m.group(1)}"
            fields[key] = count
            where[key] = f.relative_to(ROOT)
        for m in re.finditer(r"\bimpl\b(?:<[^{]*?>)?\s+([^{]+?)\s*\{", text):
            head = m.group(1)
            head = head.split(" for ")[-1] if " for " in head else head
            name = re.sub(r"<.*", "", head.split("where")[0]).strip().split("::")[-1]
            if not re.fullmatch(r"\w+", name):
                continue
            body = block(text, m.end() - 1)
            methods[f"{crate}::{name}"] += len(re.findall(r"\bfn\s+\w+", body))

    print(f"Fields per struct (top {TOP})")
    print(f"{'type':36} {'fields':>6}  file")
    for key, n in sorted(fields.items(), key=lambda kv: -kv[1])[:TOP]:
        print(f"{key:36} {n:6}  {where[key]}")
    print()
    print(f"Methods per type, all impl blocks (top {TOP})")
    print(f"{'type':36} {'methods':>7}")
    for key, n in sorted(methods.items(), key=lambda kv: -kv[1])[:TOP]:
        print(f"{key:36} {n:7}")


if __name__ == "__main__":
    main()
