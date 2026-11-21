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
hook='\nwindow.__fixture={inline,markdown,h};'
def render(page, text, kind):
    return page.evaluate('''({text,kind})=>{
      const node=kind==='markdown'?__fixture.markdown(text):__fixture.h('div',null,__fixture.inline(text));
      return {text:node.textContent,links:[...node.querySelectorAll('a')].map(a=>({text:a.textContent,href:a.getAttribute('href'),target:a.target,rel:a.rel})),code:[...node.querySelectorAll('code')].map(c=>c.textContent)};
    }''',{'text':text,'kind':kind})
def check(page):
    cases=[
      ('See HTTPS://docs.example.com/guide and HTTP://127.0.0.1:8080/health.',
       ['HTTPS://docs.example.com/guide','HTTP://127.0.0.1:8080/health']),
      ('ไทย HtTpS://Case.Example/A%2Fb?Token=AbC#Frag, then http://a.',
       ['HtTpS://Case.Example/A%2Fb?Token=AbC#Frag','http://a']),
      ('<HTTPS://Case.Example/A%2Fb?Token=AbC#Frag> and <MAILTO:Me@Example.COM>',
       ['HTTPS://Case.Example/A%2Fb?Token=AbC#Frag','MAILTO:Me@Example.COM']),
      ('<hTtP://127.0.0.1:8080/health> and <mAiLtO:me@example.com>',
       ['hTtP://127.0.0.1:8080/health','mAiLtO:me@example.com']),
      ('[docs](HTTPS://Case.Example/Guide)', ['HTTPS://Case.Example/Guide']),
    ]
    count=0
    for kind in ['inline','markdown']:
      for source,expected in cases:
        actual=render(page,source,kind)
        assert [a['href'] for a in actual['links']]==expected,(kind,source,actual)
        assert all(a['target']=='_blank' and 'noopener' in a['rel'].split() and 'noreferrer' in a['rel'].split() for a in actual['links']),actual
        if source.startswith('See ') or source.startswith('ไทย '): assert actual['text']==source,(kind,actual)
        if 'MAILTO:' in source: assert 'Me@Example.COM' in actual['text'] and 'MAILTO:' not in actual['text'],actual
        count+=1
      for unsafe in ['<JAVASCRIPT:alert(1)>','<DATA:text/html,fixture>','<FTP://example.test/file>','[unsafe](JaVaScRiPt:fixture)']:
        assert render(page,unsafe,kind)['links']==[],(kind,unsafe)
        count+=1
      code=render(page,'`HTTPS://docs.example.com/guide`',kind)
      assert code['links']==[] and code['code']==['HTTPS://docs.example.com/guide'],code
      count+=1
    print(f'PASS {count} shipped inline/Markdown browser cases: scheme case, punctuation, Unicode/URL bytes, mail labels, explicit links, safe targets and literal code')
run(Path(__file__).resolve().parent,hook,check,args.app)
