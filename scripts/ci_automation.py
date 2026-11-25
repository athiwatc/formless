"""Retry official CI and merge current, fully green same-repository PRs."""

import json
import os
from pathlib import Path
import subprocess
import sys
from urllib.parse import urlencode


CI = ".github/workflows/ci.yml"
DESKTOP = ".github/workflows/desktop.yml"
RETRYABLE = {CI, DESKTOP}
REQUIRED_JOBS = {
    CI: {
        "Windows daemon and console smoke",
        "Build and test (Linux X64)",
        "Build and test (macOS ARM64)",
    },
    DESKTOP: {
        f"Desktop build and portable tests ({platform}-latest)"
        for platform in ("macos", "ubuntu", "windows")
    },
}


def gh(*args):
    return subprocess.check_output(["gh", *args], text=True)


def api(path, paginated=False, **params):
    if params:
        path += "?" + urlencode(params)
    flags = ["--paginate", "--slurp"] if paginated else []
    return json.loads(gh("api", path, *flags))


def current_pr(repo, run):
    pulls = api(
        f"repos/{repo}/pulls",
        state="open",
        base="main",
        head=f"{repo.split('/')[0]}:{run['head_branch']}",
    )
    for pr in pulls:
        if (
            pr["head"]["sha"] == run["head_sha"]
            and (pr["head"]["repo"] or {}).get("full_name") == repo
        ):
            return pr["number"]


def retry(repo, event_run):
    run = api(f"repos/{repo}/actions/runs/{event_run['id']}")
    if (
        run["path"] not in RETRYABLE
        or run["status"] != "completed"
        or run["conclusion"] != "failure"
        or not 1 <= run["run_attempt"] == event_run["run_attempt"] < 3
        or (run["head_repository"] or {}).get("full_name") != repo
    ):
        return
    if run["event"] == "pull_request":
        if current_pr(repo, run) is None:
            return
    elif run["event"] != "push" or run["head_branch"] != "main":
        return
    print(f"Retrying failed jobs in run {run['id']} (attempt {run['run_attempt'] + 1}).")
    gh("run", "rerun", str(run["id"]), "--failed", "--repo", repo)


def needs_desktop(files):
    return any(
        path == DESKTOP
        or path.startswith(("crates/formless-desktop/", "crates/formless-protocol/"))
        for file in files
        for path in (file["filename"], file.get("previous_filename", ""))
    )


def successful_workflow(repo, workflow, pr, sha, branch):
    result = api(
        f"repos/{repo}/actions/workflows/{Path(workflow).name}/runs",
        event="pull_request",
        head_sha=sha,
        branch=branch,
        per_page=100,
    )
    runs = [
        run
        for run in result["workflow_runs"]
        if any(pull["number"] == pr for pull in run["pull_requests"])
    ]
    if not runs:
        return False
    latest = max(runs, key=lambda run: run["run_number"])
    if latest["status"] != "completed" or latest["conclusion"] != "success":
        return False
    pages = api(f"repos/{repo}/actions/runs/{latest['id']}/jobs", True, per_page=100)
    jobs = {job["name"]: job["conclusion"] for page in pages for job in page["jobs"]}
    return all(jobs.get(name) == "success" for name in REQUIRED_JOBS[workflow])


def successful_checks(repo, sha):
    pages = api(
        f"repos/{repo}/commits/{sha}/check-runs", True, filter="latest", per_page=100
    )
    checks = [check for page in pages for check in page["check_runs"]]
    if not checks or not all(
        check["status"] == "completed" and check["conclusion"] == "success"
        for check in checks
    ):
        return False
    statuses = api(f"repos/{repo}/commits/{sha}/status", per_page=1)
    return statuses["total_count"] == 0 or statuses["state"] == "success"


def merge(repo, run):
    if (
        run["path"] not in RETRYABLE
        or run["event"] != "pull_request"
        or run["conclusion"] != "success"
        or (run["head_repository"] or {}).get("full_name") != repo
    ):
        return
    pr = current_pr(repo, run)
    if pr is None:
        return
    details = api(f"repos/{repo}/pulls/{pr}")
    sha = run["head_sha"]
    if (
        details["head"]["sha"] != sha
        or (details["head"]["repo"] or {}).get("full_name") != repo
        or details["base"]["ref"] != "main"
        or details["state"] != "open"
        or details["draft"]
        or details["mergeable"] is not True
        or details["mergeable_state"] not in {"clean", "unstable"}
        or not 0 < details["changed_files"] <= 300
    ):
        return
    pages = api(f"repos/{repo}/pulls/{pr}/files", True, per_page=100)
    files = [file for page in pages for file in page]
    if len(files) != details["changed_files"]:
        return
    required = (CI, DESKTOP) if needs_desktop(files) else (CI,)
    if all(
        successful_workflow(repo, workflow, pr, sha, run["head_branch"])
        for workflow in required
    ) and successful_checks(repo, sha):
        print(f"Merging pull request #{pr} at {sha}.")
        gh("pr", "merge", str(pr), "--merge", "--match-head-commit", sha, "--repo", repo)


if __name__ == "__main__":
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    {"retry": retry, "merge": merge}[sys.argv[1]](
        os.environ["GITHUB_REPOSITORY"], event["workflow_run"]
    )
