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
parser.add_argument('--app',type=Path,help='Optional original JS for negative control')
args=parser.parse_args()
hook='\nwindow.__fixture={markdown,streamingNode,splitStreaming,S};'
def check(page):
    def render(source):
        return page.evaluate("""source=>{
          const root=__fixture.markdown(source);
          document.body.replaceChildren(root);
          const shape=n=>({tag:n.tagName?.toLowerCase(),text:n.nodeType===3?n.textContent:undefined,children:[...n.childNodes].map(shape)});
          return {top:[...root.children].map(n=>n.tagName.toLowerCase()),
            lists:[...root.querySelectorAll('ul,ol')].map(n=>({tag:n.tagName.toLowerCase(),count:n.children.length,loose:n.classList.contains('loose')})),
            text:root.textContent,shape:shape(root)};
        }""", source)
    for source,tag,count in [
        ('- parent\n - sibling\n- third','ul',3),
        (' - parent\n- sibling','ul',2),
        ('10. parent\n   11. sibling\n12. third','ol',3),
        ('- parent\n\n - sibling\n\n- third','ul',3),
    ]:
        result=render(source)
        assert result['top']==[tag] and result['lists']==[{'tag':tag,'count':count,'loose':'\n\n' in source}],(source,result)
    for source in ['- parent\n  - child\n- sibling','10. parent\n    - child\n11. sibling']:
        result=render(source)
        assert len(result['lists'])==2 and result['lists'][0]['count']==2 and result['lists'][1]['count']==1,(source,result)
    result=render('- parent\n\n  - child a\n\n  - child b\n\n- next')
    assert [(x['count'],x['loose']) for x in result['lists']]==[(2,True),(2,True)],result
    assert render('- ไทย\n continuation 日本語')['text']=='ไทยcontinuation 日本語'
    page.evaluate("""sources => {
      const f=__fixture;
      for (const [index,text] of sources.entries()) {
        f.S.current='list-fixture-'+index; f.S.messages=[];
        for (let end=0;end<=text.length;end++) {
          const prefix=text.slice(0,end), node=f.streamingNode(prefix), [done]=f.splitStreaming(prefix);
          if(node.querySelector('.md.bubble').innerHTML!==f.markdown(done).innerHTML)
            throw new Error('streamed completed list mismatch: '+JSON.stringify(prefix));
        }
      }
    }""", ['Before\n\n- parent\n - sibling\n- third\n\n',
        '10. parent\n   11. sibling\n12. third\n\n',
        '- parent\n\n - sibling\n\n- third\n\n',
        '- parent\n  - child\n - sibling\n\n'])
    print('PASS Chromium: varied-indent siblings, ordered marker width, valid children, loose nested lists, lazy continuation and Unicode')
run(Path(__file__).resolve().parent,hook,check,args.app)
