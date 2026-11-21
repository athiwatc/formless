#!/usr/bin/env python3
"""Check pending and sent attachment chips with the shipped renderer/CSS."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument("--css", type=Path, help="Optional CSS negative control")
parser.add_argument("--out", type=Path, default=artifact_dir("/tmp/formless-attachment-1653"))
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
    window.__attachments1653 = attachments => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); setView("chat");
      S.current="fixture"; S.chips.fixture=attachments; renderChips();
      S.messages=[{id:"fixture-message",role:"user",content:"Sent sibling placement",meta:JSON.stringify({attachments})}];
      renderHistory();
    };
"""
    route.fulfill(status=200, content_type="text/javascript", body=source[:end] + hook + source[end:])


attachments = [
    dict(id="long", name="unbroken_name_" * 40, mime="text/plain", bytes=4096),
    dict(id="unicode", name="日本語ไทย📄🚀" * 50, mime="image/png", bytes=2048),
    dict(path="/private/reference/" * 25, name="/private/reference/" * 25, reference=True, expired=True, bytes=0),
    dict(id="short", name="short.txt", mime="text/plain", bytes=20),
]
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(ROOT / "web")))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=["--no-sandbox"])
        for width in [320, 480, 900]:
            for font in [16, 32]:
                page = browser.new_page(viewport={"width": width, "height": 1000})
                page.set_default_timeout(5000)
                page.route("**/app.js", expose)
                if args.css:
                    page.route("**/app.css", lambda route: route.fulfill(status=200, content_type="text/css", body=args.css.read_text()))
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                page.wait_for_function("!!window.__attachments1653")
                page.evaluate("size => document.documentElement.style.fontSize=size+'px'", font)
                page.evaluate("value => __attachments1653(value)", attachments)
                assert page.locator('.sent-from .attach-chip').count() == len(attachments)
                assert page.locator('.bubble .attach-chip').count() == 0, "sent chips must be tested as siblings outside the bubble"
                for index, attachment in enumerate(attachments):
                    pending=page.locator('#attach-chips .attach-chip').nth(index)
                    sent=page.locator('#transcript .attach-chip').nth(index)
                    assert pending.locator('.attach-name').text_content() == attachment['name']
                    assert sent.locator('.attach-name').text_content() == attachment['name'] + (' · expired' if attachment.get('expired') else '')
                    assert pending.get_attribute('title') == (attachment.get('mime') or 'file') + ' · ' + str(max(1,round(attachment['bytes']/1024))) + ' KB'
                    remove=pending.get_by_role('button',name='Remove '+attachment['name'],exact=True)
                    remove.focus()
                    assert remove.evaluate("el => {const r=el.getBoundingClientRect();return el===document.activeElement && r.top>=0 && r.bottom<=innerHeight;}")
                    if attachment.get('reference'):
                        assert sent.locator('.attach-icon').text_content() == '↗'
                geometry=page.evaluate("""() => {
                  const checks=[];
                  for(const selector of ['#attach-chips','#transcript']) {
                    const root=document.querySelector(selector), bounds=root.getBoundingClientRect();
                    checks.push([selector,root.clientWidth,root.scrollWidth]);
                    for(const chip of root.querySelectorAll('.attach-chip')) {
                      const r=chip.getBoundingClientRect();
                      checks.push(['chip',chip.clientWidth,chip.scrollWidth]);
                      if(r.left < bounds.left-1 || r.right > bounds.right+1) throw new Error('chip outside column');
                      for(const el of chip.querySelectorAll('.attach-icon,button')) {
                        const c=el.getBoundingClientRect();
                        if(c.width<=0 || c.left<r.left || c.right>r.right) throw new Error('control outside chip');
                      }
                    }
                  }
                  checks.push(['page',document.documentElement.clientWidth,document.documentElement.scrollWidth]);
                  return checks;
                }""")
                for name, client, scroll in geometry:
                    assert client > 0 and scroll <= client + 1, f"attachment stays within width {width}/{font} {name}: {geometry}"
                page.screenshot(path=str(args.out / f"attachments-{width}-{font}.png"))
                print(f"PASS attachment chips {width}px / root {font}px", flush=True)
                page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
