#!/usr/bin/env python3
"""Conservative license-ID allowlist for the resolved Linux dependency graph.

Both sides of every AND/OR expression must be allowed. Unknown identifiers and
missing metadata fail closed; this is a metadata gate, not a source-file audit.
"""
import json
import re
import subprocess
import sys

ALLOWED = {
    "0BSD", "AGPL-3.0-or-later", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause",
    "BSL-1.0", "CC0-1.0", "CDLA-Permissive-2.0", "ISC", "LGPL-3.0-or-later",
    "MIT", "MIT-0", "MPL-2.0", "Unicode-3.0", "Unlicense", "Zlib",
    "LLVM-exception",
}


def accepted(expression):
    if not expression:
        return False
    tokens = re.findall(r"[A-Za-z0-9.+-]+", expression)
    return bool(tokens) and all(t in ALLOWED or t in {"AND", "OR", "WITH"} for t in tokens)


def main():
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--locked",
        "--filter-platform", "x86_64-unknown-linux-gnu",
    ]))
    failures = [p for p in metadata["packages"] if not accepted(p["license"])]
    for p in failures:
        print(f"REJECT {p['name']} {p['version']}: {p['license']}", file=sys.stderr)
    if failures:
        return 1
    print(f"License metadata check passed: {len(metadata['packages'])} packages; no unknown or forbidden license IDs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
