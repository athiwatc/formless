#!/usr/bin/env python3
"""Check what a real macOS window of the shell gives assistive technology.

Usage: check-platform-a11y.py <formless-desktop built with --features ax-dump> <output dir>

Opens the shell three times in the background (never brought to the front,
nothing typed or clicked), lets it walk its own window's NSAccessibility
elements in process (--dump-platform-a11y), and checks the walk. This is the
platform adapter's output on this Mac. It is not a screen reader and says
nothing about what VoiceOver speaks. Exit status 0 only if every check holds.
"""
import json, os, re, subprocess, sys, time

binary, out = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
failures = 0

def check(name, ok, detail=""):
    global failures
    print(f"{'PASS' if ok else 'FAIL'} {name}{': ' + detail if detail and not ok else ''}")
    failures += 0 if ok else 1

def walk(name, *args):
    path = os.path.abspath(os.path.join(out, f"{name}.txt"))
    gpui = os.path.abspath(os.path.join(out, f"{name}-gpui-tree.json"))
    if os.path.exists(path) or os.path.exists(gpui):
        sys.exit(f"{path} exists; use a fresh output directory")
    proc = subprocess.Popen([binary, "--background", "--lifetime", "6", "--sessions", "200",
                             "--blocks", "40", *args, "--dump-platform-a11y", path,
                             "--dump-a11y", gpui],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # The adapter starts on the first walk; the content is there from the second.
    deadline = time.time() + 5
    while time.time() < deadline:
        time.sleep(0.5)
        if os.path.exists(path) and len(open(path).read().splitlines()) > 5:
            time.sleep(1)
            break
    proc.wait()
    return open(path).read().splitlines() if os.path.exists(path) else []

def window_children(lines):
    return [l.strip() for l in lines if re.match(r"^    \S", l)]

# The page, wide.
page = walk("page-1440x900", "--window", "1440x900")
text = "\n".join(page)
check("page: the window has content", len(page) > 20, f"{len(page)} lines")
check("page: banner, navigation, main and status landmarks",
      all(s in text for s in ('subrole="AXLandmarkBanner"', 'subrole="AXLandmarkNavigation"',
                               'subrole="AXLandmarkMain"', 'subrole="AXApplicationStatus"')))
check("page: the open Session's title is a heading", re.search(r'^\s*Heading .*title="', text, re.M) is not None)
options = [l for l in page if "can=[press,pick,set_selected]" in l]
check("page: Session rows are pressable, selectable options", len(options) >= 10, f"{len(options)}")
check("page: exactly one Session row is selected",
      sum(" selected=true" in l for l in options) == 1)
blocks = [l for l in page if 'AXDocumentArticle' in l]
check("page: timeline blocks are named articles", len(blocks) >= 3, f"{len(blocks)}")
values = re.findall(r'value="([^"]*)"', "\n".join(
    page[i + 1] for i, l in enumerate(page) if 'AXDocumentArticle' in l and i + 1 < len(page)))
check("page: block text is plain text, no markdown syntax",
      values and not any(re.search(r"```|_block|^> |\*\*", v) for v in values), str(values[:2]))
check("page: no element is a dialog", "AXDialog" not in text)

# Each panel as a modal dialog.
for name, args in (("sessions-480x360-200pct", ("--window", "480x360", "--text-scale", "200",
                                                "--keep-window", "--open-panel", "sessions")),
                   ("context-1100x900", ("--window", "1100x900", "--open-panel", "context"))):
    lines = walk(name, *args)
    children = window_children(lines)
    check(f"{name}: the dialog is the window's only child", len(children) == 1 and "AXDialog" in children[0],
          str([c[:40] for c in children]))
    dialog = next((l for l in lines if "AXDialog" in l), "")
    check(f"{name}: the dialog is modal", "modal=true" in dialog, dialog[:120])
    # Focus is GPUI's: the platform reports none for a window that has never
    # been made key, and these windows are never brought to the front.
    tree = json.load(open(os.path.join(out, f"{name}-gpui-tree.json")))
    focus = tree["nodes"].get(tree.get("gpui_focus") or "", {}).get("aria", {})
    check(f"{name}: GPUI's focus is on the dialog", focus.get("role") == "Dialog", str(focus)[:120])
    print(f"NOTE {name}: platform focus reported on {sum('focused=true' in l for l in lines)} element(s) "
          "(none is expected in a window that was never made key)")
    check(f"{name}: the dialog has its contents", len(lines) > 4, f"{len(lines)} lines")

print(f"{'OK' if failures == 0 else 'FAILED'}: {failures} check(s) failed")
sys.exit(1 if failures else 0)
