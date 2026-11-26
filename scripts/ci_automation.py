"""Retry official CI and merge current, fully green same-repository PRs."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
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


def gh(*args, write=False):
    env = os.environ.copy()
    merge_token = env.pop("CI_AUTOMERGE_TOKEN", "")
    if write:
        if merge_token:
            print("Using CI_AUTOMERGE_TOKEN for this pull request write.", flush=True)
            env["GH_TOKEN"] = merge_token
        else:
            print("Using GITHUB_TOKEN fallback for this pull request write.", flush=True)
    return subprocess.check_output(
        ["gh", *args], text=True, env=env, stderr=subprocess.PIPE if write else None
    )


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
    pages = api(
        f"repos/{repo}/actions/runs/{latest['id']}/jobs", True, filter="all", per_page=100
    )
    jobs = {}
    for page in pages:
        for job in page["jobs"]:
            previous = jobs.get(job["name"])
            if previous is None or job.get("run_attempt", 1) > previous.get("run_attempt", 1):
                jobs[job["name"]] = job
    return all(jobs.get(name, {}).get("conclusion") == "success" for name in REQUIRED_JOBS[workflow])


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
    if pr is not None:
        return merge_pr(repo, pr, run["head_sha"])


def pr_write(pr, operation, files, *args):
    try:
        gh(*args, write=True)
    except subprocess.CalledProcessError as error:
        match = re.match(r"HTTP/\S+ (\d{3})\b", error.output or "") or re.search(
            r"\bHTTP (\d{3})\b", error.stderr or ""
        )
        status = match.group(1) if match else None
        if status == "422" and operation == "branch update":
            print(f"GitHub rejected the branch update for pull request #{pr} (HTTP 422); skipping merge.")
            return
        if status is None and (error.stderr or "").startswith("GraphQL: Resource not accessible"):
            status = "403"
        if status in {"401", "403"}:
            token = "CI_AUTOMERGE_TOKEN" if os.environ.get("CI_AUTOMERGE_TOKEN") else (
                "GITHUB_TOKEN fallback (CI_AUTOMERGE_TOKEN not configured)"
            )
            permissions = "Contents: write and Pull requests: write"
            if any(
                path.startswith(".github/workflows/")
                for file in files
                for path in (file["filename"], file.get("previous_filename", ""))
            ):
                permissions = "Workflows: write, Contents: write, and Pull requests: write"
            print(
                f"::error::GitHub denied the {operation} for pull request #{pr} "
                f"(HTTP {status}) using {token}. Verify the token is valid, "
                f"can access this repository, and has {permissions}. Skipping this PR."
            )
            return 1
        if error.stderr:
            print(error.stderr, file=sys.stderr, end="")
        raise


def update_branch(repo, pr, sha, files):
    print(f"Updating pull request #{pr} from main instead of merging.")
    if not os.environ.get("CI_AUTOMERGE_TOKEN"):
        print(
            "GITHUB_TOKEN updates require manual approval for PR CI. "
            "Approve the workflow runs or configure CI_AUTOMERGE_TOKEN for automatic runs."
        )
    return pr_write(
        pr, "branch update", files,
        "api", "--include", "--method", "PUT", f"repos/{repo}/pulls/{pr}/update-branch",
        "-f", f"expected_head_sha={sha}",
    )


def merge_pr(repo, pr, sha):
    for attempt in range(5):
        details = api(f"repos/{repo}/pulls/{pr}")
        if (
            details["head"]["sha"] != sha
            or (details["head"]["repo"] or {}).get("full_name") != repo
            or details["base"]["ref"] != "main"
            or details["state"] != "open"
            or details["draft"]
            or not 0 < details["changed_files"] <= 300
        ):
            return
        if details["mergeable"] is not None:
            break
        if attempt < 4:
            time.sleep(2)
    if (
        details["mergeable"] is not True
        or details["mergeable_state"] not in {"clean", "unstable", "behind"}
    ):
        return
    pages = api(f"repos/{repo}/pulls/{pr}/files", True, per_page=100)
    files = [file for page in pages for file in page]
    if len(files) != details["changed_files"]:
        return
    required = (CI, DESKTOP) if needs_desktop(files) else (CI,)
    if all(
        successful_workflow(repo, workflow, pr, sha, details["head"]["ref"])
        for workflow in required
    ) and successful_checks(repo, sha):
        if api(f"repos/{repo}/compare/main...{sha}")["behind_by"] != 0:
            return update_branch(repo, pr, sha, files)
        if details["mergeable_state"] == "behind":
            return
        print(f"Merging pull request #{pr} at {sha}.")
        return pr_write(
            pr, "merge", files,
            "pr", "merge", str(pr), "--merge", "--match-head-commit", sha, "--repo", repo,
        )


def dispatch(operation, repo, event):
    if operation == "retry":
        retry(repo, event["workflow_run"])
    elif "workflow_run" in event:
        return merge(repo, event["workflow_run"])
    elif event.get("action") == "ready_for_review":
        pr = event["pull_request"]
        if (pr["head"]["repo"] or {}).get("full_name") == repo and pr["base"]["ref"] == "main":
            return merge_pr(repo, pr["number"], pr["head"]["sha"])


if __name__ == "__main__":
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    raise SystemExit(dispatch(sys.argv[1], os.environ["GITHUB_REPOSITORY"], event))
