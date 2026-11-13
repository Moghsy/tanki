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

    def test_only_enabled_packages_are_checked(self):
        metadata = {"packages": [
            {"name": "app", "version": "0.0.0", "license": "AGPL-3.0-or-later"},
            {"name": "optional", "version": "1.0.0", "license": "GPL-2.0-only"},
        ]}
        active = module.selected_packages(metadata, "app v0.0.0 (/tmp/app)\napp v0.0.0 (*)\n")
        self.assertEqual([p["name"] for p in active], ["app"])
        self.assertTrue(all(module.accepted(p["license"]) for p in active))
        active = module.selected_packages(metadata, "app v0.0.0\noptional v1.0.0\n")
        self.assertFalse(all(module.accepted(p["license"]) for p in active))

    def test_missing_graph_data_fails_closed(self):
        for tree in ("", "unknown v1.0.0\n"):
            with self.assertRaises(ValueError):
                module.selected_packages({"packages": []}, tree)

    def test_reviewed_license_choice_is_specific(self):
        package = {"name": "self_cell", "version": "1.3.0", "license": "Apache-2.0 OR GPL-2.0-only"}
        self.assertEqual(module.package_license(package), "Apache-2.0")
        for change in ({"name": "other"}, {"version": "1.4.0"}, {"license": "GPL-2.0-only"}):
            self.assertFalse(module.accepted(module.package_license({**package, **change})))


if __name__ == "__main__":
    unittest.main()
