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
  const fixturePending=[];
  act=async req=>req.type==='get_tool'?new Promise(resolve=>fixturePending.push({id:req.id,resolve})):null;
  const fixtureTool=id=>({id,name:'Fixture '+id,description:'inert fixture',current_version:1,invocations:0,
    capabilities:[],memory_mb:16,tags:[],updated_at:'2026-11-01T00:00:00Z'});
  window.__fixture={
    setup(){ $('pair').classList.add('hidden');$('app').classList.remove('hidden');
      S.tools=['A','B'].map(fixtureTool);S.daemonOnline=true;S.modules=[];S.modelStats=[]; },
    start(view,id){S.view=view;if(view==='metrics')S.metricTool=id;renderChrome();void selectTool(id,true);},
    reply(index,ok){const p=fixturePending.splice(index,1)[0];p.resolve(ok?{tool:fixtureTool(p.id),versions:[],metrics:{},uses:[]}:null);},
    pending(){return fixturePending.map(p=>p.id);},
    selected(){return S.selectedTool;}
  };
'''
def check(page):
    page.set_default_timeout(5000)
    page.evaluate('__fixture.setup();__fixture.start("tools","A")')
    assert 'Loading Tool details' in page.locator('#tool-detail').inner_text()
    page.evaluate('__fixture.reply(0,false)')
    page.wait_for_function('document.querySelector("#tool-detail").textContent.includes("Could not load Tool details")')
    page.locator('#tool-detail').get_by_role('button',name='Retry',exact=True).click()
    assert page.evaluate('__fixture.pending()')==['A']
    assert 'Could not load' not in page.locator('#tool-detail').inner_text()
    page.evaluate('__fixture.reply(0,true)')
    page.wait_for_function('document.querySelector("#tool-detail h3")?.textContent.includes("Fixture A")')
    page.evaluate('__fixture.start("tools","A");__fixture.reply(0,false)')
    assert 'Fixture A' in page.locator('#tool-detail').inner_text(), 'cached detail retained after failed refresh'
    assert page.locator('#tool-detail').get_by_role('button',name='Retry',exact=True).count()==0
    page.evaluate('__fixture.start("metrics","B");__fixture.reply(0,false)')
    page.wait_for_function('document.querySelector("#metric-detail").textContent.includes("Could not load Tool details")')
    page.locator('#metric-detail').get_by_role('button',name='Retry',exact=True).click()
    assert page.evaluate('__fixture.pending()')==['B']
    page.evaluate('__fixture.reply(0,true)')
    page.wait_for_function('document.querySelector("#metric-detail h3")?.textContent.includes("Fixture B")')
    page.evaluate('__fixture.start("tools","A");__fixture.start("tools","B");__fixture.reply(0,false)')
    assert page.evaluate('__fixture.selected()')=='B'
    assert 'Could not load' not in page.locator('#tool-detail').inner_text(), 'late failure cannot replace newer selection'
    page.evaluate('__fixture.reply(0,true)')
    page.wait_for_function('document.querySelector("#tool-detail h3")?.textContent.includes("Fixture B")')
    page.evaluate('__fixture.start("tools","A");__fixture.reply(0,false)')
    page.wait_for_function('document.querySelector("#tool-detail").textContent.includes("Could not load Tool details")')
    page.evaluate('__fixture.start("tools","B")')
    assert 'Could not load' not in page.locator('#tool-detail').inner_text(), 'selection change clears prior failure'
    page.evaluate('__fixture.reply(0,true)')
    page.evaluate('__fixture.start("tools","A");__fixture.start("tools","A");__fixture.reply(1,true)')
    page.wait_for_function('document.querySelector("#tool-detail h3")?.textContent.includes("Fixture A")')
    page.evaluate('__fixture.reply(0,false)')
    assert 'Could not load' not in page.locator('#tool-detail').inner_text(), 'late same-id failure cannot replace newer success'
    print('PASS Tools/Metrics failed first load, native Retry, successful replacement, cached refresh, stale/same-id completions and selection change')
run(Path(__file__).resolve().parent,hook,check,args.app)
