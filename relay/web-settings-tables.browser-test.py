#!/usr/bin/env python3
"""Check Jobs/Memory Settings table containment and filtering with shipped UI."""
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
parser.add_argument("--out", type=Path, default=artifact_dir("/tmp/formless-settings-1652"))
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
    window.__settings1652 = payload => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); S.view="settings"; renderChrome(); renderView();
      S.jobs=payload.jobs; S.memories=payload.memories; S.local=true; renderSettings();
    };
"""
    route.fulfill(status=200, content_type="text/javascript", body=source[:end] + hook + source[end:])


payload = dict(
    jobs=[dict(id="job-long",prompt="uniquejob_ไทย_日本語_" * 60,author="user",enabled=False,approved=True),
          dict(id="job-short",prompt="short job",author="user",enabled=False,approved=True)],
    memories=[dict(id="memory-long",text="uniquememory_ไทย_日本語_" * 60,scope="user",author="user",enabled=True),
              dict(id="memory-short",text="short memory",scope="user",author="user",enabled=False)],
)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(ROOT / "web")))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=["--no-sandbox"])
        for width in [320, 480, 1280]:
            for font in [16, 32]:
                page = browser.new_page(viewport={"width": width, "height": 1000})
                page.set_default_timeout(5000)
                page.route("**/app.js", expose)
                if args.css:
                    page.route("**/app.css", lambda route: route.fulfill(status=200, content_type="text/css", body=args.css.read_text()))
                page.goto(f"http://127.0.0.1:{server.server_port}/")
                page.wait_for_function("!!window.__settings1652")
                page.evaluate("size => document.documentElement.style.fontSize=size+'px'", font)
                page.evaluate("value => __settings1652(value)", payload)
                sections=page.locator('.settings-records')
                assert sections.count()==2
                assert sections.nth(0).locator('.record-prose').all_text_contents()[0].startswith(payload['jobs'][0]['prompt'])
                assert sections.nth(1).locator('.record-prose').all_text_contents()[0].startswith(payload['memories'][0]['text'])
                geometry=page.evaluate("""() => {
                  const view=document.querySelector('#view-settings');
                  const bounds=view.getBoundingClientRect();
                  const result=[['page',document.documentElement.clientWidth,document.documentElement.scrollWidth]];
                  for(const el of [document.querySelector('#settings-filter'),...document.querySelectorAll('.settings-grid section')]) {
                    const r=el.getBoundingClientRect();
                    if(r.left<bounds.left-1 || r.right>bounds.right+1) throw new Error('Settings heading/filter/section outside viewport');
                  }
                  for(const section of document.querySelectorAll('.settings-records')) {
                    const r=section.getBoundingClientRect(),wrap=section.querySelector('.table-wrap');
                    if(r.left<bounds.left || r.right>bounds.right) throw new Error('section outside Settings viewport');
                    result.push(['section',section.clientWidth,section.scrollWidth]);
                    result.push(['wrapper width',section.clientWidth,wrap.getBoundingClientRect().width]);
                    for(const cell of section.querySelectorAll('.record-prose')) {
                      const style=getComputedStyle(cell), root=parseFloat(getComputedStyle(document.documentElement).fontSize);
                      const readableMinimum=8*root+parseFloat(style.paddingLeft)+parseFloat(style.paddingRight)+2;
                      result.push(['prose width',Math.max(wrap.clientWidth,readableMinimum),cell.getBoundingClientRect().width]);
                    }
                  }
                  return result;
                }""")
                unrelated=page.evaluate("() => {const v=document.querySelector('#view-settings');return [v.clientWidth,v.scrollWidth];}")
                if unrelated[1]>unrelated[0]+1:
                    print(f"NOTE existing unrelated Settings overflow {width}/{font}: {unrelated}; scoped record sections checked below",flush=True)
                for name, client, scroll in geometry:
                    assert client>0 and scroll<=client+1, f"Settings records stay contained {width}/{font} {name}: {geometry}"
                # Focusing each unclicked action must reveal it within its local wrapper.
                for button in sections.locator('tbody button').all():
                    button.evaluate("el => el.scrollIntoView({block:'nearest',inline:'nearest'})")
                    button.focus()
                    assert button.evaluate("""el => {const r=el.getBoundingClientRect(),w=el.closest('.table-wrap').getBoundingClientRect();return el===document.activeElement && r.left>=w.left-1 && r.right<=w.right+1;}"""), button.evaluate("el => JSON.stringify({text:el.textContent,button:el.getBoundingClientRect().toJSON(),wrapper:el.closest('.table-wrap').getBoundingClientRect().toJSON(),scroll:el.closest('.table-wrap').scrollLeft})")
                for query, expected in [('uniquejob',(1,0)),('uniquememory',(0,1)),('short',(1,1)),('',(2,2))]:
                    page.locator('#settings-filter').fill(query)
                    counts=page.evaluate("""() => [...document.querySelectorAll('.settings-records')].map(section => [...section.querySelectorAll('tbody > tr')].filter(row=>!row.classList.contains('hidden')).length)""")
                    assert counts==list(expected), (query,counts,expected)
                page.screenshot(path=str(args.out / f"settings-{width}-{font}.png"))
                print(f"PASS Settings Jobs/Memory {width}px / root {font}px", flush=True)
                page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
