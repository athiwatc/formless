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
parser.add_argument('--case',choices=['all','python','config'],default='all')
args=parser.parse_args()
hook='\nwindow.__fixture={codeBlock,markdown};'

def render(page, source, lang, kind):
    return page.evaluate(r'''({source,lang,kind})=>{
      const node=kind==='fence'?__fixture.markdown('```'+lang+'\n'+source+'\n```'):__fixture.codeBlock(source,lang);
      const code=node.querySelector('pre code');
      const tokens=selector=>[...code.querySelectorAll(selector)].map(c=>c.textContent);
      return {text:code.textContent,comments:tokens('.tok-com'),keys:tokens('.tok-key'),strings:tokens('.tok-str')};
    }''',{'source':source,'lang':lang,'kind':kind})

def check(page):
    cases=[]
    if args.case!='config':
      for lang in ['py','python','python3','PYTHON']:
        for source,comments in [
          ('x=1#keep the rest',['#keep the rest']),('print(x)#done',['#done']),
          ('def f():#entry',['#entry']),('# at the start',['# at the start']),
          ('x = 1  # spaced',['# spaced']),('s="x#y"; x=1#outside',['#outside']),
          ("s='# inside'",[]),('x=1#first\ny=2',['#first']),
        ]:cases.append((lang,source,comments))
    if args.case!='python':
      for lang in ['yaml','yml','toml','env','dotenv']:
        for source,comments in [
          ('key: a;b',[]),(r'PATH=C:\Windows;C:\Windows\System32',[]),
          ('key="a;b"',[]),('key: foo#bar',[]),('key: foo # comment',['# comment']),
        ]:cases.append((lang,source,comments))
      for lang in ['ini','properties','conf']:
        for source,comments in [('; comment',['; comment']),('key=a;b',[';b']),('key="a;b"',[])]:
          cases.append((lang,source,comments))
      for lang in ['sh','bash','shell']:
        for source,comments in [('echo foo#bar',[]),('${#arr}',[]),('echo foo # comment',['# comment'])]:
          cases.append((lang,source,comments))
    for kind in ['block','fence']:
      for lang,source,comments in cases:
        actual=render(page,source,lang,kind)
        assert actual['text']==source and actual['comments']==comments,(kind,lang,source,actual,comments)
    if args.case!='python':
      for quote, middle, tail in [
          ('"', r'\""" keep # escaped quote', ''),
          ("'", 'keep # literal', '\\'),
      ]:
        raw = quote*3 + '\n' + middle + '\nname = not a key\n# still content\n' + tail + quote*5
        source = 'text = ' + raw + ' # outside\nname = "formless" # comment'
        for kind in ['block','fence']:
          actual=render(page,source,'TOML',kind)
          assert actual=={
              'text':source,'comments':['# outside','# comment'],
              'keys':['text','name'],'strings':[raw,'"formless"'],
          },(kind,quote,actual)
      print('PASS TOML basic and literal multiline strings in code blocks and Markdown fences')
    print(f'PASS {len(cases)} comment/source controls through both shipped codeBlock and Markdown fence DOM ({2*len(cases)} views)')
run(Path(__file__).resolve().parent,hook,check,args.app)
