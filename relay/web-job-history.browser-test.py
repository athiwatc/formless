#!/usr/bin/env python3
"""Job history result states and navigation in the complete frontend.

Run: python relay/web-job-history.browser-test.py [artifact-directory]
Uses Chromium, local fixture replies and no daemon or job execution.
"""
import functools
import http.server
import pathlib
import sys
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else artifact_dir('/tmp/formless-1621-browser')
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
    const fixture1621={S,renderChrome,showJobHistory,ask,uiFormat,uiMoney,
      calls:[],replies:[],pending:[],rows:[
        {started_at:'2026-11-01T12:00:00Z',finished_at:'2026-11-01T12:00:02.600Z',
         trigger:'manual',status:'completed',cost:0,session_id:'history-session',detail:'first 日本語'},
        {started_at:'2026-10-31T12:00:00Z',finished_at:null,trigger:'scheduled',
         status:'running',cost:null,session_id:null,detail:'x'.repeat(500)}]};
    request=async req=>{
      fixture1621.calls.push(structuredClone(req));
      if(req.type==='job_runs'){
        const reply=fixture1621.replies.shift();
        if(reply==='reject')throw new Error('Fixture daemon unavailable');
        if(reply==='timeout')throw new Error('Request timed out');
        if(reply==='null-rejection')throw null;
        if(reply==='defer')return new Promise(resolve=>fixture1621.pending.push(resolve));
        return structuredClone(reply);
      }
      if(req.type==='get_session')return {session:S.sessions.find(s=>s.id===req.id),messages:[],queued_items:[]};
      if(req.type==='session_usage')return {runs:0,total:{}};
      if(req.type==='context_usage')return {used_tokens:0,context_tokens:1000};
      return {};
    };
    act=request;
    window.__history1621=fixture1621;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def open_history(page, reply):
    page.evaluate('(reply)=>__history1621.replies.push(reply)', reply)
    page.locator('#history-trigger').focus()
    page.keyboard.press('Enter')
    page.locator('#ask-backdrop:not(.hidden)').wait_for()


def close_history(page):
    page.locator('#ask-ok').click()
    page.locator('#ask-backdrop').wait_for(state='hidden')


