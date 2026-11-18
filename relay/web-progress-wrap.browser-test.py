#!/usr/bin/env python3
"""Render synthetic live progress with shipped DOM/CSS; no daemon/provider calls."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument("--css", type=Path, help="Optional CSS fixture for a negative control")
parser.add_argument("--out", type=Path, default=artifact_dir("/tmp/formless-progress-1736"))
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / "web/app.js").read_text()
    end = source.rfind("\n})();")
    assert end > 0
    hook = r'''
    window.__progress1736 = label => {
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      setView("chat");
      const tool = {name:"fixture_tool", version:1, status:"running", handle:"fixture",
        subagent:"fixture-subagent", step:"inert layout fixture", arguments:"{}",
        progress:{fraction:0.25,label}, elapsedMs:500, depth:4};
      const row=liveToolRow(tool); updateLiveTool(row,false);
      $("transcript").replaceChildren(row.node);
      return row.node;
    };
'''
    route.fulfill(status=200, content_type="text/javascript", body=source[:end] + hook + source[end:])


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(ROOT / "web")))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=["--no-sandbox"])
        for width in [320, 480, 900]:
            for font in [16, 32]:
                page = browser.new_page(viewport={"width":width,"height":1000})
                page.set_default_timeout(5000)
                page.route("**/app.js", expose)
                if args.css:
                    page.route("**/app.css", lambda route: route.fulfill(status=200, content_type="text/css", body=args.css.read_text()))
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                page.wait_for_function("!!window.__progress1736")
                page.evaluate("size => document.documentElement.style.fontSize=size+'px'", font)
                label = "Indexing_workspace_ไทย_" * 12
                page.evaluate("label => __progress1736(label)", label)
                geometry = page.evaluate('''() => {
                  const root=document.querySelector('#transcript'), row=root.querySelector('.chip.live');
                  const bounds=root.getBoundingClientRect();
                  const measure=el=>{const r=el.getBoundingClientRect();return {left:r.left,right:r.right,width:r.width};};
                  return {bounds:measure(root), row:measure(row), progress:measure(row.querySelector('.tprogress')),
                    bar:measure(row.querySelector('progress')), cancel:measure(row.querySelector('.danger')),
                    transcript:measure(row.querySelector('.ghost')), text:row.querySelector('.tprogress').textContent,
                    fraction:row.querySelector('progress').value,name:row.querySelector('.tname').textContent};
                }''')
                page.screenshot(path=str(args.out / f"progress-{width}-{font}.png"))
                for key in ["row", "progress", "bar", "cancel", "transcript"]:
                    value = geometry[key]
                    assert value["width"] > 0 and value["left"] >= geometry["bounds"]["left"] - 1 and value["right"] <= geometry["bounds"]["right"] + 1, f"live progress stays in transcript: {width}/{font} {key}: {geometry}"
                assert label in geometry["text"] and "25%" in geometry["text"]
                assert geometry["fraction"] == .25 and geometry["name"] == "fixture_tool v1"
                assert page.locator(".chip.live .danger").is_visible()
                assert page.locator(".chip.live .ghost").is_visible()
                page.evaluate("__progress1736('Short label')")
                assert page.locator(".tprogress").text_content() == " 25% Short label"
                print(f"PASS live progress {width}px / root {font}px", flush=True)
                page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
