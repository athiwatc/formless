#!/usr/bin/env python3
"""Offline renderer tests using a fake curl executable and synthetic assets."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from zipfile import ZipFile

SCRIPT = Path(__file__).with_name("render-packaging.sh").resolve()
PAYLOAD = b"synthetic source archive\n"
WINDOWS_STEM = "formless-v9.8.7-x86_64-pc-windows-msvc"
WINDOWS_EXE = f"{WINDOWS_STEM}/formless.exe"
RENDER_TOOLS = ("awk", "basename", "bash", "cut", "dirname", "grep", "mkdir",
                "mktemp", "rm", "sed")


class SourceDownload(unittest.TestCase):
    def render(
        self, src_sha=None, fail=False, *, hashers=None, zip_entry=None,
        zip_tools=("python3",), failing_python=False,
    ):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            assets = root / "assets"
            assets.mkdir()
            for target, ext in [("x86_64-unknown-linux-gnu", "tar.gz"), ("x86_64-pc-windows-msvc", "zip")]:
                (assets / f"formless-v9.8.7-{target}.{ext}.sha256").write_text("a" * 64 + "  fixture\n")
            if zip_entry is not None:
                with ZipFile(assets / f"{WINDOWS_STEM}.zip", "w") as archive:
                    archive.writestr(zip_entry, b"synthetic executable")
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
            path = str(root) + os.pathsep + os.environ["PATH"]
            if hashers is not None:
                for tool in (*RENDER_TOOLS, *hashers, *zip_tools):
                    executable = shutil.which(tool)
                    self.assertIsNotNone(executable, f"missing fixture tool {tool}")
                    (root / tool).symlink_to(executable)
                path = str(root)
            if failing_python:
                python = root / "python3"
                with python.open("x") as script:
                    script.write("#!/bin/sh\nexit 1\n")
                python.chmod(0o700)
            env = {**os.environ, "PATH": path,
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

    @unittest.skipUnless(shutil.which("sha256sum"), "requires sha256sum")
    def test_download_hashes_with_sha256sum_without_shasum(self):
        result, _, formula = self.render(hashers=("sha256sum",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(hashlib.sha256(PAYLOAD).hexdigest(), formula)

    @unittest.skipUnless(shutil.which("shasum"), "requires shasum")
    def test_download_hashes_with_shasum_without_sha256sum(self):
        result, _, formula = self.render(hashers=("shasum",))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(hashlib.sha256(PAYLOAD).hexdigest(), formula)

    def test_missing_hashers_reports_requirement_before_download(self):
        result, args, formula = self.render(hashers=())
        self.assertEqual(result.returncode, 1)
        self.assertIn("sha256sum or shasum is required", result.stderr)
        self.assertEqual(args, [])
        self.assertEqual(formula, "")

    def check_windows_zip_members(self, zip_tools):
        for entry in (WINDOWS_EXE, WINDOWS_EXE.replace("/", "\\")):
            with self.subTest(entry=entry):
                result, _, _ = self.render(
                    src_sha="b" * 64, hashers=(), zip_entry=entry, zip_tools=zip_tools,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
        result, _, _ = self.render(
            src_sha="b" * 64, hashers=(), zip_entry=WINDOWS_EXE.replace("formless.exe", "formlessXexe"),
            zip_tools=zip_tools,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"does not hold {WINDOWS_EXE}", result.stderr)

    def test_windows_zip_checks_exact_member_without_unzip(self):
        self.check_windows_zip_members(("python3",))

    @unittest.skipUnless(shutil.which("unzip"), "requires unzip")
    def test_windows_zip_checks_exact_member_without_python(self):
        self.check_windows_zip_members(("unzip", "tr"))

    @unittest.skipUnless(shutil.which("unzip"), "requires unzip")
    def test_unzip_still_checks_windows_zip_when_python_cannot_start(self):
        result, _, _ = self.render(
            src_sha="b" * 64, hashers=(), zip_entry=WINDOWS_EXE,
            zip_tools=("unzip", "tr"), failing_python=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_zip_readers_reports_the_requirement(self):
        result, _, _ = self.render(src_sha="b" * 64, hashers=(), zip_entry=WINDOWS_EXE, zip_tools=())
        self.assertEqual(result.returncode, 1)
        self.assertIn("python3 or unzip is required", result.stderr)


if __name__ == "__main__":
    unittest.main()
