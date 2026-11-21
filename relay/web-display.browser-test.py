#!/usr/bin/env python3
"""Issue #823 browser regressions. Requires Python Playwright, Chromium, poppler.

Run: python relay/web-display.browser-test.py [artifact-directory]
Uses real shipped assets and rendering; a test-only script route exposes closure
helpers so no daemon, credentials, external provider, or deployment is needed.
Nonzero safe-area values are simulated; this is not physical iOS/Android evidence.
"""
import functools
import http.server
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else artifact_dir('/tmp/formless-display-823')
OUT.mkdir(parents=True, exist_ok=True)


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


def check(value, label):
    assert value, label
    print('PASS', label, flush=True)


def rect(page, selector):
    return page.locator(selector).evaluate('(e) => { const r=e.getBoundingClientRect();return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height}; }')


def contained(r, width, height, left=0, top=0, right=0, bottom=0):
    return r['x'] >= left - 1 and r['y'] >= top - 1 and r['right'] <= width - right + 1 and r['bottom'] <= height - bottom + 1


def expose_script(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    source = source[:end] + '\nwindow.__display = {S,renderLive,setReading,applyTheme,sessionMenu,messageMenu,closePopover,renderHistory,toast,pauseWithNewOutput(){S.follow=false;const box=$("transcript");box.scrollTop=0;reading.anchor=captureReadingAnchor();reading.unseen.set([...box.querySelectorAll("[data-message-id]")].at(-1),null);updateReadingPill();}};' + source[end:]
    route.fulfill(status=200, content_type='text/javascript', body=source)


def fixture(page):
    page.evaluate('''() => {
      const d=window.__display; d.S.current='display-fixture'; d.S.daemonOnline=true;
      d.S.sessions=[{id:d.S.current,name:'Display fixture',updated_at:'2026-11-02T12:00:00Z'}];
      d.S.view='chat';
      document.querySelector('#app').classList.remove('hidden');
      document.querySelector('#pair').classList.add('hidden');
      document.querySelector('#session-title').textContent='Display fixture transcript';
      d.S.messages=[];
      for(let i=0;i<12;i++) {
        d.S.messages.push({id:'u'+i,role:'user',seq:i*2,content:'TURN_'+i+' Please explain this example.',created_at:'2026-11-02T12:00:00Z'});
        d.S.messages.push({id:'a'+i,role:'assistant',seq:i*2+1,content:'ANSWER_'+i+'\\n\\n'+('A readable paragraph for pagination and font scaling. '.repeat(18))+'\\n\\n```js\\n'+('const item = "'+('wide-code-'.repeat(30))+'";\\n').repeat(i===5?85:3)+'CODE_END_'+i+'\\n```',created_at:'2026-11-02T12:00:00Z',meta:i===3?JSON.stringify({tool_calls:[{function:{name:'display_fixture',arguments:JSON.stringify({message:'TOOL_INPUT_PRESENT'})}}]}):'{}'});
      }
      d.S.messages.push({id:'diagram',role:'assistant',seq:30,content:'```mermaid\\nflowchart LR\\n  A[Start] --> B[Diagram display fixture]\\n```',created_at:'2026-11-02T12:00:00Z'});
      d.S.messages.push({id:'diagram-fallback',role:'assistant',seq:31,content:'```mermaid\\nflowchart LR\\n PRINT_SOURCE_FALLBACK --> Waiting',created_at:'2026-11-02T12:00:00Z'});
      d.renderHistory();
    }''')


def run():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f'http://127.0.0.1:{server.server_port}/'
    try:
        with sync_playwright() as p:
            # Set Chromium's actual browser default-font preference, independently
            # of CSS and the application's own manual reading preference.
            for default_font in (32, 16):
                with tempfile.TemporaryDirectory(prefix='formless-display-browser-') as profile:
                    prefs = pathlib.Path(profile) / 'Default'
                    prefs.mkdir()
                    (prefs / 'Preferences').write_text(json.dumps({'webkit': {'webprefs': {'default_font_size': default_font}}}))
                    context = p.chromium.launch_persistent_context(profile, executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'], viewport={'width':1280,'height':900}, color_scheme='dark')
                    context.route('**/app.js', expose_script)
                    page = context.pages[0]
                    page.set_default_timeout(5000)
                    errors=[]
                    page.on('pageerror', lambda e: errors.append(str(e)))
                    page.goto(url)
                    page.wait_for_function('!!window.__display')
                    root_size=page.evaluate('parseFloat(getComputedStyle(document.documentElement).fontSize)')
                    check(abs(root_size-default_font)<0.1, f'browser default font {default_font}px controls root ({root_size}px)')
                    body_size=page.evaluate('parseFloat(getComputedStyle(document.body).fontSize)')
                    check(abs(body_size-default_font*.9375)<0.1, 'body inherits browser-relative font size')
                    for scale in (.85,1,2):
                        page.evaluate('(scale)=>__display.setReading(scale)',scale)
                        actual=page.evaluate('parseFloat(getComputedStyle(document.documentElement).fontSize)')
                        check(abs(actual-default_font*scale)<.1, f'manual {scale*100:g}% multiplies browser preference')
                        check(page.locator('#transcript').evaluate('(e)=>getComputedStyle(e).zoom')=='1','transcript has no CSS zoom')
                    if default_font==32:
                        context.close()
                        continue
                    fixture(page)
                    page.wait_for_selector('.diagram img',timeout=30000)
                    for scheme,theme,color in [('light','auto','#f6f8f7'),('dark','auto','#0f1211'),('light','dark','#0f1211'),('dark','light','#f6f8f7'),('dark','auto','#0f1211'),('light','auto','#f6f8f7')]:
                        page.emulate_media(color_scheme=scheme)
                        page.evaluate('(theme)=>__display.applyTheme(theme)',theme)
                        page.wait_for_timeout(40)
                        colors=page.locator('meta[name="theme-color"]').evaluate_all('(metas)=>metas.filter(m=>!m.media||matchMedia(m.media).matches).map(m=>m.content.toLowerCase())')
                        check(colors and all(c==color for c in colors),f'effective {theme}/{scheme} status bar color')
                    # Auto OS change without another applyTheme call.
                    page.emulate_media(color_scheme='dark')
                    page.wait_for_timeout(40)
                    check(page.locator('meta[name="theme-color"]').evaluate_all('(metas)=>metas.filter(m=>!m.media||matchMedia(m.media).matches).every(m=>m.content==="#0f1211")'),'auto reacts to OS changes')
                    for width,height in [(1280,900),(1024,768),(844,390),(390,844)]:
                        page.set_viewport_size({'width':width,'height':height})
                        page.evaluate('''() => { const r=document.documentElement;for(const [key,value] of Object.entries({top:24,right:38,bottom:22,left:46}))r.style.setProperty('--safe-'+key,value+'px');__display.setReading(2); }''')
                        page.wait_for_timeout(100)
                        page.evaluate('''() => { const b=document.querySelector('#menu-btn'); b.style.position='fixed'; b.style.left=(innerWidth-45)+'px';b.style.top=(innerHeight-45)+'px';__display.sessionMenu(b,__display.S.sessions[0]); }''')
                        check(contained(rect(page,'.popover'),width,height,46,24,38,22),f'session menu bounded at 200% in {width}x{height}')
                        page.evaluate('''() => { __display.closePopover();const b=document.querySelector('.msg-menu');b.style.position='fixed';b.style.left=(innerWidth-45)+'px';b.style.top=(innerHeight-45)+'px';__display.messageMenu(b,__display.S.messages[0],b.parentElement); }''')
                        check(contained(rect(page,'.popover'),width,height,46,24,38,22),f'message menu bounded at 200% in {width}x{height}')
                        page.evaluate("__display.closePopover();document.querySelector('#menu-btn').removeAttribute('style');document.querySelector('.msg-menu').removeAttribute('style');__display.setReading(1)")
                        page.wait_for_timeout(250)
                        # The pill represents paused reading with output below the viewport.
                        page.evaluate("__display.toast('Safe area toast');__display.pauseWithNewOutput()")
                        check(contained(rect(page,'.toasts'),width,height,46,24,38,22),f'toasts honor four simulated insets in {width}x{height}')
                        check(contained(rect(page,'#jump'),width,height,46,24,38,22),f'jump pill honors simulated insets in {width}x{height}: {rect(page, chr(35)+"jump")}')
                        check(rect(page,'.topbar')['y']>=0 and page.locator('.topbar').evaluate('(e)=>parseFloat(getComputedStyle(e).paddingTop)')>=24,'topbar respects top inset')
                        page.locator('#notes-panel').evaluate("e=>e.classList.remove('hidden')")
                        check(contained(rect(page,'#notes-panel'),width,height,46,24,38,22),f'activity drawer safe in {width}x{height}')
                        page.locator('#notes-panel').evaluate("e=>e.classList.add('hidden')")
                        page.locator('#ask-backdrop').evaluate("e=>e.classList.remove('hidden')")
                        check(contained(rect(page,'#ask-backdrop .modal'),width,height,46,24,38,22),f'modal safe in {width}x{height}')
                        page.locator('#ask-backdrop').evaluate("e=>e.classList.add('hidden')")
                        page.screenshot(path=str(OUT/f'screen-{width}x{height}.png'),full_page=True)
                        page.evaluate('__display.setReading(2)')
                        page.wait_for_timeout(250)
                        check(page.evaluate('document.documentElement.scrollWidth <= innerWidth'),f'200% page has no horizontal overflow in {width}x{height}')
                        for control in ('#theme','#menu-btn','#input','#send-btn'):
                            if page.locator(control).is_visible():
                                page.locator(control).scroll_into_view_if_needed()
                                check(contained(rect(page,control),width,height,46,24,38,22),f'{control} fits at 200% in {width}x{height}: {rect(page,control)}')
                        if page.locator('#menu-btn').is_visible():
                            page.locator('#menu-btn').click()
                            page.wait_for_timeout(220)
                            check(contained(rect(page,'#sidebar'),width,height,46,24,38,22),f'session drawer opens inside safe area at200% in {width}x{height}')
                            page.screenshot(path=str(OUT/f'screen-200-{width}x{height}.png'),full_page=True)
                            page.locator('#menu-btn').click()
                            page.wait_for_timeout(220)
                            check(not page.locator('#sidebar').evaluate('(e)=>e.classList.contains("open")'), 'session drawer closes through menu button')
                        check(page.locator('#sidebar').evaluate('(e)=>getComputedStyle(e).visibility')=='hidden','closed drawer descendants are hidden')
                        check(rect(page,'#sidebar')['right']<=1, f'closed sidebar clears safe area at 200% in {width}x{height}')
                        page.screenshot(path=str(OUT/f'screen-200-closed-{width}x{height}.png'),full_page=True)
                        page.locator('#toasts').evaluate('e=>e.replaceChildren()')
                    # Deep live calls must retain readable names and timing, even
                    # with larger browser text and either writing direction.
                    page.set_viewport_size({'width':360,'height':800})
                    page.evaluate("__display.setReading(2);document.documentElement.style.setProperty('--safe-left','0px');document.documentElement.style.setProperty('--safe-right','0px')")
                    for direction in ('ltr', 'rtl'):
                        for depth in (8, 100, 0):
                            page.evaluate("""({direction,depth}) => {
                              document.documentElement.dir=direction;
                              __display.S.liveTools=[{name:'nested_tool_with_a_long_name',depth,status:'running',invocationId:'deep-fixture',arguments:'{}',elapsedMs:1250}];
                              __display.renderLive();
                            }""", {'direction':direction,'depth':depth})
                            row=page.locator('[data-live-tool="deep-fixture"]')
                            row.scroll_into_view_if_needed()
                            geometry=row.evaluate("""e=>{
                              const r=e.getBoundingClientRect(), p=e.parentElement.getBoundingClientRect();
                              const n=e.querySelector('.tname'),t=e.querySelector('.tstat');
                              const inside=x=>{const b=x.getBoundingClientRect();return b.left>=r.left && b.right<=r.right && b.top>=r.top && b.bottom<=r.bottom;};
                              return {rowFits:r.left>=p.left-1 && r.right<=p.right+1, noOverflow:e.scrollWidth<=e.clientWidth, nameFits:inside(n)&&n.scrollWidth<=n.clientWidth, timeFits:inside(t), indent:parseFloat(getComputedStyle(e).marginInlineStart), parentWidth:p.width, depthLabel:e.querySelector('[data-live-depth]').getAttribute('aria-label')};
                            }""")
                            check(geometry['rowFits'] and geometry['noOverflow'],f'depth {depth} live row fits 360px at 200% in {direction}: {geometry}')
                            check(geometry['nameFits'] and geometry['timeFits'],f'depth {depth} full tool name and timing remain visible in {direction}')
                            check(geometry['indent']<=geometry['parentWidth']*.15+1,f'depth {depth} visual indent capped in {direction}')
                            if depth:
                                check(geometry['depthLabel']==f'Nesting depth {depth}',f'actual depth {depth} remains accessible in {direction}')
                    page.evaluate("document.documentElement.removeAttribute('dir');__display.S.liveTools=[];__display.renderLive()")
                    page.set_viewport_size({'width':1280,'height':900})
                    page.evaluate("__display.setReading(1);__display.applyTheme('dark');document.querySelectorAll('details').forEach(e=>e.open=false)")
                    # Printing from Settings still renders the current Session.
                    page.evaluate("document.querySelector('#view-chat').classList.add('hidden');document.querySelector('#view-settings').classList.remove('hidden')")
                    page.emulate_media(media='print')
                    check(page.locator('#composer').evaluate('(e)=>getComputedStyle(e).display')=='none','print hides composer')
                    check(page.locator('.topbar').evaluate('(e)=>getComputedStyle(e).display')=='none','print hides chrome')
                    check(page.locator('#transcript').evaluate('(e)=>getComputedStyle(e).overflow')=='visible','print transcript has no viewport clipping')
                    check(page.locator('.codeblock pre').first.evaluate('(e)=>getComputedStyle(e).whiteSpace') in ('pre-wrap','break-spaces'),'print code wraps instead of clipping')
                    diagram=page.locator('.diagram img').first
                    check(diagram.evaluate('(e)=>e.complete && e.naturalWidth>0'), 'print diagram decoded')
                    check(diagram.evaluate('(e)=>e.getBoundingClientRect().width<=e.closest(".diagram").getBoundingClientRect().width+1'),'print diagram width fits container')
                    page.pdf(path=str(OUT/'transcript.pdf'),format='A4',print_background=True)
                    subprocess.run(['pdftoppm','-f','1','-singlefile','-scale-to','1200','-png',str(OUT/'transcript.pdf'),str(OUT/'print-page-1')],check=True)
                    text_path=OUT/'transcript.txt'
                    subprocess.run(['pdftotext','-layout',str(OUT/'transcript.pdf'),str(text_path)],check=True)
                    text=text_path.read_text()
                    check(text.count('\f')>=3,'print creates multiple pages')
                    for i in range(12):
                        check(f'TURN_{i}' in text and f'ANSWER_{i}' in text and f'CODE_END_{i}' in text,f'PDF includes complete turn {i}')
                    check(''.join(text.split()).count('wide-code-') == (85 + 11 * 3) * 30, 'PDF preserves every long-code token across page boundaries')
                    check('display_fixture' in text,'closed tool chip summarized in PDF')
                    check('PRINT_SOURCE_FALLBACK' in text,'unfinished diagram source preserved in PDF')
                    check(page.locator('#view-chat').evaluate('(e)=>getComputedStyle(e).display')=='block','print exposes current Session while another view is active')
                    check(page.locator('#view-settings').evaluate('(e)=>getComputedStyle(e).display')=='none','print hides other active views')
                    check('Message Formless' not in text and 'Disconnect' not in text,'PDF excludes app chrome/composer')
                    page.screenshot(path=str(OUT/'print-layout.png'),full_page=True)
                    check(not errors,'no browser page errors: '+repr(errors))
                    context.close()
    finally:
        server.shutdown()
    print('Artifacts:',OUT)


if __name__=='__main__':
    run()
