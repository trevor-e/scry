#!/usr/bin/env python3
"""Exercise fixture behavior through the public scanner CLI."""
import json
import pathlib
import subprocess
import sys
import unittest


class CliFixtures(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        base = pathlib.Path(__file__).resolve().parent
        process = subprocess.run([sys.executable, str(base / "check.py"), "--fixtures", str(base / "fixtures")],
                                 capture_output=True, text=True, check=True)
        cls.report = json.loads(process.stdout)["repositories"][0]
        (base / "raw-fixtures.json").write_text(process.stdout)

    def test_supported_shadowing_is_reported(self):
        self.assertEqual({f["enum"] for f in self.report["findings"]}, {
            "RequiredSubset", "IntegerRange", "OptionalSuperset", "DefaultSuperset", "AnyFirst", "RenamedFields",
        })

    def test_unknown_semantics_are_explicit_abstentions(self):
        abstained = {a["enum"]: a["reason"] for a in self.report["abstentions"]}
        self.assertEqual(abstained["SerializeOnly"], "no_unconditional_derive_deserialize")
        for name, reason in {
            "FlattenAbstention": "flatten", "AliasAbstention": "alias",
            "CustomFieldAbstention": "deserialize_with", "ConditionalAbstention": "conditional_variant",
        }.items():
            self.assertIn(reason, abstained[name])


if __name__ == "__main__":
    unittest.main()
