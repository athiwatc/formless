#!/usr/bin/env python3
"""Issue #817 keyboard/focus regressions against the complete shipped frontend.

Run: python relay/web-focus.browser-test.py [artifact-directory]
Requires Python Playwright and /usr/bin/chromium. A test-only script route
exposes closure helpers and supplies local UI fixture replies; no daemon,
provider, credentials, Tool execution or authorization changes are involved.
Chromium DOM and accessibility-tree evidence is not a physical screen-reader
or WebKit/Firefox compatibility test.
"""
import functools
import http.server
import pathlib
import sys
import threading
from playwright.sync_api import sync_playwright

from browser_fixture import artifact_dir, chromium_executable

ROOT = pathlib.Path(__file__).resolve().parent
OUT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else artifact_dir('/tmp/formless-817-browser')
OUT.mkdir(parents=True, exist_ok=True)
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


def expose_script(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    exposure = '''
    window.__focus817 = {
      S,h,openArtifact,openModelPicker,openSwitcher,showHelp,showRunFiles,modal,
      ask,closeAsk,showHarnessStatus,renderApproval,sessionMenu,messageMenu,closePopover,toast,onEvent,
      renderHistory,renderSessions,renderChrome,setView,
      openDialog,closeDialog,activeDialog,focusReturn,
      calls:[], deferredArtifact:false, deferredModels:false,
    };
    const fixture817 = window.__focus817;
    fixture817.focusChecks=0;
    const focusVisible817=focusVisible;
    focusVisible=(...args)=>{fixture817.focusChecks++;return focusVisible817(...args);};
    const reply817 = async (req) => {
      fixture817.calls.push(req.type);
      if(req.type==='get_artifact') {
        if (fixture817.pagedArtifact) {
          fixture817.pagedArtifact.offsets.push(req.offset);
          const chunks = {0: ['Preview ', 8], 8: ['ไทย ', 12], 12: ['end', 15]};
          const [text, end] = chunks[req.offset];
          return {text, end, total_chars:15, bytes:21};
        }
        const value={text:'Local artifact output',end:21,total_chars:21,bytes:21};
        if(fixture817.deferredArtifact) return new Promise(resolve=>fixture817.finishArtifact=()=>resolve(value));
        return value;
      }
      if(req.type==='list_models') {
        const value={models:['fixture-a','fixture-b'],roles:[],aliases:{},recent:[],current:{model:'fixture-a'}};
        if(fixture817.deferredModels) return new Promise(resolve=>fixture817.finishModels=()=>resolve(value));
        return value;
      }
      if(req.type==='set_session_pinned') {
        if(fixture817.deferredPin) return new Promise(resolve=>fixture817.finishPin=()=>resolve({...S.sessions.find(s=>s.id===req.id),pinned:req.pinned}));
        return {...S.sessions.find(s=>s.id===req.id),pinned:req.pinned};
      }
      if(req.type==='run_files') return {run_id:'fixture-run',files:[{path:'example.txt',status:'unchanged',revertible:false}],untracked_writes_possible:false};
      if(req.type==='list_tools'||req.type==='list_modules') return [];
      if(req.type==='list_harnesses') return ['claude','codex','pi','opencode','antigravity','acp','muse'].map(kind=>({
        kind,installed:true,signed_in:null,detail:'Long harness probe detail. '.repeat(12)}));
      return null;
    };
    act = reply817;
    request = reply817;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end] + exposure + source[end:])


def fixture(page):
    page.evaluate('''() => {
      const d=__focus817;d.S.current='focus-session';d.S.channel={};d.S.daemonOnline=true;
      d.S.view='tools';d.S.settings=null;
      d.S.sessions=[{id:'focus-session',name:'Focus fixture',tags:[],updated_at:'2026-11-02T12:00:00Z',model:'fixture-a'},
        {id:'focus-second',name:'Second fixture',tags:[],updated_at:'2026-11-01T12:00:00Z'}];
      d.S.messages=[{id:'focus-message',seq:1,role:'user',content:'Focus fixture message',created_at:'2026-11-02T12:00:00Z'}];
      document.querySelector('#app').classList.remove('hidden');document.querySelector('#pair').classList.add('hidden');
      d.renderChrome();d.renderSessions();d.renderHistory();
      for(const view of ['tools','chat']) {
        const host=document.createElement('div');host.className='view-head';host.id='fixture-'+view+'-controls';
        const button=(id,label,run)=>{const e=document.createElement('button');e.id=id;e.textContent=label;e.type='button';e.className='btn small';e.onclick=run;host.append(e);};
        button('fixture-'+view+'-open','Open fixture',()=>{});
        button('fixture-'+view+'-outside','Outside fixture',()=>{});
        document.querySelector('#view-'+view).prepend(host);
      }
    }''')


def invoke(page, expression, trigger='#fixture-tools-open'):
    page.locator(trigger).evaluate('(e,code)=>{e.onclick=()=>{void eval(code)};}', expression)
    page.locator(trigger).focus()
    page.keyboard.press('Enter')
    page.wait_for_function('!!__focus817.activeDialog()')
    page.wait_for_timeout(25)


def active_within(page, selector):
    return page.evaluate('(s)=>!!document.querySelector(s)?.contains(document.activeElement)', selector)


def focus_cycle(page, selector, label):
    check(active_within(page, selector), label + ' starts with focus inside')
    check(page.locator('#app').evaluate('(e)=>e.inert'), label + ' makes app inert')
    check(page.locator(selector).evaluate('(e)=>!!(e.matches("[role=dialog],[role=alertdialog]")?e:e.querySelector("[role=dialog],[role=alertdialog]"))'), label + ' has dialog semantics')
    check(page.locator(selector).evaluate('''e=>{const d=e.matches('[role=dialog],[role=alertdialog]')?e:e.querySelector('[role=dialog],[role=alertdialog]');return !!(d.getAttribute('aria-label')||((d.getAttribute('aria-labelledby')||'').split(/\\s+/).map(id=>document.getElementById(id)?.textContent||'').join(' '))).trim()}'''), label + ' has an accessible name')
    count = page.locator(selector).evaluate('''e=>[...e.querySelectorAll('button,input,select,textarea,a[href],summary,[tabindex]')].filter(x=>!x.disabled&&!x.closest('.hidden')&&x.getClientRects().length&&x.tabIndex>=0).length''')
    for key in ('Tab', 'Shift+Tab'):
        for _ in range(max(3, count * 2 + 1)):
            page.keyboard.press(key)
            check(active_within(page, selector), label + ' contains ' + key)
    page.locator('#fixture-tools-outside').evaluate('(e)=>e.focus()')
    check(active_within(page, selector), label + ' blocks programmatic background focus')
    check(page.evaluate('document.activeElement!==document.body'), label + ' never focuses body')


def ax_ignored(cdp, selector):
    doc = cdp.send('DOM.getDocument')
    node = cdp.send('DOM.querySelector', {'nodeId': doc['root']['nodeId'], 'selector': selector})
    nodes = cdp.send('Accessibility.getPartialAXTree', {'nodeId': node['nodeId'], 'fetchRelatives': False})['nodes']
    return bool(nodes and nodes[0]['ignored'])


def close_and_return(page, selector, trigger, label, key='Escape'):
    page.keyboard.press(key)
    page.wait_for_function('(s)=>!document.querySelector(s)||document.querySelector(s).classList.contains("hidden")', arg=selector)
    page.wait_for_timeout(25)
    check(page.locator(trigger).evaluate('(e)=>e===document.activeElement'), label + ' restores invoking control')
    check(not page.locator('#app').evaluate('(e)=>e.inert'), label + ' restores app interactivity')


def menu_check(page, trigger, label):
    page.locator(trigger).focus()
    page.keyboard.press('Enter')
    page.wait_for_selector('.popover')
    check(page.locator(trigger).get_attribute('aria-expanded') == 'true', label + ' announces expanded')
    items = page.locator('.popover [role=menuitem]')
    count = items.count()
    check(count > 1 and count == page.locator('.popover button').count(), label + ' gives all actions menuitem roles')
    check(items.first.evaluate('(e)=>e===document.activeElement'), label + ' opens at first item')
    check(items.evaluate_all('(items)=>items.filter(e=>e.tabIndex===0).length') == 1, label + ' has one roving tab stop')
    for key, index in [('ArrowDown', 1), ('End', count - 1), ('ArrowDown', 0), ('ArrowUp', count - 1), ('Home', 0)]:
        page.keyboard.press(key)
        check(items.nth(index).evaluate('(e)=>e===document.activeElement'), label + ' navigates ' + key)
    page.keyboard.press('Escape')
    check(page.locator('.popover').count() == 0, label + ' Escape closes')
    check(page.locator(trigger).evaluate('(e)=>e===document.activeElement'), label + ' Escape restores trigger')
    check(page.locator(trigger).get_attribute('aria-expanded') is None, label + ' clears aria-expanded')
    check(page.locator(trigger).get_attribute('aria-controls') is None, label + ' clears aria-controls')
    for key in ('Tab', 'Shift+Tab'):
        page.keyboard.press('Enter')
        page.wait_for_selector('.popover')
        page.keyboard.press(key)
        check(page.locator('.popover').count() == 0, label + ' ' + key + ' closes')
        check(page.locator(trigger).evaluate('(e)=>e===document.activeElement'), label + ' ' + key + ' returns focus')
    page.keyboard.press('ArrowUp')
    page.wait_for_selector('.popover')
    check(page.locator('.popover [role=menuitem]').last.evaluate('(e)=>e===document.activeElement'), label + ' trigger ArrowUp opens last')
    page.keyboard.press('Escape')


def feedback_checks(page, cdp):
    # Static pending presentation and harmless local feedback only. No approval
    # action, authorization request, Tool execution or grant is performed.
    page.evaluate("""()=>{const d=__focus817;d.calls=[];d.feedbackActions=0;
      d.S.approvals=[{id:'feedback-approval',kind:'promotion',capabilities:[],tool_name:'feedback_fixture',deciding:true}];d.renderApproval();
      d.toast('Broad access fixture: press again to grant it',true);
      d.toast('Local feedback action',false,{label:'Fixture feedback action',ms:60000,run:()=>d.feedbackActions++});}""")
    page.wait_for_selector('#modal-backdrop:not(.hidden)')
    check(not page.locator('#toasts').evaluate('(e)=>e.inert'),'modal feedback region remains non-inert')
    check(not ax_ignored(cdp,'#toasts [role=alert]'),'modal error feedback remains in accessibility tree')
    action=page.get_by_role('button',name='Fixture feedback action',exact=True)
    check(action.evaluate('(e)=>{const r=e.getBoundingClientRect();return e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2));}'),'feedback action is topmost above approval backdrop')
    action.focus();check(action.evaluate('(e)=>document.activeElement===e'),'dialog focus boundary permits feedback action')
    action.click();check(page.evaluate('__focus817.feedbackActions')==1,'feedback action is genuinely clickable over modal')
    page.evaluate("""()=>{const d=__focus817;d.modal('Nested feedback fixture',d.h('p',null,'Harmless display-only fixture'));
      d.toast('Keyboard feedback',false,{label:'Keyboard feedback action',ms:60000,run:()=>d.feedbackActions++});}""")
    keyboard_action=page.get_by_role('button',name='Keyboard feedback action',exact=True)
    check(keyboard_action.evaluate('(e)=>{const r=e.getBoundingClientRect();return e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2));}'),'feedback remains above nested dialog')
    page.locator('.backdrop[aria-label="Nested feedback fixture"] button').last.focus();page.keyboard.press('Tab')
    dismissal=page.locator('#toasts .toast.err button[aria-label="Dismiss notification"]')
    check(dismissal.evaluate('(e)=>document.activeElement===e'),'Tab from nested modal reaches persistent toast dismissal')
    page.keyboard.press('Tab')
    check(keyboard_action.evaluate('(e)=>document.activeElement===e'),'Tab from toast dismissal reaches feedback action')
    page.keyboard.press('Enter');check(page.evaluate('__focus817.feedbackActions')==2,'feedback action supports keyboard activation')
    check(page.evaluate('__focus817.calls.length')==0,'feedback fixtures never dispatch approval or Tool requests')
    page.evaluate("""()=>{const d=__focus817;d.closeDialog(d.activeDialog().node);d.S.approvals=[];d.renderApproval();
      document.querySelectorAll('#toasts .toast .toast-dismiss').forEach(button=>button.click());}""")
    check(not page.locator('#app').evaluate('(e)=>e.inert'),'closing dialogs restores normal app interactivity')


def click_away_checks(page):
    page.evaluate("""()=>{const d=__focus817;d.setView('chat');d.renderSessions();
      const blank=document.createElement('div');blank.id='focus-click-away';blank.style.cssText='position:fixed;left:0;top:0;width:12px;height:12px;z-index:5000';document.body.append(blank);}""")
    row=page.locator('.session:not(.active)').first;menu=row.locator('.menu')
    row.hover();menu.focus();check(menu.evaluate('(e)=>e===document.activeElement'),'inactive Session menu genuinely focused before click-away')
    page.mouse.click(4,4);page.mouse.move(900,700);page.wait_for_timeout(40)
    check(page.evaluate('document.activeElement===document.body'),'blank pointer click intentionally leaves focus on body')
    check(menu.evaluate('(e)=>getComputedStyle(e).visibility')=='hidden','previous menu is hidden by hover CSS only')
    page.evaluate("""()=>{const d=__focus817;d.focusChecks=0;d.S.running.add(d.S.current);
      for(let i=0;i<50;i++)d.onEvent({type:'message_delta',session_id:d.S.current,delta:'harmless focus stream '});}""")
    page.wait_for_timeout(180)
    check(page.evaluate('document.activeElement===document.body'),'streamed deltas do not undo deliberate click-away')
    check(page.evaluate('__focus817.focusChecks')==0,'click-away streaming performs no focus visibility/layout probes')
    row.hover();menu.focus();menu.evaluate('(e)=>e.blur()');page.mouse.move(900,700)
    page.evaluate("__focus817.focusChecks=0;document.querySelector('#toasts').append(document.createTextNode('local status mutation'))")
    page.wait_for_timeout(80)
    check(page.evaluate('document.activeElement===document.body'),'CSS-only hidden menu is not treated as removed app state')
    check(page.evaluate('__focus817.focusChecks')==0,'body mutation after hover loss performs no focus visibility/layout probes')
    page.evaluate("""()=>{const e=document.createElement('button');e.id='focus-removed-fixture';e.textContent='Temporary recovery fixture';document.querySelector('#view-chat').append(e);e.focus();e.remove();}""")
    page.wait_for_timeout(80)
    check(page.evaluate('document.activeElement.matches(\'#tabs button[aria-current="page"]\')'),'actual removed invoker still recovers to active view control')
    page.evaluate("""()=>{const d=__focus817;d.S.running.delete(d.S.current);d.S.streaming='';d.S.view='tools';d.renderChrome();d.renderHistory();
      document.querySelector('#focus-click-away').remove();
      document.querySelectorAll('#toasts .toast .toast-dismiss').forEach(button=>button.click());document.querySelector('#toasts').replaceChildren();}""")


def artifact_download_checks(page):
    page.evaluate('''() => {
      const fixture = __focus817.pagedArtifact = {offsets: [], connectedClicks: 0};
      document.body.addEventListener('click', event => {
        if (event.target.matches('a[download="artifact-00000000.txt"]') && event.target.isConnected)
          fixture.connectedClicks++;
      });
    }''')
    invoke(page, "__focus817.openArtifact('00000000-0000-0000-0000-000000000000')")
    viewer = page.get_by_role('dialog', name='Full output')
    with page.expect_download() as saved:
        viewer.get_by_role('button', name='Download').click()
    check(saved.value.suggested_filename == 'artifact-00000000.txt', 'artifact has its expected download filename')
    check(saved.value.path().read_text(encoding='utf-8') == 'Preview ไทย end', 'artifact download contains all pages')
    check(page.evaluate('__focus817.pagedArtifact.connectedClicks') == 1, 'artifact download clicks an attached link')
    check(viewer.locator('pre').text_content() == 'Preview ', 'downloading leaves the artifact preview unchanged')
    check(viewer.get_by_text('8 of 15 characters').is_visible(), 'downloading leaves the preview count unchanged')
    viewer.get_by_role('button', name='Load more').click()
    viewer.get_by_text('12 of 15 characters').wait_for(state='visible')
    check(viewer.locator('pre').text_content() == 'Preview ไทย ', 'Load more resumes from the displayed preview')
    check(page.evaluate('__focus817.pagedArtifact.offsets') == [0, 8, 12, 8], 'download and preview request their own offsets')
    close_and_return(page,'.backdrop:has(.viewer)','#fixture-tools-open','downloaded artifact viewer')
    page.evaluate('__focus817.pagedArtifact=null')


def run():
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(QuietHandler, directory=str(ROOT / 'web')))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(executable_path=chromium_executable(p), headless=True, args=['--no-sandbox'])
            context = browser.new_context(viewport={'width':1280,'height':900}, reduced_motion='reduce')
            context.route('**/app.js', expose_script)
            page = context.new_page()
            page.set_default_timeout(5000)
            errors=[]
            page.on('pageerror', lambda e: errors.append(str(e)))
            page.goto(f'http://127.0.0.1:{server.server_port}/')
            page.wait_for_function('!!window.__focus817')
            fixture(page)
            cdp = context.new_cdp_session(page)
            cdp.send('Accessibility.enable')
            feedback_checks(page,cdp)
            click_away_checks(page)
            dialogs = [
                ('artifact viewer', "__focus817.openArtifact('00000000-0000-0000-0000-000000000000')", '.backdrop:has(.viewer)'),
                ('model picker', '__focus817.openModelPicker()', '.backdrop[aria-label="Model"]'),
                ('session switcher', '__focus817.openSwitcher()', '.switcher'),
                ('help', '__focus817.showHelp()', '.backdrop[aria-label="Help"]'),
                ('generic modal', '__focus817.modal("Fixture generic",__focus817.h("p",null,"Read-only fixture"))', '.backdrop[aria-label="Fixture generic"]'),
                ('files viewer', '__focus817.showRunFiles()', '.backdrop[aria-label="Files changed"]'),
                ('ask', '__focus817.ask({title:"Fixture ask",input:true,value:"retain me"})', '#ask-backdrop:not(.hidden)'),
            ]
            for label, expression, selector in dialogs:
                invoke(page, expression)
                focus_cycle(page, selector, label)
                if label=='model picker':
                    field=page.locator('.picker select')
                    page.locator('.picker input').focus();page.keyboard.press('Tab')
                    check(field.evaluate('(e)=>e===document.activeElement'),'model picker Tab reaches native effort selector')
                    native_select_key(page,'.picker select','ArrowDown')
                    check(field.evaluate('(e)=>e===document.activeElement'),'model picker effort selector retains native ArrowDown and focus')
                    page.evaluate('__focus817.calls=[]');page.keyboard.press('Enter')
                    check(page.locator('.backdrop[aria-label="Model"]').count()==1,'effort selector Enter does not apply model or close picker')
                    check(page.evaluate('!__focus817.calls.some(t=>t==="set_session_model"||t==="set_setting")'),'effort selector Enter dispatches no model setting action')
                check(ax_ignored(cdp, '#fixture-tools-outside'), label + ' hides background from accessibility tree')
                bounds=page.locator(selector+' .modal').bounding_box()
                page.mouse.move(bounds['x']+10,bounds['y']+10);page.mouse.down();page.mouse.move(3,3);page.mouse.up()
                check(page.evaluate('!!__focus817.activeDialog()'),label+' drag from content to backdrop does not dismiss')
                check(active_within(page,selector),label+' content drag retains modal focus')
                page.screenshot(path=str(OUT/(label.replace(' ','-')+'.png')))
                close_and_return(page, selector, '#fixture-tools-open', label)
                check(not ax_ignored(cdp, '#fixture-tools-outside'), label + ' restores background accessibility')
                # A second open checks stale stack entries and focus-return state.
                invoke(page, expression)
                page.mouse.click(3,3)
                page.wait_for_timeout(25)
                check(not page.evaluate('!!__focus817.activeDialog()'), label + ' safe backdrop closes on repeated open')
                check(page.locator('#fixture-tools-open').evaluate('(e)=>e===document.activeElement'), label + ' backdrop restores trigger')

            # Read-only status lists open at their heading, even when long enough
            # to scroll. Reopening must not retain the previous footer position.
            for viewport in ({'width':1280,'height':900}, {'width':390,'height':640}):
                page.set_viewport_size(viewport)
                for opening in range(2):
                    invoke(page, '__focus817.showHarnessStatus()')
                    page.wait_for_selector('#ask-backdrop:not(.hidden)')
                    check(page.locator('#ask-title').evaluate('(e)=>e===document.activeElement'), 'harness status initially focuses heading')
                    check(page.locator('#ask-form').evaluate('(e)=>e.scrollTop===0'), 'harness status starts scrolled to top')
                    check(page.locator('#ask-form').evaluate('''e=>{const bounds=e.getBoundingClientRect();
                      const title=document.querySelector('#ask-title').getBoundingClientRect();
                      const guidance=document.querySelector('#ask-text').getBoundingClientRect();
                      return title.top>=bounds.top && guidance.bottom<=bounds.bottom;}'''), 'harness heading and substitution guidance visible on open')
                    focus_cycle(page, '#ask-backdrop:not(.hidden)', 'tall harness status')
                    page.locator('#ask-form').evaluate('(e)=>e.scrollTop=e.scrollHeight')
                    close_and_return(page, '#ask-backdrop:not(.hidden)', '#fixture-tools-open', 'tall harness status')
            page.set_viewport_size({'width':1280,'height':900})

            # A deferred viewer must remember its original invoking control before
            # awaiting content, rather than whoever gains focus during the await.
            page.evaluate('__focus817.deferredArtifact=true')
            page.locator('#fixture-tools-open').evaluate("e=>e.onclick=()=>{void __focus817.openArtifact('00000000-0000-0000-0000-000000000000')}")
            page.locator('#fixture-tools-open').focus();page.keyboard.press('Enter')
            page.wait_for_function('!!__focus817.finishArtifact')
            page.locator('#fixture-tools-outside').focus()
            page.evaluate('__focus817.finishArtifact();__focus817.deferredArtifact=false')
            page.wait_for_selector('.viewer')
            close_and_return(page,'.backdrop:has(.viewer)','#fixture-tools-open','deferred viewer')

            artifact_download_checks(page)

            # A picker closed while list_models is pending must stay closed when
            # the reply eventually arrives, without stealing the restored focus.
            page.evaluate('__focus817.deferredModels=true')
            invoke(page,'__focus817.openModelPicker()')
            close_and_return(page,'.backdrop[aria-label="Model"]','#fixture-tools-open','pending model picker')
            page.evaluate('__focus817.finishModels();__focus817.deferredModels=false')
            page.wait_for_timeout(50)
            check(not page.evaluate('!!__focus817.activeDialog()'),'late model list does not reopen picker')
            check(page.locator('#fixture-tools-open').evaluate('(e)=>e===document.activeElement'),'late model list does not steal focus')

            # Nested prompts preserve the lower dialog and its exact inner trigger.
            invoke(page,'__focus817.modal("Outer fixture",__focus817.h("button",{id:"nested-trigger",type:"button",onclick:()=>__focus817.ask({title:"Nested ask",input:true})},"Nested prompt"))')
            page.locator('#nested-trigger').focus();page.keyboard.press('Enter')
            page.wait_for_selector('#ask-backdrop:not(.hidden)')
            check(page.locator('.backdrop[aria-label="Outer fixture"]').evaluate('(e)=>e.inert'),'nested ask inerts lower modal')
            focus_cycle(page,'#ask-backdrop:not(.hidden)','nested ask')
            page.keyboard.press('Escape')
            check(page.locator('#nested-trigger').evaluate('(e)=>e===document.activeElement'),'nested ask restores exact lower trigger')
            check(not page.locator('.backdrop[aria-label="Outer fixture"]').evaluate('(e)=>e.inert'),'closing top prompt reactivates lower modal')
            close_and_return(page,'.backdrop[aria-label="Outer fixture"]','#fixture-tools-open','outer modal')

            # Queued prompts must keep one focus boundary and return to the
            # original invoker rather than a hidden prompt control.
            invoke(page,'window.firstAsk817=__focus817.ask({title:"First queued",input:true,value:"typed fixture"})')
            page.evaluate('() => {window.secondAsk817=__focus817.ask({title:"Second queued",input:true});}')
            check(page.locator('#ask-input').input_value()=='typed fixture','queued prompt preserves first input')
            page.keyboard.press('Escape')
            page.wait_for_function('document.querySelector("#ask-title").textContent==="Second queued"')
            check(active_within(page,'#ask-backdrop'),'queued prompt keeps focus in replacement prompt')
            check(page.locator('#app').evaluate('(e)=>e.inert'),'queued prompt keeps background inert')
            close_and_return(page,'#ask-backdrop:not(.hidden)','#fixture-tools-open','queued prompts')

            # Explicitly non-dismissible presentation state: no decisions/actions.
            page.locator('#fixture-tools-open').focus()
            page.evaluate('''() => {const d=__focus817;window.canDismiss817=false;const b=d.h('div',{class:'backdrop','aria-label':'Locked fixture',role:'dialog','aria-modal':'true'},d.h('div',{class:'modal'},d.h('button',{type:'button'},'Wait')));d.openDialog(b,{canDismiss:()=>window.canDismiss817,onDismiss:()=>d.closeDialog(b)});}''')
            page.keyboard.press('Escape');page.mouse.click(3,3)
            check(page.locator('[aria-label="Locked fixture"]').count()==1,'unsafe Escape/backdrop dismissal is blocked')
            check(active_within(page,'[aria-label="Locked fixture"]'),'non-dismissible dialog retains focus')
            page.evaluate('window.canDismiss817=true')
            close_and_return(page,'[aria-label="Locked fixture"]','#fixture-tools-open','safe dismissal transition')

            page.locator('#fixture-tools-open').focus()
            page.evaluate('''() => {const d=__focus817;const b=d.h('div',{class:'backdrop','aria-label':'Empty fixture',role:'dialog','aria-modal':'true'},d.h('div',{class:'modal'},'No interactive content'));d.openDialog(b,{onDismiss:()=>d.closeDialog(b)});}''')
            for _ in range(3):
                page.keyboard.press('Tab')
                check(active_within(page,'[aria-label="Empty fixture"]'),'empty dialog has a focus fallback and traps Tab')
            close_and_return(page,'[aria-label="Empty fixture"]','#fixture-tools-open','empty dialog')

            # Static approval presentation uses the same layer/focus mechanism.
            # Fake pending state never executes approval actions or protocol calls.
            page.locator('#fixture-tools-open').focus()
            page.evaluate('''() => {const d=__focus817;d.S.approvals=[{id:'focus-approval',kind:'promotion',capabilities:[],tool_name:'focus_fixture',deciding:true}];d.renderApproval();}''')
            page.wait_for_selector('#modal-backdrop:not(.hidden)')
            focus_cycle(page,'#modal-backdrop:not(.hidden)','pending approval presentation')
            page.keyboard.press('Escape');page.mouse.click(3,3)
            check(not page.locator('#modal-backdrop').evaluate('(e)=>e.classList.contains("hidden")'),'pending approval cannot dismiss mid-transition')
            page.evaluate('__focus817.S.approvals[0].deciding=false;__focus817.renderApproval()')
            # Help above approval must consume shortcuts itself; keyboard input
            # in the upper layer must not reach the underlying approval actions.
            page.locator('#modal-later').focus()
            check(page.locator('#modal-later').evaluate('(e)=>e===document.activeElement'),'approval Later is actual focused invoker before Help: '+page.evaluate('JSON.stringify({active:document.activeElement.id,top:__focus817.activeDialog()?.node.outerHTML.slice(0,130),later:__focus817.S.approvalLater,root:document.querySelector("#modal-backdrop").outerHTML.slice(0,150),button:document.querySelector("#modal-later").outerHTML})'))
            page.evaluate('__focus817.showHelp();__focus817.calls=[]')
            check(page.evaluate('__focus817.activeDialog().trigger.id')=='modal-later','Help records invoking approval control')
            page.wait_for_selector('.backdrop[aria-label="Help"]')
            page.keyboard.press('y')
            check(page.evaluate('__focus817.calls.length')==0,'upper dialog keys do not invoke lower approval actions')
            check(page.locator('#modal-backdrop').evaluate('(e)=>e.inert'),'upper dialog inerts underlying approval')
            page.screenshot(path=str(OUT/'nested-help-approval.png'))
            page.keyboard.press('Escape')
            check(page.locator('#modal-later').evaluate('(e)=>e===document.activeElement'),'help restores invoking approval control: '+page.evaluate('JSON.stringify({active:document.activeElement.outerHTML.slice(0,250),top:__focus817.activeDialog()?.node.outerHTML.slice(0,120),later:__focus817.S.approvalLater,hidden:document.querySelector("#modal-backdrop").className})'))
            page.keyboard.press('Escape')
            check(page.locator('#modal-backdrop').evaluate('(e)=>e.classList.contains("hidden")'),'safe approval Escape keeps existing Later behavior')
            check(page.evaluate('__focus817.S.approvals.length')==1,'dismissal leaves approval pending')
            check(page.locator('#fixture-tools-open').evaluate('(e)=>e===document.activeElement'),'approval dismissal restores original invoking control')
            page.evaluate('__focus817.S.approvals=[];__focus817.renderApproval()')

            # An approval arriving while a question is already active is a
            # background layer and must not steal the question's focus.
            invoke(page,'__focus817.ask({title:"Foreground question",input:true})')
            page.evaluate("__focus817.S.approvals=[{id:'background-approval',kind:'promotion',capabilities:[],tool_name:'focus_fixture'}];__focus817.renderApproval()")
            check(page.evaluate('__focus817.activeDialog().node.id')=='ask-backdrop','arriving approval does not raise above active question')
            check(active_within(page,'#ask-backdrop'),'arriving approval does not steal question focus')
            page.keyboard.press('Escape')
            check(page.evaluate('__focus817.activeDialog().node.id')=='modal-backdrop','closing question exposes waiting approval')
            check(active_within(page,'#modal-backdrop'),'waiting approval receives contained focus')
            page.keyboard.press('Escape')
            check(page.evaluate('document.activeElement!==document.body && !!document.activeElement.getClientRects().length'),'overlap completion restores visible focus')
            page.evaluate('__focus817.S.approvals=[];__focus817.renderApproval()')
            invoke(page,'__focus817.showHelp()')
            page.keyboard.press('Control+k')
            check(page.locator('.switcher').count()==0,'global Session switcher shortcut does not open behind a modal')
            page.keyboard.press('Escape')

            # Removed invokers and a view changed under the overlay need a visible
            # focus target in the current view, without dropping focus to body.
            invoke(page,'__focus817.showHelp()')
            page.locator('#fixture-tools-open').evaluate('(e)=>e.remove()')
            page.keyboard.press('Escape')
            check(page.evaluate('document.activeElement!==document.body && !!document.activeElement.getClientRects().length'),'removed invoker falls back to a visible focus target')
            page.evaluate("__focus817.S.view='chat';__focus817.renderChrome()")
            invoke(page,'__focus817.showHelp()','#fixture-chat-open')
            page.evaluate('__focus817.setView("tools")')
            page.wait_for_timeout(50)
            page.keyboard.press('Escape')
            check(page.evaluate('document.activeElement!==document.body && !document.activeElement.closest(".hidden")'),'view transition restores visible focus')
            check(page.evaluate('document.activeElement.closest("#view-tools,#tabs")!==null'),'view transition fallback belongs to active view/navigation')

            # Session and message menus use real rendered triggers and keyboard.
            page.evaluate("__focus817.S.view='chat';__focus817.renderChrome();__focus817.renderSessions();__focus817.renderHistory();__focus817.calls=[]")
            session_trigger='#sessions .session.active .menu'
            message_trigger='#transcript .msg-menu'
            menu_check(page,session_trigger,'session menu')
            check(page.evaluate('!__focus817.calls.includes("get_session")'),'session trigger Enter does not bubble into row navigation')
            inactive_row='#sessions .session:not(.active)'
            inactive_trigger=inactive_row+' .menu'
            page.locator(inactive_row+' .mark').first.focus();page.keyboard.press('Tab');page.keyboard.press('Tab')
            check(page.locator(inactive_trigger).first.evaluate('(e)=>e===document.activeElement'),'inactive Session menu is reachable through native Tab order')
            menu_check(page,inactive_trigger,'inactive Session menu')

            menu_check(page,message_trigger,'message menu')
            # Activating Rename transitions from a menu into a prompt, and closing
            # that prompt returns to the still-existing original menu trigger.
            page.locator(session_trigger).focus();page.keyboard.press('Enter')
            page.get_by_role('menuitem',name='Rename',exact=True).focus();page.keyboard.press('Enter')
            page.wait_for_selector('#ask-backdrop:not(.hidden)')
            check(page.locator('.popover').count()==0,'menu action closes menu before prompt opens')
            page.keyboard.press('Escape')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'menu-to-prompt cancellation restores original trigger')

            # A popover nested in a modal remains exposed and Escape closes only
            # the popover, preserving the lower modal focus boundary.
            page.locator('#fixture-chat-open').focus()
            page.evaluate('''() => {const d=__focus817;d.modal('Menu host',d.h('button',{id:'modal-menu-trigger',type:'button',onclick:(e)=>{e.stopPropagation();d.sessionMenu(e.currentTarget,d.S.sessions[0]);}},'Actions'));}''')
            page.locator('#modal-menu-trigger').focus();page.keyboard.press('Enter')
            page.wait_for_selector('.popover')
            check(not ax_ignored(cdp,'.popover [role=menuitem]'),'menu inside dialog remains in accessibility tree')
            page.screenshot(path=str(OUT/'menu-inside-modal.png'))
            page.keyboard.press('ArrowDown');page.keyboard.press('Escape')
            check(page.locator('.popover').count()==0 and page.locator('[aria-label="Menu host"]').count()==1,'nested menu Escape preserves host dialog')
            check(page.locator('#modal-menu-trigger').evaluate('(e)=>e===document.activeElement'),'nested menu restores its modal trigger')
            page.keyboard.press('Escape')
            check(page.locator('#fixture-chat-open').evaluate('(e)=>e===document.activeElement'),'menu host restores outer trigger')
            # A successful async menu action replaces the Session row. Focus
            # must follow the replacement trigger rather than the detached node.
            page.locator(session_trigger).evaluate('(e)=>window.pinTrigger817=e')
            page.evaluate('__focus817.deferredPin=true')
            page.locator(session_trigger).focus();page.keyboard.press('Enter')
            page.get_by_role('menuitem',name='Pin',exact=True).focus();page.keyboard.press('Enter')
            page.wait_for_function('!!__focus817.finishPin')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'async menu action initially restores trigger')
            page.evaluate('__focus817.finishPin();__focus817.deferredPin=false')
            page.wait_for_timeout(50)
            check(page.evaluate('!window.pinTrigger817.isConnected'),'async Session refresh actually replaces the invoking row')
            check(page.evaluate('document.activeElement!==document.body && !!document.activeElement.getClientRects().length && !document.activeElement.closest("[inert],.hidden,[hidden]") && getComputedStyle(document.activeElement).visibility!=="hidden"'),'async Session refresh restores visible interactive focus')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'async Session refresh restores the replacement menu trigger')
            check(page.locator('.popover').count()==0 and not page.evaluate('!!__focus817.activeDialog()'),'async Session refresh leaves no overlay behind')

            # Outside pointer dismissal preserves the pointer's chosen focus.
            page.locator(message_trigger).focus();page.keyboard.press('Enter')
            page.locator('#fixture-chat-outside').click()
            check(page.locator('.popover').count()==0,'outside pointer closes menu')
            check(page.locator('#fixture-chat-outside').evaluate('(e)=>e===document.activeElement'),'outside pointer retains chosen focus')
            page.locator(message_trigger).focus();page.keyboard.press('Enter')
            page.locator(message_trigger).evaluate('(e)=>e.remove()')
            page.keyboard.press('Escape')
            check(page.evaluate('document.activeElement!==document.body && !!document.activeElement.getClientRects().length'),'removed menu trigger gets visible fallback focus')
            check(page.locator('[aria-haspopup="menu"][aria-expanded="true"]').count()==0,'all context-menu expanded states cleaned up')
            check(not page.locator('#app').evaluate('(e)=>e.inert'),'final background is interactive')
            # Mobile root scaling retains the drawer while pointer-activating
            # a harmless copy action, then returns to its visible menu trigger.
            page.set_viewport_size({'width':390,'height':844})
            page.evaluate('__focus817.S.readScale=2;document.documentElement.style.setProperty("--read-scale","2");__focus817.renderSessions();document.querySelector("#sidebar").classList.add("open")')
            page.wait_for_timeout(50)
            page.locator(session_trigger).click()
            page.get_by_role('menuitem',name='Copy link',exact=False).click()
            check(page.locator('#sidebar').evaluate('(e)=>e.classList.contains("open")'),'mobile copy action leaves sidebar open')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'mobile copy action restores visible menu trigger')
            page.wait_for_function("document.querySelector('#toasts .toast-message')?.textContent")
            page.evaluate('''() => {
              const host=document.querySelector('#toasts'),trigger=document.querySelector('#sessions .session.active .menu');
              const rect=trigger.getBoundingClientRect();
              Object.assign(host.style,{left:(rect.left+rect.width/2-16)+'px',top:(rect.top+rect.height/2-18)+'px',
                right:'auto',bottom:'auto',width:'180px',maxHeight:'none',overflow:'visible'});
            }''')
            check(not ax_ignored(cdp,'#toasts .toast'),'copy notification remains in the accessibility tree')
            check(page.evaluate('''() => {const button=document.querySelector('#sessions .session.active .menu'),r=button.getBoundingClientRect();
              return document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)===button;}'''),'notification leaves an overlapped Session menu pointer target available')
            page.locator(session_trigger).click()
            check(page.locator(session_trigger).get_attribute('aria-expanded')=='true','Session menu opens by pointer through notification')
            check(page.locator('.popover [role=menuitem]').first.evaluate('(e)=>e===document.activeElement'),'pointer-opened Session menu retains keyboard focus')
            page.keyboard.press('Escape')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'Session menu Escape restores focus after notification click-through')
            page.evaluate("document.querySelector('#toasts').removeAttribute('style')")
            # Pointer dismissal of an overlay is not an outside click on the
            # drawer behind it, including when a close button removes the modal.
            page.locator(session_trigger).click()
            page.get_by_role('menuitem',name='Rename',exact=True).click()
            page.wait_for_selector('#ask-backdrop:not(.hidden)')
            page.mouse.click(3,3)
            check(page.locator('#sidebar').evaluate('(e)=>e.classList.contains("open")'),'mobile prompt backdrop dismissal preserves background drawer')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'mobile prompt backdrop restores invoking menu trigger')
            page.locator(session_trigger).click()
            page.get_by_role('menuitem',name='Rename',exact=True).click()
            page.wait_for_selector('#ask-backdrop:not(.hidden)')
            page.locator('#ask-cancel').click()
            check(page.locator('#sidebar').evaluate('(e)=>e.classList.contains("open")'),'mobile prompt Cancel preserves background drawer')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'mobile prompt Cancel restores invoking menu trigger')
            page.evaluate('() => {__focus817.modal("Mobile close fixture",__focus817.h("p",null,"Pointer close fixture"));}')
            page.locator('[aria-label="Mobile close fixture"]').get_by_role('button',name='Close',exact=True).click()
            check(page.locator('#sidebar').evaluate('(e)=>e.classList.contains("open")'),'removed modal close-button event preserves background drawer')
            check(page.locator(session_trigger).evaluate('(e)=>e===document.activeElement'),'removed modal close button restores invoking control')
            page.locator(session_trigger).click()
            check(page.evaluate('!document.elementFromPoint(3,3).closest(".popover,#sidebar") && !document.elementFromPoint(3,3).matches("button,input,select,textarea,a[href]")'),'mobile outside point is nonfocusable and outside menu/drawer')
            page.mouse.click(3,3)
            check(page.locator('.popover').count()==0,'mobile nonfocusable outside click closes menu')
            check(page.evaluate('document.activeElement!==document.body && !document.activeElement.closest("#sidebar:not(.open)")'),'mobile outside click does not leave focus in closed drawer')
            check(page.locator('#sessions .menu').evaluate_all('(items)=>items.every(e=>getComputedStyle(e).visibility==="hidden")'),'closed mobile drawer menu descendants are hidden from Tab navigation')
            # At large text size the toolbar wraps into its own scrollport.
            # Drawer dismissal must bring an obscured return target back into view.
            for height in (740, 200, 160):
                page.set_viewport_size({'width':320,'height':height})
                toolbar = page.locator('.topbar')
                for trigger, panel, to_end in [('#notes-btn','#notes-panel:not(.hidden)',False),
                                               ('#menu-btn','#sidebar.open',True)]:
                    page.locator(trigger).click()
                    page.locator(panel).wait_for()
                    sliver = height == 160 and not to_end
                    toolbar.evaluate('''(e,{toEnd,sliver,selector})=>{
                        if (!sliver) { e.scrollTop=toEnd?e.scrollHeight:0; return; }
                        const button=document.querySelector(selector).getBoundingClientRect();
                        e.scrollTop += button.top-e.getBoundingClientRect().bottom+4;
                    }''',{'toEnd':to_end,'sliver':sliver,'selector':trigger})
                    check(toolbar.evaluate('''(e,{sliver,selector})=>{
                        const bounds=e.getBoundingClientRect(), button=document.querySelector(selector).getBoundingClientRect();
                        const overlap=Math.min(button.bottom,bounds.bottom)-Math.max(button.top,bounds.top);
                        return sliver ? overlap>0 && overlap<8 : overlap<=0;
                    }''',{'sliver':sliver,'selector':trigger}),'scrolled toolbar clips open drawer trigger '+trigger)
                    page.keyboard.press('Escape')
                    page.locator(panel).wait_for(state='hidden')
                    page.wait_for_function('''selector=>{
                        const target=document.querySelector(selector);
                        const button=target.getBoundingClientRect(), bounds=target.closest('.topbar').getBoundingClientRect();
                        const overlap=Math.min(button.bottom,bounds.bottom)-Math.max(button.top,bounds.top);
                        return target===document.activeElement && overlap>=Math.min(button.height,bounds.height-2)-1;
                    }''',arg=trigger)
                    check(page.locator(trigger).evaluate('e=>e===document.activeElement'),'closing drawer reveals restored toolbar focus '+trigger)
            page.evaluate("""()=>{document.querySelectorAll('#toasts .toast .toast-dismiss').forEach(button=>button.click());
              __focus817.toast('Hover timer fixture',false,{label:'Hover action',ms:500,run:()=>{}});} """)
            page.wait_for_function("document.querySelector('#toasts .toast-message')?.textContent==='Hover timer fixture'")
            message=page.locator('#toasts .toast-message').last.bounding_box()
            page.mouse.move(message['x']+message['width']/2,message['y']+message['height']/2)
            page.wait_for_timeout(600)
            check(page.locator('#toasts .toast').count()==1,'hovering notification text pauses its dismissal timer')
            page.mouse.move(0,0);page.wait_for_timeout(600)
            check(page.locator('#toasts .toast').count()==0,'leaving notification text resumes its dismissal timer')
            check(not errors,'no browser page errors: '+repr(errors))
            page.screenshot(path=str(OUT/'final-state.png'))
            context.close();browser.close()
    finally:
        server.shutdown()
    print(f'PASS total: {PASSES}; artifacts: {OUT}',flush=True)


if __name__=='__main__':
    run()
