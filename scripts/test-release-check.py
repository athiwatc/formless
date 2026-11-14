#!/usr/bin/env python3
"""Offline input fixtures for the release tag/version gate."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("release-check.sh").resolve()


class ReleaseVersion(unittest.TestCase):
    def check(self, manifest, tag="v0.2.0"):
        with tempfile.TemporaryDirectory() as root:
            file = Path(root) / "Cargo.toml"
            file.write_text(manifest)
            return subprocess.run(["sh", str(SCRIPT), tag, str(file)],
                                  text=True, capture_output=True, timeout=5)

    def test_valid_toml_version_formatting(self):
        for line in ['version = "0.2.0"', 'version = "0.2.0" # bump on release',
                     "version = '0.2.0' # literal", '  version = "0.2.0"',
                     "\tversion\t=\t'0.2.0'", "version='0.2.0'"]:
            with self.subTest(line=line):
                result = self.check('[package]\nversion="9.9.9"\n [workspace.package] # metadata\n' + line + '\n[dependencies]\n')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("matches Cargo.toml 0.2.0", result.stdout)

    def test_missing_version_and_mismatched_tag_still_fail(self):
        result = self.check('[workspace.package]\nversion="0.2.0"\n', tag="v0.3.0")
        self.assertEqual(result.returncode, 1)
        self.assertIn("does not match", result.stderr)
        for manifest in ['[workspaceXpackage]\nversion="0.2.0"\n', '[package]\nversion="0.2.0"\n', '[workspace.package]\n[dependencies]\nversion="0.2.0"\n']:
            result = self.check(manifest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("no [workspace.package] version", result.stderr)


if __name__ == "__main__":
    unittest.main()
