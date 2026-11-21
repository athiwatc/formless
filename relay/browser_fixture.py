"""Shared browser and artifact defaults for the standalone Playwright suites."""
import os
from pathlib import Path


def chromium_executable(playwright):
    override = os.environ.get("FORMLESS_CHROMIUM_EXECUTABLE")
    if override:
        return override
    system = Path("/usr/bin/chromium")
    return str(system) if system.is_file() else playwright.chromium.executable_path


def artifact_dir(default):
    return Path(os.environ.get("FORMLESS_BROWSER_ARTIFACTS", default))


def browser_engines():
    engine = os.environ.get("FORMLESS_BROWSER_TEST_ENGINE", "all")
    if engine == "all":
        return ("chromium", "webkit")
    if engine in ("chromium", "webkit"):
        return (engine,)
    raise ValueError(f"Unknown FORMLESS_BROWSER_TEST_ENGINE: {engine}")
