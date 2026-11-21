"""Exact context labels through the shipped model picker; local fixture replies."""
import argparse
import functools
import http.server
import json
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

ROOT = Path(__file__).resolve().parent
parser = argparse.ArgumentParser()
parser.add_argument('--app', type=Path)
args = parser.parse_args()
cases = json.loads((ROOT/'../crates/formless-tui/tests/fixtures/model-context-labels.json').read_text())
models = [f'model-{i:03}' for i in range(len(cases))]
payload = {'models':models, 'meta':{model:{'context':case[0]} for model,case in zip(models,cases)}}
hook = '\nconst payload2060='+json.dumps(payload)+';\n'+r'''
  act = async req => req.type === 'list_models' ? payload2060 : null;
  window.__fixture = { start() {
    $('pair').classList.add('hidden'); $('app').classList.remove('hidden');
    S.sessions=[{id:'fixture',model:''}]; S.current='fixture';
    void openModelPicker();
  }};
'''
class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass
server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(ROOT/'web')))
thread = threading.Thread(target=server.serve_forever,daemon=True)
thread.start()
try:
    with sync_playwright() as p:
        browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
        page=browser.new_page(viewport={'width':1000,'height':900})
        errors=[]
        page.on('pageerror',lambda e:errors.append(str(e)))
        def expose(route):
            source=(args.app or ROOT/'web/app.js').read_text()
            end=source.rfind('\n})();')
            assert end>0
            route.fulfill(status=200,content_type='text/javascript',body=source[:end]+hook+source[end:])
        page.route('**/app.js',expose)
        page.set_default_timeout(5000)
        page.goto(f'http://127.0.0.1:{server.server_port}/')
        page.wait_for_function('!!window.__fixture')
        page.evaluate('__fixture.start()')
        page.wait_for_function('document.querySelectorAll(".picker-list [role=option]").length===20')
        for model,(_,expected) in zip(models,cases):
            row=page.locator('.picker-list [role=option]').filter(has=page.locator('.picker-id',has_text=model))
            assert row.locator('.muted').inner_text()==expected,(model,expected,row.inner_text())
        page.get_by_role('combobox',name='Filter models',exact=True).fill(models[0])
        page.wait_for_function('document.querySelectorAll(".picker-list [role=option]").length===1')
        assert page.locator('.picker-list .muted').inner_text()==cases[0][1]
        page.get_by_role('button',name='Cancel',exact=True).click()
        assert not errors,errors
        print('PASS 20 shared exact context labels in real model rows; filtered row retains its label; no browser errors')
        browser.close()
finally:
    server.shutdown();server.server_close();thread.join(timeout=5)
