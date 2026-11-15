"""Tests for scripts/cargo-bounded.py.

Every test works inside tempfile.TemporaryDirectory() fake workspaces and uses
a fake cargo shell script, so the real repository, its target/ and .worktrees/,
and the real cargo are never touched.
"""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("cargo_bounded", Path(__file__).with_name("cargo-bounded.py"))
bounded = importlib.util.module_from_spec(spec)
# Registered first: the wrapper uses @dataclass, which looks up its module in sys.modules.
sys.modules[spec.name] = bounded
spec.loader.exec_module(bounded)

GuardError = bounded.GuardError
Budgets = bounded.Budgets
FAST = Budgets(poll=0.01, terminate_wait=2.0)
CARGO_TAG = "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n"
CARGO_KEYS = ("CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG", "CARGO_PROFILE_TEST_DEBUG")
GIT_KEYS = ("GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_CEILING_DIRECTORIES")

# Logs argv and relevant env, then: `clean` removes its target dir (like cargo clean);
# FAKE_GROW makes a build write FAKE_GROW bytes into the target and hang until stopped;
# otherwise exit with FAKE_EXIT (default 0). The target dir is always $3.
FAKE_CARGO = """#!/bin/sh
{
  echo "argv: $*"
  echo "target: $CARGO_TARGET_DIR"
  echo "incremental: ${CARGO_INCREMENTAL-unset}"
  echo "dev-debug: ${CARGO_PROFILE_DEV_DEBUG-unset}"
} >> '@LOG@'
if [ "$1" = clean ]; then
  rm -rf -- "$3"
  exit 0
fi
if [ -n "$FAKE_GROW" ]; then
  mkdir -p "$3/debug"
  head -c "$FAKE_GROW" /dev/zero > "$3/debug/blob"
  sleep 30
fi
exit "${FAKE_EXIT-0}"
"""


@contextlib.contextmanager
def isolated_env(**extra):
    """Drop inherited Cargo/Git/fake knobs so results do not depend on the caller's shell."""
    with patch.dict(os.environ):
        for key in CARGO_KEYS + GIT_KEYS + ("FAKE_GROW", "FAKE_EXIT"):
            os.environ.pop(key, None)
        os.environ.update(extra)
        yield


@contextlib.contextmanager
def chdir(path):
    old = os.getcwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(old)


def git(cwd, *args):
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(GIT_AUTHOR_NAME="test", GIT_AUTHOR_EMAIL="test@example.invalid",
               GIT_COMMITTER_NAME="test", GIT_COMMITTER_EMAIL="test@example.invalid")
    subprocess.run(["git", *args], cwd=cwd, env=env, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def make_git_repo(base: Path, name: str = "repo") -> Path:
    repo = base / name
    (repo / "src").mkdir(parents=True)
    (repo / "Cargo.toml").write_text("[workspace]\n")
    (repo / "src" / "lib.rs").write_text("pub fn f() {}\n")
    git(repo, "init", "-q")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "init")
    return repo.resolve()


def make_cargo_target(path: Path) -> None:
    (path / "debug").mkdir(parents=True)
    (path / "CACHEDIR.TAG").write_text(CARGO_TAG)
    (path / "debug" / "artifact.rlib").write_bytes(b"x" * 1024)


