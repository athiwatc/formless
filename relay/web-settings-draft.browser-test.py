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
  const fixtureCalls=[];
  act=async req=>{fixtureCalls.push(req);return {applies:'now'};};
  refreshSettings=async()=>{};note=()=>{};
  window.__fixture={
    calls:fixtureCalls,
    start(kind,current){$('pair').classList.add('hidden');$('app').classList.remove('hidden');void editSetting('fixture.value',current,kind);}
  };
'''
def check(page):
    page.set_default_timeout(5000)
    input=page.locator('#ask-input')
    for kind,current,invalid,correct,value in [('number',5,'  Infinity  ','7',7),('json',{},'  {"broken":  ','{"fixed":[1,2]}',{'fixed':[1,2]}),('list',['old'],'  [broken  ','[22,26]',[22,26])]:
        before=page.evaluate('__fixture.calls.length')
        page.evaluate('({kind,current})=>__fixture.start(kind,current)',{'kind':kind,'current':current})
        input.fill(invalid);page.locator('#ask-ok').click()
        page.wait_for_function('!document.querySelector("#ask-backdrop").classList.contains("hidden")')
        assert input.input_value()==invalid,('exact attempted text retained',kind)
        assert page.locator('#ask-body .error').inner_text(), 'inline parsing error'
        assert page.evaluate('__fixture.calls.length')==before,'invalid input makes no update request'
        input.fill(correct);page.locator('#ask-ok').click()
        page.wait_for_function('document.querySelector("#ask-backdrop").classList.contains("hidden")')
        assert page.evaluate('__fixture.calls.at(-1).value')==value
    page.evaluate('__fixture.start("json",{})');input.fill('{bad');page.locator('#ask-ok').click()
    page.wait_for_function('!!document.querySelector("#ask-body .error")')
    before=page.evaluate('__fixture.calls.length');page.locator('#ask-cancel').click()
    assert page.evaluate('__fixture.calls.length')==before,'Cancel discards attempted edit'
    page.evaluate('__fixture.start("number",5)');assert input.input_value()=='5','new edit starts with stored value'
    input.fill('   ');page.locator('#ask-ok').click()
    page.wait_for_function('document.querySelector("#ask-backdrop").classList.contains("hidden")')
    assert page.evaluate('__fixture.calls.at(-1).value') is None,'blank still restores default'
    print('PASS exact invalid number/JSON/list drafts, inline errors, corrected typed saves, no invalid updates, Cancel and blank default')
run(Path(__file__).resolve().parent,hook,check,args.app)
