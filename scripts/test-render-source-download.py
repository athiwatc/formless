#!/usr/bin/env python3
"""Offline renderer download tests using a fake curl executable."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("render-packaging.sh").resolve()
PAYLOAD = b"synthetic source archive\n"


class SourceDownload(unittest.TestCase):
    def render(self, src_sha=None, fail=False):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            assets = root / "assets"
            assets.mkdir()
            for target, ext in [("x86_64-unknown-linux-gnu", "tar.gz"), ("x86_64-pc-windows-msvc", "zip")]:
                (assets / f"formless-v9.8.7-{target}.{ext}.sha256").write_text("a" * 64 + "  fixture\n")
            manifest = root / "Cargo.toml"
            manifest.write_text('[workspace.package]\nversion = "9.8.7"\n')
            curl = root / "curl"
            curl.write_text('''#!/bin/sh
printf '%s\\n' "$@" > "$CURL_ARGS"
while [ "$1" != -o ]; do shift; done
shift
printf '%s' "$1" > "$CURL_TEMP"
printf 'synthetic source archive\\n' > "$1"
exit "${CURL_STATUS:-0}"
''')
            curl.chmod(0o700)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                   "MANIFEST": str(manifest), "CURL_ARGS": str(root / "args"),
                   "CURL_TEMP": str(root / "temp"), "CURL_STATUS": "22" if fail else "0"}
            env.pop("SRC_SHA", None)
            if src_sha is not None:
                env["SRC_SHA"] = src_sha
            result = subprocess.run(["bash", str(SCRIPT), "9.8.7", str(assets), str(root / "out")],
                                    env=env, text=True, capture_output=True, timeout=10)
            args = (root / "args").read_text().splitlines() if (root / "args").exists() else []
            if (root / "temp").exists():
                self.assertFalse(Path((root / "temp").read_text()).exists(), "temporary archive leaked")
            formula = root / "out/homebrew/formless.rb"
            return result, args, formula.read_text() if formula.exists() else ""

    def test_download_is_bounded_retried_hashed_and_cleaned(self):
        result, args, formula = self.render()
        self.assertEqual(result.returncode, 0, result.stderr)
        for flag, value in [("--connect-timeout", "15"), ("--max-time", "120"), ("--retry", "3")]:
            self.assertEqual(args[args.index(flag) + 1], value)
        self.assertIn("--retry-all-errors", args)
        self.assertIn(hashlib.sha256(PAYLOAD).hexdigest(), formula)

    def test_failed_download_has_a_step_diagnostic_and_cleans_partial_bytes(self):
        result, _, formula = self.render(fail=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not download https://github.com/athiwatc/formless/archive/refs/tags/v9.8.7.tar.gz", result.stderr)
        self.assertIn("set SRC_SHA to skip", result.stderr)
        self.assertEqual(formula, "")

    def test_override_skips_curl_and_retains_sha_validation(self):
        result, args, formula = self.render(src_sha="b" * 64)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(args, [])
        self.assertIn("b" * 64, formula)
        result, args, _ = self.render(src_sha="invalid")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(args, [])
        self.assertIn("no checksum", result.stderr)


if __name__ == "__main__":
    unittest.main()
