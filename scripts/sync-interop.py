#!/usr/bin/env python3
# Copyright: tanki contributors
# License: GNU AGPL, version 3 or later; http://www.gnu.org/licenses/agpl.html
"""Python side of tanki's Phase 2 exit test (crates/tanki-core/tests/sync_interop.rs).

One Anki sync server runs on loopback. tanki syncs one collection with it;
this script drives a second collection with Anki's own pinned Python package
(`anki.collection.Collection`: `sync_login`, `sync_collection`,
`full_upload_or_download`, `sync_media`, `media_sync_status`), so the test can
check that edits, reviews and media made on either side arrive on the other.

It speaks JSON lines: one request object per line on stdin, one response
object per line on stdout. Requests (`cmd`):
  version                                  -> {"version"}
  open {path}                              -> {}   (creates the collection if missing)
  login {endpoint, username, password}     -> {}
  sync {allow_full_download}               -> {"required", "full_download"}
  media                                    -> {"polls"}
  add_note {front, back, media_name?, media_hex?}
                                           -> {"note_id", "media_name"}
  edit_back {query, back}                  -> {"note_id"}
  answer {count}                           -> {"card_ids"}
  find_notes {query}                       -> {"note_ids"}
  close                                    -> {}
  dump {path}                              -> {"cards", "revlog", "notes"}
  quit                                     -> (exits)
Errors come back as {"error": message}.

Rules: imports `anki` only (never `aqt`); refuses any Anki version other than
vendor/anki/.version; touches only `collection.anki2` files in folders under
the system temp dir, never anything under a real Anki base; syncs only with an
explicit http://127.0.0.1 or http://[::1] endpoint (an auth without an
endpoint would mean AnkiWeb, so it is refused); no raw SQL or DB proxy calls.
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
import time
import traceback
from pathlib import Path
from urllib.parse import urlparse

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


def check_path(raw: str, must_exist: bool) -> str:
    # Reject known real bases lexically before touching the filesystem.
    absolute = Path(os.path.abspath(raw))
    forbidden = [
        Path.home() / ".local/share/Anki2",
        Path.home() / ".var/app/net.ankiweb.Anki/data/Anki2",
    ]
    if os.environ.get("XDG_DATA_HOME"):
        forbidden.append(Path(os.environ["XDG_DATA_HOME"]) / "Anki2")
    if os.environ.get("ANKI_BASE"):
        forbidden.append(Path(os.environ["ANKI_BASE"]))
    forbidden = [Path(os.path.abspath(base)) for base in forbidden]
    if any(absolute.is_relative_to(base) for base in forbidden):
        raise Refused(f"refusing {absolute}: inside a real Anki base")
    if any(part in ("prefs21.db", "addons21") for part in absolute.parts):
        raise Refused(f"refusing {absolute}: forbidden profile entry")
    path = absolute.resolve()
    temp = Path(tempfile.gettempdir()).resolve()
    reasons = []
    if path.name != "collection.anki2":
        reasons.append("not a collection.anki2 file")
    if not path.is_relative_to(temp) or path.parent == temp:
        reasons.append(f"not inside a folder under the system temp dir {temp}")
    for base in forbidden:
        if path.is_relative_to(base.parent.resolve() / base.name):
            reasons.append(f"inside the real Anki base {base}")
    if (path.parent / "prefs21.db").exists() or (path.parent.parent / "prefs21.db").exists():
        reasons.append("next to a prefs21.db (looks like a real profile)")
    if must_exist and not path.is_file():
        reasons.append("does not exist")
    if not path.parent.is_dir():
        reasons.append("folder does not exist")
    if reasons:
        raise Refused(f"refusing {path}: " + "; ".join(reasons))
    return str(path)


def check_endpoint(endpoint: str | None) -> str:
    if not endpoint:
        raise Refused("refusing a sync auth without an endpoint (that would mean AnkiWeb)")
    url = urlparse(endpoint)
    if url.scheme != "http" or url.hostname not in ("127.0.0.1", "::1"):
        raise Refused(f"refusing endpoint {endpoint!r}: loopback http only in this test")
    return endpoint


class Session:
    def __init__(self) -> None:
        self.col = None
        self.auth = None

    def open(self, path: str, must_exist: bool = False) -> None:
        from anki.collection import Collection

        if self.col is not None:
            raise Refused("a collection is already open")
        self.col = Collection(check_path(path, must_exist))

    def close(self) -> None:
        if self.col is not None:
            try:
                self.col.await_backup_completion()
            finally:
                self.col.close()
        self.col = None

    def need_col(self):
        if self.col is None:
            raise Refused("no collection is open")
        return self.col

    def need_auth(self):
        if self.auth is None:
            raise Refused("not logged in")
        check_endpoint(self.auth.endpoint)
        return self.auth

    def login(self, req: dict) -> dict:
        endpoint = check_endpoint(req["endpoint"])
        auth = self.need_col().sync_login(
            username=req["username"], password=req["password"], endpoint=endpoint
        )
        # The backend keeps the endpoint it was given; insist on it.
        if auth.endpoint != endpoint:
            raise Refused(f"login returned endpoint {auth.endpoint!r}, not {endpoint!r}")
        self.auth = auth
        return {}

    def sync(self, req: dict) -> dict:
        from anki.sync_pb2 import SyncCollectionResponse

        col = self.need_col()
        auth = self.need_auth()
        out = col.sync_collection(auth, False)
        if out.new_endpoint:
            raise Refused(f"unexpected redirect to {out.new_endpoint!r}")
        required = SyncCollectionResponse.ChangesRequired.Name(out.required)
        full_download = False
        if out.required == SyncCollectionResponse.FULL_DOWNLOAD and req.get(
            "allow_full_download"
        ):
            # aqt/sync.py full_download: close, transfer, reopen.
            col.close_for_full_sync()
            try:
                col.full_upload_or_download(auth=auth, server_usn=None, upload=False)
            finally:
                col.reopen(after_full_sync=True)
            full_download = True
        return {"required": required, "full_download": full_download}

    def media(self) -> dict:
        col = self.need_col()
        col.sync_media(self.need_auth())
        polls = 0
        # aqt/mediasync.py: poll until inactive; errors surface here.
        while col.media_sync_status().active:
            polls += 1
            if polls > 1200:
                raise Refused("media sync did not finish")
            time.sleep(0.05)
        col.media_sync_status()
        return {"polls": polls}

    def add_note(self, req: dict) -> dict:
        col = self.need_col()
        media_name = None
        front = req["front"]
        if req.get("media_name"):
            media_name = col.media.write_data(
                req["media_name"], bytes.fromhex(req["media_hex"])
            )
            front += f'<img src="{media_name}">'
        note = col.new_note(col.models.by_name("Basic"))
        note["Front"] = front
        note["Back"] = req["back"]
        col.add_note(note, col.decks.id_for_name("Default"))
        return {"note_id": note.id, "media_name": media_name}

    def edit_back(self, req: dict) -> dict:
        col = self.need_col()
        ids = col.find_notes(req["query"])
        if len(ids) != 1:
            raise Refused(f"{req['query']!r} matched {len(ids)} notes")
        note = col.get_note(ids[0])
        note["Back"] = req["back"]
        col.update_note(note)
        return {"note_id": note.id}

    def answer(self, req: dict) -> dict:
        from anki.cards import Card
        from anki.scheduler.v3 import CardAnswer

        col = self.need_col()
        answered = []
        for _ in range(req["count"]):
            queued = col.sched.get_queued_cards(fetch_limit=1)
            if not queued.cards:
                break
            top = queued.cards[0]
            # reviewer.py V3CardInfo.from_queue
            states = top.states
            states.current.custom_data = top.card.custom_data
            card = Card(col)
            card._load_from_backend_card(top.card)
            card.start_timer()
            answer = col.sched.build_answer(
                card=card, states=states, rating=CardAnswer.GOOD
            )
            col.sched.answer_card(answer)
            answered.append(card.id)
        return {"card_ids": answered}

    def find_notes(self, req: dict) -> dict:
        return {"note_ids": list(self.need_col().find_notes(req["query"]))}

    def dump(self, path: str) -> dict:
        self.open(path, must_exist=True)
        try:
            from anki.stats_pb2 import Dataset

            col = self.col
            cards = [col._backend.get_card(cid) for cid in sorted(col.find_cards(""))]
            note_ids = sorted(col.find_notes(""))
            notes = [col._backend.get_note(nid) for nid in note_ids]
            with tempfile.TemporaryDirectory(prefix="tanki-interop-export-") as tmp:
                target = Path(tmp) / "dataset.pb"
                col._backend.export_dataset(min_entries=0, target_path=str(target))
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
    if cmd == "login":
        return session.login(req)
    if cmd == "sync":
        return session.sync(req)
    if cmd == "media":
        return session.media()
    if cmd == "add_note":
        return session.add_note(req)
    if cmd == "edit_back":
        return session.edit_back(req)
    if cmd == "answer":
        return session.answer(req)
    if cmd == "find_notes":
        return session.find_notes(req)
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
