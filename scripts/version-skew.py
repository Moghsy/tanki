#!/usr/bin/env python3
"""Spike D driver: let an installed Anki Python package touch a fixture collection.

Called by crates/tanki-core/tests/version_skew.rs. Imports `anki` only, never
`aqt`. Opens only an explicit `collection.anki2` inside the system temp folder,
refuses anything under the real Anki base folder, never syncs, never writes
through `col.db` (it is used for read-only inspection only).

Usage:
  version-skew.py --expect-version 25.09.4 version
  version-skew.py --expect-version 25.09.4 run SCENARIO /tmp/.../collection.anki2

Prints one JSON object on stdout. Exit code 2 = wrong Anki version,
3 = the collection could not be opened, 4 = refused path.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True

SKEW_DECK = "Skew deck"
SKEW_DECK_RENAMED = "Skew deck renamed in desktop"
FILTERED_DECK = "Skew filtered"
SPECIAL_NOTETYPE = "Skew special fields"
SPARE_PRESET = "Skew spare preset"


def emit(obj: dict, code: int = 0) -> None:
    print(json.dumps(obj, sort_keys=True))
    sys.stdout.flush()
    sys.exit(code)


def check_version(expected: str) -> str:
    import anki.buildinfo

    actual = anki.buildinfo.version
    if actual != expected:
        emit(
            {"error": f"anki.buildinfo.version is {actual!r}, expected {expected!r}"},
            2,
        )
    return actual


def check_path(raw: str) -> Path:
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
        emit({"error": f"refusing {path}: " + "; ".join(reasons)}, 4)
    return path


def col_stamps(col) -> dict:
    # Read-only inspection through Anki's own DB proxy.
    ver, scm, ls, mod, usn, crt = col.db.first(
        "select ver, scm, ls, mod, usn, crt from col"
    )
    return {"ver": ver, "scm": scm, "ls": ls, "mod": mod, "usn": usn, "crt": crt}


def preset_id(col, name: str) -> int:
    for conf in col.decks.all_config():
        if conf["name"] == name:
            return conf["id"]
    raise Exception(f"no preset {name}")


def observe(col) -> dict:
    """What this Anki version reads back from the newer core's state."""
    out: dict = {}
    configs = col.decks.get_deck_configs_for_update(1)
    out["fsrs"] = configs.fsrs
    out["presets"] = {
        c.config.name: {
            "review_order": c.config.config.review_order,
            "desired_retention": round(c.config.config.desired_retention, 6),
            "fsrs_params_6_len": len(c.config.config.fsrs_params_6),
        }
        for c in configs.all_config
    }
    legacy = col.decks.get_config(1)
    out["legacy_default_preset_reviewOrder"] = legacy.get("reviewOrder")
    fdid = col.decks.id_for_name(FILTERED_DECK)
    if fdid:
        fd = col.sched.get_or_create_filtered_deck(fdid)
        out["filtered_orders_proto"] = [t.order for t in fd.config.search_terms]
        out["filtered_terms_legacy"] = col.decks.get(fdid)["terms"]
    out["config_experimentalFeatures"] = col.get_config("experimentalFeatures", None)
    out["config_customColorPickerPalette"] = col.get_config(
        "customColorPickerPalette", None
    )
    return out


def answer_one_card(col, actions: list) -> None:
    from anki.scheduler.v3 import CardAnswer

    col.decks.select(1)
    queued = col.sched.get_queued_cards(fetch_limit=1)
    if not queued.cards:
        raise Exception("nothing to study in the Default deck")
    top = queued.cards[0]
    card = col.get_card(top.card.id)
    card.start_timer()
    answer = col.sched.build_answer(card=card, states=top.states, rating=CardAnswer.GOOD)
    col.sched.answer_card(answer)
    actions.append({"answered_card": card.id})


def deck_options_save(col, change, actions: list, fsrs_reschedule: bool = False) -> None:
    """Mimic the deck options screen: send only the current preset, edited."""
    from anki.decks import UpdateDeckConfigs

    did = 1  # Default deck, which uses the edited Default preset
    data = col.decks.get_deck_configs_for_update(did)
    current = next(
        c.config for c in data.all_config if c.config.id == data.current_deck.config_id
    )
    change(current.config)
    req = UpdateDeckConfigs(
        target_deck_id=did,
        configs=[current],
        card_state_customizer=data.card_state_customizer,
        limits=data.current_deck.limits,
        new_cards_ignore_review_limit=data.new_cards_ignore_review_limit,
        apply_all_parent_limits=data.apply_all_parent_limits,
        fsrs=data.fsrs,
        fsrs_reschedule=fsrs_reschedule,
        fsrs_health_check=data.fsrs_health_check,
    )
    col.decks.update_deck_configs(req)
    actions.append({"deck_options_saved_preset": current.id})


def scenario_a(col, actions, extra) -> None:
    pass


def scenario_b(col, actions, extra) -> None:
    def change(cfg):
        cfg.new_per_day += 1

    deck_options_save(col, change, actions)


