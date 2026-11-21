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
  const fixtureData={docs:{'fixture.flag':{description:'Boolean fixture'},'fixture.number':{description:'Number fixture'}},values:{'fixture.flag':true,'fixture.number':5}};
  const fixturePending=[];
  request=async req=>{
    if(req.type==='set_setting')return new Promise(resolve=>fixturePending.push({req,resolve}));
    if(req.type==='get_settings')return structuredClone(fixtureData);
    if(req.type==='list_providers')return {providers:[],roles:{}};
    if(req.type==='status')return {secure:false,protocol:1,pid:42,sessions:0};
    return null;
  };
  window.__fixture={
    setup(){ $('pair').classList.add('hidden');$('app').classList.remove('hidden');S.view='settings';S.daemonOnline=true;
      S.settings=structuredClone(fixtureData);S.providers={providers:[],roles:{}};renderChrome();renderSettings();},
    pending(){return fixturePending.length;},
    save(remove=false){const p=fixturePending.shift();fixtureData.values[p.req.key]=p.req.value;if(remove)delete fixtureData.docs[p.req.key];p.resolve({applies:'now'});}
  };
'''
def check(page):
    page.set_default_timeout(5000)
    page.evaluate('__fixture.setup()')
    button=lambda key:page.get_by_role('button',name='Edit '+key,exact=True)
    button('fixture.flag').focus()
    page.evaluate('window.oldSetting=document.activeElement')
    page.keyboard.press('Enter')
    page.wait_for_function('__fixture.pending()===1')
    page.evaluate('__fixture.save()')
    page.wait_for_function('!oldSetting.isConnected')
    assert button('fixture.flag').evaluate('e=>e===document.activeElement'),'boolean redraw returns to matching setting'
    button('fixture.number').focus();page.keyboard.press('Enter')
    page.locator('#ask-input').fill('7');page.locator('#ask-ok').click()
    page.wait_for_function('__fixture.pending()===1')
    page.evaluate('window.oldSetting=document.activeElement;__fixture.save()')
    page.wait_for_function('!oldSetting.isConnected')
    assert button('fixture.number').evaluate('e=>e===document.activeElement'),'dialog save returns to matching setting'
    button('fixture.number').click();page.locator('#ask-cancel').click()
    assert button('fixture.number').evaluate('e=>e===document.activeElement'),'Cancel retains invoker'
    button('fixture.flag').focus();page.keyboard.press('Enter')
    page.wait_for_function('__fixture.pending()===1')
    page.locator('#settings-filter').focus()
    page.evaluate('__fixture.save()')
    page.wait_for_timeout(100)
    assert page.locator('#settings-filter').evaluate('e=>e===document.activeElement'),'deliberate outside focus is not stolen'
    button('fixture.flag').focus();page.evaluate('window.oldSetting=document.activeElement');page.keyboard.press('Enter')
    page.wait_for_function('__fixture.pending()===1');page.evaluate('__fixture.save(true)')
    page.wait_for_function('!oldSetting.isConnected')
    assert page.locator('#settings-filter').evaluate('e=>e===document.activeElement'),'removed setting falls back to filter'
    print('PASS boolean/dialog redraw focus, Cancel, deliberate outside move during save and removed-setting fallback')
run(Path(__file__).resolve().parent,hook,check,args.app)
