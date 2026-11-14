#!/usr/bin/env python3
"""Exercise generation failures and both distro asset manifests without packaging."""
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / "scripts/generate-package-completions.sh"
DESTINATIONS = {
    "formless.bash": "/usr/share/bash-completion/completions/formless",
    "_formless": "/usr/share/zsh/site-functions/_formless",
    "formless.fish": "/usr/share/fish/vendor_completions.d/formless.fish",
}


class PackageCompletions(unittest.TestCase):
    def generate(self, mode, shell="zsh"):
        with tempfile.TemporaryDirectory(prefix="package completions ") as directory:
            root = Path(directory)
            binary = root / "built formless"
            binary.write_text('''#!/bin/sh
test "$1" = completions || exit 8
if [ "$2" = "$FAIL_SHELL" ]; then
  case "$COMPLETION_MODE" in failure) printf partial; exit 9;; empty) exit 0;; esac
fi
printf '# generated %s\\n' "$2"
''')
            binary.chmod(0o700)
            output = root / "generated completions"
            result = subprocess.run(
                ["sh", str(GENERATOR), str(binary), str(output)],
                env={**os.environ, "COMPLETION_MODE": mode, "FAIL_SHELL": shell},
                capture_output=True, text=True, timeout=10,
            )
            if mode == "success":
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(set(p.name for p in output.iterdir()), set(DESTINATIONS))
                for name, shell_name in zip(DESTINATIONS, ["bash", "zsh", "fish"]):
                    path = output / name
                    self.assertEqual(path.read_text(), f"# generated {shell_name}\n")
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o644)
            else:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"{shell} completion", result.stderr)
                self.assertEqual(list(output.iterdir()), [], "partial output must not be packaged")

    def test_success_with_paths_containing_spaces(self):
        self.generate("success")

    def test_failure_and_empty_output_in_each_shell(self):
        for shell in ["bash", "zsh", "fish"]:
            for mode in ["failure", "empty"]:
                with self.subTest(shell=shell, mode=mode):
                    self.generate(mode, shell)

    def test_missing_binary_fails_without_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(["sh", str(GENERATOR), directory + "/missing", directory],
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("failed to generate bash completion", result.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_both_manifests_package_every_generated_asset_at_standard_locations(self):
        crate = ROOT / "crates/formless"
        metadata = tomllib.loads((crate / "Cargo.toml").read_text())["package"]["metadata"]
        deb = {"/" + dest: (source, mode) for source, dest, mode in metadata["deb"]["assets"]}
        rpm = {asset["dest"]: (asset["source"], asset["mode"])
               for asset in metadata["generate-rpm"]["assets"]}
        for name, dest in DESTINATIONS.items():
            expected_source = ROOT / "target/completions" / name
            for assets in [deb, rpm]:
                source, mode = assets[dest]
                self.assertEqual((crate / source).resolve(), expected_source)
                self.assertEqual(mode, "644")

    def test_release_and_baseline_generate_before_packaging_and_check_installed_assets(self):
        for name in ["release.yml", "release-baseline.yml"]:
            workflow = (ROOT / ".github/workflows" / name).read_text()
            generation = "sh scripts/generate-package-completions.sh target/release/formless target/completions"
            self.assertLess(workflow.index("cargo build --release"), workflow.index(generation))
            self.assertLess(workflow.index(generation), workflow.index("cargo deb -p"))
            for dest in DESTINATIONS.values():
                self.assertIn(dest, workflow)
            self.assertIn('test -s "$completion"', workflow)
            self.assertIn('dpkg-query -L formless | grep -Fx -- "$completion"', workflow)
            self.assertIn('rpm -qf --qf', workflow)


if __name__ == "__main__":
    unittest.main()
