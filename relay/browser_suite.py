#!/usr/bin/env python3
"""Run the standalone relay browser suites and loopback development tests."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent
HELPERS = {
    "test-bounded-wire.mjs",
    "test-room-state.mjs",
    "test-web-render.mjs",
    "test-wire.mjs",
}
BROWSER_SUFFIX = ".browser-test.py"
DEV_SUFFIX = ".dev-test.mjs"
SUITE_TIMEOUT_SECONDS = 120
TERMINATION_GRACE_SECONDS = 5


def discover_suites():
    browsers = sorted(ROOT.glob(f"*{BROWSER_SUFFIX}"))
    dev = sorted(ROOT.glob(f"*{DEV_SUFFIX}"))
    covered = {path.name for path in browsers + dev + list(ROOT.glob("*.test.mjs"))}
    unmatched = sorted(path.name for path in ROOT.glob("*test*")
                       if path.is_file() and path.name not in covered | HELPERS)
    if unmatched:
        raise SystemExit("Relay test files with no CI entry point: " + ", ".join(unmatched))
    if not browsers or not dev:
        raise SystemExit("Expected both relay browser and development suites.")
    return browsers, dev


def run_suite(path, output, failures, settings_fixture):
    browser = path.name.endswith(BROWSER_SUFFIX)
    folder = output / path.stem
    folder.mkdir()
    command = [sys.executable, "-B", str(path)] if browser else ["node", "--test", str(path)]
    env = {**os.environ, "FORMLESS_BROWSER_ARTIFACTS": str(folder),
           "FORMLESS_BROWSER_TEST_ENGINE": "chromium", "FORMLESS_SETTINGS_FIXTURE": str(settings_fixture)}
    # Chromium's Unix sockets require a short path even when the job's
    # artifact tree lives under a long runner workspace.
    with tempfile.TemporaryDirectory(dir="/tmp", prefix="flb-") as profile:
        env["TMPDIR"] = profile
        log_path = folder / "suite.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen(command, cwd=ROOT.parent, env=env, stdout=log,
                                       stderr=subprocess.STDOUT, start_new_session=True)
            try:
                status = process.wait(timeout=SUITE_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    process.wait(timeout=TERMINATION_GRACE_SECONDS)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait()
                status = 124
                log.write(f"\nTimed out after {SUITE_TIMEOUT_SECONDS} seconds.\n".encode())
    print(f"{'PASS' if status == 0 else 'FAIL'} {path.name}", flush=True)
    if status and failures:
        shutil.copytree(folder, failures / folder.name)
    return path, status, log_path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-inventory", action="store_true")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--failure-artifacts", type=Path)
    parser.add_argument("--settings-fixture", type=Path)
    args = parser.parse_args()
    browsers, dev = discover_suites()
    print(f"Discovered {len(browsers)} Chromium suites and {len(dev)} relay development suites.", flush=True)
    if args.check_inventory:
        return 0
    if not args.output or not args.settings_fixture or not args.settings_fixture.is_file():
        parser.error("--output and an existing --settings-fixture are required")
    output = args.output.resolve()
    output.mkdir(parents=True)
    failures = args.failure_artifacts
    if failures:
        failures.mkdir(parents=True, exist_ok=True)
    settings_fixture = args.settings_fixture.resolve()
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(lambda suite: run_suite(suite, output, failures, settings_fixture), browsers + dev))
    failed = [(path, status, log) for path, status, log in results if status]
    for path, status, log in failed:
        print(f"\n{path.name} exited with {status} ({log})")
        print("\n".join(log.read_text(errors="replace").splitlines()[-25:]))
    print(f"{len(results) - len(failed)}/{len(results)} suites passed. Logs: {output}")
    return int(bool(failed))


if __name__ == "__main__":
    sys.exit(main())
