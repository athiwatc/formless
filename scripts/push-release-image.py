#!/usr/bin/env python3
"""Publish the smoke-tested single-platform image after the GitHub Release."""
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import HTTPRedirectHandler, Request, build_opener

REPOSITORY = "athiwatc/formless"
IMAGE = f"ghcr.io/{REPOSITORY}"
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
STABLE = re.compile(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
VERSION = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?")
MANIFEST_TYPES = (
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
)
RESPONSE_LIMIT = 4 * 1024 * 1024


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Neither the Basic nor the short-lived registry credential may leave GHCR.
        return None


def registry_response(opener, path, authorization, accept="application/json"):
    request = Request(
        "https://ghcr.io/" + path,
        headers={"Authorization": authorization, "Accept": accept},
    )
    with opener.open(request, timeout=30) as response:
        body = response.read(RESPONSE_LIMIT + 1)
        if len(body) > RESPONSE_LIMIT:
            raise ValueError("oversized GHCR response")
        return body, response.headers


class Registry:
    def __init__(self, username, token):
        self.opener = build_opener(NoRedirect)
        auth = base64.b64encode(f"{username}:{token}".encode()).decode()
        path = "token?" + urlencode(
            {"service": "ghcr.io", "scope": f"repository:{REPOSITORY}:pull"}
        )
        body, _ = registry_response(self.opener, path, "Basic " + auth)
        parsed = json.loads(body)
        credential = parsed.get("token") or parsed.get("access_token")
        if not isinstance(credential, str) or not re.fullmatch(r"[\x21-\x7e]+", credential):
            raise ValueError("GHCR did not return a usable pull token")
        self.authorization = "Bearer " + credential

    def manifest(self, tag):
        path = f"v2/{REPOSITORY}/manifests/{tag}"
        try:
            body, headers = registry_response(
                self.opener, path, self.authorization, ", ".join(MANIFEST_TYPES)
            )
        except HTTPError as error:
            with error:
                if error.code == 404:
                    body = error.read(RESPONSE_LIMIT + 1)
                    if len(body) <= RESPONSE_LIMIT:
                        errors = json.loads(body).get("errors", [])
                        if errors and all(
                            item.get("code") in {"MANIFEST_UNKNOWN", "NAME_UNKNOWN"}
                            for item in errors
                        ):
                            return None
                raise
        parsed = json.loads(body)
        config = parsed.get("config") or {}
        config_digest = config.get("digest")
        manifest_digest = "sha256:" + hashlib.sha256(body).hexdigest()
        if (
            parsed.get("schemaVersion") != 2
            or parsed.get("mediaType", headers.get_content_type()) not in MANIFEST_TYPES
            or not isinstance(config_digest, str)
            or not DIGEST.fullmatch(config_digest)
            or headers.get("Docker-Content-Digest") != manifest_digest
        ):
            raise ValueError("unverified single-platform GHCR image manifest")
        return config_digest, manifest_digest


def docker(*args):
    if args[0:2] == ("image", "inspect"):
        return subprocess.check_output(["docker", *args], text=True).strip()
    subprocess.run(["docker", *args], check=True)


def newest_stable_tag():
    tags = subprocess.check_output(
        ["git", "tag", "--list", "v*", "--sort=-version:refname"], text=True
    )
    return next((tag for tag in tags.splitlines() if STABLE.fullmatch(tag)), None)


def publish(registry, version, image, tested_id):
    image_id = docker("image", "inspect", "--format", "{{.Id}}", image)
    if image_id != tested_id:
        raise ValueError("loaded image does not match the smoke-tested image")
    ref = f"{IMAGE}:{version}"
    manifest = registry.manifest(version)
    if manifest is None:
        docker("image", "tag", tested_id, ref)
        try:
            if docker("image", "inspect", "--format", "{{.Id}}", ref) != tested_id:
                raise ValueError("version tag did not retain the smoke-tested image")
            docker("image", "push", ref)
            manifest = registry.manifest(version)
        finally:
            docker("image", "rm", ref)
    if manifest is None or manifest[0] != tested_id:
        raise ValueError(f"refusing to replace or promote {ref} with a different image")
    print(f"Docker version {version} resolves to {manifest[1]}.")
    if STABLE.fullmatch(version) and version == newest_stable_tag():
        docker(
            "buildx", "imagetools", "create", "--prefer-index=false",
            "--tag", f"{IMAGE}:latest", f"{IMAGE}@{manifest[1]}",
        )
        if registry.manifest("latest") != manifest:
            raise ValueError("latest did not retain the verified version manifest")
        return "promoted"
    return "skipped (not the highest stable tag)"


def main():
    version = os.environ.get("GITHUB_REF_NAME", "")
    if len(version) > 128 or not VERSION.fullmatch(version):
        raise ValueError("release tag is incompatible with Docker version tags")
    if sys.argv[1:] == ["--check-version"]:
        return
    if sys.argv[1:]:
        raise ValueError("usage: push-release-image.py [--check-version]")
    required = ("RELEASE_IMAGE", "TESTED_IMAGE_ID", "GHCR_USERNAME", "GHCR_TOKEN", "GITHUB_OUTPUT")
    if any(not os.environ.get(name) for name in required):
        raise ValueError("required Docker release inputs are missing")
    tested_id = os.environ["TESTED_IMAGE_ID"]
    if not DIGEST.fullmatch(tested_id):
        raise ValueError("invalid smoke-tested image ID")
    registry = Registry(os.environ["GHCR_USERNAME"], os.environ.pop("GHCR_TOKEN"))
    decision = publish(registry, version, os.environ["RELEASE_IMAGE"], tested_id)
    print(f"Docker latest: {decision}.")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        print(f"latest={decision}", file=output)


if __name__ == "__main__":
    try:
        main()
    except (KeyError, ValueError, TypeError, AttributeError, OSError,
            subprocess.CalledProcessError) as error:
        print(f"Docker release failed: {error}", file=sys.stderr)
        sys.exit(1)
