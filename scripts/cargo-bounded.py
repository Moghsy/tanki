#!/usr/bin/env python3
"""Serialize Cargo jobs into one bounded cache shared by this repo's worktrees.

This is a monitored storage guard, not an OS quota: usage can exceed the budget
between polls. Only generated Cargo target directories are eligible for cleanup.
"""
from __future__ import annotations

import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import time
from dataclasses import dataclass

GIB = 1024**3
LIMIT_EXIT = 75
MARKER = ".tanki-cargo-target.json"
COMMANDS = {"build", "test", "clippy", "check", "run", "bench", "doc"}


class GuardError(RuntimeError):
    pass


@dataclass(frozen=True)
class Budgets:
    target: int = 12 * GIB
    worktrees: int = 2 * GIB
    poll: float = 0.25
    terminate_wait: float = 2.0


def main_root(cwd: Path) -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode:
        raise GuardError("run this wrapper inside the tanki Git workspace")
    common = Path(result.stdout.strip())
    if common.name != ".git" or common.is_symlink():
        raise GuardError("unsupported or symlinked common Git directory")
    root = common.parent
    if root.is_symlink() or root.resolve() != root:
        raise GuardError("main workspace root must not contain symlink components")
    return root


def safe_path(path: Path, root: Path) -> None:
    """Reject symlinked roots/ancestors; descendants may be ordinary symlinks."""
    try:
        parts = path.relative_to(root).parts
    except ValueError as error:
        raise GuardError(f"path escapes the workspace: {path}") from error
    if any(part in ("..", ".") for part in parts):
        raise GuardError(f"path escapes the workspace: {path}")
    if root.is_symlink() or root.resolve() != root:
        raise GuardError(f"unsafe workspace root: {root}")
    current = root
    for part in parts:
        current = current / part
        if current.is_symlink():
            raise GuardError(f"refusing symlinked path: {current}")
        if current.exists() and not current.is_dir():
            raise GuardError(f"expected a directory: {current}")


def tree_bytes(path: Path) -> int:
    """Allocated bytes, counted once per inode; never follow symlinks."""
    if path.is_symlink():
        raise GuardError(f"refusing symlinked storage root: {path}")
    if not path.exists():
        return 0
    total = 0
    pending = [path]
    seen: set[tuple[int, int]] = set()
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    try:
                        info = entry.stat(follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    if stat.S_ISLNK(info.st_mode):
                        continue
                    key = (info.st_dev, info.st_ino)
                    if key not in seen:
                        seen.add(key)
                        total += getattr(info, "st_blocks", 0) * 512 if hasattr(info, "st_blocks") else info.st_size
                    if stat.S_ISDIR(info.st_mode):
                        pending.append(Path(entry.path))
        except FileNotFoundError:
            continue
        except OSError as error:
            raise GuardError(f"cannot measure {directory}: {error}") from error
    return total


def cargo_owned(path: Path) -> bool:
    if not path.is_dir() or path.is_symlink():
        return False
    marker = path / MARKER
    if marker.is_file() and not marker.is_symlink():
        try:
            if json.loads(marker.read_text()) == {"version": 1, "target": str(path)}:
                return True
        except (OSError, ValueError):
            pass
    info = path / ".rustc_info.json"
    if info.is_file() and not info.is_symlink():
        try:
            data = json.loads(info.read_text())
            if isinstance(data, dict) and "rustc_fingerprint" in data and "outputs" in data:
                return True
        except (OSError, ValueError):
            pass
    tag = path / "CACHEDIR.TAG"
    if tag.is_file() and not tag.is_symlink():
        try:
            data = tag.read_text()
            return data.startswith("Signature: 8a477f597d28d172789f06886806bc55") and "cargo" in data.lower()
        except OSError:
            pass
    return False


def prepare_target(path: Path, root: Path) -> None:
    safe_path(path, root)
    if path.exists() and any(path.iterdir()) and not cargo_owned(path):
        raise GuardError(f"target is not a recognized Cargo cache; refusing to claim or clean it: {path}")
    path.mkdir(exist_ok=True)
    marker = path / MARKER
    if marker.is_symlink():
        raise GuardError(f"refusing symlinked ownership marker: {marker}")
    marker.write_text(json.dumps({"version": 1, "target": str(path)}) + "\n")
    # We create target before Cargo does. Recent Cargo requires its cache tag
    # for clean, but only creates the tag when it creates the directory.
    tag = path / "CACHEDIR.TAG"
    if tag.is_symlink():
        raise GuardError(f"refusing symlinked cache tag: {tag}")
    if not tag.exists():
        tag.write_text("Signature: 8a477f597d28d172789f06886806bc55\n# Cargo build cache\n")


@contextlib.contextmanager
def build_lock(root: Path):
    directory = root / ".cargo"
    safe_path(directory, root)
    directory.mkdir(exist_ok=True)
    path = directory / "bounded.lock"
    flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags, 0o600)
    except OSError as error:
        raise GuardError(f"cannot open build lock safely: {error}") from error
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise GuardError(f"build lock is not a regular file: {path}")
        print("cargo-bounded: waiting for shared build lock", file=sys.stderr, flush=True)
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def environment(target: Path) -> dict[str, str]:
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target)
    env.setdefault("CARGO_INCREMENTAL", "0")
    env.setdefault("CARGO_PROFILE_DEV_DEBUG", "0")
    env.setdefault("CARGO_PROFILE_TEST_DEBUG", "0")
    return env


