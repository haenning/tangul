#!/usr/bin/env python3
"""Run by `make release` (scripts/release.sh) once the version is bumped:

    scripts/release-migrations.py 0.1.3

For each settings file (crates/data/migrations/<file>/), the steps made while this
version was developed (`0.1.3-dev.N.json`) become one step, `0.1.3.json`, their
operations in order; and the layout this release writes becomes a fixture
(`fixtures/<file>/0.1.3.json`) if it differs from the last one. See CLAUDE.md there.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent / "crates" / "data" / "migrations"
DEV = re.compile(r"^(\d+\.\d+\.\d+)-dev\.(\d+)\.json$")
RELEASE = re.compile(r"^(\d+)\.(\d+)\.(\d+)\.json$")


def merge_dev_steps(kind: str, version: str) -> None:
    folder = ROOT / kind
    steps = []
    for path in folder.glob("*.json"):
        match = DEV.match(path.name)
        if not match:
            continue
        if match.group(1) != version:
            sys.exit(f"{path}: a step for {match.group(1)}, but this is {version}")
        steps.append((int(match.group(2)), path))
    if not steps:
        return
    target = folder / f"{version}.json"
    if target.exists():
        sys.exit(f"{target} exists already")
    ops = []
    for _, path in sorted(steps):
        ops += json.loads(path.read_text())
    target.write_text(json.dumps(ops, indent=2) + "\n")
    for _, path in steps:
        path.unlink()
    print(f"{kind}: {len(steps)} dev steps are now {target.name}")


def keep_fixture(kind: str, version: str) -> None:
    layout = subprocess.run(
        ["cargo", "run", "-q", "-p", "tangul-data", "--example", "layout", "--", kind],
        check=True, capture_output=True, text=True,
    ).stdout
    folder = ROOT / "fixtures" / kind
    folder.mkdir(parents=True, exist_ok=True)
    released = [p for p in folder.glob("*.json") if RELEASE.match(p.name)]
    released.sort(key=lambda p: tuple(int(n) for n in RELEASE.match(p.name).groups()))

    def without_version(text: str) -> dict:
        value = json.loads(text)
        value.pop("version", None)
        return value

    if released and without_version(released[-1].read_text()) == without_version(layout):
        return
    (folder / f"{version}.json").write_text(layout)
    print(f"{kind}: new layout, kept as fixtures/{kind}/{version}.json")


def main() -> None:
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("usage: release-migrations.py x.y.z")
    version = sys.argv[1]
    for kind in ("settings", "database"):
        merge_dev_steps(kind, version)
        keep_fixture(kind, version)


if __name__ == "__main__":
    main()
