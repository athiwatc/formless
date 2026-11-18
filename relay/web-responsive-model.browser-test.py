"""Exercise the single model control through real responsive layouts."""
import functools
import http.server
from pathlib import Path
import threading

from playwright.sync_api import expect, sync_playwright

from browser_fixture import chromium_executable


ROOT = Path(__file__).resolve().parent
OVERRIDE = "local/" + "long-model-name-" * 10 + "fixture"
DEFAULT = "default/model"
HOOK = """
  const modelFixtureCalls = [];
  act = async req => {
    modelFixtureCalls.push(req.type);
    return req.type === 'list_models' ? {models: ['fixture-model']} : null;
  };
  window.__modelFixture = {
    calls: modelFixtureCalls,
    show() {
      $('pair').classList.add('hidden');
      $('app').classList.remove('hidden');
      S.view = 'chat';
      S.daemonOnline = true;
    },
    select({override, fallback}) {
      S.current = 'selected';
      S.sessions = [{id: 'selected', name: 'Selected Session', model: override}];
      S.settings = {provider: {model: fallback}};
      renderStatus();
    },
    setReading,
  };
"""


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def serve_app(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + HOOK + source[end:])


def start(page, url):
    page.route('**/app.js', serve_app)
    page.set_default_timeout(5000)
    page.goto(url)
    page.wait_for_function('!!window.__modelFixture')
    page.evaluate('__modelFixture.show()')
    page.evaluate('__modelFixture.select', {'override': OVERRIDE, 'fallback': DEFAULT})


def model_picker(page):
    expect(page.locator('#status-model')).to_have_count(1)
    button = page.get_by_role('button', name=f'Change model ({OVERRIDE})', exact=True)
    expect(button).to_have_count(1)
    expect(button).to_be_visible()
    return button


def main():
    handler = functools.partial(QuietHandler, directory=str(ROOT / 'web'))
    with http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f'http://127.0.0.1:{server.server_port}/'
        try:
            with sync_playwright() as p:
                browser = p.chromium.launch(executable_path=chromium_executable(p), args=['--no-sandbox'])
                page = browser.new_page(viewport={'width': 1200, 'height': 900})
                start(page, url)
                menu = page.locator('#menu-btn')
                expect(menu).to_be_hidden()
                button = model_picker(page)
                page.locator('#input').focus()
                for _ in range(30):
                    page.keyboard.press('Tab')
                    if page.evaluate('document.activeElement.id === "status-model"'):
                        break
                expect(button).to_be_focused()

                page.evaluate('__modelFixture.setReading(0.85)')
                page.set_viewport_size({'width': 720, 'height': 900})
                assert page.evaluate('matchMedia("(max-width: 51.25rem)").matches')
                assert page.evaluate('''() => document.querySelector('#app').clientWidth
                    > 51.25 * parseFloat(getComputedStyle(document.documentElement).fontSize)''')
                expect(menu).to_be_visible()
                expect(button).to_be_focused()

                page.set_viewport_size({'width': 1200, 'height': 900})
                expect(menu).to_be_hidden()
                page.evaluate('__modelFixture.setReading(2)')
                expect(menu).to_be_visible()
                assert not page.evaluate('matchMedia("(max-width: 51.25rem)").matches')
                expect(button).to_be_focused()
                assert button.evaluate('(el) => el.scrollWidth > el.clientWidth')

                button.press('Enter')
                picker = page.get_by_role('dialog', name='Model', exact=True)
                expect(picker.get_by_text(f'Current selection: {OVERRIDE}', exact=True)).to_be_visible()
                expect(picker.get_by_role('textbox', name='Filter models')).to_be_focused()
                page.keyboard.press('Escape')
                expect(picker).to_have_count(0)
                expect(button).to_be_focused()

                page.evaluate('__modelFixture.select', {'override': '', 'fallback': DEFAULT})
                expect(page.get_by_role('button', name=f'Change model ({DEFAULT})')).to_be_focused()
                page.evaluate('__modelFixture.select', {'override': '', 'fallback': ''})
                empty = page.get_by_role('button', name='Model unavailable. Choose model.', exact=True)
                expect(empty).to_have_text('Model unavailable')
                empty.press('Space')
                expect(picker.get_by_text('Current model unavailable', exact=True)).to_be_visible()
                assert page.evaluate('__modelFixture.calls') == ['list_models', 'list_models']
                page.close()

                touch = browser.new_page(viewport={'width': 390, 'height': 844}, is_mobile=True, has_touch=True)
                start(touch, url)
                target = model_picker(touch)
                bounds = target.bounding_box()
                assert bounds and bounds['width'] >= 24 and bounds['height'] >= 24, bounds
                target.tap()
                expect(touch.get_by_text(f'Current selection: {OVERRIDE}', exact=True)).to_be_visible()
                assert touch.evaluate('__modelFixture.calls') == ['list_models']
                browser.close()
                print('PASS model picker visibility, keyboard focus, full model text, and touch activation')
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == '__main__':
    main()
