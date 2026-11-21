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
hook='\nwindow.__fixture={inline,h};'
def render(page,text):
    return page.evaluate('''text=>{
      const node=__fixture.h('div',null,__fixture.inline(text));
      return {text:node.textContent,em:[...node.querySelectorAll('em')].map(e=>e.textContent),strong:[...node.querySelectorAll('strong')].map(e=>e.textContent)};
    }''',text)
def check(page):
    for delimiter,kind in [('*','em'),('**','strong')]:
        for count in range(1,7):
            source=delimiter+'ab'+'\\'*count+delimiter+'cd'+delimiter
            expected='ab'+'\\'*(count//2)+(delimiter+'cd' if count%2 else '')
            actual=render(page,source)
            assert actual[kind]==[expected],(source,expected,actual)
            assert actual['text']==expected+('cd'+delimiter if count%2==0 else ''),(source,actual)
    assert render(page,r'\*word\*')=={'text':'*word*','em':[],'strong':[]}
    assert render(page,r'\*\*word\*\*')['text']=='**word**'
    nested=render(page,'*outer **bold** tail*')
    assert nested=={'text':'outer bold tail','em':['outer bold tail'],'strong':['bold']},nested
    nested=render(page,'**outer *italic* tail**')
    assert nested=={'text':'outer italic tail','em':['italic'],'strong':['outer italic tail']},nested
    assert render(page,'*a* **b** ไทย')['text']=='a b ไทย'
    print('PASS odd/even escape parity 1–6 for emphasis/strong, escaped opening, nested formatting and Unicode')
run(Path(__file__).resolve().parent,hook,check,args.app)
