#!/usr/bin/env python3
"""Nonpublishing regression fixtures for image identity and channel verdicts."""
from email.message import Message
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from textwrap import dedent
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release_image", ROOT / "scripts/push-release-image.py")
image = importlib.util.module_from_spec(spec)
spec.loader.exec_module(image)
TESTED = "sha256:" + "a" * 64
MANIFEST = "sha256:" + "b" * 64
REF = "ghcr.io/athiwatc/formless"


class ReleasePublication(unittest.TestCase):
    def test_preflight_checks_tag_before_reading_registry_credentials(self):
        for version, code in (("v0.1.0", 0), ("v0.1.0-rc.1", 0), ("v0.1.0+metadata", 1)):
            with self.subTest(version=version):
                result = subprocess.run(
                    [image.sys.executable, str(ROOT / "scripts/push-release-image.py"), "--check-version"],
                    env={"GITHUB_REF_NAME": version}, capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode, code, result.stderr)

    def test_summary_outcomes(self):
        for outcomes, latest, code, expected in [
            (["success"] * 5, "promoted", 0,
             ["| GitHub Release | published |", "| AUR | published |",
              "| Docker (ghcr.io) | published (latest promoted) |"]),
            (["failure", "skipped", "skipped", "skipped", "skipped"], "", 1,
             ["| GitHub Release | FAILED |", "| AUR | not run (earlier step failed or cancelled) |",
              "| Docker (ghcr.io) | not run (required publication steps did not complete) |"]),
            (["success", "skipped", "success", "skipped", "success"],
             "skipped (not the highest stable tag)", 0,
             ["| Homebrew tap | skipped (no secret) |", "| winget | published |",
              "| Docker (ghcr.io) | published (latest skipped: not the highest stable tag) |"]),
            (["success", "failure", "success", "cancelled", "failure"], "", 1,
             ["| Homebrew tap | FAILED |", "| AUR | cancelled |", "| Docker (ghcr.io) | FAILED |"]),
        ]:
            with self.subTest(outcomes=outcomes), tempfile.TemporaryDirectory() as directory:
                summary = Path(directory) / "summary"
                result = subprocess.run(
                    ["bash", str(ROOT / "scripts/release-channel-summary.sh"), *outcomes],
                    env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary), "DOCKER_LATEST": latest},
                    capture_output=True, text=True, timeout=5,
                )
                self.assertEqual(result.returncode, code, result.stderr)
                text = summary.read_text()
                for row in expected:
                    self.assertIn(row, text)

    def test_summary_preserves_partial_docker_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/release-channel-summary.sh"),
                 "success", "skipped", "skipped", "skipped", "failure"],
                env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary),
                     "DOCKER_VERSION": "published", "DOCKER_LATEST": ""},
                capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("| Docker (ghcr.io) | version published (job failure, latest unverified) |",
                          summary.read_text())

    def test_summary_distinguishes_prerelease_from_missing_channel_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            subprocess.run(
                ["bash", str(ROOT / "scripts/release-channel-summary.sh"),
                 "success", "skipped", "skipped", "skipped", "success"],
                env={"PATH": os.environ["PATH"], "GITHUB_STEP_SUMMARY": str(summary), "PRERELEASE": "true"},
                check=True, timeout=5,
            )
            for channel in ("Homebrew tap", "winget", "AUR"):
                self.assertIn(f"| {channel} | skipped (prerelease) |", summary.read_text())

    def test_prerelease_cannot_render_stable_channel_manifests(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory) / "manifests"
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[workspace.package]\nversion = "0.2.0-rc.1"\n')
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/render-packaging.sh"), "0.2.0-rc.1", directory, str(out)],
                env={**os.environ, "MANIFEST": str(manifest)}, capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("stable channel manifests cannot be rendered for prerelease", result.stderr)
            self.assertFalse(out.exists())

    def test_workflow_classifies_tags_before_publication(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        preflight = workflow.split("\n  version-check:\n", 1)[1].split("\n  build:\n", 1)[0]
        classify = re.search(r"(?m)^        id: tag-kind\n[^\n]+\n        run: \|\n((?:          [^\n]+\n)+)", preflight)
        self.assertIsNotNone(classify)
        self.assertIn("prerelease: ${{ steps.tag-kind.outputs.prerelease }}", preflight)
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            for version, expected in (("v0.2.0-rc.1", "true"), ("v0.2.0", "false"), ("v0.2.0+build", None)):
                with self.subTest(version=version):
                    output.write_text("")
                    result = subprocess.run(
                        ["bash", "-e", "-c", dedent(classify[1])], cwd=ROOT,
                        env={"PATH": os.environ["PATH"], "GITHUB_REF_NAME": version,
                             "GITHUB_OUTPUT": str(output)},
                        capture_output=True, text=True, timeout=5,
                    )
                    self.assertEqual(result.returncode, int(expected is None), result.stderr)
                    self.assertEqual(output.read_text(), f"prerelease={expected}\n" if expected else "")

        release = workflow.split("\n  publish:\n", 1)[1].split("\n  docker-publish:\n", 1)[0]
        stable = "needs.version-check.outputs.prerelease == 'false'"
        self.assertIn("prerelease: ${{ needs.version-check.outputs.prerelease }}", release)
        self.assertIn("make_latest: ${{ needs.version-check.outputs.prerelease == 'true' && 'false' || '' }}", release)
        self.assertIn(f"name: Render Homebrew, AUR and winget files\n        if: {stable}", release)
        for secret in ("TAP_TOKEN", "WINGET_TOKEN", "AUR_KEY"):
            self.assertIn(f"if: {stable} && env.{secret} != ''", release)
        for path in ("packaging-out/aur/PKGBUILD", "packaging-out/homebrew/formless.rb", "packaging-out/winget/*.yaml"):
            self.assertIn("${{ " + stable + f" && '{path}' || ''" + " }}", release)

    def test_new_version_push_and_latest_uses_the_verified_manifest(self):
        registry = Mock()
        registry.manifest.side_effect = [None, (TESTED, MANIFEST), (TESTED, MANIFEST)]
        with patch.object(image, "docker", return_value=TESTED) as docker, patch.object(
            image, "newest_stable_tag", return_value="v9.8.7"
        ):
            output = io.StringIO()
            self.assertEqual(image.publish(registry, "v9.8.7", "local-test", TESTED, output), "promoted")
        self.assertEqual(output.getvalue(), "version=published\n")
        commands = [call.args for call in docker.call_args_list]
        self.assertIn(("image", "tag", TESTED, REF + ":v9.8.7"), commands)
        self.assertIn(("image", "push", REF + ":v9.8.7"), commands)
        self.assertIn(("image", "rm", REF + ":v9.8.7"), commands)
        self.assertEqual(commands[-1], ("buildx", "imagetools", "create", "--prefer-index=false",
                                        "--tag", REF + ":latest", REF + "@" + MANIFEST))
        self.assertEqual([call.args for call in registry.manifest.call_args_list],
                         [("v9.8.7",), ("v9.8.7",), ("latest",)])

    def test_old_and_prerelease_reruns_preserve_version_and_latest(self):
        for version in ("v9.8.6", "v10.0.0-rc.1"):
            registry = Mock()
            registry.manifest.return_value = (TESTED, MANIFEST)
            with self.subTest(version=version), patch.object(image, "docker", return_value=TESTED) as docker, patch.object(
                image, "newest_stable_tag", return_value="v9.8.7"
            ):
                self.assertEqual(image.publish(registry, version, "local-test", TESTED, io.StringIO()),
                                 "skipped (not the highest stable tag)")
                docker.assert_called_once_with("image", "inspect", "--format", "{{.Id}}", "local-test")
                registry.manifest.assert_called_once_with(version)

    def test_mismatched_local_or_registry_image_refuses_mutation(self):
        for local_id in (TESTED, MANIFEST):
            registry = Mock()
            registry.manifest.return_value = (MANIFEST, TESTED)
            with self.subTest(local_id=local_id), patch.object(image, "docker", return_value=local_id) as docker:
                with self.assertRaisesRegex(ValueError, "different image|smoke-tested image"):
                    image.publish(registry, "v9.8.7", "local-test", TESTED, io.StringIO())
                docker.assert_called_once_with("image", "inspect", "--format", "{{.Id}}", "local-test")
                if local_id == MANIFEST:
                    registry.manifest.assert_not_called()

    def test_failed_version_push_does_not_promote_and_cleans_local_tag(self):
        registry = Mock()
        registry.manifest.return_value = None

        def docker_fixture(*args):
            if args[:2] == ("image", "push"):
                raise subprocess.CalledProcessError(1, args)
            return TESTED

        with patch.object(image, "docker", side_effect=docker_fixture) as docker:
            with self.assertRaises(subprocess.CalledProcessError):
                image.publish(registry, "v9.8.7", "local-test", TESTED, io.StringIO())
        self.assertEqual(docker.call_args.args, ("image", "rm", REF + ":v9.8.7"))
        self.assertFalse(any(call.args[0] == "buildx" for call in docker.call_args_list))

    def test_registry_absence_and_failure_are_distinct(self):
        for status, code, absence in [(404, "MANIFEST_UNKNOWN", True),
                                      (404, "NAME_UNKNOWN", True),
                                      (404, "DENIED", False),
                                      (401, "UNAUTHORIZED", False),
                                      (503, "UNAVAILABLE", False)]:
            with self.subTest(status=status, code=code), patch.object(image, "registry_response") as request:
                request.return_value = (b'{"token":"fixture"}', Message())
                registry = image.Registry("fixture", "fixture")
                request.side_effect = HTTPError("https://ghcr.io/fixture", status, code, {},
                                                io.BytesIO(json.dumps({"errors": [{"code": code}]}).encode()))
                if absence:
                    self.assertIsNone(registry.manifest("v0.1.0"))
                else:
                    with self.assertRaises(HTTPError):
                        registry.manifest("v0.1.0")

    def test_registry_refreshes_a_pull_token_once_after_expiry(self):
        body = json.dumps({"schemaVersion": 2, "mediaType": image.MANIFEST_TYPES[0],
                           "config": {"digest": TESTED}}).encode()
        digest = "sha256:" + hashlib.sha256(body).hexdigest()
        headers = Message()
        headers["Docker-Content-Digest"] = digest

        def expired():
            return HTTPError("https://ghcr.io/fixture", 401, "expired", {}, io.BytesIO(b""))

        for last_result in ((body, headers), expired()):
            with self.subTest(last_result=type(last_result)), patch.object(image, "registry_response") as request:
                request.side_effect = [
                    (b'{"token":"initial"}', Message()), expired(),
                    (b'{"token":"fresh"}', Message()), last_result,
                ]
                registry = image.Registry("fixture", "fixture")
                if isinstance(last_result, HTTPError):
                    with self.assertRaises(HTTPError):
                        registry.manifest("v0.1.0")
                else:
                    self.assertEqual(registry.manifest("v0.1.0"), (TESTED, digest))
                self.assertEqual(request.call_count, 4)
                self.assertEqual(request.call_args_list[1].args[2], "Bearer initial")
                self.assertEqual(request.call_args_list[3].args[2], "Bearer fresh")

    def test_failed_latest_still_records_the_verified_version(self):
        registry = Mock()
        registry.manifest.return_value = (TESTED, MANIFEST)

        def docker_fixture(*args):
            if args[0] == "buildx":
                raise subprocess.CalledProcessError(1, args)
            return TESTED

        output = io.StringIO()
        with patch.object(image, "docker", side_effect=docker_fixture), patch.object(
            image, "newest_stable_tag", return_value="v9.8.7"
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                image.publish(registry, "v9.8.7", "local-test", TESTED, output)
        self.assertEqual(output.getvalue(), "version=published\n")

    def test_registry_manifest_requires_supported_format_and_matching_digest(self):
        body = json.dumps({"schemaVersion": 2, "mediaType": image.MANIFEST_TYPES[0],
                           "config": {"digest": TESTED}}).encode()
        expected = "sha256:" + hashlib.sha256(body).hexdigest()
        headers = Message()
        headers["Docker-Content-Digest"] = expected
        with patch.object(image, "registry_response") as request:
            request.return_value = (b'{"token":"fixture"}', Message())
            registry = image.Registry("fixture", "fixture")
            request.return_value = (body, headers)
            self.assertEqual(registry.manifest("v0.1.0"), (TESTED, expected))
            headers.replace_header("Docker-Content-Digest", TESTED)
            with self.assertRaisesRegex(ValueError, "unverified"):
                registry.manifest("v0.1.0")

    def test_version_order_ignores_prereleases_and_nonversion_tags(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.run(
                    ["git", "-C", directory, *args], stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, text=True, check=True,
                )

            git("init")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "-c", "commit.gpgsign=false", "commit", "--allow-empty", "-m", "Fixture")
            for tag in ("v2.0.0-rc.1", "v1.9.0", "v1.10.0", "v99", "v0002.0.0"):
                git("tag", tag)
            with patch.object(image.subprocess, "check_output", side_effect=lambda args, **_: git(*args[1:]).stdout):
                self.assertEqual(image.newest_stable_tag(), "v1.10.0")

    def test_workflow_keeps_public_images_behind_smoke_and_github_assets(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        build = workflow.split("\n  docker:\n", 1)[1].split("\n  publish:\n", 1)[0]
        release = workflow.split("\n  publish:\n", 1)[1].split("\n  docker-publish:\n", 1)[0]
        docker = workflow.split("\n  docker-publish:\n", 1)[1].split("\n  channel-summary:\n", 1)[0]
        dependencies = re.search(r"^    needs: \[(.+)\]$", build, re.M).group(1)
        self.assertEqual(set(dependencies.split(", ")), {"version-check", "build", "linux", "linux-install-test"})
        self.assertEqual(build.count("uses: docker/build-push-action@"), 1)
        self.assertIn("load: true", build)
        self.assertNotIn("packages: write", build)
        self.assertNotIn("push: true", build)
        self.assertLess(build.index("Smoke private container"), build.index("docker image save"))
        self.assertIn("needs: [version-check, build, linux, linux-install-test, docker]", release)
        self.assertIn("fail_on_unmatched_files: true", release)
        self.assertRegex(release, r"uses: softprops/action-gh-release@[0-9a-f]{40} #[^\n]+\n        id: release")
        self.assertIn("release: ${{ steps.release.outcome }}", release)
        self.assertIn("needs: [docker, publish]", docker)
        self.assertIn("if: needs.publish.outputs.release == 'success'", docker)
        self.assertIn("artifact-ids: ${{ env.IMAGE_ARTIFACT_ID }}", docker)
        self.assertIn("merge-multiple: true", docker)
        self.assertIn("fetch-depth: 0", docker)
        self.assertIn("queue: max", docker)
        self.assertIn("version: v0.31.0", docker)
        self.assertNotIn("contents: write", docker)
        self.assertIn("DOCKER: ${{ needs.docker-publish.result }}", workflow)
        self.assertIn("DOCKER_VERSION: ${{ needs.docker-publish.outputs.version }}", workflow)

    def test_release_and_baseline_build_and_check_oci_identity(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        self.assertIn('org.opencontainers.image.source="https://github.com/athiwatc/formless"', dockerfile)
        self.assertIn('org.opencontainers.image.licenses="Apache-2.0"', dockerfile)
        for workflow in ("release.yml", "release-baseline.yml"):
            text = (ROOT / ".github/workflows" / workflow).read_text()
            for label, field in (("version", "ref_name"), ("revision", "sha")):
                self.assertIn(f"org.opencontainers.image.{label}=${{{{ github.{field} }}}}", text)
                self.assertIn(f'index .Config.Labels "org.opencontainers.image.{label}"', text)

    def test_release_uses_immutable_action_revisions(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        actions = re.findall(r"^\s*(?:- )?uses:\s+(\S+)", workflow, re.M)
        self.assertTrue(actions)
        for action in actions:
            with self.subTest(action=action):
                self.assertRegex(action, r"^[^@]+@[0-9a-f]{40}$")


if __name__ == "__main__":
    unittest.main()