def run():
    OUT.mkdir(parents=True, exist_ok=True)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'])
            context = browser.new_context(viewport={'width':900,'height':900}, locale='en-US', timezone_id='UTC', reduced_motion='reduce')
            context.route('**/app.js', expose)
            page = context.new_page()
            page.set_default_timeout(5000)
            errors = []
            page.on('pageerror', lambda e: errors.append(str(e)))
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__history1621')
            page.evaluate('''()=>{const d=__history1621;d.S.view='settings';d.S.channel={};d.S.daemonOnline=true;d.S.local=true;
              d.S.sessions=[{id:'history-session',name:'History fixture',tags:[],updated_at:'2026-11-01T12:00:00Z'}];
              document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');d.renderChrome();
              const b=document.createElement('button');b.id='history-trigger';b.type='button';b.textContent='Job history fixture';
              b.onclick=()=>d.showJobHistory({id:'fixture-job'});document.querySelector('#view-settings').prepend(b);}''')
            for failure in ['reject', 'timeout', 'null-rejection']:
                open_history(page, failure)
                check('Could not load Job history.' in page.locator('#ask-body [role=alert]').inner_text(), failure+' is a load error')
                check('It has not run yet.' not in page.locator('#ask-body').inner_text(), failure+' never becomes empty history')
                check(page.locator('#ask-form .modal-actions button:visible').all_text_contents()==['Close'], 'one dismiss action')
                page.evaluate('__history1621.replies.push([])')
                page.get_by_role('button', name='Retry', exact=True).click()
                page.wait_for_function('document.querySelector("#ask-body").textContent==="It has not run yet."')
                check(page.locator('#ask-body [role=alert]').count()==0, 'successful retry becomes genuine empty state')
                check(page.locator('#ask-ok').evaluate('e=>e===document.activeElement'), 'retry replacement preserves dialog focus')
                page.keyboard.press('Escape')
                page.locator('#ask-backdrop').wait_for(state='hidden')
                check(page.locator('#history-trigger').evaluate('e=>e===document.activeElement'), 'Escape restores invoker focus')
            rows = page.evaluate('__history1621.rows')
            open_history(page, rows)
            check(page.locator('#ask-body th[scope=col]').all_text_contents()==['Started','Trigger','Status','Duration','Cost','Session','Detail'], 'seven semantic headers')
            expected_date = page.evaluate('__history1621.uiFormat(new Date(__history1621.rows[0].started_at))')
            expected_cost = page.evaluate('__history1621.uiMoney(0,3)')
            cells = page.locator('#ask-body tbody tr').evaluate_all('(rows)=>rows.map(r=>[...r.cells].map(c=>c.textContent))')
            check(cells[0]==[expected_date,'manual','completed','3s',expected_cost,'Open','first 日本語'], 'first row retains full data and localized formatting')
            check(cells[1][1:]==['scheduled','running','…','—','','x'*500], 'second row retains order, absent fields and untruncated detail')
            for root in [16,32]:
                page.evaluate('(size)=>document.documentElement.style.fontSize=size+"px"', root)
                for width in [320,480,900]:
                    page.set_viewport_size({'width':width,'height':900})
                    page.evaluate('()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))')
                    bounds = page.locator('.job-history-table').evaluate('e=>{const r=e.getBoundingClientRect(),m=e.closest(".modal");return {left:r.left,right:r.right,width:r.width,scroll:e.scrollWidth,modalWidth:m.clientWidth,modalScroll:m.scrollWidth,cells:e.querySelectorAll("tbody td").length};}')
                    check(bounds['left']>=0 and bounds['right']<=width+1 and bounds['modalScroll']<=bounds['modalWidth']+1 and bounds['cells']==14, f'all cells locally contained at {width}px / root {root}px: {bounds}')
                    page.locator('.job-history-table').evaluate('e=>e.scrollLeft=e.scrollWidth')
                    check(page.locator('#ask-body tbody tr:first-child td:last-child').is_visible(), 'last column remains available')
            page.screenshot(path=str(OUT/'job-history-narrow.png'))
            page.set_viewport_size({'width':900,'height':900})
            page.evaluate('document.documentElement.style.fontSize="16px"')
            page.get_by_role('button', name='Open', exact=True).click()
            page.wait_for_function('__history1621.S.current==="history-session" && __history1621.calls.some(r=>r.type==="get_session")')
            check(page.evaluate('__history1621.calls.some(r=>r.type==="get_session"&&r.id==="history-session")'), 'Open uses existing Session navigation with full id')
            close_history(page)
            # Return to the fixture view without invoking Settings refresh requests.
            page.evaluate('__history1621.S.view="settings";__history1621.renderChrome()')
            open_history(page, 'reject')
            page.evaluate('__history1621.replies.push("defer")')
            page.get_by_role('button', name='Retry', exact=True).click()
            page.wait_for_function('__history1621.pending.length===1')
            check(page.get_by_role('button', name='Retrying…', exact=True).is_disabled(), 'pending retry cannot be submitted twice')
            close_history(page)
            page.evaluate('''()=>{const body=document.createElement('p');body.textContent='Keep this content';void __history1621.ask({title:'Later dialog',body,ok:'Done',hideCancel:true});}''')
            page.evaluate('__history1621.pending.shift()(__history1621.rows)')
            page.wait_for_timeout(50)
            check(page.locator('#ask-title').inner_text()=='Later dialog' and page.locator('#ask-body').inner_text()=='Keep this content', 'late retry cannot replace subsequent dialog')
            check(page.locator('#ask-ok').evaluate('e=>e===document.activeElement'), 'late reply preserves subsequent dialog focus')
            close_history(page)
            # Two history reads retain independent requests and the shared ask queue.
            page.evaluate('''()=>{const d=__history1621;d.replies.push([],d.rows);void d.showJobHistory({id:'queued-first'});void d.showJobHistory({id:'queued-second'});}''')
            page.wait_for_function('document.querySelector("#ask-body").textContent==="It has not run yet."')
            page.locator('#ask-ok').click()
            page.wait_for_function('document.querySelectorAll("#ask-body tbody tr").length===2')
            close_history(page)
            check(page.evaluate('__history1621.calls.filter(r=>r.type==="job_runs").every(r=>Object.keys(r).sort().join(",")==="id,type")'), 'history requests preserve protocol fields and limits')
            check(not errors, 'no browser errors: '+repr(errors))
            browser.close()
    finally:
        server.shutdown()
    print(f'{PASSES} Chromium Job history checks passed', flush=True)


if __name__ == '__main__':
    run()
