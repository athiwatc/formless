#!/usr/bin/env python3
"""Offline accounting fixtures; do not run any cleanup operation."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("ci-storage-budget.sh").resolve()


class Snapshot(unittest.TestCase):
    def snapshot(self, target, *, symlink_to=None):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "Cargo.toml").write_text("[workspace]\n")
            (root / ".github/workflows").mkdir(parents=True)
            for name in ["target", "custom", "cargo", "temp"]:
                (root / name).mkdir()
            if symlink_to is not None:
                (root / target).symlink_to(root / symlink_to, target_is_directory=True)
            artifact = root / "custom" / "artifact.bin"
            artifact.write_bytes(os.urandom(1024 * 1024))
            expected_custom_kib = int(subprocess.check_output(
                ["du", "-sk", str(root / "custom")], text=True).split()[0])
            env = {**os.environ, "GITHUB_WORKSPACE": str(root), "RUNNER_TEMP": str(root / "temp"), "CARGO_HOME": str(root / "cargo")}
            env.pop("CARGO_TARGET_DIR", None)
            if target is not None:
                env["CARGO_TARGET_DIR"] = str(root / target)
            result = subprocess.run(["bash", str(SCRIPT), "snapshot", "fixture"], cwd=root,
                                    env=env, text=True, capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((root / "target").is_dir(), "snapshot never removes artifacts")
            self.assertEqual(artifact.stat().st_size, 1024 * 1024,
                             "snapshot never removes artifacts")
            if target == "custom" or symlink_to == "custom":
                line = next(line for line in result.stdout.splitlines()
                            if line.startswith("storage CARGO_TARGET_DIR:"))
                reported_kib = int(line.split(": ", 1)[1].split()[0])
                self.assertEqual(reported_kib, expected_custom_kib)
                self.assertGreaterEqual(reported_kib, 1024)
            return result.stdout

    def test_private_target_is_reported_and_labeled(self):
        output = self.snapshot("custom")
        self.assertIn("storage CARGO_TARGET_DIR:", output)
        self.assertIn("/custom", output)
        self.assertEqual(output.count("storage checkout target:"), 1)
        self.assertEqual(output.count("storage Cargo home:"), 1)

    def test_symlinked_custom_target_measures_artifacts(self):
        self.snapshot("custom-link", symlink_to="custom")

    def test_symlinked_default_target_is_not_double_counted(self):
        output = self.snapshot("target-link", symlink_to="target")
        self.assertEqual(output.count("storage checkout target:"), 1)
        self.assertNotIn("storage CARGO_TARGET_DIR:", output)

    def test_default_equivalent_target_is_not_double_counted(self):
        for target in [None, "target", "target/../target", "missing"]:
            output = self.snapshot(target)
            self.assertEqual(output.count("storage checkout target:"), 1)
            self.assertNotIn("storage CARGO_TARGET_DIR:", output)


if __name__ == "__main__":
    unittest.main()
