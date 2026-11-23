#!/usr/bin/env python3
"""Regression checks for offline reference docs in release archives."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
import unittest
from urllib.parse import unquote, urlsplit
import zipfile

ROOT = Path(__file__).resolve().parents[1]
GUIDES = ("docs/usage.md", "docs/cli.md", "docs/architecture.md")
REQUIRED_ARCHIVE_FILES = {
    "docs/usage.md",
    "docs/cli.md",
    "docs/architecture.md",
    "GLOSSARY.md",
    "packaging/README.md",
    "relay/README.md",
    "crates/formless-desktop/README.md",
}


def relative_file_links(source):
    text = source.read_text(encoding="utf-8")
    text = re.sub(r"(?ms)^```.*?^```[ \t]*$", "", text)
    text = re.sub(r"(?ms)^~~~.*?^~~~[ \t]*$", "", text)
    text = re.sub(r"`+[^`\n]*`+", "", text)
    raw_links = re.findall(r"\]\(([^)\n]+)\)", text)
    raw_links.extend(re.findall(r"(?im)^\s*\[[^\]]+\]:\s*(<[^>]+>|\S+)", text))
    raw_links.extend(re.findall(r"(?i)(?:href|src)\s*=\s*[\"']([^\"']+)[\"']", text))
    for raw in raw_links:
        target = raw.strip()
        if target.startswith("<") and ">" in target:
            target = target[1:target.index(">")]
        else:
            target = target.split(None, 1)[0]
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc or not parsed.path or parsed.path.startswith("/"):
            continue
        yield Path(unquote(parsed.path))


class ReleaseDocArchive(unittest.TestCase):
    def archive_members(self, names, root_name):
        prefix = f"{root_name}/"
        return {
            name[len(prefix):] if name.startswith(prefix) else name
            for name in (name.replace("\\", "/") for name in names)
            if name and not name.endswith("/")
        }

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

    def test_readme_and_bundled_markdown_relative_file_links_resolve_offline(self):
        with tempfile.TemporaryDirectory(prefix="release archive ") as directory:
            package = Path(directory) / "formless-test"
            self.stage_reference_docs(package)
            for guide in GUIDES:
                self.assertTrue((package / guide).is_file(), guide)
            for source in package.rglob("*.md"):
                for target in relative_file_links(source):
                    resolved = (source.parent / target).resolve()
                    self.assertTrue(resolved.is_file(), f"{source.relative_to(package)} -> {target}")

    def test_linux_python_tar_and_zip_fixtures_keep_readme_link_targets(self):
        if not sys.platform.startswith("linux"):
            self.skipTest("Python-created archives are Linux layout fixtures, not native packaging")
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
            with tarfile.open(tar_path) as archive:
                members = self.archive_members(
                    [member.name for member in archive.getmembers() if member.isfile()], package.name
                )
                self.assertTrue(REQUIRED_ARCHIVE_FILES.issubset(members), str(tar_path))
            with zipfile.ZipFile(zip_path) as archive:
                members = self.archive_members(archive.namelist(), package.name)
                self.assertTrue(REQUIRED_ARCHIVE_FILES.issubset(members), str(zip_path))

    def test_windows_powershell_zip_keeps_readme_link_targets(self):
        if os.name != "nt":
            self.skipTest("native PowerShell ZIP packaging is Windows-only")
        with tempfile.TemporaryDirectory(prefix="release archive ") as directory:
            package = Path(directory) / "formless-test"
            self.stage_reference_docs(package)
            zip_path = Path(f"{package}.zip")
            script = r'''
$name = $env:FORMLESS_RELEASE_TEST_PACKAGE
Compress-Archive -Path $name -DestinationPath "$name.zip"
'''
            subprocess.run(
                ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
                cwd=ROOT,
                env={**os.environ, "FORMLESS_RELEASE_TEST_PACKAGE": str(package)},
                check=True,
                timeout=30,
            )
            with zipfile.ZipFile(zip_path) as archive:
                members = self.archive_members(archive.namelist(), package.name)
                self.assertTrue(REQUIRED_ARCHIVE_FILES.issubset(members), str(zip_path))

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
                compress_command = (
                    'Compress-Archive -Path $name -DestinationPath "$name.zip"'
                    if workflow_name == "release.yml"
                    else 'Compress-Archive -Path $name -DestinationPath "dist/$name.zip"'
                )
                self.assertEqual(workflow.count(compress_command), 1, compress_command)


if __name__ == "__main__":
    unittest.main()
