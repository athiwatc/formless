#!/usr/bin/env python3
"""Wrap the locked desktop bundle and matching CLI in a native preview installer."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import zipfile


def extract_cli(archive, destination):
    windows = archive.suffix == ".zip"
    directory = archive.name.removesuffix(".zip" if windows else ".tar.gz")
    member = f"{directory}/{destination.name}"
    if windows:
        with zipfile.ZipFile(archive) as package, package.open(member) as source, destination.open("xb") as target:
            shutil.copyfileobj(source, target)
    else:
        with tarfile.open(archive, "r:gz") as package:
            info = package.getmember(member)
            if not info.isfile():
                raise ValueError("CLI archive member must be a regular file")
            with package.extractfile(info) as source, destination.open("xb") as target:
                shutil.copyfileobj(source, target)
        destination.chmod(0o755)


def macos(bundle, output):
    (bundle.parent / "Applications").symlink_to("/Applications")
    subprocess.run(["hdiutil", "create", "-volname", "Formless", "-srcfolder",
                    str(bundle.parent), "-format", "UDZO", str(output)], check=True)
    subprocess.run(["hdiutil", "verify", str(output)], check=True)


def linux(bundle, output, version):
    work = output.parent.parent / "debian-build"
    root = work / "package"
    control = root / "DEBIAN" / "control"
    control.parent.mkdir(parents=True)
    installed = root / "usr/lib/formless-desktop"
    shutil.copytree(bundle, installed)
    launcher = root / "usr/share/applications/formless-desktop.desktop"
    launcher.parent.mkdir(parents=True)
    launcher.write_text(
        "[Desktop Entry]\nType=Application\nName=Formless\n"
        "Exec=/usr/lib/formless-desktop/formless-desktop --start-daemon\n"
        "Terminal=false\nCategories=Utility;\n", encoding="utf-8",
    )
    (work / "debian").mkdir()
    (work / "debian/control").write_text("Source: formless-desktop\n", encoding="ascii")
    result = subprocess.check_output(
        ["dpkg-shlibdeps", "-O", f"-e{installed / 'formless-desktop'}", f"-e{installed / 'formless'}"],
        cwd=work, text=True,
    )
    depends = result.strip().removeprefix("shlibs:Depends=")
    if not depends or depends == result.strip():
        raise ValueError("dpkg-shlibdeps did not emit runtime dependencies")
    architecture = subprocess.check_output(["dpkg", "--print-architecture"], text=True).strip()
    control.write_text(
        f"Package: formless-desktop\nVersion: {version}\nArchitecture: {architecture}\n"
        f"Maintainer: Formless maintainers\nDepends: {depends}\n"
        "Recommends: libvulkan1, mesa-vulkan-drivers\n"
        "Description: Formless desktop client (main preview)\n", encoding="utf-8",
    )
    subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(root), str(output)], check=True)


def windows(bundle, output):
    makensis = shutil.which("makensis") or str(
        Path(os.environ["ProgramFiles(x86)"]) / "NSIS" / "makensis.exe"
    )
    subprocess.run([makensis, f"/DBUNDLE={bundle}", f"/DOUTPUT={output}",
                    str(Path(__file__).with_name("preview-installer.nsi"))], check=True)


def main():
    work = Path(os.environ["PACKAGE_DIR"]).resolve()
    target = os.environ["PACKAGE_TARGET"]
    extension = os.environ["PACKAGE_EXT"]
    release = os.environ["RELEASE_ID"]
    archive = work / "cli" / f"formless-{release}-{target}.{os.environ['CLI_ARCHIVE']}"
    cli = work / ("formless.exe" if extension == "exe" else "formless")
    extract_cli(archive, cli)
    staged = work / "staged"
    subprocess.run(
        [sys.executable, str(Path(__file__).with_name("package-desktop.py")), "--profile", "release",
         "--output", str(staged), "--cli-binary", str(cli)], check=True,
    )
    bundle = staged / ("Formless.app" if extension == "dmg" else "formless-desktop-development")
    resources = bundle / "Contents/Resources" if extension == "dmg" else bundle
    provenance = json.loads((resources / "BUILD.json").read_text(encoding="utf-8"))
    if provenance["target"] != target:
        raise ValueError(f"expected desktop target {target}, built {provenance['target']}")
    output = work / "release" / f"formless-desktop-{release}-{target}.{extension}"
    output.parent.mkdir()
    if extension == "dmg":
        image_root = work / "image"
        image_root.mkdir()
        bundle = bundle.rename(image_root / bundle.name)
        macos(bundle, output)
    elif extension == "deb":
        version = f"{provenance['version']}~main.{os.environ['GITHUB_RUN_NUMBER']}"
        linux(bundle, output, version)
    elif extension == "exe":
        windows(bundle, output)
    else:
        raise ValueError(f"unknown installer format: {extension}")
    with output.open("rb") as installer:
        checksum = hashlib.file_digest(installer, "sha256").hexdigest()
    output.with_suffix(f".{extension}.sha256").write_text(
        f"{checksum}  {output.name}\n", encoding="ascii"
    )


if __name__ == "__main__":
    main()