def scenario_c(col, actions, extra) -> None:
    conf = col.decks.get_config(1)
    conf["new"]["perDay"] += 1
    col.decks.update_config(conf)
    spare = col.decks.get_config(preset_id(col, SPARE_PRESET))
    spare["rev"]["perDay"] += 1
    col.decks.save(spare)
    actions.append({"legacy_saved_presets": [1, spare["id"]]})


def scenario_d(col, actions, extra) -> None:
    answer_one_card(col, actions)
    ntid = col.models.id_for_name(SPECIAL_NOTETYPE)
    nid = col.find_notes(f"mid:{ntid}")[0]
    note = col.get_note(nid)
    note.fields[0] += " (edited in desktop)"
    col.update_note(note)
    actions.append({"edited_note": nid})
    col.set_config("collapseTime", 1500)
    actions.append({"set_config": "collapseTime"})
    deck = col.decks.by_name(SKEW_DECK)
    col.decks.rename(deck["id"], SKEW_DECK_RENAMED)
    actions.append({"renamed_deck": deck["id"]})
    nt = col.models.get(ntid)
    col.models.save(nt)
    actions.append({"saved_notetype": ntid})


def scenario_e_check_database(col, actions, extra) -> None:
    report, ok = col.fix_integrity()
    extra["check_database_report"] = report
    extra["check_database_ok"] = ok


def scenario_e_empty_cards(col, actions, extra) -> None:
    # As qt/aqt/emptycards.py does with "keep notes" ticked (its default).
    report = col.get_empty_cards()
    to_delete = []
    for note in report.notes:
        ids = list(note.card_ids)
        if note.will_delete_note:
            ids = ids[1:]
        to_delete.extend(ids)
    extra["empty_cards_reported"] = sorted(to_delete)
    if to_delete:
        col.remove_cards_and_orphaned_notes(to_delete)
    actions.append({"deleted_cards": sorted(to_delete)})


def scenario_e_filtered(col, actions, extra) -> None:
    fdid = col.decks.id_for_name(FILTERED_DECK)
    # 1. Proto round trip, as the filtered deck dialog's backend call does.
    fd = col.sched.get_or_create_filtered_deck(fdid)
    col.sched.add_or_update_filtered_deck(fd)
    # 2. Legacy dict saves of the filtered deck and a normal deck.
    col.decks.save(col.decks.get(fdid))
    col.decks.save(col.decks.by_name(SKEW_DECK))
    # 3. Rebuild.
    col.sched.rebuild_filtered_deck(fdid)
    actions.append({"filtered_round_trip": fdid})


def scenario_e_filtered_dialog(col, actions, extra) -> None:
    # The 25.09.4 dialog fills a QComboBox that has items 0..9 with
    # setCurrentIndex(term.order) and saves currentIndex(). For order 10,
    # Qt 6.9 reports -1 (checked once, outside this script). Store what the
    # dialog would store when the user saves without touching the order.
    fdid = col.decks.id_for_name(FILTERED_DECK)
    fd = col.sched.get_or_create_filtered_deck(fdid)
    labels = col.sched.filtered_deck_order_labels()
    extra["filtered_order_label_count"] = len(labels)
    for term in fd.config.search_terms:
        if term.order >= len(labels):
            term.order = -1
    col.sched.add_or_update_filtered_deck(fd)
    actions.append({"filtered_dialog_saved": fdid})


def scenario_e_fsrs_recompute(col, actions, extra) -> None:
    def change(cfg):
        cfg.desired_retention = 0.88

    deck_options_save(col, change, actions)


SCENARIOS = {
    "a_open_close": scenario_a,
    "b_deck_options": scenario_b,
    "c_legacy_deck_config": scenario_c,
    "d_daily_use": scenario_d,
    "e1_check_database": scenario_e_check_database,
    "e2_empty_cards": scenario_e_empty_cards,
    "e3_filtered_round_trip": scenario_e_filtered,
    "e4_filtered_dialog_save": scenario_e_filtered_dialog,
    "e5_fsrs_recompute": scenario_e_fsrs_recompute,
}


def run(version: str, scenario: str, path: Path) -> None:
    from anki.collection import Collection

    report: dict = {"version": version, "scenario": scenario, "opened": False}
    try:
        col = Collection(str(path))
    except Exception as exc:
        report["error"] = f"open failed: {exc!r}"
        report["traceback"] = traceback.format_exc()
        emit(report, 3)
    report["opened"] = True
    actions: list = []
    extra: dict = {}
    try:
        report["col_before"] = col_stamps(col)
        report["observed"] = observe(col)
        SCENARIOS[scenario](col, actions, extra)
        report["col_after"] = col_stamps(col)
    except Exception as exc:
        report["error"] = f"{exc!r}"
        report["traceback"] = traceback.format_exc()
    finally:
        col.close()
    report["closed"] = True
    report["actions"] = actions
    report["extra"] = extra
    emit(report, 1 if "error" in report else 0)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--expect-version", required=True)
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("version")
    r = sub.add_parser("run")
    r.add_argument("scenario", choices=sorted(SCENARIOS))
    r.add_argument("collection")
    args = parser.parse_args()
    version = check_version(args.expect_version)
    if args.cmd == "version":
        emit({"version": version})
    run(version, args.scenario, check_path(args.collection))


if __name__ == "__main__":
    main()
