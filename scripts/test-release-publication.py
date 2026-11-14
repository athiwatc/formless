#!/usr/bin/env python3
"""Nonpublishing regression fixtures for image identity and channel verdicts."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


class ReleasePublication(unittest.TestCase):
    def test_summary_outcomes(self):
        for outcomes, code, expected in [
            (["success"] * 4, 0, ["| GitHub Release | published |", "| AUR | published |"]),
            (["failure", "skipped", "skipped", "skipped"], 1,
             ["| GitHub Release | FAILED |", "| AUR | not run (earlier step failed or cancelled) |"]),
            (["skipped"] * 4, 1, ["| GitHub Release | not run (earlier step failed or cancelled) |"]),
            (["success", "skipped", "success", "skipped"], 0,
             ["| Homebrew tap | skipped (no secret) |", "| winget | published |"]),
            (["success", "failure", "success", "cancelled"], 1,
             ["| Homebrew tap | FAILED |", "| AUR | cancelled |"]),
        ]:
            with self.subTest(outcomes=outcomes), tempfile.TemporaryDirectory() as directory:
                summary = Path(directory) / "summary"
                result = subprocess.run(["bash", str(ROOT / "scripts/release-channel-summary.sh"), *outcomes],
                                        env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary)},
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, code, result.stderr)
                text = summary.read_text()
                for row in expected:
                    self.assertIn(row, text)
                self.assertIn("pushed by prerequisite Docker job", text)
                if outcomes[0] != "success":
                    self.assertNotIn("skipped (no secret)", text)
                    self.assertNotIn("| GitHub Release | published |", text)

    def push_fixture(self, mode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            docker = root / "docker"
            # This fixture cannot run Docker or publish; it records exact commands.
            docker.write_text('''#!/usr/bin/env python3
import json, os, sys
args=sys.argv[1:]
with open(os.environ['DOCKER_TRACE'],'a') as trace: trace.write(json.dumps(args)+'\\n')
mode=os.environ['DOCKER_MODE']
if args[:2]==['image','inspect']:
 print('sha256:'+('b' if mode=='mismatch' and args[-1].startswith('ghcr.io/') else 'a')*64)
elif args[0]=='push' and mode=='failure': sys.exit(9)
elif args[0] not in ['tag','push','image']: sys.exit(10)
''')
            docker.chmod(0o700)
            trace = root / "trace"
            result = subprocess.run(["bash", str(ROOT / "scripts/push-release-image.sh")],
                                    env={**os.environ, "PATH": str(root) + ":" + os.environ["PATH"],
                                         "DOCKER_TRACE": str(trace), "DOCKER_MODE": mode,
                                         "RELEASE_IMAGE": "formless:private-fixture", "GITHUB_REF_NAME": "v9.8.7"},
                                    capture_output=True, text=True, timeout=5)
            return result, [json.loads(line) for line in trace.read_text().splitlines()]

    def test_pushes_same_immutable_image_without_rebuilding_and_removes_local_tags(self):
        result, calls = self.push_fixture("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        tags = ["ghcr.io/athiwatc/formless:v9.8.7", "ghcr.io/athiwatc/formless:latest"]
        self.assertEqual([call for call in calls if call[0] == "tag"],
                         [["tag", "sha256:" + "a" * 64, tag] for tag in tags])
        self.assertEqual([call for call in calls if call[0] == "push"], [["push", tag] for tag in tags])
        self.assertEqual(calls[-1], ["image", "rm", *tags])
        self.assertFalse(any("build" in call for call in calls))

    def test_identity_mismatch_and_push_failure_stop_publication_and_clean_local_tags(self):
        result, calls = self.push_fixture("mismatch")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[0] == "push" for call in calls))
        self.assertEqual(calls[-1][:2], ["image", "rm"])
        result, calls = self.push_fixture("failure")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len([call for call in calls if call[0] == "push"]), 1)
        self.assertEqual(calls[-1][:2], ["image", "rm"])

    def test_workflow_dependencies_gate_all_publication_and_report_real_release_outcome(self):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        docker = workflow.split("\n  docker:\n", 1)[1].split("\n  publish:\n", 1)[0]
        needs = re.search(r"^    needs: \[(.+)\]$", docker, re.M).group(1)
        self.assertEqual(set(needs.split(", ")), {"version-check", "build", "linux", "linux-install-test"})
        self.assertEqual(docker.count("uses: docker/build-push-action@"), 1)
        self.assertIn("load: true", docker)
        self.assertNotIn("push: true", docker)
        self.assertLess(docker.index("Smoke private container"), docker.index("bash scripts/push-release-image.sh"))
        self.assertRegex(workflow, r"uses: softprops/action-gh-release@v2\n        id: release")
        self.assertIn("'${{ steps.release.outcome }}'", workflow)


if __name__ == "__main__":
    unittest.main()
