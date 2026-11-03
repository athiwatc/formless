#!/usr/bin/env python3
"""Live reduced-motion settings in actual shipped UI, Chromium and WebKit.

Run: python relay/web-motion.browser-test.py [artifact-directory]
Local fixture replies replace requests; no daemon/provider/Tool operations run.
WebKit is engine evidence, not physical Safari/device/assistive-technology proof.
"""
import functools
import http.server
import json
import os
import pathlib
import sys
import threading
from playwright.sync_api import sync_playwright

ROOT = pathlib.Path(__file__).resolve().parent
os.environ.setdefault('PLAYWRIGHT_BROWSERS_PATH', '/workspace/.formless-browsers')
PASSES = 0


class QuietHandler(http.server.SimpleHTTPRequestHandler):
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
    hooks = r'''
    const fixture992={S,onEvent,renderAll,next:true,fail:false,calls:[]};
    request=async req=>{
      fixture992.calls.push(req.type);
      if(req.type==='get_settings'){
        if(fixture992.fail)throw new Error('fixture settings unavailable');
        return {tui:{animations:fixture992.next},provider:{model:'fixture'},limits:{}};
      }
      return null;
    };
    act=request;
    window.__motion992=fixture992;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def state(page):
    return page.evaluate(r'''() => {
      const style=id=>getComputedStyle(document.querySelector(id));
      return {reduced:document.documentElement.hasAttribute('data-reduced-motion'),
        spinner:style('#motion-spinner').animationName,pulse:style('#motion-pulse').animationName,
        toast:style('#motion-toast').animationName,transition:style('#sidebar').transitionDuration,
        scroll:style('#motion-samples').scrollBehavior,
        symbol:getComputedStyle(document.querySelector('#motion-spinner'),'::before').content};
    }''')


def assert_motion(page, reduced, label):
    page.wait_for_function('(reduced)=>document.documentElement.hasAttribute("data-reduced-motion")===reduced', arg=reduced)
    page.wait_for_timeout(70)
    s = state(page)
    check(s['reduced'] == reduced, label+' effective attribute')
    check(s['spinner'] == ('none' if reduced else 'spin'), label+' spinner animation')
    check(s['pulse'] == ('none' if reduced else 'pulse'), label+' session pulse')
    check(s['toast'] == ('none' if reduced else 'rise'), label+' toast animation')
    check(s['transition'] == ('0s' if reduced else '0.18s'), label+' drawer transition')
    check(s['scroll'] == ('auto' if reduced else 'smooth'), label+' scroll behavior')
    check(('⋯' in s['symbol']) == reduced, label+' readable static spinner')


def settings(page, setting):
    before = page.evaluate('__motion992.calls.filter(x=>x==="get_settings").length')
    page.evaluate('(setting)=>{__motion992.next=setting;__motion992.onEvent({type:"settings_changed"});}', setting)
    page.wait_for_function('(before)=>__motion992.calls.filter(x=>x==="get_settings").length>before', arg=before)
    page.wait_for_function('(setting)=>__motion992.S.settings?.tui?.animations===setting', arg=setting)


def run_engine(p, engine, url, artifacts):
    start = PASSES
    artifacts.mkdir(parents=True, exist_ok=True)
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox']) if engine == 'chromium' else p.webkit.launch(executable_path=p.webkit.executable_path, headless=True)
    context = browser.new_context(viewport={'width':760, 'height':800}, reduced_motion='no-preference')
    context.route('**/app.js', expose)
    page = context.new_page()
    page.set_default_timeout(5000)
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(url)
    page.wait_for_function('!!window.__motion992')
    page.evaluate(r'''() => {
      const d=__motion992;d.S.view='chat';d.S.current='motion-fixture';d.S.channel={};d.S.daemonOnline=true;
      document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');
      d.renderAll();
      const samples=document.createElement('div');samples.id='motion-samples';
      samples.style.cssText='position:fixed;left:10px;top:10px;z-index:100;scroll-behavior:smooth';
      samples.innerHTML='<span class="spinner" id="motion-spinner"></span><div class="session"><span class="running" id="motion-pulse"></span></div><div class="toast" id="motion-toast">Motion fixture</div>';
      document.body.append(samples);
    }''')
    assert_motion(page, False, engine+' startup')
    for i, setting in enumerate([False, True, False, True]):
        settings(page, setting)
        assert_motion(page, not setting, engine+f' live toggle{i}')
    page.screenshot(path=str(artifacts / 'moving-preview.png'))
    page.wait_for_function('document.querySelector("#motion-spinner").getAnimations().some(a=>!a.pending&&a.startTime!==null&&a.currentTime>0)')
    a = page.locator('#motion-spinner').evaluate('(e)=>getComputedStyle(e).transform')
    page.wait_for_timeout(120)
    b = page.locator('#motion-spinner').evaluate('(e)=>getComputedStyle(e).transform')
    check(a != b, engine+' restored spinner actually advances')
    page.emulate_media(reduced_motion='reduce')
    assert_motion(page, True, engine+' OS override')
    settings(page, False)
    settings(page, True)
    assert_motion(page, True, engine+' setting cannot override OS reduction')
    page.emulate_media(reduced_motion='no-preference')
    assert_motion(page, False, engine+' OS restoration')
    settings(page, False)
    page.emulate_media(reduced_motion='reduce')
    page.emulate_media(reduced_motion='no-preference')
    assert_motion(page, True, engine+' shared preference survives OS roundtrip')
    # A failed settings fetch cannot silently re-enable an already reduced UI.
    page.evaluate('__motion992.fail=true;__motion992.onEvent({type:"settings_changed"})')
    page.wait_for_function('!!__motion992.S.settings?.error')
    page.emulate_media(reduced_motion='reduce')
    page.emulate_media(reduced_motion='no-preference')
    assert_motion(page, True, engine+' failed refresh retains reduction')
    before = page.locator('#motion-spinner').evaluate('(e)=>getComputedStyle(e).transform')
    page.wait_for_timeout(120)
    check(before == page.locator('#motion-spinner').evaluate('(e)=>getComputedStyle(e).transform'), engine+' reduced spinner remains still')
    artifacts.mkdir(parents=True, exist_ok=True)
    page.screenshot(path=str(artifacts / 'reduced.png'))
    page.evaluate('__motion992.fail=false')
    settings(page, True)
    assert_motion(page, False, engine+' recovery restores motion')
    page.screenshot(path=str(artifacts / 'moving.png'))
    check(not errors, engine+' no page errors: '+repr(errors))
    report={'engine':engine, 'version':browser.version, 'assertions':PASSES-start, 'errors':errors, 'physicalSafari':False}
    (artifacts / 'report.json').write_text(json.dumps(report, indent=2))
    print('ENGINE', json.dumps(report), flush=True)
    browser.close()


def run():
    artifacts = pathlib.Path(sys.argv[1] if len(sys.argv)>1 else '/tmp/formless-motion-check')
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(QuietHandler,directory=str(ROOT / 'web')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with sync_playwright() as p:
            for engine in ['chromium','webkit']:
                run_engine(p, engine, f'http://127.0.0.1:{server.server_port}/', artifacts / engine)
        print(f'PASS total: {PASSES}; artifacts: {artifacts}', flush=True)
    finally:
        server.shutdown()


if __name__ == '__main__':
    run()
