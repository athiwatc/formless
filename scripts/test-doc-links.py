#!/usr/bin/env python3
"""Offline regression tests; never use real credentials or network."""
import importlib.util
from pathlib import Path
import os
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("doc_links", Path(__file__).with_name("check-doc-links.py"))
links = importlib.util.module_from_spec(spec)
spec.loader.exec_module(links)

ROOT = "https://github.com/athiwatc/formless"


class LinkChecks(unittest.TestCase):
    def test_private_root(self):
        calls = []
        self.assertEqual(links.validate_repository_url(ROOT, lambda p: calls.append(p) or {}), "^https://github\\.com/athiwatc/formless$")
        self.assertEqual(calls, [""])

    def test_file_checked_not_just_repository(self):
        calls = []
        links.validate_repository_url(ROOT + "/blob/main/Cargo.toml",
            lambda p: calls.append(p) or {"type": "file"})
        self.assertEqual(calls, ["/contents/Cargo.toml?ref=main"])

    def test_directory_not_file(self):
        with self.assertRaises(ValueError):
            links.validate_repository_url(ROOT + "/blob/main/missing", lambda p: {"type": "dir"})

    def test_latest_release_missing_still_fails(self):
        def missing(path):
            self.assertEqual(path, "/releases/latest")
            raise ValueError("HTTP 404")
        with self.assertRaises(ValueError):
            links.validate_repository_url(ROOT + "/releases/latest", missing)

    def test_empty_releases_index_valid(self):
        links.validate_repository_url(ROOT + "/releases", lambda p: [])

    def test_changelog_release_and_comparison(self):
        for suffix, expected in [
            ("/releases/tag/v0.1.0", "/releases/tags/v0.1.0"),
            ("/compare/v0.1.0...HEAD", "/compare/v0.1.0...HEAD"),
        ]:
            calls = []
            links.validate_repository_url(ROOT + suffix, lambda p: calls.append(p) or {})
            self.assertEqual(calls, [expected])

    def test_missing_changelog_targets_fail(self):
        def missing(path):
            raise ValueError("HTTP 404")
        for suffix in ["/releases/tag/missing", "/compare/missing...HEAD"]:
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                links.validate_repository_url(ROOT + suffix, missing)

    def test_unsafe_changelog_forms_fail_before_api(self):
        def unexpected(path):
            self.fail("unexpected API request: " + path)
        for suffix in ["/releases/tag/../secret", "/releases/tag/v1?x=1",
                       "/releases/tag/v1%2fother", "/compare/...HEAD",
                       "/compare/base...", "/compare/base....HEAD",
                       "/compare/base...HEAD?x=1", "/compare/base...HEAD#unknown",
                       "/compare/base...other/repo", "/compare/base%2f..%2f...HEAD"]:
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                links.validate_repository_url(ROOT + suffix, unexpected)

    def test_issue_navigation(self):
        for suffix in ["/issues", "/issues/new?labels=bug", "/issues/new?labels=enhancement"]:
            links.validate_repository_url(ROOT + suffix, lambda p: {"has_issues": True})

    def test_disabled_issues_fail(self):
        with self.assertRaises(ValueError):
            links.validate_repository_url(ROOT + "/issues", lambda p: {"has_issues": False})

    def test_unknown_private_path_fails(self):
        with self.assertRaises(ValueError):
            links.validate_repository_url(ROOT + "/definitely-missing", lambda p: {})

    def test_unsafe_or_unchecked_forms_fail(self):
        for url in [ROOT + "#unknown", ROOT + "/blob/main/../secret",
                    ROOT + "/blob/main/file?raw=1", ROOT.replace("https:", "http:"),
                    ROOT.replace("github.com", "github.com:443")]:
            with self.subTest(url=url), self.assertRaises(ValueError):
                links.validate_repository_url(url, lambda p: {"type": "file"})

    def test_host_matching(self):
        self.assertFalse(links.is_repository_url(ROOT.replace("github.com", "github.com.evil.example")))
        self.assertFalse(links.is_repository_url(ROOT + "-other"))
        self.assertTrue(links.is_repository_url(ROOT + "/issues"))

    def test_no_redirects(self):
        self.assertIsNone(links.NoRedirects().redirect_request(None, None, 302, "", {}, "https://evil.example"))

    def test_api_token_only_fixed_host(self):
        captured = []
        class Response:
            def __enter__(self): return self
            def __exit__(self, *args): pass
            def read(self): return b'{"has_issues": true}'
        class Opener:
            def open(self, request, timeout):
                captured.append((request.full_url, request.get_header("Authorization"), timeout))
                return Response()
        with patch.object(links, "build_opener", return_value=Opener()):
            links.api_get("", "dummy")
        self.assertEqual(captured, [("https://api.github.com/repos/athiwatc/formless", "Bearer dummy", 20)])

    def test_child_process_has_no_tokens(self):
        calls = []
        def run(command, **kwargs):
            self.assertNotIn("FORMLESS_LINK_GITHUB_TOKEN", kwargs["env"])
            self.assertNotIn("GITHUB_TOKEN", kwargs["env"])
            self.assertNotIn("GH_TOKEN", kwargs["env"])
            calls.append(command)
            return SimpleNamespace(stdout=ROOT + "\n", returncode=0)
        with patch.dict(os.environ, {"FORMLESS_LINK_GITHUB_TOKEN": "dummy", "GITHUB_TOKEN": "dummy", "GH_TOKEN": "dummy"}), patch.object(links.subprocess, "run", side_effect=run), patch.object(links, "api_get", return_value={}):
            self.assertEqual(links.main(["script", "lychee", "README.md"]), 0)
        self.assertNotIn("dummy", " ".join(calls[-1]))

if __name__ == "__main__":
    unittest.main()
