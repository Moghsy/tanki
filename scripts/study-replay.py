#!/usr/bin/env python3
# Copyright: tanki contributors
# License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html
"""Python side of tanki's Phase 1 exit test (crates/tanki-core/tests/study_replay.rs).

The Rust test drives tanki-core and this script in lock step on two copies
of the same closed fixture collection: at every step both sides fetch the
next card, the test checks they offer the same card, and both answer it with
the same rating, `answered_at_millis` and `milliseconds_taken`. Afterwards
the script reads the card and note service messages and exports the review
log through Anki’s public dataset service so the test can compare them.

It is a long-running process speaking JSON lines: one request object per
line on stdin, one response object per line on stdout. Anki's scheduler
reads the wall clock (learning due times, modification times), so the two
sides must answer within the same second; a separate process per step would
be too slow.

Requests (`cmd`):
  version                                   -> {"version"}
  open {path}                               -> {}
  select {deck_id}                          -> {"counts": [new, learning, review]}
  queue                                     -> {"card_id"|null, "queue", "counts"}
  answer {card_id, rating, answered_at_millis, milliseconds_taken}
                                            -> {"leech", "suspended"}
  close                                     -> {}
  dump {path}                               -> {"cards", "revlog", "notes"}
  quit                                      -> (exits)
Errors come back as {"error": message}.

Run only by the test, with the interpreter of Anki's launcher and
PYTHONPATH pointing at the pinned Python package (built from vendor/anki by
scripts/build-pinned-python.py), for example:

    TANKI_PINNED_PYTHON=~/.local/share/AnkiProgramFiles/.venv/bin/python \\
    TANKI_PINNED_PYTHONPATH="$PWD/.tools/pinned-python" \\
        cargo test -p tanki-core --test study_replay -- --nocapture

Rules: imports `anki` only (never `aqt`); refuses any Anki version other
than vendor/anki/.version; opens only `collection.anki2` files in folders
under the system temp dir, never anything under the real Anki base folder;
only one collection is open at a time; no raw SQL or DB proxy calls.
Dataset export preserves every scheduling log field, but upstream deliberately
normalizes its sync USN to zero, so this test does not compare revlog USNs.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import traceback
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent


class Refused(Exception):
    pass


def pinned_version() -> str:
    return (ROOT / "vendor" / "anki" / ".version").read_text(encoding="utf8").strip()


def check_version() -> str:
    import anki.buildinfo

    version = anki.buildinfo.version
    expected = pinned_version()
    if version != expected:
        raise Refused(
            f"anki {version} imported from {os.path.dirname(anki.buildinfo.__file__)}, "
            f"but vendor/anki pins {expected}; set PYTHONPATH to the pinned build"
        )
    return version


def check_path(raw: str) -> str:
    path = Path(raw).resolve()
    temp = Path(tempfile.gettempdir()).resolve()
    forbidden = [Path.home() / ".local/share/Anki2"]
    if os.environ.get("ANKI_BASE"):
        forbidden.append(Path(os.environ["ANKI_BASE"]))
    reasons = []
    if path.name != "collection.anki2":
        reasons.append("not a collection.anki2 file")
    if not path.is_relative_to(temp) or path.parent == temp:
        reasons.append(f"not inside a folder under the system temp dir {temp}")
    for base in forbidden:
        if path.is_relative_to(base.resolve()):
            reasons.append(f"inside the real Anki base {base}")
    if (path.parent / "prefs21.db").exists() or (path.parent.parent / "prefs21.db").exists():
        reasons.append("next to a prefs21.db (looks like a real profile)")
    if not path.is_file():
        reasons.append("does not exist")
    if reasons:
        raise Refused(f"refusing {path}: " + "; ".join(reasons))
    return str(path)


class Session:
    def __init__(self) -> None:
        self.col = None
        # What reviewer.py keeps for the card on screen.
        self.card = None
        self.states = None

    def open(self, path: str) -> None:
        from anki.collection import Collection

        if self.col is not None:
            raise Refused("a collection is already open")
        checked = check_path(path)
        (Path(checked).parent / "backups").mkdir(exist_ok=True)
        self.col = Collection(checked)
        # Match the actor's backup-before-command rule. Closing and reopening
        # reacquires the POSIX locks released by backup file reads.
        self.col.create_backup(
            backup_folder=str(Path(checked).parent / "backups"),
            force=True,
            wait_for_completion=True,
        )
        self.col.close()
        self.col = Collection(checked)
        if not self.col.v3_scheduler():
            raise Refused("fixture is not on the v3 scheduler")

    def close(self) -> None:
        if self.col is not None:
            try:
                self.col.await_backup_completion()
            finally:
                self.col.close()
        self.col = None
        self.card = None
        self.states = None

    def select(self, deck_id: int) -> dict:
        # Deck browser "open" -> overview, as desktop does.
        self.col.decks.select(deck_id)
        return {"counts": list(self.col.sched.counts())}

    def queue(self) -> dict:
        from anki.cards import Card

        queued = self.col.sched.get_queued_cards(fetch_limit=1)
        counts = [queued.new_count, queued.learning_count, queued.review_count]
        if not queued.cards:
            self.card = self.states = None
            return {"card_id": None, "queue": None, "counts": counts}
        top = queued.cards[0]
        # reviewer.py V3CardInfo.from_queue
        states = top.states
        states.current.custom_data = top.card.custom_data
        card = Card(self.col)
        card._load_from_backend_card(top.card)
        card.start_timer()
        self.card, self.states = card, states
        return {"card_id": card.id, "queue": top.queue, "counts": counts}

    def answer(self, req: dict) -> dict:
        from anki.scheduler.v3 import CardAnswer

        if self.card is None or self.card.id != req["card_id"]:
            raise Refused(f"card {req['card_id']} is not the queued card")
        rating = [CardAnswer.AGAIN, CardAnswer.HARD, CardAnswer.GOOD, CardAnswer.EASY][
            req["rating"]
        ]
        answer = self.col.sched.build_answer(
            card=self.card, states=self.states, rating=rating
        )
        # The test's fixed timestamps instead of the wall clock.
        answer.answered_at_millis = req["answered_at_millis"]
        answer.milliseconds_taken = req["milliseconds_taken"]
        self.col.sched.answer_card(answer)
        # Reviewer._answerCard after_answer
        self.card.load()
        suspended = self.card.queue < 0
        leech = self.col.sched.state_is_leech(answer.new_state)
        self.card = self.states = None
        return {"leech": leech, "suspended": suspended}

    def dump(self, path: str) -> dict:
        self.open(path)
        try:
            from anki.stats_pb2 import Dataset

            cards = [self.col._backend.get_card(cid)
                     for cid in sorted(self.col.find_cards(""))]
            note_ids = sorted({card.note_id for card in cards})
            notes = [self.col._backend.get_note(nid) for nid in note_ids]
            with tempfile.TemporaryDirectory(prefix="tanki-replay-export-") as tmp:
                target = Path(tmp) / "dataset.pb"
                self.col._backend.export_dataset(min_entries=0, target_path=str(target))
                dataset = Dataset.FromString(target.read_bytes())
            def optional(card, field):
                return getattr(card, field) if card.HasField(field) else None
            return {
                "cards": [[
                    c.id, c.note_id, c.deck_id, c.template_idx, c.mtime_secs,
                    c.usn, c.ctype, c.queue, c.due, c.interval, c.ease_factor,
                    c.reps, c.lapses, c.remaining_steps, c.original_due,
                    c.original_deck_id, c.flags, optional(c, "original_position"),
                    [c.memory_state.stability, c.memory_state.difficulty]
                    if c.HasField("memory_state") else None,
                    optional(c, "desired_retention"), optional(c, "decay"),
                    optional(c, "last_review_time_secs"), c.custom_data,
                ] for c in cards],
                "revlog": [[
                    e.id, e.cid, e.button_chosen, e.interval, e.last_interval,
                    e.ease_factor, e.taken_millis, e.review_kind,
                ] for e in sorted(dataset.revlogs, key=lambda e: e.id)],
                "notes": [[
                    n.id, n.mtime_secs, n.usn, list(n.tags), list(n.fields),
                    n.guid, n.notetype_id,
                ] for n in notes],
            }
        finally:
            self.close()


def handle(session: Session, req: dict) -> dict:
    cmd = req.get("cmd")
    if cmd == "version":
        return {"version": check_version()}
    if cmd == "open":
        session.open(req["path"])
        return {}
    if cmd == "select":
        return session.select(req["deck_id"])
    if cmd == "queue":
        return session.queue()
    if cmd == "answer":
        return session.answer(req)
    if cmd == "close":
        session.close()
        return {}
    if cmd == "dump":
        return session.dump(req["path"])
    raise Refused(f"unknown command {cmd!r}")


def main() -> None:
    check_version()
    session = Session()
    try:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            req = json.loads(line)
            if req.get("cmd") == "quit":
                break
            try:
                out = handle(session, req)
            except Refused as e:
                out = {"error": str(e)}
            except Exception:
                out = {"error": traceback.format_exc()}
            print(json.dumps(out), flush=True)
    finally:
        session.close()


if __name__ == "__main__":
    try:
        main()
    except Refused as e:
        print(json.dumps({"error": str(e)}), flush=True)
        sys.exit(2)
