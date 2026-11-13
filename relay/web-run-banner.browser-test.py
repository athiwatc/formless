#!/usr/bin/env python3
"""Check both unfinished Run banner branches with shipped DOM/CSS."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument("--css", type=Path, help="Optional CSS negative control")
parser.add_argument("--out", type=Path, default=Path("/tmp/formless-banner-1654"))
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
    window.__banner1654 = unfinished => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); setView("chat");
      S.unfinished = unfinished; S.running.clear(); renderRunBanner();
      S.step = {step_id:"inert-step",tool_name:"fixture",index:0,count:1,arguments:"{}"};
      renderStep(); $("step").classList.add("hidden");
    };
"""
    route.fulfill(status=200, content_type="text/javascript", body=source[:end] + hook + source[end:])


long_error = "error_token_ไทย_" * 50
fixtures = [
    dict(status="paused", error=long_error),
    dict(status="interrupted", error=long_error, explained=dict(message=long_error, action="retry_" * 50, details="details")),
    dict(status="paused", error="short"),
]
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(ROOT / "web")))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path="/usr/bin/chromium", headless=True, args=["--no-sandbox"])
        for width in [240, 360, 900]:
            for font in [16, 32]:
                for index, fixture in enumerate(fixtures):
                    page = browser.new_page(viewport={"width": width, "height": 1000})
                    page.set_default_timeout(5000)
                    page.route("**/app.js", expose)
                    if args.css:
                        page.route("**/app.css", lambda route: route.fulfill(status=200, content_type="text/css", body=args.css.read_text()))
                    page.goto(f"http://127.0.0.1:{server.server_port}/")
                    page.wait_for_function("!!window.__banner1654")
                    page.evaluate("size => document.documentElement.style.fontSize=size+'px'", font)
                    page.evaluate("value => __banner1654(value)", fixture)
                    text = page.locator("#run-banner > .banner-text").text_content()
                    if fixture.get("explained"):
                        assert fixture["explained"]["message"] in text and fixture["explained"]["action"] in text
                        page.locator("#run-banner summary").click()
                        assert page.locator("#run-banner code").text_content() == long_error
                    else:
                        assert "The last Run was " + fixture["status"] in text
                    geometry = page.evaluate("""() => {
                      const box=document.querySelector('#run-banner'), text=box.querySelector('span');
                      const rect=el=>{const r=el.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,width:r.width,client:el.clientWidth,scroll:el.scrollWidth};};
                      return {box:rect(box),text:rect(text),buttons:[...box.querySelectorAll('button')].map(rect),
                        page:[document.documentElement.clientWidth,document.documentElement.scrollWidth],
                        stepWrap:getComputedStyle(document.querySelector('#step .banner-text')).overflowWrap};
                    }""")
                    for name, value in [("text",geometry["text"]), *[("action",v) for v in geometry["buttons"]]]:
                        assert value["width"] > 0 and value["left"] >= geometry["box"]["left"] and value["right"] <= geometry["box"]["right"] and value["scroll"] <= value["client"] + 1, f"banner text/actions stay contained {width}/{font}/{index} {name}: {geometry}"
                    assert geometry["page"][1] <= geometry["page"][0] + 1
                    assert geometry["stepWrap"] == "normal", "step widget text styles are unchanged"
                    buttons=page.locator('#run-banner > button')
                    assert buttons.all_text_contents() == ["Resume", "Abandon"]
                    for button in buttons.all():
                        button.focus()
                        assert button.evaluate("el => el === document.activeElement")
                        assert button.evaluate("el => {const r=el.getBoundingClientRect();return r.top >= 0 && r.bottom <= innerHeight;}"), "keyboard focus brings each action into the visible viewport"
                    if width == 900 and font == 16 and index == 2:
                        assert abs(geometry["text"]["top"] - geometry["buttons"][0]["top"]) < 10, "short text shares the row on wide screens"
                    page.screenshot(path=str(args.out / f"banner-{width}-{font}-{index}.png"))
                    print(f"PASS banner {width}px / root {font}px / branch {index}", flush=True)
                    page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
