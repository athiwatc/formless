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

    def shipping_metadata(self):
        return dict(packages=[dict(id="desktop", name="formless-desktop"),
                              dict(id="kit", name="gpui-kit"), dict(id="protocol", name="formless-protocol")],
                    resolve=dict(root="desktop", nodes=[
                        dict(id="desktop", features=["gui"], deps=[dict(pkg="kit", dep_kinds=[dict(kind=None)]),dict(pkg="protocol",dep_kinds=[dict(kind="build")])]),
                        dict(id="kit", features=[], deps=[]), dict(id="protocol",features=[],deps=[])]))

    def test_shipping_graph_is_gui_only_and_checks_transitive_normal_and_build_edges(self):
        metadata=self.shipping_metadata()
        receipt=package.shipping_graph(metadata)
        self.assertEqual(receipt["normal_and_build_package_ids"],["desktop","kit","protocol"])
        self.assertEqual(receipt["features"],["gui"])
        package.verify_shipping_artifacts(metadata,{"desktop","kit","protocol"},["gui"])
        for name in package.SERVER_PACKAGES:
            with self.subTest(name=name):
                metadata=self.shipping_metadata();metadata["packages"].append(dict(id="server",name=name))
                metadata["resolve"]["nodes"][-1]["deps"]=[dict(pkg="server",dep_kinds=[dict(kind=None)])]
                metadata["resolve"]["nodes"].append(dict(id="server",features=[],deps=[]))
                with self.assertRaisesRegex(ValueError,"server package"):
                    package.shipping_graph(metadata)

    def test_shipping_graph_refuses_feature_and_resolution_ambiguity(self):
        for features in [[],["default","gui"],["gui","daemon-tests"],["gui","test-support"],["gui","thai-breaks"],["gui","ax-dump"]]:
            with self.subTest(features=features):
                metadata=self.shipping_metadata();metadata["resolve"]["nodes"][0]["features"]=features
                with self.assertRaisesRegex(ValueError,"only gui"):
                    package.shipping_graph(metadata)
        for change in ["root","unresolved","kind","kit","duplicate"]:
            metadata=self.shipping_metadata()
            if change=="root":metadata["resolve"]["root"]="missing"
            elif change=="unresolved":metadata["resolve"]["nodes"].pop()
            elif change=="kind":metadata["resolve"]["nodes"][0]["deps"][0]["dep_kinds"]=[dict(kind="future")]
            elif change=="kit":metadata["resolve"]["nodes"][1]["features"]=["test-support"]
            else:metadata["packages"].append(metadata["packages"][0].copy())
            with self.subTest(change=change),self.assertRaises(ValueError):
                package.shipping_graph(metadata)

    def test_shipping_graph_ignores_dev_only_edges_but_refuses_server_build_artifacts(self):
        metadata=self.shipping_metadata();metadata["packages"].append(dict(id="server",name="formless-core"))
        metadata["resolve"]["nodes"].append(dict(id="server",features=[],deps=[]))
        metadata["resolve"]["nodes"][0]["deps"].append(dict(pkg="server",dep_kinds=[dict(kind="dev")]))
        self.assertNotIn("server",package.shipping_graph(metadata)["normal_and_build_package_ids"])
        for built,features in [({"desktop","server"},["gui"]),({"desktop","missing"},["gui"]),({"kit"},["gui"]),({"desktop"},["gui","daemon-tests"]),({"desktop"},None)]:
            with self.subTest(built=built,features=features),self.assertRaises(ValueError):
                package.verify_shipping_artifacts(metadata,built,features)

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

    def reviewed_fixture(self):
        base = Path(tempfile.mkdtemp(dir=self.root))
        root = base / "registry-crate"
        root.mkdir()
        (root / ".cargo_vcs_info.json").write_text('{"git":{"sha1":"fixture revision"}}')
        upstream = base / "desktop/notices"
        local = "upstream/fixture/revision/LICENSE-MIT"
        source = upstream / local
        source.parent.mkdir(parents=True)
        source.write_text("fixture upstream terms")
        lock = upstream.parent / "Cargo.lock"
        lock.write_text("fixture locked dependency graph")
        metadata = self.metadata(root)
        metadata.update(source="registry+fixture", repository="https://github.com/fixture/project")
        entry = dict(name="fixture", version="1.0", license="MIT", source_kind="registry",
                     repository_field="repository", repository_metadata_value=metadata["repository"],
                     vcs_sha256=package.digest(root / ".cargo_vcs_info.json"), revision="fixture revision",
                     files=[dict(local=local, source="LICENSE-MIT", sha256=package.digest(source),
                                 url="https://example.invalid/immutable/LICENSE-MIT", git_blob="fixture blob")])
        document = dict(schema=1, cargo_lock_sha256=package.digest(lock), packages=[entry])
        (upstream / "UPSTREAM.json").write_text(json.dumps(document))
        return root, upstream, lock, metadata, document, source

    def test_reviewed_notice_is_bound_to_lock_vcs_repository_and_content(self):
        root, upstream, lock, metadata, _, source = self.reviewed_fixture()
        destination = self.root / "collected"
        inventory = package.collect_notices([metadata], {"test-id"}, destination, upstream, lock)
        notice = inventory[0]["notice_files"][0]
        self.assertEqual((destination / notice).read_bytes(), source.read_bytes())
        receipt = inventory[0]["notice_sources"][0]
        self.assertEqual(receipt["revision"], "fixture revision")
        self.assertEqual(receipt["sha256"], package.digest(source))

    def test_reviewed_notice_input_changes_fail_closed(self):
        for changed in ["lock", "vcs", "repository", "license", "bytes"]:
            with self.subTest(changed=changed):
                root, upstream, lock, metadata, _, source = self.reviewed_fixture()
                if changed == "lock":
                    lock.write_text("different graph")
                elif changed == "vcs":
                    (root / ".cargo_vcs_info.json").write_text("different receipt")
                elif changed == "repository":
                    metadata["repository"] = "https://example.invalid/other"
                elif changed == "license":
                    metadata["license"] = "different declaration"
                else:
                    source.write_text("different terms")
                with self.assertRaises(ValueError):
                    package.collect_notices([metadata], {"test-id"}, root / "out", upstream, lock)

    def test_supplemental_notice_keeps_its_own_revision_and_review_limitation(self):
        root, upstream, lock, metadata, document, source = self.reviewed_fixture()
        entry = document["packages"][0]
        entry["collection_limitations"] = ["Earlier release attribution remains open"]
        supplemental = source.with_name("LICENSE-REFERENCE")
        supplemental.write_text("fixture later upstream text")
        entry["files"].append(dict(local=supplemental.relative_to(upstream).as_posix(),
            source="LICENSE-REFERENCE", sha256=package.digest(supplemental),
            revision="later reference revision", role="supplemental-reference"))
        (upstream / "UPSTREAM.json").write_text(json.dumps(document))
        inventory = package.collect_notices([metadata], {"test-id"}, root / "out", upstream, lock)
        receipts = inventory[0]["notice_sources"]
        self.assertEqual([receipt["revision"] for receipt in receipts],
                         ["fixture revision", "later reference revision"])
        self.assertEqual([receipt["role"] for receipt in receipts],
                         ["pinned-release-notice", "supplemental-reference"])
        self.assertEqual(inventory[0]["notice_review_limitations"], entry["collection_limitations"])
        supplemental.write_text("changed reference")
        with self.assertRaisesRegex(ValueError, "hash/size changed"):
            package.collect_notices([metadata], {"test-id"}, root / "changed-out", upstream, lock)

    def test_reviewed_notice_paths_and_symlinks_cannot_escape(self):
        for local in ["../outside", "/absolute", "C:\\outside"]:
            with self.subTest(local=local):
                root, upstream, lock, metadata, document, _ = self.reviewed_fixture()
                document["packages"][0]["files"][0]["local"] = local
                (upstream / "UPSTREAM.json").write_text(json.dumps(document))
                with self.assertRaises(ValueError):
                    package.collect_notices([metadata], {"test-id"}, root / "out", upstream, lock)
        root, upstream, lock, metadata, _, source = self.reviewed_fixture()
        source.unlink()
        source.symlink_to(lock)
        with self.assertRaises(ValueError):
            package.collect_notices([metadata], {"test-id"}, root / "out", upstream, lock)

    def toolchain_fixture(self):
        upstream = Path(tempfile.mkdtemp(dir=self.root))
        commit = "a" * 40
        files = []
        for name in ["LICENSE-APACHE", "LICENSE-MIT", "COPYRIGHT"]:
            local = "toolchain/" + name
            path = upstream / local
            path.parent.mkdir(exist_ok=True)
            path.write_text("fixture " + name)
            files.append(dict(name=name, local=local, sha256=package.digest(path)))
        document = dict(schema=1, compiler_commit_hash=commit, files=files)
        (upstream / "TOOLCHAIN.json").write_text(json.dumps(document))
        return upstream, document, "commit-hash: " + commit

    def test_toolchain_notices_are_bound_to_compiler_and_exact_content(self):
        upstream, document, compiler = self.toolchain_fixture()
        destination = self.root / "toolchain-out"
        receipt = package.collect_toolchain_notices(upstream, destination, compiler)
        self.assertEqual(receipt, package.digest(upstream / "TOOLCHAIN.json"))
        for notice in document["files"]:
            self.assertEqual(package.digest(destination / notice["name"]), notice["sha256"])
        self.assertTrue((destination / "PROVENANCE.json").is_file())

    def test_toolchain_changes_and_incomplete_sets_fail_closed(self):
        for changed in ["compiler", "bytes", "incomplete", "path"]:
            with self.subTest(changed=changed):
                upstream, document, compiler = self.toolchain_fixture()
                if changed == "compiler":
                    compiler = "commit-hash: " + "b" * 40
                elif changed == "bytes":
                    (upstream / document["files"][0]["local"]).write_text("changed terms")
                elif changed == "incomplete":
                    document["files"].pop()
                else:
                    document["files"][0]["name"] = "../outside"
                (upstream / "TOOLCHAIN.json").write_text(json.dumps(document))
                with self.assertRaises(ValueError):
                    package.collect_toolchain_notices(upstream, self.root / "not-published", compiler)

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

    def test_server_graph_is_refused_before_build_or_staging(self):
        repository=self.root / "repo"
        scripts=repository / "crates/formless-desktop/scripts"
        scripts.mkdir(parents=True)
        metadata=self.shipping_metadata()
        metadata["packages"][-1]["name"]="formless-core"
        def command(arguments,_cwd):
            if arguments[:3]==["git","rev-parse","HEAD"]:return "expected-commit"
            if arguments[:2]==["git","show"]:return "1700000000"
            if arguments[:2]==["git","status"]:return ""
            if arguments==["rustc","-vV"]:return "host: aarch64-apple-darwin"
            if arguments[:2]==["cargo","metadata"]:
                self.assertIn("--no-default-features",arguments)
                self.assertEqual(arguments[arguments.index("--features")+1],"gui")
                return json.dumps(metadata)
            self.fail(f"unexpected command: {arguments}")
        output=self.root / "refused-output"
        with patch.object(package,"__file__",str(scripts / "package-desktop.py")), \
                patch.object(package.sys,"platform","darwin"), \
                patch.object(package,"run",side_effect=command), \
                patch.object(package.subprocess,"Popen") as popen:
            with self.assertRaisesRegex(ValueError,"server package"):
                package.package(argparse.Namespace(output=str(output),allow_dirty=False,offline=True,profile="dev"))
            popen.assert_not_called()
        self.assertFalse(output.exists())
        self.assertEqual(list(self.root.glob("formless-package-*")),[])

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
        metadata["resolve"] = dict(root="fixture-id", nodes=[dict(id="fixture-id", features=["gui"], deps=[])])
        artifact = dict(reason="compiler-artifact", package_id="fixture-id", features=["gui"], profile=dict(test=False),
                        target=dict(name="formless-desktop", kind=["bin"]), executable=str(binary))
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
