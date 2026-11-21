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
  let fixtureOnline=true,fixtureRetries=0;
  Object.defineProperty(navigator,'onLine',{get:()=>fixtureOnline,configurable:true});
  retryNow=()=>fixtureRetries++;
  window.__fixture={
    state({online=true,local=true,ws=1,daemon=true,channel='secured',reconnect=false,handshakeFailed=false}){
      $("pair").classList.add("hidden");$("app").classList.remove("hidden");
      fixtureOnline=online;S.local=local;S.ws=ws===null?null:{readyState:ws};S.daemonOnline=daemon;
      S.channel=channel;S.reconnect=reconnect?{at:Date.now()+60000}:null;S.handshakeFailed=handshakeFailed;
      renderChrome();const conn=$('conn');return {text:conn.textContent,on:conn.classList.contains('on'),clickable:!!conn.onclick};
    },retries(){return fixtureRetries;}
  };
'''
def check(page):
    page.set_default_timeout(5000)
    local=page.evaluate('__fixture.state({online:false})')
    assert local['on'] and 'connected' in local['text'] and 'unreachable' not in local['text'],local
    assert 'browser offline' in local['text'],local
    assert page.evaluate('__fixture.state({online:true})')['text']=='● connected'
    for state in [{'online':False,'ws':None},{'online':False,'daemon':False}]:
        actual=page.evaluate('s=>__fixture.state(s)',state)
        assert not actual['on'] and 'offline' in actual['text'],actual
    disconnected=page.evaluate('__fixture.state({online:true,ws:null})')
    assert disconnected['text']=='daemon disconnected',disconnected
    assert page.evaluate('__fixture.state({online:true,ws:0})')['text']=='connecting…'
    assert page.evaluate('__fixture.state({online:true,daemon:false})')['text']=='daemon offline'
    remote=page.evaluate('__fixture.state({online:false,local:false})')
    assert not remote['on'] and 'offline' in remote['text'],remote
    securing=page.evaluate('__fixture.state({online:true,local:false,channel:null})')
    assert not securing['on'] and 'securing' in securing['text'],securing
    backoff=page.evaluate('__fixture.state({online:false,reconnect:true})')
    assert not backoff['on'] and 'daemon offline · retry in' in backoff['text'] and backoff['clickable'],backoff
    page.locator('#conn').click()
    assert page.evaluate('__fixture.retries()')==1,'existing manual retry handler retained'
    print('PASS established local/browser-offline distinction, online/disconnected/connecting/daemon-offline, relay handshake and retry')
run(Path(__file__).resolve().parent,hook,check,args.app)
