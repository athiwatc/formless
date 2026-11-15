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
parser=argparse.ArgumentParser()
parser.add_argument('--app',type=Path,help='Optional baseline JS for negative control')
args=parser.parse_args()
hook=r'''
  const fixturePending=[];
  request=async()=>({});
  act=async req=>req.type==='get_session'?new Promise(resolve=>fixturePending.push({id:req.id,resolve})):null;
  window.__fixture={
    setup(){ $('pair').classList.add('hidden');$('app').classList.remove('hidden');S.view='chat';S.daemonOnline=true;
      S.sessions=['A','B'].map(id=>({id,name:'Fixture '+id,updated_at:'2026-11-01T00:00:00Z'})); },
    start(id){void openSession(id);},
    reply(index,text){const p=fixturePending.splice(index,1)[0];p.resolve(text===null?null:{
      session:S.sessions.find(s=>s.id===p.id),messages:text?[{id:'stored-'+p.id,session_id:p.id,seq:0,role:'assistant',content:text,meta:'{}'}]:[],queued_items:[]});},
    repaint(){renderHistory();},
    live(text){onEvent({type:'message_appended',session_id:S.current,message:{id:'live-fixture',session_id:S.current,seq:1,role:'assistant',content:text,meta:'{}'}});},
    disconnect(){clearLiveState();renderHistory();}
  };
'''
def check(page):
    page.set_default_timeout(5000)
    page.evaluate('__fixture.setup();__fixture.start("A")')
    assert page.locator('#transcript .welcome').count()==0, 'pending snapshot must not show empty-session welcome'
    notice=page.locator('[data-session-load="pending"]')
    assert notice.inner_text()=='Loading transcript…'
    assert notice.get_attribute('role')=='status'
    page.evaluate('window.pendingNotice=document.querySelector("[data-session-load]");__fixture.repaint();__fixture.repaint()')
    assert page.evaluate('pendingNotice===document.querySelector("[data-session-load]")'), 'repaint retains status node'
    page.evaluate('__fixture.reply(0,null)')
    page.wait_for_function('!!document.querySelector("[data-session-load=failed]")')
    page.get_by_role('button',name='Retry loading transcript',exact=True).click()
    page.evaluate('__fixture.reply(0,"Stored transcript arrived")')
    page.wait_for_function('document.querySelector("#transcript").textContent.includes("Stored transcript arrived")')
    assert page.locator('[data-session-load]').count()==0
    page.evaluate('__fixture.start("B")')
    assert 'Stored transcript arrived' not in page.locator('#transcript').inner_text(), 'previous Session not shown under new identity'
    page.evaluate('__fixture.reply(0,"")')
    page.wait_for_function('!!document.querySelector("#transcript .welcome")')
    page.evaluate('__fixture.start("A");__fixture.start("B");__fixture.reply(0,null)')
    assert page.locator('[data-session-load="pending"]').count()==1, 'stale A failure does not replace B pending'
    page.evaluate('__fixture.reply(0,"Current B")')
    page.wait_for_function('document.querySelector("#transcript").textContent.includes("Current B")')
    page.evaluate('__fixture.start("A");__fixture.live("Live event during load");__fixture.reply(0,"Stored snapshot")')
    page.wait_for_function('document.querySelector("#transcript").textContent.includes("Stored snapshot")')
    assert 'Live event during load' in page.locator('#transcript').inner_text(), 'snapshot merges live messages'
    page.evaluate('__fixture.start("A");__fixture.disconnect();__fixture.reply(0,"Invalidated reply")')
    assert page.locator('[data-session-load="failed"]').count()==1, 'invalidated pending load becomes retryable'
    assert 'Invalidated reply' not in page.locator('#transcript').inner_text()
    print('PASS pending/failure/Retry/success/empty, stable notices, navigation, stale reply, live merge and disconnect invalidation')
run(Path(__file__).resolve().parent,hook,check,args.app)
