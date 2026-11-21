#!/usr/bin/env python3
"""Offline input fixtures for the release version and notes gates."""
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("release-check.sh").resolve()
NOTES = SCRIPT.with_name("release-notes.sh")


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


class ReleaseNotes(unittest.TestCase):
    def notes(self, changelog, tag="v0.2.0"):
        with tempfile.TemporaryDirectory() as root:
            file = Path(root) / "CHANGELOG.md"
            file.write_text(changelog)
            return subprocess.run(["sh", str(NOTES), tag, str(file)],
                                  text=True, capture_output=True, timeout=5)

    def test_extracts_exact_version_until_the_next_level_two_heading(self):
        for heading in ["0.2.0", "[0.2.0]"]:
            with self.subTest(heading=heading):
                result = self.notes(
                    "## [0.2.0-rc.1]\n- prerelease\n## 0.2.0.1\n- another version\n"
                    f"## {heading} - 2026-11-18\n \t\n### Fixed\n- a fix\n\n"
                    "## Earlier changes\n- other notes\n"
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "### Fixed\n- a fix\n\n")

    def test_build_metadata_and_prerelease_characters_are_literal(self):
        result = self.notes("## 0.2.0-rc.1+ci.2\n- preview\n", "0.2.0-rc.1+ci.2")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "- preview\n")

    def test_missing_and_empty_sections_fail_without_producing_notes(self):
        for changelog in ["## 0x2x0\n- different\n",
                          "## 0.2.0\n\t\n\n## 0.1.0\n- older\n"]:
            with self.subTest(changelog=changelog):
                result = self.notes(changelog)
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")
                self.assertIn("no nonempty CHANGELOG section for 0.2.0", result.stderr)


if __name__ == "__main__":
    unittest.main()
