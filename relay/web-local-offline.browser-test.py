"""Local shipped-UI fixture support; no daemon, credentials or remote sources."""
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import expect, sync_playwright

from browser_fixture import browser_engines, chromium_executable

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def run(root, hook, check, app=None):
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(root/'web')))
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:
        with sync_playwright() as p:
            for engine in browser_engines():
                browser = (p.chromium.launch(executable_path=chromium_executable(p), args=['--no-sandbox'])
                           if engine == 'chromium' else p.webkit.launch())
                page = browser.new_page(viewport={'width': 1000, 'height': 900}, has_touch=True)
                def expose(route):
                    source=(app or root/'web/app.js').read_text();end=source.rfind('\n})();');assert end>0
                    route.fulfill(status=200,content_type='text/javascript',body=source[:end]+hook+source[end:])
                page.route('**/app.js',expose)
                page.goto(f'http://127.0.0.1:{server.server_port}/')
                page.wait_for_function('!!window.__fixture')
                check(page)
                print(f'PASS {engine} {browser.version} keyboard and touch status controls', flush=True)
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
  const contextRequests = [];
  act = async request => {
    if (request.type === 'list_models') return {models:['fixture-model']};
    if (request.type === 'context_usage') {
      contextRequests.push(request);
      return {model:'fixture-model', used_tokens:80, context_tokens:100, measured:true, context_source:'configured'};
    }
    return null;
  };
  window.__fixture={
    state({online=true,local=true,ws=1,daemon=true,channel='secured',reconnect=false,handshakeFailed=false}){
      $("pair").classList.add("hidden");$("app").classList.remove("hidden");
      fixtureOnline=online;S.local=local;S.ws=ws===null?null:{readyState:ws};S.daemonOnline=daemon;
      S.channel=channel;S.reconnect=reconnect?{at:Date.now()+60000,fn:()=>fixtureRetries++}:null;S.handshakeFailed=handshakeFailed;
      renderChrome();const conn=$('conn'), retry=$('conn-retry');
      return {text:conn.textContent,on:conn.classList.contains('on'),retryVisible:!!retry&&!retry.hidden};
    },
    retries(){return fixtureRetries;},
    tick(){S.reconnect.at-=2000;renderChrome();},
    cancel: cancelReconnect,
    scheduled(){
      S.backoff=250;
      scheduleReconnect(()=>fixtureRetries++);
      $('conn-retry').focus();
      return document.activeElement.id;
    },
    context(visible){
      S.current='context-fixture';
      S.sessions=[{id:S.current,model:'fixture-model',name:'Context fixture'}];
      S.contextFill=visible?{[S.current]:{used:80,size:100}}:{};
      renderStatus();
    },
    contextRequests,
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
    retry = page.get_by_role('button', name='Retry now', exact=True)
    expect(retry).to_have_count(0)
    pill = page.locator('#conn')
    expect(pill).to_have_attribute('aria-live', 'polite')
    activity = page.locator('#notes-btn')
    for retries, key in enumerate(('Enter', 'Space'), 1):
        backoff=page.evaluate('__fixture.state({online:false,reconnect:true})')
        assert not backoff['on'] and 'daemon offline · retry in' in backoff['text'], backoff
        expect(retry).to_be_visible()
        assert backoff['retryVisible']
        page.locator('#tabs [data-view=settings]').focus()
        page.keyboard.press('Tab')
        expect(retry).to_be_focused()
        assert retry.evaluate("el => el.matches(':focus-visible') && getComputedStyle(el).outlineStyle === 'solid'")
        page.evaluate("window.__retryNode = document.activeElement; __fixture.tick()")
        expect(retry).to_be_focused()
        assert page.evaluate('window.__retryNode === document.activeElement')
        retry.press(key)
        assert page.evaluate('__fixture.retries()') == retries
        expect(retry).to_have_count(0)
        expect(activity).to_be_focused()

    page.clock.install()
    assert page.evaluate('__fixture.scheduled()') == 'conn-retry'
    page.clock.run_for(250)
    page.wait_for_function('__fixture.retries() === 3')
    expect(retry).to_have_count(0)
    expect(activity).to_be_focused()
    page.evaluate('() => { __fixture.scheduled(); __fixture.cancel(); }')
    page.clock.run_for(250)
    expect(retry).to_have_count(0)
    expect(activity).to_be_focused()
    assert page.evaluate('__fixture.retries()') == 3
    page.set_viewport_size({'width': 390, 'height': 844})
    page.evaluate('__fixture.state({online:false,reconnect:true})')
    expect(retry).to_be_visible()
    retry.tap()
    assert page.evaluate('__fixture.retries()') == 4
    expect(retry).to_have_count(0)

    page.evaluate('__fixture.context(true)')
    context = page.get_by_role('button', name='Context window (80% used)', exact=True)
    model = page.get_by_role('button', name='Change model (fixture-model)', exact=True)
    model.focus()
    page.keyboard.press('Tab')
    expect(context).to_be_focused()
    assert context.evaluate("el => el.matches(':focus-visible') && getComputedStyle(el).outlineStyle === 'solid'")
    for key in ('Enter', 'Space'):
        context.press(key)
        expect(context).to_be_focused()
    assert page.evaluate('__fixture.contextRequests') == [
        {'type': 'context_usage', 'session_id': 'context-fixture'},
        {'type': 'context_usage', 'session_id': 'context-fixture'},
    ]
    page.evaluate('__fixture.context(false)')
    expect(context).to_have_count(0)
    expect(model).to_be_focused()
    for key in ('Enter', 'Space'):
        model.press(key)
        expect(page.get_by_role('dialog', name='Model', exact=True)).to_be_visible()
        page.keyboard.press('Escape')
        expect(model).to_be_focused()
run(Path(__file__).resolve().parent,hook,check,args.app)
