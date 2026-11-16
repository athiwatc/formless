#!/usr/bin/env python3
"""Build an optional, unsigned development bundle; never install or launch it.

Uses only Python's standard library. Release signing, installers, updating and
native acceptance are separate gates. Existing output directories are refused.
"""

import argparse
import datetime
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile


def run(arguments, cwd):
    return subprocess.check_output(arguments, cwd=cwd, text=True).strip()


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def ensure_source(repository, commit, dirty, stage=None):
    if run(["git", "rev-parse", "HEAD"], repository) != commit:
        raise ValueError("source commit changed during packaging")
    status = ["git", "status", "--porcelain", "--untracked-files=normal"]
    if stage is not None:
        try:
            relative = stage.relative_to(repository)
        except ValueError:
            pass
        else:
            # An output inside the checkout creates our own untracked temp
            # directory. Exclude only that exact owned directory, not its
            # parent or unrelated new source files.
            status += ["--", ".", ":(top,literal,exclude)" + relative.as_posix()]
    if bool(run(status, repository)) != dirty:
        raise ValueError("source cleanliness changed during packaging")


def write_archive(bundle, executable, archive, epoch):
    # Normalize ZIP timestamps/modes rather than recording staging mtimes.
    # Reproducible archive bytes require the same built binary and inputs;
    # this does not claim a reproducible Rust build across hosts/toolchains.
    earliest = datetime.datetime(1980, 1, 1, tzinfo=datetime.timezone.utc).timestamp()
    latest = datetime.datetime(2107, 12, 31, 23, 59, 58, tzinfo=datetime.timezone.utc).timestamp()
    timestamp = datetime.datetime.fromtimestamp(max(earliest, min(latest, epoch)), datetime.timezone.utc)
    date_time = (timestamp.year, timestamp.month, timestamp.day, timestamp.hour, timestamp.minute, timestamp.second // 2 * 2)
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as target:
        for source in sorted(bundle.rglob("*")):
            if source.is_symlink():
                raise ValueError("symlink in staged bundle: " + source.name)
            if not source.is_file():
                continue
            source.resolve().relative_to(bundle.resolve())
            info = zipfile.ZipInfo(source.relative_to(bundle.parent).as_posix(), date_time)
            info.create_system = 3
            info.compress_type = zipfile.ZIP_DEFLATED
            mode = 0o755 if source == executable else 0o644
            info.external_attr = (stat.S_IFREG | mode) << 16
            with source.open("rb") as contents, target.open(info, "w") as entry:
                shutil.copyfileobj(contents, entry)


def verified_notice(upstream, notice):
    local = notice.get("local", "")
    path = Path(local)
    if not local or "\\" in local or ":" in local or path.is_absolute() or ".." in path.parts:
        raise ValueError("invalid reviewed notice path")
    source = upstream / path
    if upstream.is_symlink() or source.is_symlink() or not source.is_file():
        raise ValueError("missing or symlinked reviewed notice: " + local)
    source.resolve().relative_to(upstream.resolve())
    if source.stat().st_size > 2 * 1024 * 1024 or digest(source) != notice.get("sha256"):
        raise ValueError("reviewed notice hash/size changed: " + local)
    return source


def collect_toolchain_notices(upstream, destination, rustc):
    manifest = upstream / "TOOLCHAIN.json"
    if not manifest.exists():
        return None
    if upstream.is_symlink() or manifest.is_symlink() or manifest.stat().st_size > 2 * 1024 * 1024:
        raise ValueError("invalid toolchain notice manifest")
    document = json.loads(manifest.read_text(encoding="utf-8"))
    commit = re.search(r"^commit-hash: ([0-9a-f]{40})$", rustc, re.M)
    if document.get("schema") != 1 or commit is None or commit.group(1) != document.get("compiler_commit_hash"):
        raise ValueError("toolchain notices do not match rustc; refresh notice review")
    notices = document.get("files", [])
    names = {notice.get("name") for notice in notices}
    required = {"LICENSE-APACHE", "LICENSE-MIT", "COPYRIGHT"}
    if len(notices) != len(names) or not required <= names or not names <= required | {"COPYRIGHT-library.html"}:
        raise ValueError("invalid toolchain notice set")
    for notice in notices:
        source = verified_notice(upstream, notice)
        destination.mkdir(parents=True, exist_ok=True)
        target = destination / notice["name"]
        shutil.copyfile(source, target)
        if digest(target) != notice["sha256"]:
            raise ValueError("toolchain notice changed while copying")
    shutil.copyfile(manifest, destination / "PROVENANCE.json")
    return digest(manifest)


def reviewed_notices(package, root, upstream, entry):
    if package.get("license") != entry.get("license"):
        raise ValueError("reviewed notice license declaration changed: " + package["name"])
    if entry.get("source_kind") == "workspace":
        expected = upstream.parent / "Cargo.toml"
        if package.get("source") is not None or Path(package["manifest_path"]).resolve() != expected.resolve():
            raise ValueError("reviewed workspace notice does not match this package")
    elif entry.get("source_kind") == "registry":
        if not str(package.get("source", "")).startswith("registry+"):
            raise ValueError("reviewed upstream notice requires the registry package")
        field = entry.get("repository_field")
        if field not in ("repository", "homepage") or package.get(field) != entry.get("repository_metadata_value"):
            raise ValueError("reviewed upstream repository changed: " + package["name"])
        vcs = root / ".cargo_vcs_info.json"
        if not vcs.is_file() or vcs.is_symlink() or digest(vcs) != entry.get("vcs_sha256"):
            raise ValueError("reviewed upstream VCS receipt changed: " + package["name"])
    else:
        raise ValueError("unsupported reviewed notice provenance")
    result = []
    for notice in entry.get("files", []):
        source = verified_notice(upstream, notice)
        if notice.get("role") == "published-source-header":
            # Preserve an exact leading source comment, not a synthesized grant
            # or a claim that the publisher supplied a standalone notice file.
            if not isinstance(notice.get("published_source"), str):
                raise ValueError("missing published source header path")
            published = verified_notice(root, {
                "local": notice.get("published_source"),
                "sha256": notice.get("published_source_sha256"),
            })
            size = notice.get("prefix_bytes")
            if type(size) is not int or not 0 < size <= published.stat().st_size:
                raise ValueError("invalid published source header length")
            if published.read_bytes()[:size] != source.read_bytes():
                raise ValueError("published source header differs from source")
        if entry["source_kind"] == "workspace":
            if digest(upstream.parent.parents[1] / "LICENSE") != notice["sha256"]:
                raise ValueError("workspace license changed; refresh notice review")
        result.append((source, dict(source=notice["source"], sha256=notice["sha256"],
            url=notice.get("url"), revision=notice.get("revision", entry.get("revision")),
            role=notice.get("role", "pinned-release-notice"),
            git_blob=notice.get("git_blob"), registry_checksum=entry.get("registry_checksum"),
            reviewed_local=notice["local"],
            **({key: notice[key] for key in ("published_source", "published_source_sha256", "prefix_bytes")}
               if notice.get("role") == "published-source-header" else {}))))
    return result


def collect_notices(packages, built, destination, upstream=None, lockfile=None):
    reviewed = {}
    if upstream is not None and upstream.is_symlink():
        raise ValueError("symlinked reviewed notice root")
    if upstream is not None and (upstream / "UPSTREAM.json").exists():
        manifest = upstream / "UPSTREAM.json"
        if manifest.is_symlink() or manifest.stat().st_size > 2 * 1024 * 1024:
            raise ValueError("invalid reviewed notice manifest")
        document = json.loads(manifest.read_text(encoding="utf-8"))
        if document.get("schema") != 1 or lockfile is None or digest(lockfile) != document.get("cargo_lock_sha256"):
            raise ValueError("reviewed notices do not match Cargo.lock; refresh notice review")
        for entry in document.get("packages", []):
            if not entry.get("name") or not entry.get("version") or not isinstance(entry.get("files"), list) or not entry["files"]:
                raise ValueError("invalid reviewed notice package")
            key = (entry["name"], entry["version"])
            if key in reviewed:
                raise ValueError("duplicate reviewed notice package")
            reviewed[key] = entry
    inventory = []
    for package in sorted(packages, key=lambda item: (item["name"], item["version"])):
        if package["id"] not in built:
            continue
        root = Path(package["manifest_path"]).parent.resolve()
        candidates = set()
        def notice(path):
            return re.fullmatch(r"(?:licen[cs]e|copying|notice)(?:[-_.][A-Za-z0-9][A-Za-z0-9_.-]*)?", path.name, re.I)
        candidates.update(path for path in root.iterdir() if notice(path))
        # Embedded data/assets may carry a different licence from crate code
        # (for example the protocol's pinned MPL Public Suffix List).
        for name in ("data", "assets", "fonts"):
            directory = root / name
            if directory.is_dir() and not directory.is_symlink():
                candidates.update(path for path in directory.rglob("*") if notice(path))
        if package.get("license_file"):
            candidates.add(root / package["license_file"])
        copied = []
        sources = []
        for source in sorted(candidates):
            # Manifest paths and symlinks must not make packaging read files
            # outside a dependency's package root.
            if source.is_symlink() or not source.is_file():
                continue
            try:
                relative = source.resolve().relative_to(root)
            except ValueError:
                continue
            if source.stat().st_size > 2 * 1024 * 1024:
                raise ValueError("oversized dependency notice: " + source.name)
            target = destination / (package["name"] + "-" + package["version"]) / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            copied.append(str(target.relative_to(destination)))
            sources.append({"source": relative.as_posix(), "sha256": digest(target)})
        entry = reviewed.get((package["name"], package["version"]))
        if entry is not None:
            for source, receipt in reviewed_notices(package, root, upstream, entry):
                relative = Path(receipt["reviewed_local"])
                target = destination / (package["name"] + "-" + package["version"]) / "reviewed" / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
                if digest(target) != receipt["sha256"]:
                    raise ValueError("reviewed notice changed while copying")
                copied.append(str(target.relative_to(destination)))
                sources.append(receipt)
        inventory.append({
            "name": package["name"], "version": package["version"],
            "license": package.get("license"), "repository": package.get("repository"),
            "notice_files": copied,
            "notice_sources": sources,
            "notice_review_limitations": entry.get("collection_limitations", []) if entry else [],
        })
    return inventory


# The development bundle is a native client, not a Core/schema-upgrade rollout.
# Keep the feature set explicit; adding a default feature must not silently
# change which capability reaches a packaged binary.
SHIPPING_FEATURES = ["gui"]
SERVER_PACKAGES = {"formless-core", "formless-runtime", "formless-tui", "formless"}


def shipping_graph(metadata):
    packages = {p["id"]: p for p in metadata["packages"]}
    resolve = metadata.get("resolve") or {}
    root = resolve.get("root")
    nodes = {n["id"]: n for n in resolve.get("nodes", [])}
    if len(packages) != len(metadata["packages"]) or len(nodes) != len(resolve.get("nodes", [])):
        raise ValueError("shipping graph contains duplicate package identities")
    if root not in packages or root not in nodes or packages[root]["name"] != "formless-desktop":
        raise ValueError("shipping graph has no resolved desktop root")
    if sorted(nodes[root].get("features", [])) != SHIPPING_FEATURES:
        raise ValueError("shipping graph must enable only gui; validation/default/diagnostic features are refused")
    reached = set()
    pending = [root]
    while pending:
        current = pending.pop()
        if current in reached:
            continue
        if current not in nodes or current not in packages:
            raise ValueError("shipping graph contains an unresolved dependency")
        reached.add(current)
        if packages[current]["name"] in SERVER_PACKAGES:
            raise ValueError("shipping graph includes server package: " + packages[current]["name"])
        if packages[current]["name"] == "gpui-kit" and "test-support" in nodes[current].get("features", []):
            raise ValueError("shipping graph includes the GPUI test harness")
        for edge in nodes[current].get("deps", []):
            kinds = edge.get("dep_kinds", [])
            if not kinds or any(kind.get("kind") not in (None, "build", "dev") for kind in kinds):
                raise ValueError("shipping graph contains an unknown dependency kind")
            if any(kind.get("kind") in (None, "build") for kind in kinds):
                pending.append(edge["pkg"])
    return dict(root_package_id=root, features=SHIPPING_FEATURES.copy(),
                normal_and_build_package_ids=sorted(reached),
                server_packages_in_graph=[], gpui_test_harness=False)


def verify_shipping_artifacts(metadata, built, root_features):
    packages = {p["id"]: p for p in metadata["packages"]}
    if sorted(root_features or []) != SHIPPING_FEATURES:
        raise ValueError("desktop artifact must enable only gui")
    root = (metadata.get("resolve") or {}).get("root")
    if root not in built:
        raise ValueError("desktop root artifact missing from build")
    for identity in built:
        if identity not in packages:
            raise ValueError("build reported an unknown package")
        if packages[identity]["name"] in SERVER_PACKAGES:
            raise ValueError("build reported a server artifact: " + packages[identity]["name"])


def package(args):
    desktop = Path(__file__).resolve().parents[1]
    repository = desktop.parents[1]
    output = Path(args.output).resolve()
    if output.exists():
        raise ValueError("output already exists; choose a new directory: " + str(output))
    output.parent.mkdir(parents=True, exist_ok=True)
    commit = run(["git", "rev-parse", "HEAD"], repository)
    epoch = int(run(["git", "show", "-s", "--format=%ct", commit], repository))
    dirty = bool(run(["git", "status", "--porcelain", "--untracked-files=normal"], repository))
    if dirty and not args.allow_dirty:
        raise ValueError("source changes exist; commit them or explicitly use --allow-dirty for a development bundle")
    platform = {"darwin": "macos", "win32": "windows", "linux": "linux"}.get(sys.platform)
    if platform is None:
        raise ValueError("unsupported packaging host: " + sys.platform)
    rustc = run(["rustc", "-vV"], repository)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    common = ["--manifest-path", "crates/formless-desktop/Cargo.toml", "--locked",
              "--no-default-features", "--features", ",".join(SHIPPING_FEATURES)]
    if args.offline:
        common.append("--offline")
    metadata = json.loads(run(["cargo", "metadata", *common, "--format-version", "1", "--filter-platform", host], repository))
    graph = shipping_graph(metadata)  # Refuse server/validation inputs before a build.
    command = ["cargo", "build", *common, "--bin", "formless-desktop", "--message-format=json-render-diagnostics"]
    if args.profile == "release":
        command.append("--release")
    built = set()
    binary = None
    root_features = None
    with subprocess.Popen(command, cwd=repository, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            event = json.loads(line)
            if event.get("reason") == "compiler-artifact":
                built.add(event["package_id"])
                if event["package_id"] == graph["root_package_id"] and event["target"]["name"] == "formless-desktop" and event.get("executable"):
                    if binary is not None or event["target"].get("kind") != ["bin"] or event.get("profile", {}).get("test") is not False:
                        raise ValueError("ambiguous or test desktop executable artifact")
                    binary = Path(event["executable"])
                    root_features = event.get("features")
            elif event.get("reason") == "compiler-message" and event.get("message", {}).get("rendered"):
                print(event["message"]["rendered"], end="", file=sys.stderr)
        code = process.wait()
    if code != 0:
        raise ValueError("desktop build failed; no package published")
    if binary is None or not binary.is_file():
        raise ValueError("Cargo did not report the desktop executable")
    verify_shipping_artifacts(metadata, built, root_features)
    ensure_source(repository, commit, dirty)
    with binary.open("rb") as source:
        magic = source.read(4)
    valid = {"macos": magic in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe"),
             "windows": magic.startswith(b"MZ"), "linux": magic == b"\x7fELF"}[platform]
    if not valid:
        raise ValueError("binary format does not match the native packaging host")
    relative = binary.resolve().relative_to(Path(metadata["target_directory"]).resolve())
    targets = run(["rustc", "--print", "target-list"], repository).splitlines()
    if relative.parts[0] in targets and relative.parts[0] != host:
        raise ValueError("the Cargo configuration selected a cross target; use a native build configuration")
    own = next(item for item in metadata["packages"] if item["name"] == "formless-desktop")
    version = own["version"]
    with tempfile.TemporaryDirectory(prefix="formless-package-", dir=output.parent) as temporary:
        stage = Path(temporary)
        if platform == "macos":
            bundle = stage / "Formless Development.app"
            executable = bundle / "Contents/MacOS/formless-desktop"
            resources = bundle / "Contents/Resources"
            executable.parent.mkdir(parents=True)
            resources.mkdir(parents=True)
            info = {"CFBundleName": "Formless Development", "CFBundleDisplayName": "Formless Development",
                    "CFBundleIdentifier": "xyz.formless.desktop.development", "CFBundlePackageType": "APPL",
                    "CFBundleExecutable": executable.name, "CFBundleShortVersionString": version,
                    "CFBundleVersion": version.split("-")[0], "NSHighResolutionCapable": True}
            load_commands = run(["otool", "-l", str(binary)], repository)
            minimum = re.search(r"cmd LC_BUILD_VERSION.*?minos ([0-9.]+)", load_commands, re.S)
            if minimum:
                info["LSMinimumSystemVersion"] = minimum.group(1)
            with (bundle / "Contents/Info.plist").open("wb") as target:
                plistlib.dump(info, target)
        else:
            bundle = stage / "formless-desktop-development"
            resources = bundle
            resources.mkdir()
            executable = bundle / ("formless-desktop.exe" if platform == "windows" else "formless-desktop")
        shutil.copy2(binary, executable)
        shutil.copyfile(repository / "LICENSE", resources / "LICENSE")
        shutil.copyfile(desktop / "README.md", resources / "README.md")
        upstream = desktop / "notices"
        inventory = collect_notices(metadata["packages"], built, resources / "third-party-notices", upstream, desktop / "Cargo.lock")
        if (upstream / "UPSTREAM.json").is_file():
            shutil.copyfile(upstream / "UPSTREAM.json", resources / "UPSTREAM_NOTICES.json")
        toolchain_manifest = collect_toolchain_notices(upstream, resources / "toolchain-notices", rustc)
        write_json(resources / "DEPENDENCIES.json", inventory)
        provenance = {"commit": commit, "dirty": dirty, "profile": args.profile, "target": host,
                      "version": version, "binary_sha256": digest(executable),
                      "lockfile_sha256": digest(desktop / "Cargo.lock"), "build_command": command,
                      "distribution": "development only; no distribution signing performed",
                      "native_acceptance": False,
                      "shipping_graph": graph,
                      "server_build_artifacts": [],
                      "upstream_notice_manifest_sha256": digest(upstream / "UPSTREAM.json") if (upstream / "UPSTREAM.json").is_file() else None,
                      "toolchain_notice_manifest_sha256": toolchain_manifest,
                      "archive_timestamp_source": "git commit time, UTC, normalized to ZIP range/granularity",
                      "license_review": "Cargo, reviewed upstream and top-level Rust notices collected; full asset/stdlib and distribution review still required",
                      "packages_with_notice_review_limitations": [p["name"] + "-" + p["version"] for p in inventory if p["notice_review_limitations"]],
                      "packages_without_notice_files": [p["name"] + "-" + p["version"] for p in inventory if not p["notice_files"]]}
        write_json(resources / "BUILD.json", provenance)
        archive = stage / ("formless-desktop-" + version + "-" + host + ".zip")
        write_archive(bundle, executable, archive, epoch)
        with zipfile.ZipFile(archive) as target:
            if target.testzip() is not None:
                raise ValueError("archive integrity check failed")
            if hashlib.sha256(target.read(executable.relative_to(stage).as_posix())).hexdigest() != provenance["binary_sha256"]:
                raise ValueError("archive executable differs from built executable")
        (stage / "SHA256SUMS").write_text(digest(archive) + "  " + archive.name + "\n", encoding="ascii")
        ensure_source(repository, commit, dirty, stage)
        output.mkdir()  # Refuse any output that appeared while the build ran.
        for source in stage.iterdir():
            source.rename(output / source.name)
    print(json.dumps({"output": str(output), "commit": commit, "dirty": dirty, "profile": args.profile,
                      "target": host, "binary_sha256": provenance["binary_sha256"], "archive": archive.name}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="new directory for the bundle and ZIP")
    parser.add_argument("--profile", choices=("dev", "release"), default="release")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--allow-dirty", action="store_true", help="record tracked changes in development provenance")
    try:
        package(parser.parse_args())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print("Packaging failed: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
