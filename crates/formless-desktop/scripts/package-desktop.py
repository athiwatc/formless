#!/usr/bin/env python3
"""Build an optional, unsigned development bundle; never install or launch it.

Uses only Python's standard library. Release signing, installers, updating and
native acceptance are separate gates. Existing output directories are refused.
"""

import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import re
import shutil
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


def collect_notices(packages, built, destination):
    inventory = []
    for package in sorted(packages, key=lambda item: (item["name"], item["version"])):
        if package["id"] not in built:
            continue
        root = Path(package["manifest_path"]).parent.resolve()
        candidates = set()
        for pattern in ("LICENSE*", "LICENCE*", "COPYING*", "NOTICE*"):
            candidates.update(root.glob(pattern))
        if package.get("license_file"):
            candidates.add(root / package["license_file"])
        copied = []
        for source in sorted(candidates):
            # Manifest paths and symlinks must not make packaging read files
            # outside a dependency's package root.
            if source.is_symlink() or not source.is_file():
                continue
            try:
                source.resolve().relative_to(root)
            except ValueError:
                continue
            if source.stat().st_size > 2 * 1024 * 1024:
                raise ValueError("oversized dependency notice: " + source.name)
            target = destination / (package["name"] + "-" + package["version"]) / source.name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            copied.append(str(target.relative_to(destination)))
        inventory.append({
            "name": package["name"], "version": package["version"],
            "license": package.get("license"), "repository": package.get("repository"),
            "notice_files": copied,
        })
    return inventory


def package(args):
    desktop = Path(__file__).resolve().parents[1]
    repository = desktop.parents[1]
    output = Path(args.output).resolve()
    if output.exists():
        raise ValueError("output already exists; choose a new directory: " + str(output))
    output.parent.mkdir(parents=True, exist_ok=True)
    commit = run(["git", "rev-parse", "HEAD"], repository)
    dirty = bool(run(["git", "status", "--porcelain", "--untracked-files=normal"], repository))
    if dirty and not args.allow_dirty:
        raise ValueError("source changes exist; commit them or explicitly use --allow-dirty for a development bundle")
    platform = {"darwin": "macos", "win32": "windows", "linux": "linux"}.get(sys.platform)
    if platform is None:
        raise ValueError("unsupported packaging host: " + sys.platform)
    rustc = run(["rustc", "-vV"], repository)
    host = next(line.removeprefix("host: ") for line in rustc.splitlines() if line.startswith("host: "))
    common = ["--manifest-path", "crates/formless-desktop/Cargo.toml", "--locked"]
    if args.offline:
        common.append("--offline")
    command = ["cargo", "build", *common, "--message-format=json-render-diagnostics"]
    if args.profile == "release":
        command.append("--release")
    built = set()
    binary = None
    with subprocess.Popen(command, cwd=repository, stdout=subprocess.PIPE, text=True) as process:
        for line in process.stdout:
            event = json.loads(line)
            if event.get("reason") == "compiler-artifact":
                built.add(event["package_id"])
                if event["target"]["name"] == "formless-desktop" and event.get("executable"):
                    binary = Path(event["executable"])
            elif event.get("reason") == "compiler-message" and event.get("message", {}).get("rendered"):
                print(event["message"]["rendered"], end="", file=sys.stderr)
        code = process.wait()
    if code != 0:
        raise ValueError("desktop build failed; no package published")
    if binary is None or not binary.is_file():
        raise ValueError("Cargo did not report the desktop executable")
    with binary.open("rb") as source:
        magic = source.read(4)
    valid = {"macos": magic in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xca\xfe\xba\xbe"),
             "windows": magic.startswith(b"MZ"), "linux": magic == b"\x7fELF"}[platform]
    if not valid:
        raise ValueError("binary format does not match the native packaging host")
    metadata = json.loads(run(["cargo", "metadata", *common, "--format-version", "1", "--filter-platform", host], repository))
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
        inventory = collect_notices(metadata["packages"], built, resources / "third-party-notices")
        write_json(resources / "DEPENDENCIES.json", inventory)
        provenance = {"commit": commit, "dirty": dirty, "profile": args.profile, "target": host,
                      "version": version, "binary_sha256": digest(executable),
                      "lockfile_sha256": digest(desktop / "Cargo.lock"), "build_command": command,
                      "distribution": "development only; no distribution signing performed",
                      "native_acceptance": False,
                      "license_review": "Cargo dependency notices collected; manual asset/stdlib and distribution review still required",
                      "packages_without_notice_files": [p["name"] + "-" + p["version"] for p in inventory if not p["notice_files"]]}
        write_json(resources / "BUILD.json", provenance)
        archive = stage / ("formless-desktop-" + version + "-" + host + ".zip")
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as target:
            for source in sorted(bundle.rglob("*")):
                if source.is_file():
                    target.write(source, source.relative_to(stage).as_posix())
        with zipfile.ZipFile(archive) as target:
            if target.testzip() is not None:
                raise ValueError("archive integrity check failed")
            if hashlib.sha256(target.read(executable.relative_to(stage).as_posix())).hexdigest() != provenance["binary_sha256"]:
                raise ValueError("archive executable differs from built executable")
        (stage / "SHA256SUMS").write_text(digest(archive) + "  " + archive.name + "\n", encoding="ascii")
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
