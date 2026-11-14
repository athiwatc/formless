#!/usr/bin/env python3
"""Check the shipped comparison formatter/modal/CSS with synthetic local data."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument("--css", type=Path, help="Optional CSS negative control")
parser.add_argument("--out", type=Path, default=Path("/tmp/formless-comparison-1716"))
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / "web/app.js").read_text()
    end = source.rfind("\n})();")
    assert end > 0
    hook = r"""
    window.__comparison1716 = comparison => {
      $("pair").classList.add("hidden");
      $("app").classList.remove("hidden");
      const text = sessionComparisonText(comparison);
      const body = h("pre", {class:"session-comparison"}, text);
      showBidi(body);
      modal("Stored Session comparison", body);
      return text.replace(BIDI, c => "⟨U+" + c.charCodeAt(0).toString(16).toUpperCase().padStart(4,"0") + "⟩");
    };
"""
    route.fulfill(status=200, content_type="text/javascript", body=source[:end] + hook + source[end:])


usage = dict(scope="whole Runs after fork boundary", recorded_prompt_tokens=21,
             recorded_completion_tokens=8, recorded_cached_tokens=3, recorded_cache_write_tokens=2,
             requests=1, unallocated_requests=0, unallocated_prompt_tokens=0, unallocated_completion_tokens=0,
             unknown_runs=0, usage_complete=False, cost=None, priced_cost_subtotal=.25,
             unpriced=["private/model/" * 30], warnings=[])
comparison = dict(left=dict(session=dict(id="left-" * 60, name="Left"), usage=usage),
                  right=dict(session=dict(id="right-" * 60, name="Right"), usage=usage),
                  common_ancestor="parent", fork_point_seq=3, diff_from_seq=0,
                  prefix_changed=False, warnings=[], messages=[dict(kind="left_only", left=dict(
                      id="message-" * 60, seq=4, role="assistant",
                      content=("Long prose ไทย 日本語 سلام. " * 40) + "\nExplicit second line\n" +
                              "unbroken_path_" * 100 + "\u202e<script>quoted data</script>",
                      meta=dict(path="private/path/" * 100)), right=None)])
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(ROOT / "web")))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path="/usr/bin/chromium", headless=True, args=["--no-sandbox"])
        for width in [360, 1280]:
            for font in [16, 32]:
                page = browser.new_page(viewport={"width": width, "height": 1000})
                page.set_default_timeout(5000)
                page.route("**/app.js", expose)
                if args.css:
                    page.route("**/app.css", lambda route: route.fulfill(status=200, content_type="text/css", body=args.css.read_text()))
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                page.wait_for_function("!!window.__comparison1716")
                page.evaluate("size => document.documentElement.style.fontSize=size+'px'", font)
                expected = page.evaluate("value => __comparison1716(value)", comparison)
                body = page.locator(".session-comparison")
                assert body.text_content() == expected, "formatter newlines and Bidi presentation must survive layout"
                assert "\nExplicit second line\n" in expected and "⟨U+202E⟩" in expected
                assert body.locator("script").count() == 0
                geometry = page.evaluate("""() => {
                  const body=document.querySelector('.session-comparison');
                  const parent=body.closest('.files-body');
                  return {body:[body.clientWidth,body.scrollWidth],parent:[parent.clientWidth,parent.scrollWidth],
                    viewport:[document.documentElement.clientWidth,document.documentElement.scrollWidth]};
                }""")
                page.screenshot(path=str(args.out / f"comparison-{width}-{font}.png"))
                for name, (client, scroll) in geometry.items():
                    assert client > 0 and scroll <= client + 1, f"comparison stays within width {width}/{font} {name}: {geometry}"
                print(f"PASS comparison {width}px / root {font}px", flush=True)
                page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
