#!/usr/bin/env python3
"""Width-only composer regressions with shipped app/CSS and local inert draft."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

root = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--app', type=Path, help='Optional baseline JS for negative control')
args = parser.parse_args()

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(root / 'web')))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()

def expose(route):
    source = (args.app or root / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '''
      let fixtureAutosizes = 0;
      const fixtureOriginalBounds = viewportBounds;
      let fixtureZoom = null;
      viewportBounds = () => fixtureZoom || fixtureOriginalBounds();
      const fixtureOriginalAutosize = autosize;
      autosize = () => { fixtureAutosizes++; fixtureOriginalAutosize(); };
      window.__widthFixture = {
        setup() {
          $("pair").classList.add("hidden"); $("app").classList.remove("hidden");
          S.current = "width-fixture"; S.daemonOnline = true; setView("chat");
          $("input").value = "Wrapping draft content with enough words. ".repeat(10);
          autosize(); scheduleViewportSync();
        },
        measure() { const t=$("input"); return {height:t.getBoundingClientRect().height,
          width:t.getBoundingClientRect().width,max:getComputedStyle(t).maxHeight,
          limit:document.documentElement.style.getPropertyValue("--composer-input-limit"),calls:fixtureAutosizes}; },
        width(value) { $("app").style.width = value + "px"; },
        zoom(on) {
          fixtureZoom = on ? {left:20,top:30,right:520,bottom:730,width:500,height:700,scale:1.5} : null;
          scheduleViewportSync();
        },
        longDraft() { $("input").value = "long wrapping draft ".repeat(500); autosize(); }
      };
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])

try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'])
        page = browser.new_page(viewport={'width':1100,'height':1000})
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.route('**/app.js', expose)
        page.goto(f'http://127.0.0.1:{server.server_port}/')
        page.wait_for_function('!!window.__widthFixture')
        page.evaluate('__widthFixture.setup()')
        page.wait_for_timeout(200)
        wide = page.evaluate('__widthFixture.measure()')
        page.evaluate('__widthFixture.width(540)')
        page.wait_for_timeout(200)
        narrow = page.evaluate('__widthFixture.measure()')
        assert narrow['limit'] == wide['limit'] and narrow['max'] == wide['max'], (wide,narrow)
        assert narrow['width'] < wide['width'] and narrow['height'] > wide['height'], ('narrowing resizes without typing',wide,narrow)
        page.evaluate('__widthFixture.width(1000)')
        page.wait_for_timeout(200)
        wider = page.evaluate('__widthFixture.measure()')
        assert wider['height'] < narrow['height'], ('widening shrinks without typing',narrow,wider)
        page.evaluate('__widthFixture.longDraft()')
        page.wait_for_timeout(200)
        capped = page.evaluate('__widthFixture.measure()')
        assert capped['height'] <= float(capped['max'].removesuffix('px')) + 2, capped
        page.evaluate('__widthFixture.width(540)')
        page.wait_for_timeout(200)
        stable = page.evaluate('__widthFixture.measure()')
        page.wait_for_timeout(250)
        assert page.evaluate('__widthFixture.measure().calls') == stable['calls'], 'no continuing resize feedback'
        page.evaluate('__widthFixture.zoom(true)')
        page.wait_for_timeout(200)
        assert page.locator('#composer').evaluate('e => e.style.width') == '484px', 'zoom uses bounded floating width'
        page.evaluate('__widthFixture.zoom(false)')
        page.wait_for_timeout(200)
        assert page.locator('#composer').evaluate('e => e.style.width') == '', 'normal mode clears floating width'
        assert not errors, errors
        print('PASS observed width-only narrowing/widening, unchanged limits, max height, settled resize frames and simulated zoom transitions')
        print({'wide':wide,'narrow':narrow,'wider':wider,'capped':capped,'stable':stable})
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
