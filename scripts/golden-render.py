#!/usr/bin/env python3
# Copyright: tanki contributors
# License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html
"""Write golden card renders from Anki's own Python package.

tanki-core re-implements the steps pylib performs after Anki's Rust template
engine (custom filters, {{FrontSide}}, AV-tag extraction, LaTeX). This script
renders every card of tanki's synthetic fixture collection with
`card.render_output()` from pylib at the pinned Anki version and writes the
result as JSON. `crates/tanki-core/tests/golden_render.rs` compares tanki's
renders with that file in plain `cargo test`, without Python.

Regenerate on every Anki bump (run from the repository root; nothing is
downloaded, and the script refuses any other Anki version than the one in
vendor/anki/.version):

    PYTHONPATH="$PWD/.tools/pinned-python" PYTHONDONTWRITEBYTECODE=1 \
        ~/.local/share/AnkiProgramFiles/.venv/bin/python \
        scripts/golden-render.py tests/fixtures/golden/render.json

`.tools/pinned-python` is built by scripts/build-pinned-python.py (in a
worktree, point PYTHONPATH at the main checkout's copy). Without
--collection the script runs
`cargo run -p xtask --features fixtures -- fixtures --base <new temp dir>`
to create the fixture, renders it, and deletes the temp dir. With
--collection it renders an existing fixture collection, which must live under
the system temp dir and must not be open in another process.

Card keys are "<note type name>|<template ordinal>|<label>", where the label
comes from the note's single `golden::<label>` tag (see
crates/tanki-core/src/fixture.rs), so they never depend on ids or timestamps.

Rules: imports `anki` only (never `aqt`), opens only temp collections, never
runs a LaTeX binary (pylib's generator is replaced by a function that fails).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LABEL_PREFIX = "golden::"


def fail(message: str) -> None:
    sys.exit(f"golden-render: {message}")


def pinned_version() -> str:
    return (ROOT / "vendor" / "anki" / ".version").read_text(encoding="utf8").strip()


def check_safe_collection(path: Path) -> Path:
    real = path.resolve()
    temp = Path(tempfile.gettempdir()).resolve()
    if not real.is_relative_to(temp):
        fail(f"{real} is not under the system temp dir {temp}; refusing to open it")
    if real.name != "collection.anki2" or not real.is_file():
        fail(f"{real} is not a fixture collection.anki2")
    return real


def make_fixture(base: Path) -> Path:
    result = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "xtask",
            "--features",
            "fixtures",
            "--",
            "fixtures",
            "--base",
            str(base),
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    lines = result.stdout.strip().splitlines()
    if not lines:
        fail("xtask printed no profile folder")
    return Path(lines[-1]) / "collection.anki2"


def checked_anki_version() -> str:
    import anki.buildinfo

    version = anki.buildinfo.version
    if version != pinned_version():
        fail(
            f"anki {version} imported from "
            f"{os.path.dirname(anki.buildinfo.__file__)}, but vendor/anki pins "
            f"{pinned_version()}; set PYTHONPATH to the pinned build "
            "(.tools/pinned-python)"
        )
    return version


def av_tag(tag: object) -> dict:
    from anki.sound import SoundOrVideoTag, TTSTag

    if isinstance(tag, SoundOrVideoTag):
        return {"sound_or_video": tag.filename}
    if isinstance(tag, TTSTag):
        return {
            "tts": {
                "field_text": tag.field_text,
                "lang": tag.lang,
                "voices": list(tag.voices),
                "speed": tag.speed,
                "other_args": list(tag.other_args),
            }
        }
    fail(f"unknown AV tag {tag!r}")
    raise AssertionError


def render_all(collection: Path) -> dict:
    from anki.collection import Collection  # before anki.latex (import cycle)

    import anki.latex

    version = checked_anki_version()

    def no_latex_binaries(*_args: object, **_kwargs: object) -> str | None:
        raise RuntimeError("golden-render must not run LaTeX; ship the image instead")

    anki.latex._save_latex_image = no_latex_binaries  # type: ignore[assignment]

    col = Collection(str(collection))
    try:
        cards: dict[str, dict] = {}
        for card_id in col.find_cards(""):
            card = col.get_card(card_id)
            note = card.note()
            labels = [t for t in note.tags if t.startswith(LABEL_PREFIX)]
            if len(labels) != 1:
                fail(f"note of card {card_id} has labels {labels}, expected one")
            key = f"{note.note_type()['name']}|{card.ord}|{labels[0][len(LABEL_PREFIX):]}"
            if key in cards:
                fail(f"duplicate card key {key}")
            out = card.render_output(reload=True, browser=False)
            cards[key] = {
                "question": out.question_text,
                "answer": out.answer_text,
                "css": out.css,
                "question_av_tags": [av_tag(t) for t in out.question_av_tags],
                "answer_av_tags": [av_tag(t) for t in out.answer_av_tags],
            }
    finally:
        col.close()
    return {
        "anki_version": version,
        "generated_by": "scripts/golden-render.py",
        "cards": cards,
    }


def main() -> None:
    if not os.environ.get("PYTHONDONTWRITEBYTECODE"):
        fail("set PYTHONDONTWRITEBYTECODE=1")
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("output", type=Path, help="golden JSON file to write")
    parser.add_argument(
        "--collection",
        type=Path,
        help="existing fixture collection.anki2 under the temp dir "
        "(default: generate one with xtask)",
    )
    args = parser.parse_args()
    checked_anki_version()

    base: Path | None = None
    try:
        if args.collection:
            collection = check_safe_collection(args.collection)
        else:
            base = Path(tempfile.mkdtemp(prefix="tanki-golden-"))
            collection = check_safe_collection(make_fixture(base))
        golden = render_all(collection)
    finally:
        if base is not None:
            shutil.rmtree(base, ignore_errors=True)

    text = json.dumps(golden, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text, encoding="utf8", newline="\n")
    print(f"wrote {len(golden['cards'])} cards to {args.output}")


if __name__ == "__main__":
    main()
