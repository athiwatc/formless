#!/usr/bin/env python3
"""Archived group order, filtering, lineage and keyboard semantics in Chromium.

Run: python relay/web-session-groups.browser-test.py
Uses the complete frontend and local Session data, with no daemon mutations.
"""
import functools
import http.server
import pathlib
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
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
    source = (ROOT/'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hooks = '''
    window.__groups1625={S,renderSessions,renderChrome};
    request=async()=>{throw new Error('Grouping must not issue a request');};
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def order(page):
    return page.locator('#sessions > .session').evaluate_all('rows=>rows.map(row=>row.classList.contains("archived-toggle")?"Archived":row.querySelector(".name").title)')


def run():
    server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(QuietHandler,directory=str(ROOT/'web')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with sync_playwright() as p:
            browser=p.chromium.launch(executable_path=chromium_executable(p),headless=True,args=['--no-sandbox'])
            page=browser.new_page(viewport={'width':1200,'height':900},reduced_motion='reduce')
            page.route('**/app.js',expose)
            errors=[]
            page.on('pageerror',lambda error:errors.append(str(error)))
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__groups1625')
            page.evaluate('''()=>{const d=__groups1625;d.S.view='chat';d.S.local=true;
              d.S.sessions=[
                {id:'archived-child',archived:true,forked_from:'active-parent',tags:['keep']},
                {id:'active-child',forked_from:'archived-parent',tags:['keep']},
                {id:'archived-parent',archived:true,tags:['keep']},
                {id:'active-parent',tags:['keep']},
                {id:'archived-nested',archived:true,forked_from:'archived-parent',tags:['other']},
                {id:'active-other',tags:['other']}].map(s=>({...s,name:s.id,updated_at:'2026-11-01T00:00:00Z',fork_point_seq:s.forked_from?2:null}));
              document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');d.renderChrome();d.renderSessions();}''')
            toggle=page.locator('.archived-toggle')
            check(order(page)==['active-child','active-parent','active-other','Archived'],'collapsed group hides only archived rows')
            check(toggle.get_attribute('aria-expanded')=='false' and 'Archived (3)' in toggle.inner_text(),'collapsed state and count exposed')
            toggle.focus();page.keyboard.press('Enter')
            check(order(page)==['active-child','active-parent','active-other','Archived','archived-child','archived-parent','archived-nested'],'interleaved flat input partitions around header without reordering')
            check(toggle.get_attribute('aria-expanded')=='true','expanded state exposed')
            toggle.focus();page.keyboard.press('Space')
            check(order(page)==['active-child','active-parent','active-other','Archived'],'Space collapses the group')
            page.get_by_role('button',name='#keep',exact=True).click()
            check('Archived (2)' in toggle.inner_text(),'archived count follows active tag filter')
            toggle.click()
            check(order(page)==['active-child','active-parent','Archived','archived-child','archived-parent'],'tag filter applies to both partitions')
            page.get_by_role('button',name='#keep',exact=True).click()
            page.get_by_role('button',name='Tree off',exact=True).click()
            check(order(page)==['active-child','active-parent','active-other','Archived','archived-child','archived-parent','archived-nested'],'tree renders every Session once in its partition')
            for name in ['active-child','archived-child']:
                row=page.locator('#sessions .name[title="'+name+'"]')
                check('parent filtered' in row.inner_text() and 'parent unavailable' not in row.inner_text(),'cross-group parent remains known for '+name)
            check('↳' in page.locator('#sessions .name[title="archived-nested"]').inner_text(),'within-group lineage keeps nested depth')
            page.get_by_role('button',name='#other',exact=True).click()
            check(order(page)==['active-other','Archived','archived-nested'],'tree tag filtering has no duplicates or missing rows')
            row=page.locator('#sessions .name[title="archived-nested"]')
            check('parent filtered' in row.inner_text() and 'parent unavailable' not in row.inner_text(),'filtered parent remains known in tree mode')
            toggle.focus();page.keyboard.press('Enter')
            check(order(page)==['active-other','Archived'],'tree collapse preserves active membership')
            page.evaluate('__groups1625.S.sessions=__groups1625.S.sessions.filter(s=>!s.archived);__groups1625.renderSessions()')
            check(toggle.count()==0,'no empty Archived control')
            check(not errors,'no browser errors: '+repr(errors))
            browser.close()
    finally:
        server.shutdown()
    print(f'{PASSES} Chromium Session grouping checks passed')


if __name__=='__main__':
    run()
