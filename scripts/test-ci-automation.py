import contextlib
import copy
import io
import unittest
from unittest.mock import patch

import ci_automation as automation


REPO = "owner/repo"
SHA = "a" * 40
OTHER = "b" * 40


class AutomationTests(unittest.TestCase):
    def setUp(self):
        self.pr = {
            "number": 42,
            "state": "open",
            "draft": False,
            "mergeable": True,
            "mergeable_state": "clean",
            "changed_files": 1,
            "base": {"ref": "main"},
            "head": {"sha": SHA, "ref": "change", "repo": {"full_name": REPO}},
        }
        self.run = {
            "id": 17,
            "run_number": 1,
            "run_attempt": 1,
            "event": "pull_request",
            "conclusion": "success",
            "status": "completed",
            "head_branch": "change",
            "head_sha": SHA,
            "head_repository": {"full_name": REPO},
            "path": automation.CI,
            "pull_requests": [{"number": 42}],
        }
        self.desktop = {**self.run, "id": 18, "path": automation.DESKTOP}
        self.files = [{"filename": "README.md"}]
        self.check_pages = [
            {"check_runs": [{"status": "completed", "conclusion": "success"}]},
        ]
        self.status = {"total_count": 0, "state": "pending"}
        self.jobs = {
            run["id"]: [
                {"name": name, "conclusion": "success", "run_attempt": 1}
                for name in automation.REQUIRED_JOBS[run["path"]]
            ]
            for run in (self.run, self.desktop)
        }
        self.payloads = {
            "pulls": [self.pr],
            "pulls/42": self.pr,
            "actions/runs/17": self.run,
            "actions/workflows/ci.yml/runs": {"workflow_runs": [self.run]},
            "actions/workflows/desktop.yml/runs": {"workflow_runs": [self.desktop]},
        }
        api_patch = patch.object(automation, "api", side_effect=self.request)
        api_patch.start()
        self.addCleanup(api_patch.stop)
        gh_patch = patch.object(automation, "gh")
        self.gh = gh_patch.start()
        self.addCleanup(gh_patch.stop)
        output = contextlib.redirect_stdout(io.StringIO())
        output.__enter__()
        self.addCleanup(output.__exit__, None, None, None)

    def request(self, path, *args, **kwargs):
        self.assertTrue(path.startswith(f"repos/{REPO}/"))
        path = path.removeprefix(f"repos/{REPO}/")
        if path == "pulls/42/files":
            self.assertEqual(args, (True,))
            return [self.files]
        if path == f"commits/{SHA}/check-runs":
            self.assertEqual(args, (True,))
            self.assertEqual(kwargs["filter"], "latest")
            return self.check_pages
        if path == f"commits/{SHA}/status":
            return self.status
        if path in {"actions/runs/17/jobs", "actions/runs/18/jobs"}:
            self.assertEqual(kwargs["filter"], "all")
            return [{"jobs": [job]} for job in self.jobs[int(path.split("/")[2])]]
        return self.payloads[path]

    def assert_merge(self):
        self.gh.assert_called_once_with(
            "pr", "merge", "42", "--merge", "--match-head-commit", SHA, "--repo", REPO
        )

    def test_retries_only_failed_jobs_on_first_two_current_attempts(self):
        for attempt in (1, 2):
            with self.subTest(attempt=attempt):
                self.gh.reset_mock()
                self.run.update(conclusion="failure", run_attempt=attempt)
                automation.retry(REPO, copy.deepcopy(self.run))
                self.gh.assert_called_once_with("run", "rerun", "17", "--failed", "--repo", REPO)

    def test_never_retries_cancelled_exhausted_stale_or_fork_runs(self):
        self.run.update(conclusion="failure")
        for change in (
            {"conclusion": "cancelled"},
            {"run_attempt": 3},
            {"status": "queued"},
            {"head_sha": OTHER},
            {"head_repository": {"full_name": "fork/repo"}},
            {"path": ".github/workflows/release.yml"},
        ):
            with self.subTest(change=change):
                self.payloads["actions/runs/17"] = {**self.run, **change}
                automation.retry(REPO, self.run)
                self.gh.assert_not_called()

    def test_merge_requires_green_workflows_for_the_current_head(self):
        automation.merge(REPO, self.run)
        self.assert_merge()
        self.gh.reset_mock()

        self.files = [{"filename": "crates/formless-protocol/src/lib.rs"}]
        self.desktop.update(status="in_progress", conclusion=None)
        automation.merge(REPO, self.run)
        self.gh.assert_not_called()

        self.desktop.update(status="completed", conclusion="success")
        automation.merge(REPO, self.desktop)
        self.assert_merge()

    def test_newer_workflow_attempt_or_missing_job_blocks_merge(self):
        self.payloads["actions/workflows/ci.yml/runs"]["workflow_runs"].append(
            {**self.run, "id": 19, "run_number": 2, "status": "queued", "conclusion": None}
        )
        automation.merge(REPO, self.run)
        self.gh.assert_not_called()
        self.payloads["actions/workflows/ci.yml/runs"]["workflow_runs"].pop()

        self.jobs[17].pop()
        automation.merge(REPO, self.run)
        self.gh.assert_not_called()

    def test_partial_rerun_uses_most_recent_result_of_each_job(self):
        self.run["run_attempt"] = 2
        previous = self.jobs[17][0]
        previous["conclusion"] = "failure"
        rerun = {**previous, "run_attempt": 2, "conclusion": "success"}
        self.jobs[17].insert(0, rerun)
        automation.merge(REPO, self.run)
        self.assert_merge()

        self.gh.reset_mock()
        rerun["conclusion"] = "failure"
        previous["conclusion"] = "success"
        automation.merge(REPO, self.run)
        self.gh.assert_not_called()

    def test_missing_job_attempt_defaults_to_first_attempt(self):
        self.run["run_attempt"] = 2
        previous = self.jobs[17][0]
        del previous["run_attempt"]
        previous["conclusion"] = "failure"
        rerun = {**previous, "run_attempt": 2, "conclusion": "success"}
        other_jobs = self.jobs[17][1:]

        for jobs in ([previous, rerun], [rerun, previous]):
            with self.subTest(jobs=jobs):
                self.gh.reset_mock()
                self.jobs[17] = jobs + other_jobs
                automation.merge(REPO, self.run)
                self.assert_merge()

    def test_ready_for_review_reevaluates_previously_green_draft(self):
        self.pr["draft"] = True
        automation.dispatch("merge", REPO, {"workflow_run": self.run})
        self.gh.assert_not_called()

        self.pr["draft"] = False
        event = {"action": "ready_for_review", "pull_request": copy.deepcopy(self.pr)}
        automation.dispatch("merge", REPO, event)
        self.assert_merge()

        self.gh.reset_mock()
        self.pr["head"]["sha"] = OTHER
        automation.dispatch("merge", REPO, event)
        self.gh.assert_not_called()

        event["pull_request"]["head"]["repo"]["full_name"] = "fork/repo"
        self.pr["head"]["sha"] = SHA
        automation.dispatch("merge", REPO, event)
        self.gh.assert_not_called()

    def test_non_green_checks_and_commit_statuses_block_merge(self):
        for conclusion in ("failure", "skipped", "neutral", None):
            with self.subTest(conclusion=conclusion):
                self.check_pages.append(
                    {"check_runs": [{"status": "completed", "conclusion": conclusion}]}
                )
                automation.merge(REPO, self.run)
                self.gh.assert_not_called()
                self.check_pages.pop()
        self.status = {"total_count": 2, "state": "pending"}
        automation.merge(REPO, self.run)
        self.gh.assert_not_called()

    def test_ready_for_review_waits_for_mergeability(self):
        event = {"action": "ready_for_review", "pull_request": copy.deepcopy(self.pr)}
        self.payloads["pulls/42"] = {**self.pr, "mergeable": None}

        def finish_mergeability(_):
            self.payloads["pulls/42"] = self.pr

        with patch.object(automation.time, "sleep", side_effect=finish_mergeability) as wait:
            automation.dispatch("merge", REPO, event)

        wait.assert_called_once_with(2)
        self.assert_merge()

    def test_draft_conflicted_stale_or_fork_prs_never_merge(self):
        for change in (
            {"mergeable": None},
            {"mergeable": False},
            {"mergeable_state": "blocked"},
            {"draft": True},
            {"head": {"sha": OTHER, "repo": {"full_name": REPO}}},
            {"head": {"sha": SHA, "repo": {"full_name": "fork/repo"}}},
            {"changed_files": 301},
        ):
            with self.subTest(change=change):
                self.payloads["pulls/42"] = {**self.pr, **change}
                with patch.object(automation.time, "sleep") as wait:
                    automation.merge(REPO, self.run)
                self.assertEqual(wait.call_count, 4 if change == {"mergeable": None} else 0)
                self.gh.assert_not_called()


if __name__ == "__main__":
    unittest.main()