def clean_target(path: Path, root: Path, cargo: str, env: dict[str, str]) -> None:
    safe_path(path, root)
    if not path.exists():
        return
    if not any(path.iterdir()):
        return
    if not cargo_owned(path):
        raise GuardError(f"refusing to clean an unrecognized target directory: {path}")
    print(f"cargo-bounded: cleaning generated cache {path}", file=sys.stderr, flush=True)
    # This command executes only after acquiring the one cross-worktree lock.
    result = subprocess.run([cargo, "clean", "--target-dir", str(path)], cwd=root, env=env, check=False)
    if result.returncode:
        raise GuardError(f"cargo clean failed ({result.returncode}) for {path}")


def clean_worktree_targets(root: Path, cargo: str, env: dict[str, str]) -> None:
    directory = root / ".worktrees"
    safe_path(directory, root)
    if not directory.exists():
        return
    for worktree in directory.iterdir():
        if worktree.is_symlink() or not worktree.is_dir():
            continue
        for relative in (Path("target"), Path("xtask/bootstrap/target")):
            target = worktree / relative
            try:
                safe_path(target, root)
            except GuardError:
                # Never follow a worktree's link to an unrelated target/cache.
                continue
            if cargo_owned(target):
                clean_target(target, root, cargo, env)


def stop_group(process: subprocess.Popen, wait: float) -> None:
    """Stop/reap our entire session, including children after Cargo exits."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    deadline = time.monotonic() + wait
    while time.monotonic() < deadline:
        process.poll()  # Reap the group leader; do not confuse zombies with work.
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            process.wait()
            return
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=max(wait, 1.0))


def over_budget(root: Path, budgets: Budgets) -> str | None:
    safe_path(root / "target", root)
    safe_path(root / ".worktrees", root)
    target = tree_bytes(root / "target")
    if target > budgets.target:
        return f"target uses {target} bytes (budget {budgets.target})"
    worktrees = tree_bytes(root / ".worktrees")
    if worktrees > budgets.worktrees:
        return f".worktrees uses {worktrees} bytes (budget {budgets.worktrees})"
    return None


def run(root: Path, cwd: Path, arguments: list[str], *, budgets: Budgets = Budgets(),
        cargo: str = "cargo", clean: bool = False) -> int:
    root = Path(root)
    cwd = Path(cwd)
    if budgets.target <= 0 or budgets.worktrees <= 0 or budgets.poll <= 0 or budgets.terminate_wait <= 0:
        raise GuardError("budgets and poll/termination intervals must be positive")
    if arguments and arguments[0] == "--":
        arguments = arguments[1:]
    if arguments and arguments[0] == "clean":
        if len(arguments) != 1:
            raise GuardError("use --clean or clean without additional Cargo flags")
        clean, arguments = True, []
    if not clean and (not arguments or arguments[0] not in COMMANDS):
        raise GuardError("expected build, test, clippy, check, run, bench, doc, or --clean")
    if any(arg == "--target-dir" or arg.startswith("--target-dir=") for arg in arguments):
        raise GuardError("--target-dir is managed by this wrapper")
    if clean and arguments:
        raise GuardError("--clean cannot be combined with a build command")
    target = root / "target"
    env = environment(target)
    with build_lock(root):
        safe_path(target, root)
        clean_worktree_targets(root, cargo, env)
        if clean:
            clean_target(target, root, cargo, env)
            return 0
        # Generated duplicate targets are removed first; remaining worktree
        # content is source/uncommitted work and must never be removed here.
        worktrees = tree_bytes(root / ".worktrees")
        if worktrees > budgets.worktrees:
            raise GuardError(f".worktrees uses {worktrees} bytes (budget {budgets.worktrees}); reduce source/worktree content before building")
        if tree_bytes(target) > budgets.target:
            clean_target(target, root, cargo, env)
        prepare_target(target, root)
        if reason := over_budget(root, budgets):
            raise GuardError(reason)
        command = [cargo, arguments[0], "--target-dir", str(target), *arguments[1:]]
        print(f"cargo-bounded: shared cache {target}; monitored budget {budgets.target / GIB:g} GiB", file=sys.stderr, flush=True)
        process = subprocess.Popen(command, cwd=cwd, env=env, start_new_session=True)
        try:
            while True:
                result = process.poll()
                reason = over_budget(root, budgets)
                if reason:
                    stop_group(process, budgets.terminate_wait)
                    try:
                        clean_target(target, root, cargo, env)
                        clean_worktree_targets(root, cargo, env)
                    finally:
                        print(f"cargo-bounded: LIMIT REACHED: {reason}; build stopped. This is a monitored guard, not an OS hard quota.", file=sys.stderr, flush=True)
                    return LIMIT_EXIT
                if result is not None:
                    return result if result >= 0 else 128 - result
                time.sleep(budgets.poll)
        except BaseException:
            stop_group(process, budgets.terminate_wait)
            raise


def cli(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-budget-gib", type=float, default=12)
    parser.add_argument("--worktrees-budget-gib", type=float, default=2)
    parser.add_argument("--poll-seconds", type=float, default=0.25)
    parser.add_argument("--clean", action="store_true", help="clean recognized generated targets while holding the shared build lock")
    parser.add_argument("cargo_args", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    try:
        cwd = Path.cwd()
        root = main_root(cwd)
        return run(root, cwd, args.cargo_args, budgets=Budgets(
            target=int(args.target_budget_gib * GIB),
            worktrees=int(args.worktrees_budget_gib * GIB), poll=args.poll_seconds,
        ), clean=args.clean)
    except (GuardError, OSError, ValueError) as error:
        print(f"cargo-bounded: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(cli())
