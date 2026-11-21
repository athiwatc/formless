#!/usr/bin/env python3
"""Gregorian UI formatting and live document language in Chromium/WebKit.

Run: python relay/web-locale.browser-test.py [artifact-directory]
The full shipped UI uses local fixture replies; no daemon/provider operations.
WebKit is engine evidence, not physical Safari or screen-reader testing.
"""
import functools
import http.server
import json
import pathlib
import sys
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, browser_engines, chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
PASSES = 0
STAMP = '2026-12-31T20:04:05Z'


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
    const fixture1040={S,h,renderAll,uiLang,uiLocale,uiFormat,uiFixed,uiMoney,clock,relTime,onEvent,viewportVariable,next:'auto',calls:[]};
    request=async req=>{
      fixture1040.calls.push(req.type);
      if(req.type==='get_settings')return {tui:{language:fixture1040.next,animations:false},provider:{model:'fixture'},limits:{}};
      return null;
    };
    act=request;window.__locale1040=fixture1040;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def snapshot(page):
    return page.evaluate(r'''stamp=>{
      const d=__locale1040,language=d.uiLang(),locale=language==='th'?'th-TH-u-ca-gregory-nu-latn':'en-US-u-ca-gregory-nu-latn';
      const date=new Date(stamp),options={year:'numeric',month:'long',day:'numeric'};
      const renderedTime={hour:'2-digit',minute:'2-digit'};
      if(date.toDateString()!==new Date().toDateString())Object.assign(renderedTime,{year:'numeric',month:'short',day:'numeric'});
      const expectedDate=date.toLocaleString(locale,options),year=new Intl.DateTimeFormat(locale,{year:'numeric'}).format(date);
      const option=d.h('option',{value:12345.67},12345.67);
      d.viewportVariable('--locale-machine',12.125);
      return {language,locale:d.uiLocale(),html:document.documentElement.lang,
        date:d.uiFormat(date,options),expectedDate,year,
        full:d.uiFormat(date),expectedFull:date.toLocaleString(locale),
        clock:d.clock(stamp),expectedClock:date.toLocaleString(locale,{hour:'2-digit',minute:'2-digit'}),
        notes:document.querySelector('#notes time')?.textContent,
        transcript:document.querySelector('#transcript time [aria-hidden="true"]')?.textContent,
        expectedTranscript:date.toLocaleString(locale,renderedTime),
        number:d.uiFormat(1234567.89),expectedNumber:new Intl.NumberFormat(locale).format(1234567.89),
        money:d.uiMoney(1234.5678,4),expectedMoney:new Intl.NumberFormat(locale,{style:'currency',currency:'USD',minimumFractionDigits:4,maximumFractionDigits:4}).format(1234.5678),
        fixed:d.uiFixed(1234.5,2),expectedFixed:new Intl.NumberFormat(locale,{minimumFractionDigits:2,maximumFractionDigits:2,useGrouping:false}).format(1234.5),
        small:d.uiMoney(.000001,null),optionValue:option.value,optionLabel:option.textContent,
        expectedLabel:new Intl.NumberFormat(locale).format(12345.67),
        css:document.documentElement.style.getPropertyValue('--locale-machine'),
        raw:document.querySelector('#transcript .bubble')?.textContent,
        iso:date.toISOString(),invalid:d.uiFormat(new Date('invalid'))};
    }''', STAMP)


