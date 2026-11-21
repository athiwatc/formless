"""Local shipped-UI fixture support; no daemon, credentials or remote sources."""
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def run(root, hook, check, app=None):
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(root/'web')))
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
            page=browser.new_page(viewport={'width':1000,'height':900})
            def expose(route):
                source=(app or root/'web/app.js').read_text();end=source.rfind('\n})();');assert end>0
                route.fulfill(status=200,content_type='text/javascript',body=source[:end]+hook+source[end:])
            page.route('**/app.js',expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__fixture')
            check(page)
            browser.close()
    finally:
        server.shutdown();server.server_close();thread.join(timeout=5)

import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--app',type=Path,help='Optional baseline JS for negative control')
args=parser.parse_args()
hook=r'''
  window.__fixture={payload,toolCallChip,toolResultChip};
'''
def check(page):
    cases=[
        '{"large":12345678901234567890,"repeat":1,"repeat":2,"decimal":1.0,"exp":1e2}',
        '{\n  "text": "ไทย 😀",\n  "spaces": [1,  2]\n}\n',
        '12345678901234567890',
        '[1.0,1e2,-0,1e999]',
        '{"markup":"<img src=x onerror=alert(1)>"}',
        '{ invalid JSON\nkept exactly',
        'not JSON\n\nordinary text\n',
        '',
    ]
    for value in cases:
        actual=page.evaluate('text=>{const p=__fixture.payload(text);document.body.append(p);const result=p.textContent;p.remove();return result;}',value)
        assert actual==value, ('payload must preserve all original tokens/whitespace',value,actual)
    sample=cases[0]
    for kind in ['call','result']:
        data=page.evaluate('''({kind,text})=>{
          const chip=kind==='call'?__fixture.toolCallChip({function:{name:'inert_fixture',arguments:text}}):__fixture.toolResultChip(text);
          chip.open=true;document.body.append(chip);
          const result={text:chip.querySelector('pre').textContent,highlighted:!!chip.querySelector('pre span')};chip.remove();return result;
        }''',{'kind':kind,'text':sample})
        assert data['text']==sample and data['highlighted'],data
    assert page.locator('img[src="x"]').count()==0, 'payload markup remains text'
    print('PASS original numeric spellings, repeated keys, Unicode, whitespace, invalid/plain text; argument/result chip highlighting')
run(Path(__file__).resolve().parent,hook,check,args.app)
