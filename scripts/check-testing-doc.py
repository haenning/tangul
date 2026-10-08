#!/usr/bin/env python3
"""Checks that every test docs/TESTING.md names exists, so its list of what backs
each promise can't go stale. A test is written `crates/<path>.rs::<function>`.

    check-testing-doc.py docs/TESTING.md
"""

import re
import sys


def main(doc):
    text = open(doc).read()
    refs = re.findall(r"`(crates/[^`:]+\.rs)::([A-Za-z0-9_]+)`", text)
    if not refs:
        sys.exit(f"{doc}: names no tests")
    missing = []
    for path, name in refs:
        try:
            source = open(path).read()
        except OSError:
            missing.append(f"{path} (no such file)")
            continue
        if not re.search(rf"\bfn {re.escape(name)}\b", source):
            missing.append(f"{path}::{name}")
    if missing:
        sys.exit(f"{doc} names tests that don't exist:\n  " + "\n  ".join(missing))
    print(f"{doc}: {len(refs)} tests, all there")


if __name__ == "__main__":
    main(sys.argv[1])
