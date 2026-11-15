"""Real Chromium role-editor controls; all saves use local fixture replies."""
import argparse
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--app', type=Path, help='Optional original app for negative control')
args = parser.parse_args()

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

hook = r'''
  const calls = [];
  act = async req => { calls.push(structuredClone(req)); return {}; };
  refreshSettings = async () => {};
  window.__fixture = { calls, start(effort) {
    $('pair').classList.add('hidden'); $('app').classList.remove('hidden');
    void editRole('fast', { provider:'local', model:'fixture-model', description:'Original',
      reasoning_effort:effort, system:'fixture instructions', candidates:['one','two'], max_turns:3 },
      ['default','local']);
  }};
'''
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Quiet, directory=str(ROOT/'web')))
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser = p.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox'])
        page = browser.new_page(viewport={'width':1000,'height':900})
        errors = []
        page.on('pageerror', lambda e: errors.append(str(e)))
        def expose(route):
            source = (args.app or ROOT/'web/app.js').read_text()
            end = source.rfind('\n})();')
            assert end > 0
            route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hook+source[end:])
        page.route('**/app.js', expose)
        page.set_default_timeout(5000)
        page.goto(f'http://127.0.0.1:{server.server_port}/')
        page.wait_for_function('!!window.__fixture')
        def start(current):
            page.evaluate('e=>__fixture.start(e)', current)
            page.locator('#ask-body label').filter(has_text='Reasoning effort').locator('select').wait_for()
        def save(expected):
            before = page.evaluate('__fixture.calls.length')
            page.get_by_label('Description', exact=True).fill('Changed description')
            page.locator('#ask-ok').click()
            page.wait_for_function('n=>__fixture.calls.length===n+1', arg=before)
            request = page.evaluate('__fixture.calls.at(-1)')
            assert request == {'type':'upsert_role','name':'fast','role':{
                'provider':'local','model':'fixture-model','description':'Changed description',
                'reasoning_effort':expected,'system':'fixture instructions','candidates':['one','two'],'max_turns':3}}, request
        for effort in ['minimal','low','medium','high',None]:
            start(effort)
            assert page.locator('#ask-body label').filter(has_text='Reasoning effort').locator('select').input_value() == (effort or ''), ('stored effort selected',effort)
            save(effort)
            print('PASS description-only save preserves effort', effort)
        start(None)
        page.locator('#ask-body label').filter(has_text='Reasoning effort').locator('select').select_option('minimal')
        save('minimal')
        print('PASS select minimal from default')
        start('minimal')
        page.locator('#ask-body label').filter(has_text='Reasoning effort').locator('select').select_option('')
        save(None)
        print('PASS explicit default clears effort')
        start('minimal')
        before = page.evaluate('__fixture.calls.length')
        page.locator('#ask-cancel').click()
        assert page.evaluate('__fixture.calls.length') == before
        assert not errors, errors
        print('PASS cancel makes no update; no browser errors')
        browser.close()
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)
