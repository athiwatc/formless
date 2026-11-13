#!/usr/bin/env python3
"""Issue #1715: actual CSS drawer visibility and keyboard focus in Chromium.

Run with Python Playwright and /usr/bin/chromium. Local UI fixtures only;
no daemon/provider calls. This does not claim screen-reader or WebKit coverage.
"""
import functools
import http.server
import pathlib
import threading
from playwright.sync_api import sync_playwright

ROOT = pathlib.Path(__file__).resolve().parent


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '''
    window.__sidebar={S,renderChrome,openSidebar,closeSidebar,setView,COMMANDS};
    act=async()=>[];request=async()=>[];
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])


def state(page, expanded, focus=None):
    page.wait_for_function('(expected)=>document.querySelector("#menu-btn").getAttribute("aria-expanded")===String(expected)', arg=expanded)
    assert page.locator('#menu-btn').get_attribute('aria-controls') == 'sidebar'
    visible = page.locator('#sidebar').evaluate('e=>!!e.getClientRects().length&&getComputedStyle(e).visibility!=="hidden"')
    assert visible == expanded
    if focus:
        assert page.evaluate('document.activeElement.id') == focus, page.evaluate('document.activeElement.id')
    assert not page.locator('#app').evaluate('e=>e.inert')
    assert page.locator('#sidebar').get_attribute('role') != 'dialog'


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with sync_playwright() as pw:
        browser = pw.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox'])
        for width, root_size, container in [(390, 16, None), (1280, 32, None), (1280, 16, 600)]:
            page = browser.new_page(viewport={'width': width, 'height': 900})
            page.route('**/app.js*', expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.evaluate('''([size,container])=>{
              document.documentElement.style.fontSize=size+'px';
              if(container)document.querySelector('#app').style.width=container+'px';
              document.querySelector('#pair').classList.add('hidden');
              document.querySelector('#app').classList.remove('hidden');
              __sidebar.S.view='chat';__sidebar.renderChrome();
            }''', [root_size, container])
            state(page, False)
            page.locator('#menu-btn').focus()
            page.keyboard.press('Enter')
            state(page, True, 'new-session')
            # A nonmodal drawer permits intentional focus outside itself.
            page.locator('#input').focus()
            assert page.evaluate('document.activeElement.id') == 'input'
            page.locator('#search').focus()
            page.keyboard.press('Escape')
            state(page, False, 'menu-btn')
            page.evaluate('__sidebar.COMMANDS.find(c=>c.name==="search").run("")')
            state(page, True, 'search')
            page.locator('#theme').click()
            state(page, False, 'theme')
            page.evaluate('__sidebar.openSidebar()')
            page.locator('#search').focus()
            page.evaluate('__sidebar.setView("tools")')
            state(page, False)
            assert page.evaluate('document.activeElement.id') != 'search'
            assert page.evaluate('getComputedStyle(document.activeElement).visibility!=="hidden"&&document.activeElement.getClientRects().length>0')
            page.evaluate('__sidebar.setView("chat")')
            state(page, True)
            page.evaluate('__sidebar.closeSidebar()')
            state(page, False)
            print(f'PASS overlay {width}px root {root_size}px container {container}', flush=True)
            page.close()

        page = browser.new_page(viewport={'width': 1280, 'height': 900})
        page.route('**/app.js*', expose)
        page.goto(f'http://127.0.0.1:{server.server_port}/')
        page.evaluate('''()=>{
          document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');
          __sidebar.S.view='chat';__sidebar.renderChrome();
        }''')
        state(page, True)
        page.locator('#search').focus()
        page.set_viewport_size({'width': 390, 'height': 900})
        state(page, False, 'menu-btn')
        page.keyboard.press('Enter')
        state(page, True, 'new-session')
        page.keyboard.press('Escape')
        state(page, False, 'menu-btn')
        page.set_viewport_size({'width': 1280, 'height': 900})
        state(page, True, 'new-session')
        # Reading-size changes trigger container CSS without viewport resizing.
        page.locator('#search').focus()
        page.evaluate('document.documentElement.style.fontSize="32px"')
        state(page, False, 'menu-btn')
        page.evaluate('document.documentElement.style.fontSize="16px"')
        state(page, True, 'new-session')
        print('PASS desktop, viewport and reading-size transitions', flush=True)
        browser.close()
finally:
    server.shutdown()
