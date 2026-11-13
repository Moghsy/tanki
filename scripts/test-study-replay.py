"""Guard checks do not import Anki or access a real profile."""
import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("replay", Path(__file__).with_name("study-replay.py"))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


class ReplayPathTests(unittest.TestCase):
    def test_real_base_is_rejected_before_filesystem_resolution(self):
        forbidden = Path.home() / ".local/share/Anki2/User/collection.anki2"
        with patch.object(Path, "resolve", side_effect=AssertionError("filesystem access")):
            with self.assertRaises(replay.Refused):
                replay.check_path(str(forbidden))

    def test_configured_base_is_rejected_before_filesystem_resolution(self):
        with patch.dict(os.environ, {"ANKI_BASE": "/tmp/tanki-user-base"}):
            with patch.object(Path, "resolve", side_effect=AssertionError("filesystem access")):
                with self.assertRaises(replay.Refused):
                    replay.check_path("/tmp/tanki-user-base/Profile/collection.anki2")


if __name__ == "__main__":
    unittest.main()
