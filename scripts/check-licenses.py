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

# Explicitly reviewed alternatives, rather than accepting every SPDX OR.
# self_cell 1.3.0 ships LICENSE-APACHE and offers Apache-2.0 OR GPL-2.0-only;
# tanki uses the Apache-2.0 option. Exact version and expression must match.
LICENSE_CHOICES = {
    ("self_cell", "1.3.0", "Apache-2.0 OR GPL-2.0-only"): "Apache-2.0",
}


def accepted(expression):
    if not expression:
        return False
    tokens = re.findall(r"[A-Za-z0-9.+-]+", expression)
    return bool(tokens) and all(t in ALLOWED or t in {"AND", "OR", "WITH"} for t in tokens)


def selected_packages(metadata, tree):
    # Cargo metadata also includes disabled optional backends (e.g. termwiz),
    # even with --filter-platform. cargo tree uses the enabled feature graph.
    selected = set(re.findall(r"^(\S+) v(\S+)", tree, re.MULTILINE))
    if not selected:
        raise ValueError("cargo tree returned no packages")
    packages = [p for p in metadata["packages"] if (p["name"], p["version"]) in selected]
    missing = selected - {(p["name"], p["version"]) for p in packages}
    if missing:
        raise ValueError(f"active packages absent from cargo metadata: {sorted(missing)}")
    return packages


def package_license(package):
    key = (package["name"], package["version"], package["license"])
    return LICENSE_CHOICES.get(key, package["license"])


def main():
    target = "x86_64-unknown-linux-gnu"
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--format-version", "1", "--locked", "--all-features",
        "--filter-platform", target,
    ]))
    tree = subprocess.check_output([
        "cargo", "tree", "--workspace", "--all-features", "--locked", "--target", target,
        "--prefix", "none", "--format", "{p}",
    ], text=True)
    packages = selected_packages(metadata, tree)
    failures = [p for p in packages if not accepted(package_license(p))]
    for p in failures:
        print(f"REJECT {p['name']} {p['version']}: {p['license']}", file=sys.stderr)
    if failures:
        return 1
    print(f"License metadata check passed: {len(packages)} active packages; no unknown or forbidden license IDs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
