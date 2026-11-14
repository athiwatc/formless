#!/usr/bin/env python3
"""Check exact private-repository links without trusting lychee's repo-wide fallback."""
import json
import os
import re
import subprocess
import sys
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

REPOSITORY = "athiwatc/formless"
API = "https://api.github.com/repos/" + REPOSITORY


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def api_get(endpoint, token):
    # Endpoints are constructed below, never taken as arbitrary input URLs.
    if not endpoint.startswith("/") and endpoint != "":
        raise ValueError("invalid API endpoint")
    request = Request(API + endpoint, headers={
        "Authorization": "Bearer " + token,
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
    })
    try:
        with build_opener(NoRedirects()).open(request, timeout=20) as response:
            return json.load(response)
    except HTTPError as error:
        raise ValueError("GitHub API check failed: HTTP " + str(error.code)) from None
    except (URLError, TimeoutError):
        raise ValueError("GitHub API check failed: network error") from None


def is_repository_url(url):
    parsed = urlsplit(url)
    return (parsed.hostname == "github.com" and
            (parsed.path == "/" + REPOSITORY or
             parsed.path.startswith("/" + REPOSITORY + "/")))


def validate_repository_url(url, get):
    parsed = urlsplit(url)
    if (parsed.scheme != "https" or parsed.netloc != "github.com" or parsed.fragment):
        raise ValueError("unsupported private-repository URL form")
    path = parsed.path[len("/" + REPOSITORY):].rstrip("/")
    query = parse_qs(parsed.query, keep_blank_values=True)
    if path == "" and not query:
        get("")
    elif path in ("/issues", "/issues/new"):
        if path == "/issues" and query:
            raise ValueError("unsupported issues query")
        if path == "/issues/new" and (set(query) - {"labels"}):
            raise ValueError("unsupported issue form query")
        if not get("").get("has_issues"):
            raise ValueError("repository issues are disabled")
        # These are navigation/form routes, not claims that an issue exists.
    elif path == "/releases" and not query:
        get("/releases?per_page=1")  # An empty list is a valid releases index.
    elif path == "/releases/latest" and not query:
        get("/releases/latest")  # Must actually exist; never accept repo alone.
    elif path.startswith("/releases/tag/") and not query:
        tag = path[len("/releases/tag/"):]
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", tag):
            raise ValueError("unsupported release tag")
        get("/releases/tags/" + quote(tag, safe=""))
    elif path.startswith("/compare/") and not query:
        comparison = path[len("/compare/"):]
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*\.\.\.[A-Za-z0-9][A-Za-z0-9._-]*", comparison):
            raise ValueError("unsupported comparison")
        base, head = comparison.split("...", 1)
        if ".." in base or ".." in head or base.endswith(".") or head.startswith("."):
            raise ValueError("invalid comparison refs")
        get("/compare/" + quote(base, safe="") + "..." + quote(head, safe=""))
    elif path.startswith("/blob/main/") and not query:
        file_path = path[len("/blob/main/"):]
        if not file_path or any(p in (".", "..", "") for p in file_path.split("/")):
            raise ValueError("invalid repository file path")
        data = get("/contents/" + quote(file_path, safe="/") + "?ref=main")
        if not isinstance(data, dict) or data.get("type") != "file":
            raise ValueError("repository file does not exist")
    else:
        raise ValueError("unsupported private-repository path; add an exact validator")
    return "^" + re.escape(url) + "$"


def clean_environment():
    env = os.environ.copy()
    # Never pass authentication to the general URL checker or child processes.
    env.pop("FORMLESS_LINK_GITHUB_TOKEN", None)
    env.pop("GITHUB_TOKEN", None)
    env.pop("GH_TOKEN", None)
    return env


def main(argv):
    if len(argv) < 3:
        raise ValueError("usage: check-doc-links.py LYCHEE SOURCE...")
    binary, sources = argv[1], argv[2:]
    token = os.environ.get("FORMLESS_LINK_GITHUB_TOKEN")
    if not token:
        raise ValueError("missing scoped GitHub token")
    env = clean_environment()
    dumped = subprocess.run([binary, "--dump", *sources], env=env,
                            text=True, capture_output=True, check=True)
    private_urls = sorted({line.strip() for line in dumped.stdout.splitlines()
                           if is_repository_url(line.strip())})
    cache = {}
    def get(endpoint):
        if endpoint not in cache:
            cache[endpoint] = api_get(endpoint, token)
        return cache[endpoint]
    exclusions = []
    for url in private_urls:
        exclusions.extend(["--exclude", validate_repository_url(url, get)])
    print("Validated " + str(len(private_urls)) + " exact private GitHub URLs.")
    command = [binary, "--no-progress", "--include-fragments",
               "--accept", "200,206,429", "--max-retries", "2", "--timeout", "20",
               *exclusions, *sources]
    return subprocess.run(command, env=env).returncode


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except (ValueError, subprocess.CalledProcessError) as error:
        print("Documentation link check failed: " + str(error), file=sys.stderr)
        sys.exit(1)
