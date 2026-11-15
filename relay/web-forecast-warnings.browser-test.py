#!/usr/bin/env python3
"""Local synthetic forecast responses; no daemon/provider or sends."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--out', type=Path, default=Path('/tmp/formless-forecast-warnings-1560'))
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)

class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def expose(route):
    script = (ROOT / 'web/app.js').read_text()
    end = script.rfind('\n})();')
    assert end > 0
    hook = r'''
    let fixtureRevision1560 = 0;
    window.__forecastRequests1560 = [];
    window.__forecast1560 = payload => {
      $("pair").classList.add("hidden"); $("app").classList.remove("hidden"); setView("chat");
      S.daemonOnline = true; S.current = "fixture"; S.sessions = [{id:"fixture",model:"fixture",cwd:"/fixture"}]; S.hello = true; S.running.clear();
      request = async req => {
        if (req.type !== "forecast_run" || req.for_send) throw new Error("unexpected fixture request");
        window.__forecastRequests1560.push(req);
        return payload;
      };
      $("input").value = "synthetic draft " + (++fixtureRevision1560);
      scheduleForecast();
    };
    window.__invalidateForecast1560 = () => { S.daemonOnline = false; scheduleForecast(); };
'''
    route.fulfill(status=200, content_type='text/javascript', body=script[:end] + hook + script[end:])

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox'])
        for width, font in [(320, 16), (320, 32), (900, 16)]:
            page = browser.new_page(viewport={'width': width, 'height': 1100}, has_touch=width == 320)
            page.set_default_timeout(5000)
            page.route('**/app.js', expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__forecast1560')
            page.evaluate("font => document.documentElement.style.fontSize = font + 'px'", font)
            warnings = ['Budget may be exceeded — ไทย', '<img src=x onerror=alert(1)>', 'Long_' + 'warning' * 50]
            page.evaluate('payload => __forecast1560(payload)', {'hint': 'Estimated $0.04', 'warnings': warnings})
            disclosure = page.locator('#forecast-warnings')
            disclosure.wait_for(state='visible')
            assert page.locator('#forecast-hint').text_content() == 'Estimated $0.04'
            assert page.locator('#forecast-hint').get_attribute('aria-live') == 'polite'
            assert disclosure.evaluate("el => !el.closest('[aria-live]')")
            summary = disclosure.locator('summary')
            assert summary.text_content() == 'Forecast warnings'
            assert not disclosure.evaluate('el => el.open')
            summary.focus()
            summary.press('Enter')
            assert disclosure.evaluate('el => el.open')
            assert disclosure.locator('li').all_text_contents() == warnings
            assert disclosure.locator('img').count() == 0
            summary.press('Space')
            assert not disclosure.evaluate('el => el.open')
            if width == 320:
                summary.tap()
            else:
                summary.click()
            assert disclosure.evaluate('el => el.open')
            dimensions = disclosure.locator('ul').evaluate('el => [el.scrollWidth,el.clientWidth]')
            assert dimensions[0] <= dimensions[1] + 1, dimensions
            page.screenshot(path=str(args.out / f'warnings-{width}-{font}.png'))
            page.evaluate('__invalidateForecast1560()')
            assert not disclosure.is_visible()
            assert disclosure.locator('li').count() == 0
            assert not disclosure.evaluate('el => el.open')
            page.evaluate('payload => __forecast1560(payload)', {'hint': 'No warnings', 'warnings': []})
            page.wait_for_function("document.getElementById('forecast-hint').textContent === 'No warnings'")
            assert not disclosure.is_visible() and disclosure.locator('li').count() == 0
            assert page.evaluate('__forecastRequests1560.length') == 2
            print(f'PASS forecast warning keyboard/touch/text/stale state {width}px / {font}px', flush=True)
            page.close()
        browser.close()
finally:
    server.shutdown()
    server.server_close()
