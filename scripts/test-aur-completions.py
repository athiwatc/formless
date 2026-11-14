#!/usr/bin/env python3
"""Run the rendered package function without makepkg, network or pipefail."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

TEMPLATE = Path(__file__).resolve().parents[1] / "packaging/aur/PKGBUILD.in"


class AurCompletions(unittest.TestCase):
    def package(self, mode):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            stage = root / "formless-v9.8.7-x86_64-unknown-linux-gnu"
            stage.mkdir()
            for name in ["formless.1", "README.md", "LICENSE"]:
                (stage / name).write_text("fixture\n")
            binary = stage / "formless"
            binary.write_text('''#!/bin/sh
if [ "$2" = zsh ]; then
  case "$COMPLETION_MODE" in failure) printf partial; exit 9;; empty) exit 0;; esac
fi
printf '# completion %s\\n' "$2"
''')
            binary.chmod(0o700)
            template = root / "PKGBUILD"
            template.write_text(TEMPLATE.read_text().replace("@VERSION@", "9.8.7").replace("@SHA256_LINUX@", "a" * 64))
            result = subprocess.run(["bash", "-ec", 'set +o pipefail; source "$1"; package', "fixture", str(template)],
                                    cwd=root, env={**os.environ, "pkgdir": str(root / "pkg"), "COMPLETION_MODE": mode},
                                    text=True, capture_output=True, timeout=10)
            generated = root / "pkg/usr/share/bash-completion/completions/formless"
            if mode == "success":
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(generated.read_text().startswith("# completion bash"))
            else:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("zsh completion", result.stderr)
                self.assertFalse(generated.exists(), "failed generators must not install partial completions")
            return result

    def test_generator_exit_is_not_hidden_by_install(self):
        self.package("failure")

    def test_empty_output_is_rejected(self):
        self.package("empty")

    def test_success_installs_generated_completions(self):
        self.package("success")


if __name__ == "__main__":
    unittest.main()
