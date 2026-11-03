#!/usr/bin/env python3
"""Generated Settings inventory and typed edits in real Chromium/WebKit.

First export the actual Rust payload with FORMLESS_SETTINGS_FIXTURE=/path/file
cargo test -p formless-core --test settings_inventory --locked. Then run:
python relay/web-settings.browser-test.py /artifact-dir /path/file
All requests below are local fixture replies; no daemon/provider/Tool operation runs.
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


def native_select_key(page, selector, key):
    # Native closed-select key semantics vary by OS. Compare the application
    # field with an unhandled clone in the same modal, and independently prove
    # that application handlers did not prevent or intercept the key.
    field = page.locator(selector)
    field.evaluate("""e => {const ref=e.cloneNode(true);ref.id='native-select-reference';ref.value=e.value;
      window.__nativeSelectKey=null;ref.addEventListener('keydown',event=>window.__nativeSelectKey=event,{once:true});e.parentElement.append(ref);}""")
    reference = page.locator('#native-select-reference')
    reference.focus();page.keyboard.press(key)
    expected = reference.evaluate("e=>({value:e.value,key:window.__nativeSelectKey?.key,prevented:window.__nativeSelectKey?.defaultPrevented})")
    reference.evaluate('(e)=>e.remove()')
    field.focus();field.evaluate("e=>{window.__appSelectKey=null;e.addEventListener('keydown',event=>window.__appSelectKey=event,{once:true});}")
    page.keyboard.press(key)
    actual = field.evaluate("e=>({value:e.value,key:window.__appSelectKey?.key,prevented:window.__appSelectKey?.defaultPrevented})")
    assert expected['key']==key and expected['prevented'] is False, ('native reference key',expected)
    assert actual['key']==key and actual['prevented'] is False, ('application intercepted native key',actual)
    assert actual['value']==expected['value'], ('application differs from native select',actual,expected)


def check(value, label):
    global PASSES
    assert value, label
    PASSES += 1
    print('PASS', label, flush=True)


def expose(route, payload):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hooks = '\nconst original822=' + json.dumps(payload) + ';\n' + r'''
    const fixture822={S,renderAll,renderSettings,filterSettings,setReading,settingsRows,
      data:structuredClone(original822),calls:[],reject:false};
    request=async req=>{
      fixture822.calls.push(structuredClone(req));
      if(req.type==='get_settings')return structuredClone(fixture822.data);
      if(req.type==='status')return {secure:true,protocol:1,pid:42,sessions:0};
      if(req.type==='set_setting'){
        if(!Object.hasOwn(fixture822.data.values,req.key))throw new Error('read-only fixture key');
        if(fixture822.reject)throw new Error('Fixture daemon rejected this value');
        fixture822.data.values[req.key]=structuredClone(req.value);
        const [prefix,leaf]=req.key.split('.');
        if(fixture822.data[prefix])fixture822.data[prefix][leaf]=structuredClone(req.value);
        return {applies:(fixture822.data.on_tui_start||[]).includes(req.key)?'on_tui_start':'now'};
      }
      if(req.type==='list_providers')return {providers:[],roles:{}};
      return null;
    };
    window.__settings822=fixture822;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def calls(page):
    return page.evaluate('__settings822.calls.filter(r=>r.type==="set_setting")')


def button(page, key):
    page.locator('#settings-filter').fill(key)
    result = page.get_by_role('button', name='Edit '+key, exact=True)
    result.scroll_into_view_if_needed()
    return result


def edit_text(page, key, text, expected):
    before = len(calls(page))
    button(page, key).click()
    page.locator('#ask-input').fill(text)
    page.get_by_role('button', name='Save', exact=True).click()
    page.wait_for_function('(before)=>__settings822.calls.filter(r=>r.type==="set_setting").length>before', arg=before)
    page.wait_for_function('(key)=>__settings822.S.settings.values[key]===__settings822.data.values[key] || JSON.stringify(__settings822.S.settings.values[key])===JSON.stringify(__settings822.data.values[key])', arg=key)
    check(calls(page)[-1] == {'type':'set_setting','key':key,'value':expected}, 'typed edit '+key)


def run_engine(p, engine, url, artifacts, payload):
    start = PASSES
    artifacts.mkdir(parents=True, exist_ok=True)
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', headless=True, args=['--no-sandbox']) if engine == 'chromium' else p.webkit.launch(executable_path=p.webkit.executable_path, headless=True)
    context = browser.new_context(viewport={'width':1280,'height':900})
    context.route('**/app.js', lambda route: expose(route, payload))
    page = context.new_page()
    page.set_default_timeout(7000)
    errors = []
    page.on('pageerror', lambda error: errors.append(str(error)))
    page.goto(url)
    page.wait_for_function('!!window.__settings822')
    page.evaluate(r'''()=>{const d=__settings822;d.S.view='settings';d.S.channel={};d.S.daemonOnline=true;d.S.local=true;
      d.S.settings=structuredClone(d.data);d.S.status={secure:true,protocol:1,pid:42,sessions:0};
      document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');d.renderAll();}''')
    for key in payload['values']:
        row = page.locator('#settings dt[data-setting-key="'+key+'"]')
        check(row.count() == 1, engine+' one generated row '+key)
        check(page.get_by_role('button', name='Edit '+key, exact=True).count() == 1, engine+' editable '+key)
        check(row.locator('xpath=following-sibling::dd[1]').locator('.setting-desc').count() == 1, engine+' SPEC description '+key)
    for key in payload['read_only']:
        row = page.locator('#settings dt[data-setting-key="'+key+'"]')
        check(row.count() == 1, engine+' read-only discoverable '+key)
        check(row.locator('xpath=following-sibling::dd[1]').get_by_role('button').count() == 0, engine+' no setter for '+key)
    text = page.locator('#settings').inner_text()
    for sentinel in ['HeaderSentinel822','QuerySentinel822','PatternSentinel822','McpEnvSentinel822']:
        check(sentinel not in text, engine+' private sentinel excluded '+sentinel)
    check('Manage MCP servers above.' in text and 'relay and MCP servers are changed in config.toml' not in text, engine+' accurate MCP/footer guidance')
    for word in ['compaction','limits','daemon','budget']:
        page.locator('#settings-filter').fill(word)
        shown = page.locator('#settings dt:not(.hidden)').evaluate_all('(rows)=>rows.map(r=>r.title)')
        check(bool(shown), engine+' filter finds '+word)
        if word != 'budget':
            check(any(key.startswith(word+'.') for key in shown), engine+' filter exposes '+word+' keys')
    page.locator('#settings-filter').fill('')
    page.screenshot(path=str(artifacts/'settings-desktop.png'))
    for expected in [False, True, False]:
        before = len(calls(page))
        button(page, 'naming.enabled').click()
        page.wait_for_function('(before)=>__settings822.calls.filter(r=>r.type==="set_setting").length>before', arg=before)
        page.wait_for_function('(value)=>__settings822.S.settings.values["naming.enabled"]===value', arg=expected)
        check(calls(page)[-1]['value'] is expected, engine+' repeated bool edit '+str(expected))
    edit_text(page,'provider.max_retries','9',9)
    edit_text(page,'limits.run_usd','2.5',2.5)
    edit_text(page,'limits.run_usd','',None)
    edit_text(page,'provider.fallback_models','["first,comma","second"]',['first,comma','second'])
    edit_text(page,'tui.pane_widths','[20,30]',[20,30])
    edit_text(page,'provider.prices','{"fixture":{"input":0.001,"output":0.002}}',{'fixture':{'input':0.001,'output':0.002}})
    edit_text(page,'naming.model','fixture-title','fixture-title')
    before = len(calls(page))
    button(page,'tui.on_start').click()
    check(page.evaluate('document.activeElement.id')=='setting-choice', engine+' choice initially focused')
    native_select_key(page,'#setting-choice','End')
    check(page.evaluate('document.activeElement.id')=='setting-choice', engine+' native choice keyboard navigation retains focus')
    # Select a known typed value separately from OS-specific End behavior.
    page.locator('#setting-choice').select_option('picker')
    check(page.locator('#setting-choice').input_value()=='picker', engine+' typed choice selected for Save')
    # Safari on macOS uses Option+Tab to include every native control without
    # changing the user's system keyboard-navigation preference.
    tab_key = 'Alt+Tab' if sys.platform == 'darwin' and engine == 'webkit' else 'Tab'
    page.keyboard.press(tab_key)
    page.keyboard.press(tab_key)
    check(page.evaluate('document.activeElement.id')=='ask-ok',engine+' choice keyboard reaches Save')
    page.keyboard.press('Enter')
    page.wait_for_function('(before)=>__settings822.calls.filter(r=>r.type==="set_setting").length>before',arg=before)
    page.wait_for_function('__settings822.S.settings.values["tui.on_start"]==="picker"')
    check(calls(page)[-1]['value']=='picker', engine+' choice submits typed value')
    check(page.locator('#notes').inner_text().find('when the TUI next starts')>=0, engine+' startup-only setting notice')
    # Cancellation and invalid local syntax never reach the daemon boundary.
    before = len(calls(page))
    button(page,'naming.model').click();page.locator('#ask-input').fill('cancelled draft');page.keyboard.press('Escape')
    check(len(calls(page))==before,engine+' Escape cancels without submit')
    check(page.evaluate('document.activeElement.getAttribute("aria-label")')=='Edit naming.model',engine+' Escape restores invoking Edit')
    for key,text in [('provider.max_retries','Infinity'),('provider.prices','{broken')]:
        button(page,key).click();page.locator('#ask-input').fill(text);page.get_by_role('button',name='Save',exact=True).click()
        check(len(calls(page))==before,engine+' invalid syntax does not submit '+key)
    page.evaluate('__settings822.reject=true')
    button(page,'provider.max_retries').click();page.locator('#ask-input').fill('-1');page.get_by_role('button',name='Save',exact=True).click()
    page.wait_for_function('document.querySelector("#toasts").textContent.includes("Fixture daemon rejected this value")')
    check(page.evaluate('__settings822.data.values["provider.max_retries"]')==9,engine+' rejected server value preserves prior setting')
    page.evaluate('__settings822.reject=false')
    # A new documented key renders without adding a UI row definition.
    page.evaluate('''()=>{const d=__settings822;d.data.values['future.local_flag']=false;d.data.docs['future.local_flag']={description:'Future fixture flag',default:false};d.S.settings=structuredClone(d.data);d.renderSettings();}''')
    button(page,'future.local_flag').click()
    page.wait_for_function('__settings822.S.settings.values["future.local_flag"]===true')
    check(calls(page)[-1]['key']=='future.local_flag',engine+' future documented key generated and editable')
    # Filter and modal controls remain contained on a narrow viewport at 200%.
    page.set_viewport_size({'width':390,'height':844})
    page.evaluate('__settings822.setReading(2)')
    button(page,'provider.prices')
    check(page.evaluate('document.documentElement.scrollWidth<=innerWidth'),engine+' no page overflow at 200%/390px')
    check(page.locator('#settings').evaluate('(e)=>e.scrollWidth<=e.clientWidth+1'),engine+' generated settings contain long keys and JSON')
    rect = button(page,'provider.prices').bounding_box()
    check(rect['x']>=0 and rect['x']+rect['width']<=390,engine+' Edit fits at 200%')
    page.screenshot(path=str(artifacts/'settings-mobile-200.png'))
    button(page,'tui.theme').click()
    select = page.locator('#setting-choice').bounding_box()
    check(select['x']>=0 and select['x']+select['width']<=390,engine+' choice modal fits at 200%')
    page.keyboard.press('Escape')
    # Older summaries expose values without pretending to know the allow-list.
    page.evaluate('''()=>{const d=__settings822;const old=structuredClone(d.data);delete old.values;delete old.read_only;delete old.choices;d.S.settings=old;document.querySelector('#settings-filter').value='';d.renderSettings();}''')
    check(page.locator('#settings .setting-readonly').count()>10,engine+' legacy summary remains readable')
    check(page.locator('#settings button[aria-label^="Edit "]').count()==0,engine+' legacy summary grants no inferred edit rights')
    check('Update the daemon' in page.locator('#settings').inner_text(),engine+' legacy metadata upgrade guidance')
    check(not errors,engine+' no page errors: '+repr(errors))
    report={'engine':engine,'version':browser.version,'assertions':PASSES-start,'errors':errors,'physicalSafari':False,'projectedKeys':len(payload['values'])}
    (artifacts/'report.json').write_text(json.dumps(report,indent=2));print('ENGINE',json.dumps(report),flush=True)
    browser.close()


def run():
    artifacts=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else '/tmp/formless-settings-check')
    payload=json.loads(pathlib.Path(sys.argv[2] if len(sys.argv)>2 else '/workspace/scratch/formless-822-settings-fixture.json').read_text())
    artifacts.mkdir(parents=True,exist_ok=True)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHandler,directory=str(ROOT/'web')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    try:
        with sync_playwright() as p:
            for engine in ['chromium','webkit']:
                run_engine(p,engine,f'http://127.0.0.1:{server.server_port}/',artifacts/engine,payload)
    finally:
        server.shutdown()
    print('PASS TOTAL',PASSES,flush=True)


if __name__=='__main__':
    run()