class FakeWorkspace(unittest.TestCase):
    """Fake (non-Git) workspace with source files and a fake cargo on disk."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.base = Path(tmp.name).resolve()
        self.root = self.base / "workspace"
        (self.root / ".git").mkdir(parents=True)  # marker only; never run git here
        (self.root / "src").mkdir()
        (self.root / "Cargo.toml").write_text("[workspace]\n")
        (self.root / "src" / "lib.rs").write_text("pub fn f() {}\n")
        self.log = self.base / "cargo.log"
        self.fake = self.base / "fakebin" / "cargo"
        self.fake.parent.mkdir()
        self.fake.write_text(FAKE_CARGO.replace("@LOG@", str(self.log)))
        self.fake.chmod(0o755)

    def log_lines(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def write(self, path: Path, data: str = "source\n") -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)
        return path


class RootDetectionTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.base = Path(tmp.name).resolve()

    def test_nested_cwd_resolves_to_main_root(self):
        repo = make_git_repo(self.base)
        nested = repo / "crates" / "tanki-core" / "src"
        nested.mkdir(parents=True)
        with isolated_env():
            self.assertEqual(bounded.main_root(nested), repo)

    def test_worktree_resolves_to_main_root(self):
        repo = make_git_repo(self.base)
        (repo / ".worktrees").mkdir()
        with isolated_env():
            git(repo, "worktree", "add", "-q", "-b", "feat", str(repo / ".worktrees" / "feat"))
            self.assertEqual(bounded.main_root(repo / ".worktrees" / "feat" / "src"), repo)

    def test_outside_git_workspace_is_rejected(self):
        plain = self.base / "plain"
        plain.mkdir()
        # Stop git from discovering any repository above the temporary base.
        with isolated_env(GIT_CEILING_DIRECTORIES=str(self.base)):
            with self.assertRaises(GuardError):
                bounded.main_root(plain)


class SafePathTests(FakeWorkspace):
    def test_ordinary_nested_path_is_accepted(self):
        bounded.safe_path(self.root / "target" / "debug", self.root)

    def test_path_outside_root_is_refused(self):
        (self.base / "elsewhere").mkdir()
        with self.assertRaises(GuardError):
            bounded.safe_path(self.base / "elsewhere", self.root)

    def test_sibling_with_shared_prefix_is_refused(self):
        evil = self.base / "workspace-evil"
        evil.mkdir()
        with self.assertRaises(GuardError):
            bounded.safe_path(evil / "target", self.root)

    def test_symlinked_ancestor_is_refused(self):
        outside = self.base / "outside"
        outside.mkdir()
        (self.root / "escape").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(GuardError):
            bounded.safe_path(self.root / "escape" / "x", self.root)

    def test_symlinked_leaf_is_refused(self):
        outside = self.base / "outside"
        outside.mkdir()
        (self.root / "target").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(GuardError):
            bounded.safe_path(self.root / "target", self.root)

    def test_file_where_directory_expected_is_refused(self):
        self.write(self.root / "file.txt")
        with self.assertRaises(GuardError):
            bounded.safe_path(self.root / "file.txt" / "x", self.root)

    def test_dotdot_escape_is_refused(self):
        (self.base / "outside").mkdir()
        with self.assertRaises(GuardError):
            bounded.safe_path(self.root / ".." / "outside", self.root)


class CargoOwnedTests(FakeWorkspace):
    def test_plain_source_directory_is_not_cargo_owned(self):
        self.assertFalse(bounded.cargo_owned(self.root))
        self.assertFalse(bounded.cargo_owned(self.root / "src"))

    def test_missing_path_is_not_cargo_owned(self):
        self.assertFalse(bounded.cargo_owned(self.root / "target"))

    def test_cachedir_tag_from_cargo_is_owned(self):
        target = self.root / "target"
        make_cargo_target(target)
        self.assertTrue(bounded.cargo_owned(target))

    def test_foreign_cachedir_tag_is_not_owned(self):
        target = self.root / "target"
        target.mkdir()
        (target / "CACHEDIR.TAG").write_text("Signature: 8a477f597d28d172789f06886806bc55\nbackup tool\n")
        self.assertFalse(bounded.cargo_owned(target))

    def test_rustc_info_marker_is_owned(self):
        target = self.root / "target"
        target.mkdir()
        (target / ".rustc_info.json").write_text(json.dumps({"rustc_fingerprint": 1, "outputs": {}}))
        self.assertTrue(bounded.cargo_owned(target))

    def test_ownership_marker_must_name_this_path(self):
        target = self.root / "target"
        target.mkdir()
        (target / bounded.MARKER).write_text(json.dumps({"version": 1, "target": str(self.base / "other")}))
        self.assertFalse(bounded.cargo_owned(target))
        (target / bounded.MARKER).write_text(json.dumps({"version": 1, "target": str(target)}))
        self.assertTrue(bounded.cargo_owned(target))

    def test_symlinked_target_is_never_owned(self):
        real = self.base / "real-cache"
        make_cargo_target(real)
        (self.root / "target").symlink_to(real, target_is_directory=True)
        self.assertFalse(bounded.cargo_owned(self.root / "target"))

    def test_symlinked_tag_file_is_not_trusted(self):
        target = self.root / "target"
        target.mkdir()
        tag = self.base / "tag-outside"
        tag.write_text(CARGO_TAG)
        (target / "CACHEDIR.TAG").symlink_to(tag)
        self.assertFalse(bounded.cargo_owned(target))


class PrepareTargetTests(FakeWorkspace):
    def test_empty_target_gets_ownership_marker(self):
        target = self.root / "target"
        bounded.prepare_target(target, self.root)
        marker = json.loads((target / bounded.MARKER).read_text())
        self.assertEqual(marker, {"version": 1, "target": str(target)})
        self.assertTrue((target / "CACHEDIR.TAG").read_text().startswith("Signature: 8a477f597d28d172789f06886806bc55"))

    def test_unrecognized_non_empty_target_is_refused_and_untouched(self):
        target = self.write(self.root / "target" / "notes.txt", "keep me\n")
        with self.assertRaises(GuardError):
            bounded.prepare_target(target.parent, self.root)
        self.assertEqual(target.read_text(), "keep me\n")

    def test_symlinked_marker_is_refused_without_writing_outside(self):
        target = self.root / "target"
        target.mkdir()
        outside = self.write(self.base / "outside-marker", "original\n")
        (target / bounded.MARKER).symlink_to(outside)
        with self.assertRaises(GuardError):
            bounded.prepare_target(target, self.root)
        self.assertEqual(outside.read_text(), "original\n")


class BuildLockTests(FakeWorkspace):
    def test_lock_file_is_created_as_regular_file(self):
        with bounded.build_lock(self.root), contextlib.redirect_stderr(io.StringIO()):
            lock = self.root / ".cargo" / "bounded.lock"
            self.assertTrue(lock.is_file() and not lock.is_symlink())

    def test_second_acquirer_waits_until_release(self):
        entered = threading.Event()

        def second():
            with bounded.build_lock(self.root):
                entered.set()

        with contextlib.redirect_stderr(io.StringIO()):
            with bounded.build_lock(self.root):
                worker = threading.Thread(target=second, daemon=True)
                worker.start()
                self.assertFalse(entered.wait(0.3), "second acquirer must wait while the lock is held")
            self.assertTrue(entered.wait(10), "second acquirer must proceed after release")
            worker.join(10)

    def test_symlinked_lock_file_is_refused(self):
        (self.root / ".cargo").mkdir()
        outside = self.write(self.base / "outside-lock", "original\n")
        (self.root / ".cargo" / "bounded.lock").symlink_to(outside)
        with self.assertRaises(GuardError), contextlib.redirect_stderr(io.StringIO()):
            with bounded.build_lock(self.root):
                pass
        self.assertEqual(outside.read_text(), "original\n")

    def test_symlinked_cargo_directory_is_refused(self):
        outside = self.base / "outside-dir"
        outside.mkdir()
        (self.root / ".cargo").symlink_to(outside, target_is_directory=True)
        with self.assertRaises(GuardError), contextlib.redirect_stderr(io.StringIO()):
            with bounded.build_lock(self.root):
                pass
        self.assertEqual(list(outside.iterdir()), [])


class EnvironmentTests(FakeWorkspace):
    def test_sets_shared_target_and_debug_defaults(self):
        with isolated_env():
            env = bounded.environment(self.root / "target")
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.root / "target"))
        self.assertEqual(env["CARGO_INCREMENTAL"], "0")
        self.assertEqual(env["CARGO_PROFILE_DEV_DEBUG"], "0")
        self.assertEqual(env["CARGO_PROFILE_TEST_DEBUG"], "0")

    def test_target_dir_always_overrides_inherited_value(self):
        with isolated_env(CARGO_TARGET_DIR="/somewhere/else"):
            env = bounded.environment(self.root / "target")
        self.assertEqual(env["CARGO_TARGET_DIR"], str(self.root / "target"))

    def test_inherited_profile_settings_are_kept(self):
        # setdefault: an explicit ambient setting wins (deliberate opt-in); asserted
        # so the behaviour is visible rather than accidental.
        with isolated_env(CARGO_INCREMENTAL="1"):
            env = bounded.environment(self.root / "target")
        self.assertEqual(env["CARGO_INCREMENTAL"], "1")

    def test_returns_copy_without_mutating_process_environment(self):
        with isolated_env():
            env = bounded.environment(self.root / "target")
            self.assertNotIn("CARGO_TARGET_DIR", os.environ)
        self.assertIsNot(env, os.environ)


class CleanTargetTests(FakeWorkspace):
    def test_unrecognized_target_is_refused_and_untouched(self):
        target = self.write(self.root / "target" / "notes.txt", "keep me\n")
        with self.assertRaises(GuardError):
            bounded.clean_target(target.parent, self.root, str(self.fake), {})
        self.assertEqual(target.read_text(), "keep me\n")
        self.assertEqual(self.log_lines(), [], "cargo must not run for a refused target")

    def test_missing_or_empty_target_is_a_no_op(self):
        bounded.clean_target(self.root / "target", self.root, str(self.fake), {})
        (self.root / "target").mkdir()
        bounded.clean_target(self.root / "target", self.root, str(self.fake), {})
        self.assertEqual(self.log_lines(), [])

    def test_owned_target_is_cleaned_and_source_survives(self):
        make_cargo_target(self.root / "target")
        bounded.clean_target(self.root / "target", self.root, str(self.fake), dict(os.environ))
        self.assertFalse((self.root / "target").exists())
        self.assertEqual((self.root / "src" / "lib.rs").read_text(), "pub fn f() {}\n")
        self.assertTrue(any(line.startswith("argv: clean --target-dir") for line in self.log_lines()))


class OverBudgetTests(FakeWorkspace):
    def test_reports_target_over_budget(self):
        self.write(self.root / "target" / "big", "x" * 65536)
        reason = bounded.over_budget(self.root, Budgets(target=1024))
        self.assertIsNotNone(reason)
        self.assertIn("target uses", reason)

    def test_reports_worktrees_over_budget(self):
        self.write(self.root / ".worktrees" / "feat" / "big", "x" * 65536)
        reason = bounded.over_budget(self.root, Budgets(worktrees=1024))
        self.assertIsNotNone(reason)
        self.assertIn(".worktrees uses", reason)

    def test_small_tree_is_within_budget(self):
        self.assertIsNone(bounded.over_budget(self.root, Budgets()))

    def test_hard_links_are_counted_once(self):
        first = self.write(self.root / "target" / "a", "x" * 65536)
        os.link(first, self.root / "target" / "b")
        self.assertLess(bounded.tree_bytes(self.root / "target"), 2 * 65536)

    def test_non_positive_budget_is_rejected(self):
        with self.assertRaises(GuardError):
            bounded.run(self.root, self.root, ["build"], budgets=Budgets(target=0), cargo=str(self.fake))

    def test_worktrees_over_budget_refuses_build_and_keeps_source(self):
        big = self.write(self.root / ".worktrees" / "feat" / "src" / "big.rs", "x" * 65536)
        with isolated_env(), self.assertRaises(GuardError):
            bounded.run(self.root, self.root, ["build"], budgets=Budgets(worktrees=1024, poll=0.01), cargo=str(self.fake))
        self.assertEqual(len(big.read_text()), 65536)
        self.assertEqual(self.log_lines(), [], "cargo must not run over the worktree budget")

    def test_build_over_target_budget_is_stopped_and_only_target_is_cleaned(self):
        self.write(self.root / ".worktrees" / "feat" / "xtask" / "bootstrap" / "target" / "keep.txt", "not cargo\n")
        with isolated_env(FAKE_GROW="1048576"), contextlib.redirect_stderr(io.StringIO()):
            code = bounded.run(self.root, self.root, ["build"],
                               budgets=Budgets(target=64 * 1024, poll=0.01), cargo=str(self.fake))
        self.assertEqual(code, bounded.LIMIT_EXIT)
        self.assertFalse((self.root / "target").exists())
        self.assertEqual((self.root / "src" / "lib.rs").read_text(), "pub fn f() {}\n")
        self.assertEqual((self.root / "Cargo.toml").read_text(), "[workspace]\n")
        self.assertTrue((self.root / ".worktrees" / "feat" / "xtask" / "bootstrap" / "target" / "keep.txt").exists())


class CleanModeTests(FakeWorkspace):
    def test_clean_removes_owned_generated_targets_only(self):
        make_cargo_target(self.root / "target")
        make_cargo_target(self.root / ".worktrees" / "owned" / "target")
        self.write(self.root / ".worktrees" / "owned" / "src" / "lib.rs", "wt source\n")
        self.write(self.root / ".worktrees" / "owned" / "xtask" / "bootstrap" / "target" / "keep.txt", "not cargo\n")
        outside_cache = self.base / "outside-cache"
        make_cargo_target(outside_cache)
        (self.root / ".worktrees" / "linked").mkdir()
        (self.root / ".worktrees" / "linked" / "target").symlink_to(outside_cache, target_is_directory=True)
        with isolated_env():
            code = bounded.run(self.root, self.root, [], budgets=FAST, cargo=str(self.fake), clean=True)
        self.assertEqual(code, 0)
        self.assertFalse((self.root / "target").exists())
        self.assertFalse((self.root / ".worktrees" / "owned" / "target").exists())
        self.assertEqual((self.root / ".worktrees" / "owned" / "src" / "lib.rs").read_text(), "wt source\n")
        self.assertTrue((self.root / ".worktrees" / "owned" / "xtask" / "bootstrap" / "target" / "keep.txt").exists())
        self.assertTrue((outside_cache / "CACHEDIR.TAG").exists(), "symlinked worktree target must not be followed")
        self.assertEqual((self.root / "src" / "lib.rs").read_text(), "pub fn f() {}\n")

    def test_clean_subcommand_is_same_as_clean_flag(self):
        make_cargo_target(self.root / "target")
        with isolated_env():
            self.assertEqual(bounded.run(self.root, self.root, ["clean"], budgets=FAST, cargo=str(self.fake)), 0)
        self.assertFalse((self.root / "target").exists())

    def test_clean_with_extra_flags_is_rejected(self):
        with self.assertRaises(GuardError):
            bounded.run(self.root, self.root, ["clean", "--workspace"], budgets=FAST, cargo=str(self.fake))
        with self.assertRaises(GuardError):
            bounded.run(self.root, self.root, ["build"], budgets=FAST, cargo=str(self.fake), clean=True)

    def test_clean_refuses_unrecognized_target(self):
        self.write(self.root / "target" / "notes.txt", "keep me\n")
        with self.assertRaises(GuardError):
            bounded.run(self.root, self.root, [], budgets=FAST, cargo=str(self.fake), clean=True)
        self.assertTrue((self.root / "target" / "notes.txt").exists())


class RunTests(FakeWorkspace):
    def test_build_passes_arguments_through_and_returns_exit_code(self):
        with isolated_env(FAKE_EXIT="7"):
            code = bounded.run(self.root, self.root, ["--", "test", "--workspace"], budgets=FAST, cargo=str(self.fake))
        self.assertEqual(code, 7)
        target = self.root / "target"
        self.assertEqual(self.log_lines(), [
            f"argv: test --target-dir {target} --workspace",
            f"target: {target}",
            "incremental: 0",
            "dev-debug: 0",
        ])
        self.assertTrue(bounded.cargo_owned(target), "wrapper must claim its own target with a marker")

    def test_successful_build_returns_zero(self):
        with isolated_env():
            self.assertEqual(bounded.run(self.root, self.root, ["check"], budgets=FAST, cargo=str(self.fake)), 0)

    def test_unsupported_command_and_target_dir_override_are_rejected(self):
        with isolated_env():
            for arguments in (["install", "x"], [], ["build", "--target-dir", "/x"], ["build", "--target-dir=/x"]):
                with self.subTest(arguments=arguments), self.assertRaises(GuardError):
                    bounded.run(self.root, self.root, arguments, budgets=FAST, cargo=str(self.fake))
        self.assertEqual(self.log_lines(), [])

    def test_build_refuses_unrecognized_target_before_running_cargo(self):
        self.write(self.root / "target" / "notes.txt", "keep me\n")
        with isolated_env(), self.assertRaises(GuardError):
            bounded.run(self.root, self.root, ["build"], budgets=FAST, cargo=str(self.fake))
        self.assertEqual((self.root / "target" / "notes.txt").read_text(), "keep me\n")
        self.assertEqual(self.log_lines(), [])


class CliTests(unittest.TestCase):
    """cli() finds the workspace from cwd and uses `cargo` from PATH; both are faked."""

    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.base = Path(tmp.name).resolve()
        self.repo = make_git_repo(self.base)
        self.log = self.base / "cargo.log"
        fakebin = self.base / "fakebin"
        fakebin.mkdir()
        self.fake = fakebin / "cargo"
        self.fake.write_text(FAKE_CARGO.replace("@LOG@", str(self.log)))
        self.fake.chmod(0o755)

    def run_cli(self, argv, cwd, **extra):
        path = f"{self.fake.parent}{os.pathsep}{os.environ.get('PATH', '')}"
        with isolated_env(PATH=path, **extra), chdir(cwd), contextlib.redirect_stderr(io.StringIO()):
            return bounded.cli(argv)

    def test_build_from_nested_cwd_uses_shared_target(self):
        nested = self.repo / "src"
        code = self.run_cli(["build", "--workspace"], nested, FAKE_EXIT="3")
        self.assertEqual(code, 3)
        target = self.repo / "target"
        self.assertIn(f"argv: build --target-dir {target} --workspace", self.log.read_text().splitlines())

    def test_clean_flag_removes_owned_target(self):
        make_cargo_target(self.repo / "target")
        self.assertEqual(self.run_cli(["--clean"], self.repo), 0)
        self.assertFalse((self.repo / "target").exists())
        self.assertTrue((self.repo / "src" / "lib.rs").exists())

    def test_zero_budget_is_a_usage_error(self):
        self.assertEqual(self.run_cli(["--target-budget-gib", "0", "build"], self.repo), 2)
        self.assertFalse(self.log.exists())

    def test_fractional_budget_is_accepted_and_limit_exits_75(self):
        code = self.run_cli(["--target-budget-gib", "0.00006", "--poll-seconds", "0.01", "build"],
                            self.repo, FAKE_GROW="1048576")
        self.assertEqual(code, bounded.LIMIT_EXIT)
        self.assertFalse((self.repo / "target").exists())
        self.assertTrue((self.repo / "src" / "lib.rs").exists())

    def test_outside_git_workspace_is_a_usage_error(self):
        plain = self.base / "plain"
        plain.mkdir()
        self.assertEqual(self.run_cli(["build"], plain, GIT_CEILING_DIRECTORIES=str(self.base)), 2)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