def assert_locale(page, expected, label):
    page.wait_for_function('(expected)=>document.documentElement.lang===expected', arg=expected)
    s = snapshot(page)
    check(s['language'] == expected and s['html'] == expected, label+' language metadata')
    check(s['locale'] == ('th-TH' if expected == 'th' else 'en-US')+'-u-ca-gregory-nu-latn', label+' explicit locale/calendar/digits')
    check(s['date'] == s['expectedDate'], label+' localized date')
    check(s['year'] in s['date'] and str(int(s['year'])+543) not in s['date'], label+' Gregorian year')
    check(s['full'] == s['expectedFull'], label+' localized full timestamp')
    check(s['clock'] == s['expectedClock'], label+' localized clock')
    check(s['notes'] == s['expectedClock'], label+' rendered note timestamp refreshed')
    check(s['transcript'] == s['expectedTranscript'], label+' rendered transcript timestamp refreshed')
    check(s['number'] == s['expectedNumber'], label+' grouped numeric display')
    check(s['money'] == s['expectedMoney'], label+' localized USD and precision')
    check(s['fixed'] == s['expectedFixed'] and '0.000001' in s['small'], label+' decimals and small prices preserved')
    check(s['optionValue'] == '12345.67' and s['optionLabel'] == s['expectedLabel'], label+' machine form value remains ASCII')
    check(s['css'] == '12.125px' and s['iso'] == STAMP.replace('Z','.000Z'), label+' CSS/ISO serialization stays machine-readable')
    check('Raw source 12345.67' in s['raw'] and s['invalid'] == '—', label+' source text and invalid-date fallback')


def run_engine(p, engine, url, out):
    start = PASSES
    browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox']) if engine == 'chromium' else p.webkit.launch(executable_path=p.webkit.executable_path, headless=True)
    contexts = []
    for native, zone in [('en-US','UTC'), ('th-TH','Asia/Bangkok')]:
        context = browser.new_context(locale=native, timezone_id=zone, reduced_motion='reduce', viewport={'width':1100,'height':800})
        context.route('**/app.js', expose)
        page = context.new_page()
        page.set_default_timeout(5000)
        errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(url)
        page.wait_for_function('!!window.__locale1040')
        check(page.locator('html').get_attribute('lang') == ('th' if native.startswith('th') else 'en'), engine+' startup language '+native)
        page.evaluate(r'''stamp=>{
          const d=__locale1040;d.S.view='chat';d.S.current='locale-fixture';d.S.daemonOnline=true;
          d.S.ws={readyState:1};d.S.channel={local:true};d.S.local=true;
          d.S.sessions=[{id:'locale-fixture',name:'Locale fixture',updated_at:stamp}];
          d.S.messages=[{id:'locale-message',seq:1,role:'assistant',content:'Raw source 12345.67',created_at:stamp,meta:'{}'}];
          d.S.notes=[{at:Date.parse(stamp),text:'Locale fixture note',err:false}];
          document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');d.renderAll();
        }''', STAMP)
        for setting in ['en','th','auto']:
            page.evaluate('(setting)=>{__locale1040.next=setting;__locale1040.onEvent({type:"settings_changed"})}', setting)
            page.wait_for_function('(setting)=>__locale1040.S.settings?.tui?.language===setting', arg=setting)
            effective = ('th' if native.startswith('th') else 'en') if setting == 'auto' else setting
            assert_locale(page, effective, f'{engine} {native}/{zone} setting={setting}')
        changed = 'en-US' if native.startswith('th') else 'th-TH'
        page.evaluate('(language)=>{Object.defineProperty(navigator,"language",{configurable:true,get:()=>language});window.dispatchEvent(new Event("languagechange"));}', changed)
        assert_locale(page, 'th' if changed.startswith('th') else 'en', engine+' Auto languagechange')
        out.mkdir(parents=True, exist_ok=True)
        page.screenshot(path=str(out / f'{native}.png'))
        check(not errors, engine+' no page errors '+repr(errors))
        contexts.append({'locale':native,'timezone':zone,'errors':errors})
        context.close()
    report={'engine':engine,'version':browser.version,'assertions':PASSES-start,'contexts':contexts,'physicalSafari':False}
    (out/'report.json').write_text(json.dumps(report,indent=2))
    print('ENGINE',json.dumps(report),flush=True)
    browser.close()


def run():
    out=pathlib.Path(sys.argv[1]) if len(sys.argv)>1 else artifact_dir('/tmp/formless-locale-check')
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHandler,directory=str(ROOT/'web')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with sync_playwright() as p:
            for engine in browser_engines():
                run_engine(p,engine,f'http://127.0.0.1:{server.server_port}/',out/engine)
        print(f'PASS total: {PASSES}; artifacts: {out}',flush=True)
    finally:
        server.shutdown()


if __name__=='__main__':
    run()
