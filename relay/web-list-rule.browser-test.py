"""Local shipped-UI fixture support; no daemon, credentials or remote sources."""
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def run(root, hook, check, app=None):
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(root/'web')))
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path='/usr/bin/chromium',headless=True,args=['--no-sandbox'])
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
import json
parser=argparse.ArgumentParser()
parser.add_argument('--app',type=Path)
args=parser.parse_args()
hook='\nwindow.__fixture={markdown,streamingNode,splitStreaming,S};'
def check(page):
    def render(source):
        return page.evaluate('''source=>{
          const root=__fixture.markdown(source);
          return {top:[...root.children].map(n=>n.tagName.toLowerCase()),
            lists:[...root.querySelectorAll('ul,ol')].map(n=>n.children.length),
            text:root.textContent,rules:root.querySelectorAll('hr').length};
        }''',source)
    sources=['- item\n * * *','- item\n - - -','- item\n - sibling\n- third','- item\n  * * *\n - sibling']
    results=[render(source) for source in sources]
    print(json.dumps(list(zip(sources,results)),ensure_ascii=False))
    for result in results[:2]:
        assert result['top']==['ul','hr'] and result['lists']==[1] and result['rules']==1,result
    assert results[2]['top']==['ul'] and results[2]['lists']==[3] and results[2]['rules']==0,results[2]
    assert results[3]['top']==['ul'] and results[3]['lists']==[2] and results[3]['rules']==1,results[3]
    page.evaluate(r'''sources=>{
      const f=__fixture;
      for (const [index,source] of sources.entries()) {
        f.S.current='rule-fixture-'+index;f.S.messages=[];
        const text=source+'\n\n';
        for(let end=0;end<=text.length;end++) {
          const prefix=text.slice(0,end),node=f.streamingNode(prefix),[done]=f.splitStreaming(prefix);
          if(node.querySelector('.md.bubble').innerHTML!==f.markdown(done).innerHTML)
            throw new Error('completed streaming rule mismatch '+JSON.stringify(prefix));
        }
      }
    }''',sources)
    print('PASS Chromium: both reported rules, mixed-indent siblings, nested rule and production streaming prefixes')
run(Path(__file__).resolve().parent,hook,check,args.app)
