#!/usr/bin/env python3
"""#1481 Mermaid lifecycle in shipped Chromium UI; no daemon/provider/deployment.

Uses the real vendored engine for load/retry and a controlled engine for queued
completion/layout races. Not physical assistive-technology or other-browser proof.
"""
import functools
import http.server
import pathlib
import sys
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else artifact_dir('/tmp/formless-mermaid-1481')
OUT.mkdir(parents=True, exist_ok=True)
PASSES = 0

class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass

def check(value, label):
    global PASSES
    assert value, label
    PASSES += 1
    print('PASS', label, flush=True)

def expose(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hook = '''
    window.__m = {S,diagramBlock,renderHistory,refreshDiagrams,
      mount(src,id='manual'){const box=diagramBlock(src,true);box.id=id;$('transcript').append(box);},
      pause(id){S.follow=false;const box=$('transcript'),node=document.querySelector('[data-message-id="'+id+'"]');box.scrollTop+=node.getBoundingClientRect().top-box.getBoundingClientRect().top-90;reading.anchor=captureReadingAnchor();},
      show(){S.current='mermaid-fixture';S.view='chat';S.daemonOnline=true;S.sessions=[{id:S.current,name:'Mermaid fixture'}];$('app').classList.remove('hidden');$('pair').classList.add('hidden');}
    };
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hook+source[end:])

def theme(page, value):
    page.evaluate("v=>{const el=document.querySelector('#theme');el.value=v;el.dispatchEvent(new Event('change',{bubbles:true}));}", value)

def open_page(browser, url, loader=None, fake=False):
    context = browser.new_context(viewport={'width':1000,'height':700}, color_scheme='dark', accept_downloads=True)
    context.route('**/app.js', expose)
    if loader:
        context.route('**/vendor/mermaid.min.js', loader)
    if fake:
        context.add_init_script('''
        window.__renders=[];window.__parses=0;window.__delay=false;window.__pending=[];
        window.__fakeMermaid={initialize(c){this.config=c;},parse(src){window.__parses++;return !src.includes('INVALID');},render(id,src){
          const theme=this.config.theme,colors=this.config.themeVariables;
          const item={theme,src,colors};window.__renders.push(item);
          const height=theme==='dark'?120:330;
          const result={svg:`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 ${height}" data-theme="${theme}"><rect width="400" height="${height}" fill="${theme==='dark'?'#123456':'#abcdef'}"/><text x="10" y="30">${src}</text></svg>`};
          return window.__delay?new Promise(resolve=>window.__pending.push(()=>resolve(result))):Promise.resolve(result);
        }};
        ''')
        context.route('**/vendor/mermaid.min.js', lambda r:r.fulfill(status=200, content_type='text/javascript', body='window.mermaid=window.__fakeMermaid;'))
    page=context.new_page()
    errors=[]
    page.on('pageerror', lambda e:errors.append(str(e)))
    page.set_default_timeout(8000)
    page.goto(url)
    page.wait_for_function('!!window.__m')
    page.evaluate('__m.show()')
    return context,page,errors

def run():
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(Quiet,directory=str(ROOT/'web')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    url=f'http://127.0.0.1:{server.server_port}/'
    try:
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
            for missing in [False,True]:
                attempts=[]
                def loader(route):
                    attempts.append(1)
                    if len(attempts)==1:
                        if missing: route.fulfill(status=200,content_type='text/javascript',body='/* missing Mermaid export */')
                        else: route.abort()
                    else: route.fulfill(status=200,content_type='text/javascript',path=str(ROOT/'web/vendor/mermaid.min.js'))
                context,page,errors=open_page(browser,url,loader)
                page.evaluate("__m.mount('flowchart LR\\n A[日本語] --> B[Recovered]','first')")
                page.locator('#first').get_by_role('button',name='Retry',exact=True).wait_for()
                check(page.locator('#first').evaluate("e=>e.classList.contains('show-src')"),f'{missing}: loader failure keeps source fallback')
                if not missing:
                    # Same key must recover even when a different block is made.
                    page.evaluate("__m.mount('flowchart LR\\n A[日本語] --> B[Recovered]','second')")
                    page.wait_for_selector('#second img',timeout=30000)
                    check(len(attempts)==2,'recreated source/theme evicts the failed loader job')
                page.locator('#first').get_by_role('button',name='Retry',exact=True).click()
                page.wait_for_selector('#first img',timeout=30000)
                check(page.locator('#first img').get_attribute('data-theme')=='dark',f'{missing}: explicit Retry recovers real vendored engine')
                check(page.locator('#first').get_by_role('button',name='Source',exact=True).evaluate('e=>e===document.activeElement'),f'{missing}: recovered Retry moves focus to retained source control')
                check(page.locator('#first').get_by_role('button',name='Retry',exact=True).is_hidden(),f'{missing}: successful retry retires retry control')
                previous=page.locator('#first img').get_attribute('src')
                theme(page,'light')
                page.wait_for_function("document.querySelector('#first img')?.dataset.theme==='default'")
                check(page.locator('#first img').get_attribute('src')!=previous,f'{missing}: real vendored SVG changes with explicit theme')
                check(not errors,f'{missing}: no unhandled browser errors')
                context.close()

            context,page,errors=open_page(browser,url,fake=True)
            page.evaluate('''() => {
              __m.S.messages=[{id:'diagram',seq:1,role:'assistant',content:'```mermaid\\nflowchart LR\\n A-->B\\n```'}];
              for(let i=0;i<30;i++)__m.S.messages.push({id:'p'+i,seq:i+2,role:'assistant',content:'Paragraph '+i+' '+('reader position text '.repeat(40))});
              __m.renderHistory();
            }''')
            page.wait_for_selector('[data-message-id="diagram"] img')
            page.evaluate("window.__body=document.querySelector('[data-message-id=\"p12\"]');window.__box=document.querySelector('.diagram');")
            box=page.locator('.diagram').first
            box.locator('img').evaluate('e=>e.click()')
            box.get_by_role('button',name='Source',exact=True).evaluate('e=>e.click()')
            source=box.get_by_role('button',name='Diagram',exact=True)
            source.evaluate('e=>e.focus({preventScroll:true})')
            theme(page,'light')
            page.wait_for_function("document.querySelector('.diagram img')?.dataset.theme==='default'")
            check(page.evaluate("document.querySelector('.diagram')===__box && document.querySelector('[data-message-id=\"p12\"]')===__body"),'theme update retains diagram and unrelated message identity')
            check(box.evaluate("e=>e.classList.contains('show-src') && e.classList.contains('zoom')"),'theme update retains source and zoom choices')
            check(source.evaluate('e=>e===document.activeElement'),'theme update retains source-control focus')
            with page.expect_download() as event:
                box.get_by_role('button',name='SVG',exact=True).evaluate('e=>e.click()')
            check('data-theme="default"' in pathlib.Path(event.value.path()).read_text(),'SVG download action follows the displayed theme')
            source.evaluate('e=>e.click()')
            page.evaluate("__m.pause('p12')")
            page.wait_for_timeout(80)
            before=page.locator('[data-message-id="p12"]').evaluate('e=>e.getBoundingClientRect().top')
            theme(page,'auto')
            page.wait_for_function("document.querySelector('.diagram img')?.dataset.theme==='dark'")
            page.wait_for_timeout(100)
            after=page.locator('[data-message-id="p12"]').evaluate('e=>e.getBoundingClientRect().top')
            check(abs(after-before)<2 and page.evaluate('__m.S.follow===false'),'retheming and image load preserve paused reader anchor across changed image height')
            page.emulate_media(color_scheme='light')
            page.wait_for_function("document.querySelector('.diagram img')?.dataset.theme==='default'")
            check(True,'Auto responds to OS scheme change without reopening Session')
            # Reused detached message body must pick up the effective theme too.
            page.locator('[data-message-id="diagram"]').evaluate('e=>e.remove()')
            theme(page,'dark')
            page.evaluate('__m.renderHistory()')
            page.wait_for_function("document.querySelector('.diagram img')?.dataset.theme==='dark'")
            check(page.evaluate("document.querySelector('.diagram')===__box"),'reused detached diagram refreshes without rebuilding its cached message')
            page.evaluate("__m.mount('INVALID source','invalid')")
            page.locator('#invalid .error').wait_for()
            parses=page.evaluate('__parses')
            page.evaluate("__m.refreshDiagrams();__m.refreshDiagrams();__m.mount('INVALID source','invalid-copy')")
            page.locator('#invalid-copy .error').wait_for()
            check(page.evaluate('__parses')==parses,'invalid source stays cached and does not retry during paint or recreation')
            check(page.locator('#invalid').get_by_role('button',name='Retry',exact=True).is_hidden(),'invalid source has clear fallback without loader Retry')
            # Restrict the race fixture to one block, with serialized render jobs.
            page.evaluate("document.querySelector('#invalid').remove();document.querySelector('#invalid-copy').remove();__delay=true;")
            theme(page,'light')
            page.wait_for_function('__pending.length===1')
            theme(page,'dark')
            theme(page,'light')
            old_src=box.locator('img').get_attribute('src')
            page.evaluate('__pending.shift()()')
            page.wait_for_function('__pending.length===1')
            check(box.locator('img').get_attribute('src')==old_src,'older first completion cannot replace current diagram')
            page.evaluate('__pending.shift()()')
            page.wait_for_function('__pending.length===1')
            check(box.locator('img').get_attribute('src')==old_src,'intermediate theme completion cannot replace current diagram')
            page.evaluate('__pending.shift()()')
            page.wait_for_function("document.querySelector('.diagram img')?.dataset.theme==='default' && !document.querySelector('.diagram-view').hasAttribute('aria-busy')")
            check(page.evaluate("__renders.slice(-3).map(r=>r.theme).join(',')==='default,dark,default'"),'Mermaid initialization/render queue retains captured theme order')
            check(not errors,'controlled completion and invalid-source fixtures have no unhandled browser errors')
            page.screenshot(path=str(OUT/'mermaid-theme.png'))
            context.close();browser.close()
    finally:
        server.shutdown();server.server_close()
    print(f'{PASSES} Mermaid browser checks passed',flush=True)

if __name__=='__main__':
    run()
