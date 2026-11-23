#!/usr/bin/env python3
"""Regression checks for offline reference docs in release archives."""
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
from urllib.parse import unquote, urlsplit
import zipfile

ROOT = Path(__file__).resolve().parents[1]
GUIDES = ("docs/usage.md", "docs/cli.md", "docs/architecture.md")


def local_markdown_links(source):
    for raw in re.findall(r"\]\(([^)]+)\)", source.read_text(encoding="utf-8")):
        target = raw.strip().split(None, 1)[0].strip("<>")
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc or not parsed.path:
            continue
        path = unquote(parsed.path)
        if path.lower().endswith(".md"):
            yield Path(path)


class ReleaseDocArchive(unittest.TestCase):
    def stage_reference_docs(self, destination):
        if os.name == "nt":
            script = r'''
$name = $env:FORMLESS_RELEASE_TEST_PACKAGE
New-Item -ItemType Directory -Path $name -Force | Out-Null
Copy-Item README.md,SPEC.md,LICENSE $name
New-Item -ItemType Directory -Path "$name/packaging","$name/relay","$name/crates/formless-desktop" -Force | Out-Null
Copy-Item GLOSSARY.md "$name"
Copy-Item -Path docs -Destination "$name" -Recurse
Copy-Item -Path packaging/*.md -Destination "$name/packaging"
Copy-Item -Path relay/*.md -Destination "$name/relay"
Copy-Item crates/formless-desktop/README.md "$name/crates/formless-desktop"
'''
            subprocess.run(
                ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
                cwd=ROOT,
                env={**os.environ, "FORMLESS_RELEASE_TEST_PACKAGE": str(destination)},
                check=True,
                timeout=30,
            )
        else:
            script = '''
mkdir -p "$name" "$name/packaging" "$name/relay" "$name/crates/formless-desktop"
cp README.md SPEC.md LICENSE "$name/"
cp GLOSSARY.md "$name/"
cp -R docs "$name/"
cp packaging/*.md "$name/packaging/"
cp relay/*.md "$name/relay/"
cp crates/formless-desktop/README.md "$name/crates/formless-desktop/"
'''
            subprocess.run(
                ["bash", "-eu", "-c", script],
                cwd=ROOT,
                env={**os.environ, "name": str(destination)},
                check=True,
                timeout=30,
            )

    def test_readme_and_bundled_markdown_links_resolve_offline(self):
        with tempfile.TemporaryDirectory(prefix="release archive ") as directory:
            package = Path(directory) / "formless-test"
            self.stage_reference_docs(package)
            for guide in GUIDES:
                self.assertTrue((package / guide).is_file(), guide)
            for source in package.rglob("*.md"):
                for target in local_markdown_links(source):
                    resolved = (source.parent / target).resolve()
                    self.assertTrue(resolved.is_file(), f"{source.relative_to(package)} -> {target}")

    def test_tar_and_zip_layout_keeps_readme_link_targets(self):
        required = {"docs/usage.md", "docs/cli.md", "docs/architecture.md",
                    "GLOSSARY.md", "packaging/README.md", "relay/README.md",
                    "crates/formless-desktop/README.md"}
        with tempfile.TemporaryDirectory(prefix="release archive ") as directory:
            root = Path(directory)
            package = root / "formless-test"
            self.stage_reference_docs(package)
            tar_path = root / "formless-test.tar.gz"
            with tarfile.open(tar_path, "w:gz") as archive:
                archive.add(package, arcname=package.name)
            zip_path = root / "formless-test.zip"
            with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
                for source in package.rglob("*"):
                    if source.is_file():
                        archive.write(source, Path(package.name) / source.relative_to(package))
            for archive_path, open_archive, members in (
                (tar_path, tarfile.open, lambda archive: {Path(member.name).relative_to(package.name).as_posix()
                                                          for member in archive.getmembers() if member.isfile()}),
                (zip_path, zipfile.ZipFile, lambda archive: {Path(name).relative_to(package.name).as_posix()
                                                            for name in archive.namelist()}),
            ):
                with open_archive(archive_path) as archive:
                    self.assertTrue(required.issubset(members(archive)), str(archive_path))

    def test_each_archive_workflow_stages_the_same_reference_layout(self):
        for workflow_name in ("release.yml", "release-baseline.yml"):
            workflow = (ROOT / ".github/workflows" / workflow_name).read_text(encoding="utf-8")
            with self.subTest(workflow=workflow_name):
                for command in (
                    'cp GLOSSARY.md "$name/"',
                    'cp -R docs "$name/"',
                    'cp packaging/*.md "$name/packaging/"',
                    'cp relay/*.md "$name/relay/"',
                    'cp crates/formless-desktop/README.md "$name/crates/formless-desktop/"',
                ):
                    self.assertEqual(workflow.count(command), 2, command)
                for command in (
                    'Copy-Item GLOSSARY.md "$name"',
                    'Copy-Item -Path docs -Destination "$name" -Recurse',
                    'Copy-Item -Path packaging/*.md -Destination "$name/packaging"',
                    'Copy-Item -Path relay/*.md -Destination "$name/relay"',
                    'Copy-Item crates/formless-desktop/README.md "$name/crates/formless-desktop"',
                ):
                    self.assertEqual(workflow.count(command), 1, command)


if __name__ == "__main__":
    unittest.main()
