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

def run(root, hook, check, app=None, css=None):
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
            if css: page.route('**/app.css',lambda route:route.fulfill(status=200,content_type='text/css',body=css.read_text()))
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__fixture')
            check(page)
            browser.close()
    finally:
        server.shutdown();server.server_close();thread.join(timeout=5)

import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--css',type=Path,help='Optional baseline CSS for negative control')
args=parser.parse_args()
hook=r'''
  window.__fixture={
    setup(width,mode){
      const completed=mode!=="running";
      $('pair').classList.add('hidden');$('app').classList.remove('hidden');S.view='chat';renderChrome();
      $('app').style.width=width+'px';
      const long='/workspace/'+('very_long_unbroken_path_'.repeat(35))+'file.txt';
      S.plan=[{content:long,status:completed?'completed':'in_progress'},{content:'Ordinary sentence with normal wrapping opportunities.',status:'completed'}];
      S.planState=mode==='collapsed'?null:{run_status:completed?'completed':'running',origin:'live'};
      renderPlan();return long;
    }
  };
'''

def check(page):
    for width in [320,480,900]:
        page.set_viewport_size({'width':width,'height':1000})
        for mode in ['running','completed','collapsed']:
            completed=mode!='running'
            original=page.evaluate('({width,mode})=>__fixture.setup(width,mode)',{'width':width,'mode':mode})
            if mode=='collapsed':
                assert page.locator('#plan li').count()==0, 'existing completed collapse retained'
                assert 'Plan done' in page.locator('#plan').inner_text()
                continue
            item=page.locator('#plan li').first
            assert original in item.inner_text(), 'long text is unchanged'
            geometry=page.evaluate('''()=>{
              const ids=['plan','view-chat'];return Object.fromEntries(ids.map(id=>{const e=document.getElementById(id);return [id,{client:e.clientWidth,scroll:e.scrollWidth}]}));
            }''')
            assert all(g['scroll']<=g['client']+1 for g in geometry.values()),(width,completed,geometry)
            assert page.locator('#plan li').nth(1).inner_text().endswith('Ordinary sentence with normal wrapping opportunities.')
            assert item.get_attribute('class')==('completed' if completed else 'in_progress')
    print('PASS 320/480/900px expanded/running and collapsed/completed plans: no horizontal overflow, exact text/status')
run(Path(__file__).resolve().parent,hook,check,css=args.css)
