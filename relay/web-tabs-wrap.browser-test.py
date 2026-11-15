"""Local shipped-UI fixture support; no daemon, credentials or remote sources."""
import functools
import http.server
from pathlib import Path
import threading
from playwright.sync_api import sync_playwright

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def run(root, hook, check, app=None, css=None):
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
  act=async()=>null;request=async()=>null;
  window.__fixture={setup(){ $('pair').classList.add('hidden');$('app').classList.remove('hidden');S.view='chat';renderChrome(); }};
'''
def check(page):
    page.set_default_timeout(5000)
    page.evaluate('__fixture.setup()')
    for width,font in [(320,16),(480,16),(320,32),(1200,16)]:
        page.set_viewport_size({'width':width,'height':1000})
        page.evaluate('size=>document.documentElement.style.fontSize=size+"px"',font)
        geometry=page.locator('#tabs').evaluate('''e=>{
          const r=e.getBoundingClientRect();return {client:e.clientWidth,scroll:e.scrollWidth,top:r.top,bottom:r.bottom,left:r.left,right:r.right,
            buttons:[...e.querySelectorAll('button')].map(b=>{const r=b.getBoundingClientRect();return {top:r.top,bottom:r.bottom,left:r.left,right:r.right};})};
        }''')
        assert geometry['scroll']<=geometry['client']+1,(width,font,'no hidden horizontal tabs',geometry)
        assert all(b['left']>=geometry['left']-1 and b['right']<=geometry['right']+1 and b['top']>=geometry['top']-1 and b['bottom']<=geometry['bottom']+1 for b in geometry['buttons']),(width,font,geometry)
        if width==1200: assert len({round(b['top']) for b in geometry['buttons']})==1,'fitting desktop stays one row'
    page.set_viewport_size({'width':320,'height':1000})
    buttons=page.locator('#tabs button')
    for index in range(buttons.count()):
        button=buttons.nth(index);button.focus();page.keyboard.press('Enter')
        assert button.get_attribute('aria-current')=='page','native Enter selects matching view'
        assert button.evaluate('e=>e===document.activeElement && getComputedStyle(e).outlineStyle!=="none"'),'focus remains perceivable'
        assert button.is_visible()
    page.locator('#tabs button').first.click()
    assert page.locator('#tabs button').first.get_attribute('aria-current')=='page','pointer activation unchanged'
    print('PASS all six tabs visible at 320/480px and 200% text, one fitting desktop row, native keyboard/pointer/current state')
run(Path(__file__).resolve().parent,hook,check,css=args.css)
