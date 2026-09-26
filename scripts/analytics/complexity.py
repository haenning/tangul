#!/usr/bin/env python3
"""Summary of `rust-code-analysis-cli -m -O json` output: the most complex functions
and the files with the most to follow. Test modules are left out. Closures count
as functions.

    complexity.py <json dir> [repo root] [top N]

- cognitive complexity: how hard a function is to follow (nesting counts extra);
  over ~15 is worth a look, over ~25 is hard to review.
- cyclomatic complexity: paths through a function (tests needed to cover it).
- per file: the sum of its functions' cognitive complexity (how much there is to
  hold in your head) and the worst one. (rust-code-analysis's maintainability
  index isn't shown: for files it bottoms out at 0 from ~400 lines on.)
"""

import json
import sys
from pathlib import Path

DIR = Path(sys.argv[1])
ROOT = Path(sys.argv[2] if len(sys.argv) > 2 else ".").resolve()
TOP = int(sys.argv[3]) if len(sys.argv) > 3 else 15


def functions(space, path, owner=""):
    if space.get("name") == "tests":
        return
    if space["kind"] == "function":
        m = space["metrics"]
        yield {
            "name": f"{owner}{space['name']}",
            "file": path,
            "line": space["start_line"],
            "cognitive": m["cognitive"]["sum"],
            "cyclomatic": m["cyclomatic"]["sum"],
            "sloc": m["loc"]["sloc"],
            "args": m["nargs"]["total_functions"] if "total_functions" in m["nargs"] else 0,
        }
    prefix = f"{space['name']}::" if space["kind"] in ("impl", "trait") else owner
    for child in space.get("spaces", []):
        yield from functions(child, path, prefix)


funcs, files = [], []
for f in sorted(DIR.rglob("*.json")):
    data = json.loads(f.read_text())
    path = Path(data["name"]).resolve()
    try:
        path = path.relative_to(ROOT)
    except ValueError:
        pass
    funcs.extend(functions(data, path))
    mine = [fn for fn in funcs if fn["file"] == path]
    files.append(
        (
            sum(fn["cognitive"] for fn in mine),
            max((fn["cognitive"] for fn in mine), default=0),
            data["metrics"]["loc"]["sloc"],
            len(mine),
            path,
        )
    )

print(f"Most complex functions (top {TOP}, by cognitive complexity)")
print(f"{'cognitive':>9} {'cyclomatic':>10} {'lines':>6}  function")
for fn in sorted(funcs, key=lambda f: -f["cognitive"])[:TOP]:
    print(
        f"{fn['cognitive']:9.0f} {fn['cyclomatic']:10.0f} {fn['sloc']:6.0f}  "
        f"{fn['name']}  ({fn['file']}:{fn['line']})"
    )
print()
print(f"Longest functions (top {TOP}, source lines)")
for fn in sorted(funcs, key=lambda f: -f["sloc"])[:TOP]:
    print(f"{fn['sloc']:6.0f}  {fn['name']}  ({fn['file']}:{fn['line']})")
print()
print(f"Files with the most to follow (top {TOP}, summed cognitive complexity)")
print(f"{'total':>6} {'worst':>6} {'lines':>6} {'fns':>5}  file")
for total, worst, sloc, n, path in sorted(files, key=lambda f: -f[0])[:TOP]:
    print(f"{total:6.0f} {worst:6.0f} {sloc:6.0f} {n:5}  {path}")
over = [fn for fn in funcs if fn["cognitive"] > 25]
print()
print(f"{len(funcs)} functions; {len(over)} over cognitive complexity 25")
