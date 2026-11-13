import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location("licenses", pathlib.Path(__file__).with_name("check-licenses.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LicenseTests(unittest.TestCase):
    def test_reject_unknown_and_gpl2_even_as_alternative(self):
        for license in (None, "", "GPL-2.0-only", "MIT OR GPL-2.0-only", "LicenseRef-Proprietary"):
            self.assertFalse(module.accepted(license))

    def test_known_expressions(self):
        for license in ("AGPL-3.0-or-later", "MIT/Apache-2.0", "(MIT OR Apache-2.0) AND Unicode-3.0", "Apache-2.0 WITH LLVM-exception"):
            self.assertTrue(module.accepted(license))


if __name__ == "__main__":
    unittest.main()
