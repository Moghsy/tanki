#!/usr/bin/env python3
"""Build the pinned upstream Python binding locally, without a binary download.

Use a Python installation containing Anki's Python runtime dependencies. This
script never imports aqt or opens a collection. Cargo source dependencies must
already be cached; builds run offline. Generated files stay under .tools/target.
"""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
sys.dont_write_bytecode = True

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument("--python", required=True)
args = parser.parse_args()
protoc = os.environ.get("PROTOC", "protoc")
subprocess.run([protoc, "--version"], check=True)
bridge = root / ".tools/python-bridge"
bridge.mkdir(parents=True, exist_ok=True)
manifest = bridge / "Cargo.toml"
manifest.write_text('''[workspace]
[package]
name = "rsbridge"
version = "0.0.0"
edition = "2021"
license = "AGPL-3.0-or-later"
[lib]
name = "rsbridge"
crate-type = ["cdylib"]
path = "../../vendor/anki/pylib/rsbridge/lib.rs"
[dependencies]
anki = { path = "../../vendor/anki/rslib", features = ["rustls"] }
pyo3 = { version = "0.29.0", features = ["extension-module", "abi3", "abi3-py39"] }
''')
if not (bridge / "Cargo.lock").exists():
    shutil.copyfile(root / "vendor/anki/Cargo.lock", bridge / "Cargo.lock")
env = dict(os.environ, PYO3_PYTHON=args.python, PROTOC=protoc, STRINGS_PY=str(bridge / "_fluent.py"))
subprocess.run(["cargo", "build", "--offline", "--manifest-path", str(manifest), "--target-dir", str(root / "target/python-bridge")], cwd=root, env=env, check=True)
package_root = root / ".tools/pinned-python"
package = package_root / "anki"
shutil.copytree(root / "vendor/anki/pylib/anki", package, dirs_exist_ok=True)
# Isolate this source build from the installed namespace package.
(package / "__init__.py").write_text("")
shutil.copyfile(root / "target/python-bridge/debug/librsbridge.so", package / "_rsbridge.so")
shutil.copyfile(bridge / "_fluent.py", package / "_fluent.py")
shutil.copyfile(root / "vendor/anki/out/pylib/anki/_backend_generated.py", package / "_backend_generated.py")
version = (root / "vendor/anki/.version").read_text().strip()
bridge_hash = subprocess.check_output([args.python, "-c", "from anki import _rsbridge; print(_rsbridge.buildhash())"], env=dict(os.environ, PYTHONPATH=str(package_root), PYTHONDONTWRITEBYTECODE="1"), text=True).strip()
(package / "buildinfo.py").write_text(f"version = {version!r}\nbuildhash = {bridge_hash!r}\n")
proto_root = root / "vendor/anki/proto"
subprocess.run([protoc, "-I", str(proto_root), f"--python_out={package_root}", *map(str, sorted((proto_root / "anki").glob("*.proto")))], check=True)
# Use upstream hook generation verbatim, omitting only its optional ruff formatter.
sys.path.insert(0, str(root / "vendor/anki/pylib/tools"))
sys.path.insert(0, str(root / "vendor/anki/pylib/anki/_vendor"))
import genhooks
code = genhooks.prefix + "\n" + "".join(h.code() for h in sorted(genhooks.hooks, key=lambda h: h.name)) + "\n" + genhooks.suffix
(package / "hooks_gen.py").write_text(code)
env = dict(os.environ, PYTHONPATH=str(package_root), PYTHONDONTWRITEBYTECODE="1")
subprocess.run([args.python, "-c", "import anki.buildinfo; from anki.collection import Collection; print('Pinned Python package:', anki.buildinfo.version, anki.buildinfo.buildhash)"], env=env, check=True)
print(f"PYTHONPATH={package_root}")
