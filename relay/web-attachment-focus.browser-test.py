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
parser.add_argument('--app',type=Path)
args=parser.parse_args()
hook=r"""
 const pendingRemovals=[];
 request=async req=>req.type==='list_pending_attachments'?{attachments:S.chips[req.session_id]||[]}:null;
 act=async req=>req.type==='detach_file'?new Promise(resolve=>pendingRemovals.push({req,resolve})):null;
 window.__fixture={
  setup(count){$('pair').classList.add('hidden');$('app').classList.remove('hidden');S.view='chat';S.current='fixture';S.storageScope='fixture';S.channel={};S.daemonOnline=true;
    S.chips.fixture=Array.from({length:count},(_,i)=>({id:'chip-'+i,name:'file '+i,mime:'text/plain',bytes:10}));S.attachmentInventories.fixture=S.chips.fixture;renderChrome();renderChips();},
  pending(){return pendingRemovals.map(x=>x.req);},
  reference(){S.chips.fixture=[{path:"/fixture/reference.txt",name:"reference",mime:"text/plain",bytes:1}];renderChips();},
  resolve(ok){const pending=pendingRemovals.shift();if(!pending)throw new Error('no pending removal');if(ok){S.chips.fixture=S.chips.fixture.filter(a=>(a.id||a.path)!==pending.req.id);S.attachmentInventories.fixture=S.chips.fixture;}pending.resolve(ok);},
  ids(){return (S.chips.fixture||[]).map(x=>x.id);},
 };
"""
def check(page):
    for count,index,expected in [(3,0,'Remove file 1'),(3,1,'Remove file 2'),(3,2,'Remove file 1'),(1,0,None)]:
        page.evaluate('__fixture.setup',count)
        button=page.get_by_role('button',name='Remove file '+str(index),exact=True)
        button.focus();button.press('Enter')
        assert page.evaluate('__fixture.pending()')==[{'type':'detach_file','session_id':'fixture','id':'chip-'+str(index)}]
        page.evaluate('__fixture.resolve(true)')
        page.wait_for_function('expected => __fixture.ids().length===expected',arg=count-1)
        page.wait_for_timeout(30)
        active=page.evaluate('({id:document.activeElement.id,label:document.activeElement.getAttribute("aria-label")})')
        assert (active['label']==expected if expected else active['id']=='input'),(count,index,active)
        assert page.evaluate('__fixture.ids()')==['chip-'+str(i) for i in range(count) if i!=index]
    # A focus move during the asynchronous operation is intentional.
    page.evaluate('__fixture.setup(3)')
    button=page.get_by_role('button',name='Remove file 0',exact=True)
    button.focus();button.press('Enter');page.locator('#input').focus()
    page.evaluate('__fixture.resolve(true)')
    page.wait_for_function('__fixture.ids().length===2')
    assert page.locator('#input').evaluate('e=>e===document.activeElement')
    page.evaluate('__fixture.setup(3)')
    button=page.get_by_role('button',name='Remove file 0',exact=True)
    button.focus();button.press('Enter')
    page.get_by_role('button',name='Remove file 2',exact=True).focus()
    page.evaluate('__fixture.resolve(true)');page.wait_for_function('__fixture.ids().length===2')
    assert page.get_by_role('button',name='Remove file 2',exact=True).evaluate('e=>e===document.activeElement')
    page.evaluate('__fixture.setup(1)')
    button=page.get_by_role('button',name='Remove file 0',exact=True)
    button.focus();button.press('Enter');page.evaluate('__fixture.resolve(false)')
    page.wait_for_timeout(30)
    assert button.evaluate('e=>e===document.activeElement') and page.evaluate('__fixture.ids().length')==1
    page.evaluate('__fixture.setup(0)');page.evaluate('__fixture.reference()')
    button=page.get_by_role('button',name='Remove reference',exact=True)
    button.focus();button.press('Enter')
    assert page.evaluate('__fixture.pending()')==[{'type':'detach_file','session_id':'fixture','id':'/fixture/reference.txt'}]
    page.evaluate('__fixture.resolve(true)');page.wait_for_function('__fixture.ids().length===0')
    assert page.locator('#input').evaluate('e=>e===document.activeElement')
    print('PASS Chromium: first/middle/last/final removal, labels/data, async outside/other-chip focus, failed removal')
run(Path(__file__).resolve().parent,hook,check,args.app)
