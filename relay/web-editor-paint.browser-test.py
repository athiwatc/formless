#!/usr/bin/env python3
"""Issue #1620 instrumented Chromium checks, using the complete local app.

Python Playwright and /usr/bin/chromium are required. Optional
FORMLESS_EDITOR_SOURCE points to an alternate app.js for a negative control.
Private UI/request fixtures only; no daemon, Tool or provider execution.
Counts demonstrate skipped work, not elapsed-time or allocation ratios.
"""
import functools
import http.server
import os
import pathlib
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
SOURCE = pathlib.Path(os.environ.get('FORMLESS_EDITOR_SOURCE', ROOT / 'web/app.js'))


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def expose(route):
    source = SOURCE.read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '''
    const stats={highlight:0,hl:0,marks:0,lines:0};
    const oldHighlight=highlight;
    highlight=(...args)=>{stats.highlight++;return oldHighlight(...args);};
    const oldReplace=Element.prototype.replaceChildren;
    Element.prototype.replaceChildren=function(...args){
      if(this.classList.contains('ed-hl'))stats.hl++;
      if(this.classList.contains('ed-marks'))stats.marks++;
      if(this.classList.contains('ed-line'))stats.lines++;
      return oldReplace.apply(this,args);
    };
    const pending=[];
    request=req=>new Promise(resolve=>pending.push({req,resolve}));
    window.__paint={S,renderChrome,codeEditor,stats,pending,
      reset(){for(const key of Object.keys(stats))stats[key]=0;},
      answer(index,result){pending[index].resolve(result);},
      remember(){this.hl=[...document.querySelector('.ed-hl').childNodes];this.lines=[...document.querySelectorAll('.ed-line')];},
      retained(){return this.hl.every((node,i)=>node===document.querySelector('.ed-hl').childNodes[i]) && this.hl.length===document.querySelector('.ed-hl').childNodes.length && this.lines.every((node,i)=>node===document.querySelectorAll('.ed-line')[i]) && this.lines.length===document.querySelectorAll('.ed-line').length;}
    };
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + hook + source[end:])


def stats(page):
    return page.evaluate('({...__paint.stats})')


def wait_request(page, count):
    page.wait_for_function('(n)=>__paint.pending.length>=n', arg=count)


def unchanged(page):
    assert page.evaluate('__paint.retained()'), 'highlight/line node identity changed'
    actual = stats(page)
    assert actual['highlight'] == actual['hl'] == actual['marks'] == 0, actual


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with sync_playwright() as pw:
        browser = pw.chromium.launch(executable_path=chromium_executable(pw), headless=True, args=['--no-sandbox'])
        for root_size in [16, 32]:
            page = browser.new_page(viewport={'width': 1200, 'height': 900})
            errors = []
            page.on('pageerror', lambda error: errors.append(str(error)))
            page.route('**/app.js*', expose)
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            source = 'function run() {\n  const 日本語 = [1, {x: 2}];\n  return (日本語[0]);\n}\n'
            page.evaluate('''([size,text])=>{
              document.documentElement.style.fontSize=size+'px';
              document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');
              __paint.S.view='tools';__paint.renderChrome();
              const host=document.querySelector('#view-tools');host.replaceChildren();
              const area=document.createElement('textarea');area.id='paint-fixture';area.className='editor';
              area.value=text;area.setSelectionRange(0,0);area.style.height='140px';area.style.minHeight='0';
              host.append(__paint.codeEditor(area));
            }''', [root_size, source])
            wait_request(page, 1)
            assert stats(page)['highlight'] == 1
            page.evaluate('__paint.remember();__paint.reset()')
            positions = [source.index('('), source.index('['), source.index('return'), source.index('{x')]
            page.evaluate('''positions=>{
              const area=document.querySelector('#paint-fixture');
              for(const position of positions){area.setSelectionRange(position,position);for(const type of ['click','keyup','select'])area.dispatchEvent(new Event(type,{bubbles:true}));}
            }''', positions)
            print(f'caret counts root {root_size}: {stats(page)}', flush=True)
            unchanged(page)
            assert 0 < stats(page)['lines'] <= 8, stats(page)
            # Marking or clearing errors does not rebuild bracket children.
            page.evaluate('window.markChildren=[...document.querySelectorAll(".ed-line")].map(e=>[...e.childNodes]);__paint.reset();__paint.answer(0,{ok:false,line:2,error:"fixture"})')
            page.wait_for_function('document.querySelectorAll(".ed-err").length===1')
            unchanged(page)
            assert stats(page)['lines'] == 0
            assert page.evaluate('markChildren.every((nodes,i)=>nodes.every((node,j)=>node===document.querySelectorAll(".ed-line")[i].childNodes[j]))')
            # Input with the same value and keyup in a later frame both skip
            # highlighting. Debounce and request sequencing remain exercised.
            for count, result in [(2, {'ok': False, 'line': 4, 'error': 'moved'}), (3, {'ok': True})]:
                page.locator('#paint-fixture').dispatch_event('input')
                page.evaluate('()=>new Promise(resolve=>requestAnimationFrame(()=>{document.querySelector("#paint-fixture").dispatchEvent(new KeyboardEvent("keyup",{key:"ArrowRight"}));resolve();}))')
                wait_request(page, count)
                page.evaluate('([index,result])=>__paint.answer(index,result)', [count - 1, result])
                page.wait_for_function('(ok)=>document.querySelectorAll(".ed-err").length===(ok?0:1)', arg=result['ok'])
                unchanged(page)
                if not result['ok']:
                    assert page.locator('.ed-line').nth(result['line'] - 1).evaluate('e=>e.classList.contains("ed-err")')
                    assert not page.locator('.ed-line').nth(1).evaluate('e=>e.classList.contains("ed-err")')
            page.locator('#paint-fixture').dispatch_event('input');wait_request(page, 4)
            page.locator('#paint-fixture').dispatch_event('input');wait_request(page, 5)
            page.evaluate('__paint.answer(4,{ok:true})')
            page.wait_for_function('document.querySelector("#editor-status").textContent.includes("syntax ok")')
            page.evaluate('__paint.answer(3,{ok:false,line:3,error:"stale"})')
            page.wait_for_timeout(20)
            assert page.locator('.ed-err').count() == 0
            unchanged(page)
            new_source = 'function run() {\n' + ('  // 日本語 ' + 'x' * 200 + '\n') * 60 + '  return 1;\n}\n\n'
            page.evaluate('__paint.reset()')
            page.locator('#paint-fixture').fill(new_source)
            page.evaluate('()=>new Promise(resolve=>requestAnimationFrame(()=>{document.querySelector("#paint-fixture").dispatchEvent(new KeyboardEvent("keyup",{key:"x"}));resolve();}))')
            assert stats(page)['highlight'] == stats(page)['hl'] == stats(page)['marks'] == 1, stats(page)
            for selector in ['.ed-hl', '.ed-marks']:
                assert page.locator(selector).text_content() == new_source + '\n'
            assert page.locator('.ed-line').count() == len((new_source + '\n').split('\n'))
            page.evaluate('''()=>{const area=document.querySelector('#paint-fixture');area.scrollTop=70;area.scrollLeft=100;area.dispatchEvent(new Event('scroll'));}''')
            assert page.evaluate('''()=>{const area=document.querySelector('#paint-fixture');return area.scrollTop>0&&area.scrollLeft>0&&[...document.querySelectorAll('.ed-layer')].every(e=>e.scrollTop===area.scrollTop&&e.scrollLeft===area.scrollLeft);}''')
            # Composition input still paints the changed source exactly once.
            page.evaluate(r'''()=>{__paint.reset();const area=document.querySelector('#paint-fixture');area.dispatchEvent(new CompositionEvent('compositionstart'));area.value+='// ชื่อ 🦀\n';area.dispatchEvent(new InputEvent('input',{bubbles:true,isComposing:true,inputType:'insertCompositionText',data:'ชื่อ'}));area.dispatchEvent(new CompositionEvent('compositionend'));area.dispatchEvent(new KeyboardEvent('keyup',{key:'Process'}));}''')
            assert stats(page)['highlight'] == stats(page)['hl'] == stats(page)['marks'] == 1
            assert page.locator('.ed-hl').text_content() == new_source + '// ชื่อ 🦀\n\n'
            assert page.locator('.ed-marks').text_content() == new_source + '// ชื่อ 🦀\n\n'
            assert errors == [], errors
            print(f'PASS retained nodes, marks/errors, request order, edits, scroll and composition at root {root_size}', flush=True)
            page.close()
        browser.close()
finally:
    server.shutdown()
