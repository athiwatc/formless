#!/usr/bin/env python3
"""Pure packaging checks: temporary files only, no Cargo, app or OS launch."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location("package_desktop", Path(__file__).with_name("package-desktop.py"))
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class Packaging(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="formless-package-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def metadata(self, root, **fields):
        return dict(id="test-id", name="fixture", version="1.0", manifest_path=str(root / "Cargo.toml"), license="MIT", repository=None, **fields)

    def test_lowercase_and_embedded_notices_retain_paths_and_hashes(self):
        root = self.root / "crate"
        (root / "data").mkdir(parents=True)
        (root / "license").write_text("fixture crate terms")
        (root / "data/LICENSE-MPL-2.0").write_text("fixture embedded-data terms")
        destination = self.root / "notices"
        inventory = package.collect_notices([self.metadata(root)], {"test-id"}, destination)
        self.assertEqual(inventory[0]["notice_files"], ["fixture-1.0/data/LICENSE-MPL-2.0", "fixture-1.0/license"])
        self.assertEqual([notice["source"] for notice in inventory[0]["notice_sources"]], ["data/LICENSE-MPL-2.0", "license"])
        for notice, source in zip(inventory[0]["notice_files"], inventory[0]["notice_sources"]):
            self.assertEqual(package.digest(destination / notice), source["sha256"])

    def test_root_and_asset_names_do_not_overwrite_each_other(self):
        root = self.root / "crate"
        (root / "assets").mkdir(parents=True)
        (root / "LICENSE").write_text("root terms")
        (root / "assets/LICENSE").write_text("asset terms")
        destination = self.root / "notices"
        package.collect_notices([self.metadata(root)], {"test-id"}, destination)
        self.assertEqual((destination / "fixture-1.0/LICENSE").read_text(), "root terms")
        self.assertEqual((destination / "fixture-1.0/assets/LICENSE").read_text(), "asset terms")

    def test_outside_manifest_paths_and_symlinks_are_not_copied(self):
        root = self.root / "crate"
        root.mkdir()
        (self.root / "outside").write_text("must remain outside")
        (root / "LICENSE").symlink_to(self.root / "outside")
        (root / "assets").symlink_to(self.root, target_is_directory=True)
        inventory = package.collect_notices([self.metadata(root, license_file="../outside")], {"test-id"}, self.root / "notices")
        self.assertEqual(inventory[0]["notice_files"], [])

    def test_oversized_notice_fails_and_unbuilt_packages_are_skipped(self):
        root = self.root / "crate"
        root.mkdir()
        with (root / "LICENSE").open("wb") as source:
            source.truncate(2 * 1024 * 1024 + 1)
        self.assertEqual(package.collect_notices([self.metadata(root)], set(), self.root / "ignored"), [])
        with self.assertRaisesRegex(ValueError, "oversized"):
            package.collect_notices([self.metadata(root)], {"test-id"}, self.root / "notices")

    def bundle(self):
        bundle = self.root / "Formless Development.app"
        executable = bundle / "Contents/MacOS/formless-desktop"
        executable.parent.mkdir(parents=True)
        executable.write_bytes(b"fixture executable bytes; never executed")
        notice = bundle / "Contents/Resources/LICENSE"
        notice.parent.mkdir()
        notice.write_text("fixture terms")
        return bundle, executable, notice

    def test_archives_are_identical_despite_mtime_and_permission_changes(self):
        bundle, executable, notice = self.bundle()
        first, second = self.root / "first.zip", self.root / "second.zip"
        package.write_archive(bundle, executable, first, 1700000001)
        for source in [executable, notice]:
            os.utime(source, (1800000000, 1800000000))
            source.chmod(0o777)
        package.write_archive(bundle, executable, second, 1700000001)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            self.assertIsNone(archive.testzip())
            self.assertEqual(archive.namelist(), sorted(archive.namelist()))
            for info in archive.infolist():
                self.assertEqual(info.date_time, (2023, 11, 14, 22, 13, 20))
                self.assertTrue(stat.S_ISREG(info.external_attr >> 16))
                self.assertEqual(stat.S_IMODE(info.external_attr >> 16), 0o755 if info.filename.endswith("/formless-desktop") else 0o644)

    def test_archive_timestamp_is_clamped_to_zip_range(self):
        bundle, executable, _ = self.bundle()
        for epoch, year in [(-100, 1980), (9999999999, 2107)]:
            path = self.root / f"{year}.zip"
            package.write_archive(bundle, executable, path, epoch)
            with zipfile.ZipFile(path) as archive:
                self.assertTrue(all(info.date_time[0] == year for info in archive.infolist()))

    def test_staged_symlink_is_refused(self):
        bundle, executable, notice = self.bundle()
        (bundle / "linked-license").symlink_to(notice)
        with self.assertRaisesRegex(ValueError, "symlink"):
            package.write_archive(bundle, executable, self.root / "bad.zip", 1700000000)

    def test_source_commit_or_cleanliness_change_is_refused(self):
        with patch.object(package, "run", side_effect=["other-commit"]):
            with self.assertRaisesRegex(ValueError, "commit changed"):
                package.ensure_source(self.root, "expected-commit", False)
        with patch.object(package, "run", side_effect=["expected-commit", " M source.rs"]):
            with self.assertRaisesRegex(ValueError, "cleanliness changed"):
                package.ensure_source(self.root, "expected-commit", False)
        with patch.object(package, "run", side_effect=["expected-commit", ""]):
            package.ensure_source(self.root, "expected-commit", False)

    def test_existing_output_is_refused_before_any_build(self):
        marker = self.root / "preserved"
        marker.write_text("unchanged")
        with patch.object(package, "run") as run:
            with self.assertRaisesRegex(ValueError, "output already exists"):
                package.package(argparse.Namespace(output=str(self.root)))
            run.assert_not_called()
        self.assertEqual(marker.read_text(), "unchanged")

    def test_source_check_excludes_only_the_owned_stage_inside_checkout(self):
        repository = self.root / "source-check-fixture"
        repository.mkdir()
        def git(*arguments):
            return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-C", str(repository), *arguments], text=True, stderr=subprocess.PIPE).strip()
        git("init", "--quiet", "--template=")
        (repository / "tracked").write_text("fixture source")
        git("add", "tracked")
        git("-c", "user.name=Package Fixture", "-c", "user.email=fixture@invalid", "commit", "--quiet", "-m", "fixture")
        commit = git("rev-parse", "HEAD")
        stage = repository / "nested parent/formless-package-owned"
        stage.mkdir(parents=True)
        (stage / "staged-bytes").write_text("owned artifact bytes")
        package.ensure_source(repository, commit, False, stage)
        (repository / "nested parent/other-source.rs").write_text("unrelated new source")
        with self.assertRaisesRegex(ValueError, "cleanliness changed"):
            package.ensure_source(repository, commit, False, stage)

    def test_late_source_change_prevents_publication_of_staged_bundle(self):
        repository = self.root / "repo"
        desktop = repository / "crates/formless-desktop"
        scripts = desktop / "scripts"
        scripts.mkdir(parents=True)
        (repository / "LICENSE").write_text("fixture project terms")
        (desktop / "README.md").write_text("fixture readme")
        (desktop / "Cargo.lock").write_text("fixture locked inputs")
        binary = repository / "target/debug/formless-desktop"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"\xcf\xfa\xed\xfe fixture only; never executed")
        metadata = dict(target_directory=str(repository / "target"), packages=[dict(
            id="fixture-id", name="formless-desktop", version="0.1.0",
            manifest_path=str(desktop / "Cargo.toml"), license="Apache-2.0",
            repository=None,
        )])
        artifact = dict(reason="compiler-artifact", package_id="fixture-id",
                        target=dict(name="formless-desktop"), executable=str(binary))
        revisions = iter(["expected-commit", "expected-commit", "changed-commit"])

        def command(arguments, _cwd):
            if arguments[:3] == ["git", "rev-parse", "HEAD"]:
                return next(revisions)
            if arguments[:2] == ["git", "show"]:
                return "1700000000"
            if arguments[:2] == ["git", "status"]:
                return ""
            if arguments == ["rustc", "-vV"]:
                return "host: aarch64-apple-darwin"
            if arguments[:2] == ["cargo", "metadata"]:
                return json.dumps(metadata)
            if arguments == ["rustc", "--print", "target-list"]:
                return "aarch64-apple-darwin"
            if arguments[:2] == ["otool", "-l"]:
                return "cmd LC_BUILD_VERSION\nminos 13.0"
            self.fail(f"unexpected command: {arguments}")

        output = self.root / "unpublished"
        with patch.object(package, "__file__", str(scripts / "package-desktop.py")), \
                patch.object(package.sys, "platform", "darwin"), \
                patch.object(package, "run", side_effect=command), \
                patch.object(package.subprocess, "Popen") as popen:
            process = popen.return_value.__enter__.return_value
            process.stdout = [json.dumps(artifact)]
            process.wait.return_value = 0
            with self.assertRaisesRegex(ValueError, "commit changed"):
                package.package(argparse.Namespace(output=str(output), allow_dirty=False,
                                                   offline=True, profile="dev"))
        self.assertFalse(output.exists(), "source change published a partial output")
        self.assertEqual(list(self.root.glob("formless-package-*")), [])


if __name__ == "__main__":
    unittest.main()
