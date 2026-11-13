#!/usr/bin/env python3
"""Check what a real macOS window of the shell gives assistive technology.

Usage: check-platform-a11y.py <formless-desktop built with --features ax-dump> <output dir>
                              [--timeout SECONDS]

Opens the shell three times in the background (never brought to the front,
nothing typed or clicked), lets it walk its own window's NSAccessibility
elements in process (--dump-platform-a11y), and checks the walk. This is the
platform adapter's output on this Mac. It is not a screen reader and says
nothing about what VoiceOver speaks. Exit status 0 only if every check holds.

Each run's stdout and stderr are kept beside its tree. A run that does not
exit within the timeout is terminated (only that process, the one this
script started) and counts as a failure, as does a nonzero exit status or a
missing or unreadable tree.
"""
import json
import os
import re
import subprocess
import sys
import time

# The shell quits by itself after this many seconds (--lifetime).
LIFETIME = 6


class Checker:
    def __init__(self, binary, out, timeout):
        self.binary, self.out, self.timeout = binary, out, timeout
        self.failures = 0
        os.makedirs(out, exist_ok=True)

    def check(self, name, ok, detail=""):
        print(f"{'PASS' if ok else 'FAIL'} {name}{': ' + detail if detail and not ok else ''}")
        self.failures += 0 if ok else 1
        return ok

    def path(self, name, suffix):
        return os.path.abspath(os.path.join(self.out, f"{name}{suffix}"))

    def walk(self, name, *args):
        """Run one window; return its platform walk as lines, or None if the
        run failed (each failure already reported)."""
        tree, gpui = self.path(name, ".txt"), self.path(name, "-gpui-tree.json")
        for existing in (tree, gpui):
            if os.path.exists(existing):
                sys.exit(f"{existing} exists; use a fresh output directory")
        with open(self.path(name, ".stdout"), "wb") as stdout, \
                open(self.path(name, ".stderr"), "wb") as stderr:
            proc = subprocess.Popen(
                [self.binary, "--background", "--lifetime", str(LIFETIME), "--sessions", "200",
                 "--blocks", "40", *args, "--dump-platform-a11y", tree, "--dump-a11y", gpui],
                stdout=stdout, stderr=stderr)
            try:
                code = proc.wait(timeout=self.timeout)
            except subprocess.TimeoutExpired:
                # Only the child started here: terminate, then kill if it
                # does not go.
                proc.terminate()
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                self.check(f"{name}: the run exits by itself", False,
                           f"still running after {self.timeout} s; terminated")
                return None
        if not self.check(f"{name}: the run exits with status 0", code == 0,
                          f"status {code}; stderr: {self.tail(name)}"):
            return None
        try:
            with open(tree, encoding="utf-8") as walked:
                lines = walked.read().splitlines()
        except OSError as error:
            self.check(f"{name}: the run wrote its tree", False, str(error))
            return None
        if not self.check(f"{name}: the window has content", len(lines) > 4, f"{len(lines)} lines"):
            return None
        return lines

    def tail(self, name):
        try:
            with open(self.path(name, ".stderr"), errors="replace") as file:
                return file.read()[-300:].strip() or "(empty)"
        except OSError:
            return "(none)"

    def gpui_focus(self, name):
        """(readable, role GPUI's own tree has focus on). A tree that cannot
        be read is reported as a failed check."""
        try:
            with open(self.path(name, "-gpui-tree.json"), encoding="utf-8") as file:
                tree = json.load(file)
            return True, tree["nodes"].get(tree.get("gpui_focus") or "", {}).get("aria", {}).get("role")
        except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
            self.check(f"{name}: GPUI's tree is readable", False, f"{type(error).__name__}: {error}")
            return False, None


def window_children(lines):
    return [line.strip() for line in lines if re.match(r"^    \S", line)]


def run(binary, out, timeout):
    c = Checker(binary, out, timeout)

    page = c.walk("page-1440x900", "--window", "1440x900")
    if page is not None:
        text = "\n".join(page)
        c.check("page: banner, navigation, main and status landmarks",
                all(s in text for s in ('subrole="AXLandmarkBanner"', 'subrole="AXLandmarkNavigation"',
                                         'subrole="AXLandmarkMain"', 'subrole="AXApplicationStatus"')))
        c.check("page: the open Session's title is a heading",
                re.search(r'^\s*Heading .*title="', text, re.M) is not None)
        options = [line for line in page if "can=[press,pick,set_selected]" in line]
        c.check("page: Session rows are pressable, selectable options", len(options) >= 10, f"{len(options)}")
        c.check("page: exactly one Session row is selected",
                sum(" selected=true" in line for line in options) == 1)
        c.check("page: timeline blocks are named articles",
                sum("AXDocumentArticle" in line for line in page) >= 3)
        values = re.findall(r'value="([^"]*)"', "\n".join(
            page[i + 1] for i, line in enumerate(page) if "AXDocumentArticle" in line and i + 1 < len(page)))
        c.check("page: block text is plain text, no markdown syntax",
                bool(values) and not any(re.search(r"```|_block|^> |\*\*", v) for v in values),
                str(values[:2]))
        c.check("page: no element is a dialog", "AXDialog" not in text)

    for name, args in (("sessions-480x360-200pct", ("--window", "480x360", "--text-scale", "200",
                                                    "--keep-window", "--open-panel", "sessions")),
                       ("context-1100x900", ("--window", "1100x900", "--open-panel", "context"))):
        lines = c.walk(name, *args)
        if lines is None:
            continue
        children = window_children(lines)
        c.check(f"{name}: the dialog is the window's only child",
                len(children) == 1 and "AXDialog" in children[0], str([x[:40] for x in children]))
        dialog = next((line for line in lines if "AXDialog" in line), "")
        c.check(f"{name}: the dialog is modal", "modal=true" in dialog, dialog[:120])
        # Focus is GPUI's: the platform reports none for a window that has
        # never been made key, and these windows are never brought to the front.
        readable, role = c.gpui_focus(name)
        if readable:
            c.check(f"{name}: GPUI's focus is on the dialog", role == "Dialog", str(role))
        print(f"NOTE {name}: platform focus reported on "
              f"{sum('focused=true' in line for line in lines)} element(s) "
              "(none is expected in a window that was never made key)")

    print(f"{'OK' if c.failures == 0 else 'FAILED'}: {c.failures} check(s) failed")
    return 1 if c.failures else 0


def main(argv):
    args = list(argv[1:])
    timeout = LIFETIME + 14
    if "--timeout" in args:
        index = args.index("--timeout")
        timeout = float(args[index + 1])
        del args[index:index + 2]
    if len(args) != 2:
        sys.exit(__doc__)
    return run(args[0], args[1], timeout)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
