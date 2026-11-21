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
            page=browser.new_page(viewport={'width':1000,'height':900});page.set_default_timeout(5000)
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
  const fixtureCopies=[];
  Object.defineProperty(navigator,'clipboard',{value:{writeText:async text=>{fixtureCopies.push(text);}}});
  window.__fixture={
    copies:fixtureCopies,
    role(role,content){
      const m={role,content,seq:1,id:'whitespace-fixture'};
      const bubble=h('div',{class:'bubble'},content);
      const node=h('div',{class:'turn '+role},bubble);
      node.style.cssText='position:fixed;top:100px;left:50px;width:700px;z-index:2';document.body.append(node);
      const btn=h('button',{},'Menu');node.append(btn);
      messageMenu(btn,m,node);
      return bubble.innerText;
    },
    setup(){
      $('pair').classList.add('hidden');$('app').classList.remove('hidden');S.current='copy-fixture';S.daemonOnline=true;
      const content='Copy is literal prose.\n\n```js\nconst action = "Copy";\nconsole.log(action);\n```\n\nBetween blocks.\n\n```text\nCopy this code too.\n```\n\nFinal paragraph.';
      const m={role:'assistant',content,seq:0,id:'fixture-message'};
      const node=h('div',{id:'fixture-message'},markdown(content));
      node.style.cssText='position:fixed;top:100px;left:50px;width:700px;z-index:2;background:var(--bg)';document.body.append(node);
      const btn=h('button',{id:'fixture-menu'},'Menu');node.append(btn);
      this.menu=()=>messageMenu(btn,m,node);
      this.content=content;
    }
  };
'''
def check(page):
    page.evaluate('__fixture.setup();__fixture.menu()')
    page.locator('.popover button').filter(has_text='Copy').first.click()
    text=page.evaluate('__fixture.copies[0]')
    assert text.count('Copy')==3, ('only literal Copy occurrences remain',text)
    assert 'Copy is literal prose.' in text and 'Final paragraph.' in text,text
    assert 'const action = "Copy";\nconsole.log(action);' in text,text
    assert '\n\n' in text and 'Between blocks.' in text,text
    page.evaluate('__fixture.menu()')
    page.get_by_role('menuitem',name='Copy as Markdown',exact=True).click()
    assert page.evaluate('__fixture.copies[1] === __fixture.content'), 'Markdown remains original'
    page.locator('#fixture-message .codeblock .copy').first.evaluate('(e)=>e.click()')
    assert page.evaluate('__fixture.copies[2]')=='const action = "Copy";\nconsole.log(action);', 'code copy remains exact'
    assert page.locator('#fixture-message .codeblock .copy').count()==2, 'live controls retained'
    failures=[]
    original='first  line\n  indented second\n\nlast   line'
    for role in ['user','system-note']:
        rendered=page.evaluate('({role,text})=>__fixture.role(role,text)',{'role':role,'text':original})
        assert rendered==original, ('fixture keeps original rendered whitespace',role,rendered)
        page.get_by_role('menuitem',name='Copy',exact=True).click()
        copied=page.evaluate('__fixture.copies.at(-1)')
        if copied!=original: failures.append({'role':role,'expected':original,'copied':copied})
    assert not failures, failures
    print('PASS user/system-note whitespace and plain text excludes generated controls, preserves literal Copy/line breaks; Markdown and code-copy unchanged')
run(Path(__file__).resolve().parent,hook,check,args.app)
