#!/usr/bin/env python3
"""Issue #815 mobile input/visible-viewport regressions in Chromium and WebKit.

Run: python relay/web-mobile.browser-test.py [artifact-directory]
Uses installed /usr/bin/chromium and official Playwright WebKit (normal headless
WPE executable), with no installation or provider/Tool execution. Keyboard
geometry is an explicit visualViewport EventTarget simulation; native resize,
rotation and Chromium CDP pinch are separate checks. This is browser-engine
emulation, not evidence from physical iOS/Android keyboards or hybrid hardware.
"""
import argparse
import functools
import http.server
import json
import os
import pathlib
import threading
from playwright.sync_api import sync_playwright

ROOT=pathlib.Path(__file__).resolve().parent
os.environ.setdefault('PLAYWRIGHT_BROWSERS_PATH','/workspace/.formless-browsers')
PASSES=0


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self,*_):
        pass


def check(value,label):
    global PASSES
    assert value,label
    PASSES+=1
    print('PASS',label,flush=True)


INIT=r'''(() => {
  const real=window.visualViewport,target=new EventTarget();
  let native=false,simulated=false;
  const state={offsetLeft:0,offsetTop:0,width:innerWidth,height:innerHeight,scale:1};
  const read=()=>native&&real?real:simulated?state:{offsetLeft:0,offsetTop:0,width:innerWidth,height:innerHeight,scale:1};
  for(const key of ['offsetLeft','offsetTop','width','height','scale'])Object.defineProperty(target,key,{get:()=>read()[key]});
  Object.defineProperty(target,'pageLeft',{get:()=>scrollX+target.offsetLeft});
  Object.defineProperty(target,'pageTop',{get:()=>scrollY+target.offsetTop});
  const emit=()=>{target.dispatchEvent(new Event('resize'));target.dispatchEvent(new Event('scroll'));};
  for(const type of ['resize','scroll'])real?.addEventListener(type,()=>{if(native)target.dispatchEvent(new Event(type));});
  Object.defineProperty(window,'visualViewport',{configurable:true,get:()=>target});
  window.__vv815={
    set(value){native=false;simulated=true;Object.assign(state,value);emit();},
    native(){native=true;emit();},
    snapshot(){return {left:target.offsetLeft,top:target.offsetTop,width:target.width,height:target.height,right:target.offsetLeft+target.width,bottom:target.offsetTop+target.height,scale:target.scale};},
    real(){return real?{left:real.offsetLeft,top:real.offsetTop,width:real.width,height:real.height,scale:real.scale}:null;},
  };
})();'''


def expose_script(route):
    source=(ROOT/'web/app.js').read_text();end=source.rfind('\n})();');assert end>0
    hooks=r'''
    const fixture815={S,h,renderAll,renderHistory,renderLive,autosize,setReading,onEvent,scrollToBottom,resetReadingSession,
      updatePalette,updateMentions,closePalette,closeMentions,sessionMenu,messageMenu,closePopover,ask,closeAsk,
      openModelPicker,codeEditor,modal,activeDialog,closeDialog,calls:[]};
    const reply815=async req=>{
      fixture815.calls.push({...req});
      if(req.type==='send_message')return {accepted:true};
      if(req.type==='list_tools'||req.type==='list_modules')return [];
      if(req.type==='list_files')return {files:[{path:'fixture.txt',dir:false},{path:'fixtures/example.txt',dir:false},{path:'files/notes.txt',dir:false}]};
      if(req.type==='list_models')return {models:['fixture-a','fixture-b'],roles:[],aliases:{},recent:[],current:{model:'fixture-a'}};
      if(req.type==='check_tool_code')return {ok:true};
      return null;
    };
    request=reply815;act=reply815;
    window.__mobile815=fixture815;
    '''
    route.fulfill(status=200,content_type='text/javascript',body=source[:end]+hooks+source[end:])


def settle(page,ms=100):
    page.wait_for_timeout(ms)


def fixture(page):
    page.evaluate(r'''() => {const d=__mobile815;d.S.current='mobile-A';d.S.view='chat';d.S.channel={};d.S.daemonOnline=true;d.S.approvalLater=false;d.S.settings={provider:{model:'fixture'},tui:{notify:'off'},limits:{}};
      d.S.sessions=[{id:'mobile-A',name:'Mobile fixture',updated_at:'2026-11-02T12:00:00Z'}];d.S.messages=[];
      for(let i=0;i<24;i++)d.S.messages.push({id:'mobile-message-'+i,seq:i,role:i%2?'assistant':'user',content:'Message '+i+' '+('Stable mobile reading content. '.repeat(18)),created_at:'2026-11-02T12:00:00Z',meta:'{}'});
      d.S.running.add('mobile-A');d.S.liveRun={id:'mobile-run',ended:false};d.S.streaming='Visible live transcript tail';d.S.activity={label:'responding',since:Date.now()};
      document.querySelector('#pair').classList.add('hidden');document.querySelector('#app').classList.remove('hidden');d.resetReadingSession('mobile-A');d.renderAll();d.scrollToBottom(true);
    }''')
    settle(page)


def rect(page,selector):
    return page.locator(selector).evaluate('(e)=>{const r=e.getBoundingClientRect();return {x:r.x,y:r.y,right:r.right,bottom:r.bottom,width:r.width,height:r.height};}')


def fits(r,b):
    return r['width']>0 and r['height']>0 and r['x']>=b['left']-1 and r['y']>=b['top']-1 and r['right']<=b['right']+1 and r['bottom']<=b['bottom']+1


def visible_controls(page,label,tail=False):
    b=page.evaluate('__vv815.snapshot()')
    for selector in ['#composer','#input','#send-btn']:
        r=rect(page,selector)
        check(fits(r,b),label+' '+selector+' fits visible viewport: '+json.dumps({'rect':r,'viewport':b}))
    if tail:
        info=page.locator('#transcript').evaluate('''e=>{const tail=e.querySelector('.stream-tail'),r=tail?.getBoundingClientRect(),v=e.getBoundingClientRect(),c=document.querySelector('#composer').getBoundingClientRect(),b=__vv815.snapshot();return {tail:r?{top:r.top,bottom:r.bottom}:null,top:Math.max(v.top,b.top),bottom:Math.min(v.bottom,c.top,b.bottom),atBottom:Math.abs(e.scrollHeight-e.clientHeight-e.scrollTop)<=1};}''')
        check(info['atBottom'],label+' following transcript stays at tail')
        check(info['tail'] and info['tail']['top']>=info['top']-1 and info['tail']['bottom']<=info['bottom']+1,label+' live tail is visible above composer/keyboard: '+json.dumps(info))


def viewport(page,state):
    page.evaluate('(s)=>__vv815.set(s)',state)
    settle(page,130)


def palette_check(page,text,label):
    page.locator('#input').fill(text);page.locator('#input').focus()
    selector='.compose-popup[aria-label="Commands"]' if text.startswith('/') else '.compose-popup[aria-label="Files"]'
    page.wait_for_selector(selector);settle(page,40)
    check(fits(rect(page,selector),page.evaluate('__vv815.snapshot()')),label+' palette fits visible viewport')
    page.evaluate('__mobile815.closePalette();__mobile815.closeMentions()')


def pause(page,name):
    page.evaluate('__mobile815.closePalette();__mobile815.closeMentions();document.querySelector("#input").value="";__mobile815.autosize()')
    if name=='webkit':
        page.locator('#transcript').focus();page.keyboard.press('Home')
    else:
        page.locator('#input').focus();page.locator('#transcript').hover();page.mouse.wheel(0,-900)
    page.wait_for_function('__mobile815.S.follow===false')
    page.evaluate('''async()=>{let last=-1,quiet=0;for(let i=0;i<120;i++){await new Promise(r=>requestAnimationFrame(r));const top=document.querySelector('#transcript').scrollTop;quiet=top===last?quiet+1:0;last=top;if(quiet>=8)return;}throw new Error('Wheel did not settle');}''')
    check(page.locator('#transcript').evaluate('(e)=>e.scrollTop<e.scrollHeight-e.clientHeight-1'),name+' native pause gesture actually scrolls above tail')
    page.locator('#input').evaluate('(e)=>e.focus({preventScroll:true})');settle(page)
    return page.evaluate('''()=>{const box=document.querySelector('#transcript'),r=box.getBoundingClientRect();const bounds=__vv815.snapshot(),chat=document.querySelector('#view-chat').getBoundingClientRect(),top=Math.max(r.top,chat.top,bounds.top),bottom=Math.min(r.bottom,chat.bottom,bounds.bottom);const node=[...box.querySelectorAll('[data-message-id] p,[data-message-id] .bubble:not(.md)')].find(e=>{const b=e.getBoundingClientRect();return b.bottom>top+1&&b.top<bottom;});if(!node)throw new Error('No visible mobile anchor');window.mobileAnchor815=node;return node.getBoundingClientRect().top-r.top;}''')


def anchor_check(page,offset,label):
    state=page.evaluate('''()=>({connected:mobileAnchor815.isConnected,offset:mobileAnchor815.getBoundingClientRect().top-document.querySelector('#transcript').getBoundingClientRect().top,follow:__mobile815.S.follow})''')
    check(state['connected'] and state['follow'] is False,label+' retains paused anchor/follow state')
    check(state['offset']==offset,label+f' pixel-identical reading offset ({offset} → {state["offset"]})')


def input_checks(page,coarse,label):
    page.evaluate('__mobile815.closePopover();__mobile815.closePalette();__mobile815.closeMentions();__mobile815.S.running.clear();__mobile815.S.streaming="";__mobile815.renderAll();__mobile815.calls=[]')
    check(page.evaluate('matchMedia("(pointer: coarse)").matches')==coarse,label+' primary pointer capability is real browser media')
    check(page.locator('#input').get_attribute('enterkeyhint')=='send',label+' composer advertises send key hint')
    page.locator('#input').fill('first line');page.keyboard.press('Enter');settle(page)
    sent=page.evaluate('__mobile815.calls.filter(r=>r.type==="send_message")')
    if coarse:
        check(page.locator('#input').input_value()=='first line\n' and not sent,label+' coarse Enter inserts native newline without sending')
        page.locator('#send-btn').click();settle(page)
        check(page.evaluate('__mobile815.calls.filter(r=>r.type==="send_message").length')==1,label+' Send button sends the multiline draft once')
    else:
        check(len(sent)==1 and sent[0]['content']=='first line' and sent[0]['mode']=='send',label+' desktop Enter keeps existing send behavior')
    page.locator('#input').fill('shift line');page.locator('#input').focus();page.evaluate('__mobile815.calls=[]');page.keyboard.press('Shift+Enter');settle(page,30)
    check(page.locator('#input').input_value()=='shift line\n' and page.evaluate('!__mobile815.calls.some(r=>r.type==="send_message")'),label+' Shift+Enter remains a native line break')
    for key in ['Control+Enter','Meta+Enter']:
        page.locator('#input').fill('queued local fixture');page.locator('#input').focus();page.evaluate('__mobile815.calls=[]');page.keyboard.press(key);settle(page)
        calls=page.evaluate('__mobile815.calls.filter(r=>r.type==="send_message")')
        check(len(calls)==1 and calls[0]['mode']=='queue',label+' '+key+' keeps explicit queue behavior')
    native_alt=None
    if coarse:
        page.evaluate('''()=>{const e=document.createElement('textarea');e.id='native-alt-control';e.style.cssText='position:fixed;left:0;top:0;width:120px;height:32px;z-index:1000';document.body.append(e);}''')
        page.locator('#native-alt-control').fill('alternate fixture');page.keyboard.press('Alt+Enter');native_alt=page.locator('#native-alt-control').input_value();page.locator('#native-alt-control').evaluate('(e)=>e.remove()')
    page.locator('#input').fill('alternate fixture');page.locator('#input').focus();page.evaluate('__mobile815.calls=[]');page.keyboard.press('Alt+Enter');settle(page)
    calls=page.evaluate('__mobile815.calls.filter(r=>r.type==="send_message")')
    check((page.locator('#input').input_value()==native_alt and not calls) if coarse else (len(calls)==1 and calls[0]['mode']=='send'),label+' Alt+Enter follows pointer policy')
    for kind in ['composition','229']:
        page.locator('#input').fill('IME fixture');page.locator('#input').focus();page.evaluate('__mobile815.calls=[]')
        result=page.locator('#input').evaluate('''(e,kind)=>{const k=new KeyboardEvent('keydown',{key:'Enter',code:'Enter',bubbles:true,cancelable:true,isComposing:kind==='composition',keyCode:kind==='229'?229:13});if(kind==='229'&&k.keyCode!==229)Object.defineProperty(k,'keyCode',{value:229});e.dispatchEvent(k);return {prevented:k.defaultPrevented,value:e.value};}''',kind)
        check(not result['prevented'] and result['value']=='IME fixture' and page.evaluate('!__mobile815.calls.some(r=>r.type==="send_message")'),label+' IME '+kind+' confirmation performs no app action')
    if coarse:
        for text in ['/he','@fi']:
            page.locator('#input').fill(text);page.locator('#input').focus();page.wait_for_selector('.compose-popup');page.evaluate('__mobile815.calls=[]');page.keyboard.press('Enter');settle(page)
            check(page.locator('#input').input_value()==text+'\n',label+' Enter with '+text+' palette is still native newline')
            check(page.evaluate('!__mobile815.calls.some(r=>["send_message","attach_path"].includes(r.type)) && !__mobile815.activeDialog()'),label+' coarse palette Enter neither executes nor attaches')
            page.evaluate('__mobile815.closePalette();__mobile815.closeMentions()')
    # Native multiline handlers must never fall through to the composer form.
    page.evaluate('''()=>{const d=__mobile815;const area=d.h('textarea',{id:'mobile-multiline-fixture',rows:3,'aria-label':'Multiline fixture'});d.ask({title:'Multiline fixture',body:area});}''')
    page.locator('#mobile-multiline-fixture').fill('native area');page.locator('#mobile-multiline-fixture').focus();page.evaluate('__mobile815.calls=[]');page.keyboard.press('Enter');settle(page,30)
    check(page.locator('#mobile-multiline-fixture').input_value()=='native area\n' and page.evaluate('!__mobile815.calls.some(r=>r.type==="send_message")'),label+' dialog textarea Enter is a native newline only')
    page.keyboard.press('Escape')
    # Factory-created code/secret/filter fields exercise production metadata and
    # the real highlighted editor, without any save or Tool execution action.
    page.evaluate(r'''()=>{const d=__mobile815;const code=d.h('textarea',{id:'mobile-code-fixture',class:'editor',spellcheck:'false','aria-label':'Code fixture'});code.value='const example = "'+('wide-code-'.repeat(35))+'";'+String.fromCharCode(10)+Array.from({length:30},(_,i)=>'const line'+i+' = '+i+';').join(String.fromCharCode(10));const body=d.h('div',null,d.h('input',{id:'mobile-secret-fixture',type:'password','aria-label':'Secret fixture'}),d.h('input',{id:'mobile-filter-fixture',type:'search','aria-label':'Filter fixture'}),d.codeEditor(code));d.modal('Text field fixtures',body);}''')
    page.wait_for_selector('#mobile-code-fixture');settle(page)
    page.evaluate('__mobile815.setReading(.85)');settle(page)
    if coarse:
        fields=page.locator('input:not([type=checkbox]):not([type=radio]):not([type=file]):not([type=hidden]),textarea').evaluate_all('(fields)=>fields.map(e=>({name:e.id||e.getAttribute("aria-label")||e.type,size:parseFloat(getComputedStyle(e).fontSize)}))')
        for field in fields:
            check(field['size']>=16,label+' touch field '+field['name']+' is at least16px at85% ('+str(field['size'])+')')
    for selector in ['#secret','#search','#tools-filter','#settings-filter','#mobile-secret-fixture','#mobile-filter-fixture','#mobile-code-fixture']:
        check(page.locator(selector).get_attribute('autocapitalize')=='none' and page.locator(selector).get_attribute('autocorrect')=='off',label+' '+selector+' disables machine-text keyboard correction/caps')
    metrics=page.locator('.code-editor').evaluate('''e=>{const t=e.querySelector('textarea'),a=getComputedStyle(t),b=getComputedStyle(e.querySelector('.ed-hl'));const keys=['fontFamily','fontSize','lineHeight','letterSpacing','paddingTop','paddingRight','paddingBottom','paddingLeft','tabSize'];return keys.map(k=>({key:k,text:a[k],highlight:b[k]}));}''')
    for m in metrics:
        check(m['text']==m['highlight'],label+' code highlight '+m['key']+' matches native textarea')
    page.locator('#mobile-code-fixture').evaluate('(e)=>{e.scrollTop=80;e.scrollLeft=50;e.dispatchEvent(new Event("scroll"));}')
    check(page.locator('.code-editor').evaluate('e=>{const t=e.querySelector("textarea");return [...e.querySelectorAll(".ed-layer")].every(l=>l.scrollTop===t.scrollTop&&l.scrollLeft===t.scrollLeft);}'),label+' code highlight scroll offsets stay aligned')
    page.locator('#mobile-code-fixture').focus();page.keyboard.press('End');page.keyboard.press('Enter');settle(page,30)
    check(page.evaluate('!__mobile815.calls.some(r=>r.type==="send_message")'),label+' code textarea native Enter never sends a message')
    page.keyboard.press('Escape')
    page.evaluate('() => {void __mobile815.openModelPicker();}')
    page.wait_for_selector('.picker input');settle(page)
    check(page.locator('.picker input').get_attribute('autocapitalize')=='none' and page.locator('.picker input').get_attribute('autocorrect')=='off',label+' actual dynamic model filter disables correction/caps')
    if coarse:
        check(page.locator('.picker input').evaluate('(e)=>parseFloat(getComputedStyle(e).fontSize)')>=16,label+' actual dynamic model filter stays16px at85%')
    page.keyboard.press('Escape')
    page.evaluate('__mobile815.setReading(1);__mobile815.calls=[]')
    for i in range(3):
        page.locator('#input').fill('Repeated local send '+str(i));page.locator('#send-btn').click();settle(page,30)
    check(page.evaluate('__mobile815.calls.filter(r=>r.type==="send_message").length')==3,label+' repeated Send uses harmless local replies')
    check('user-scalable=no' not in (page.locator('meta[name=viewport]').get_attribute('content') or '').replace(' ',''),label+' zoom remains available')


def geometry_checks(page,name,out,context):
    page.evaluate('__mobile815.S.running.add("mobile-A");__mobile815.S.streaming="Visible live transcript tail";__mobile815.renderAll();__mobile815.scrollToBottom(true)')
    settle(page)
    sequences=[
        {'offsetLeft':0,'offsetTop':0,'width':390,'height':844,'scale':1},
        {'offsetLeft':0,'offsetTop':0,'width':390,'height':430,'scale':1},
        {'offsetLeft':6,'offsetTop':30,'width':378,'height':410,'scale':1},
        {'offsetLeft':3,'offsetTop':42,'width':384,'height':440,'scale':1},
        {'offsetLeft':0,'offsetTop':0,'width':390,'height':844,'scale':1},
    ]
    for i,state in enumerate(sequences):
        viewport(page,state);visible_controls(page,name+f' simulated keyboard cycle{i}',tail=True)
        palette_check(page,'/he',name+f' cycle{i} slash')
        palette_check(page,'@fi',name+f' cycle{i} mention')
    page.screenshot(path=str(out/'keyboard-portrait.png'))
    # Existing open menus survive and reposition across visual viewport events.
    for kind in ['session','message']:
        page.evaluate('''kind=>{const d=__mobile815,b=document.querySelector('#notes-btn');b.focus();if(kind==='session')d.sessionMenu(b,d.S.sessions[0]);else d.messageMenu(b,d.S.messages[d.S.messages.length-1],document.querySelector('[data-message-id="mobile-message-23"]'));window.mobileMenu815=document.querySelector('.popover');}''',kind)
        page.keyboard.press('End');selected=page.evaluate('document.activeElement.textContent')
        for i,state in enumerate(sequences[1:]):
            viewport(page,state)
            check(page.evaluate('mobileMenu815===document.querySelector(".popover")'),name+f' {kind} menu survives viewport cycle{i}')
            check(fits(rect(page,'.popover'),page.evaluate('__vv815.snapshot()')),name+f' {kind} menu stays within visible viewport cycle{i}')
            check(page.evaluate('document.activeElement.textContent')==selected,name+f' {kind} menu keeps keyboard selection on reposition')
        page.keyboard.press('Escape')
    # Paused reading remains anchored through keyboard and offset changes, while
    # real new stream text below creates a jump pill placed above the composer.
    viewport(page,sequences[0]);offset=pause(page,name)
    for i,state in enumerate(sequences[1:]):
        viewport(page,state)
        page.evaluate('__mobile815.onEvent({type:"message_delta",session_id:"mobile-A",run_id:"mobile-run",delta:" new below"})');settle(page)
        anchor_check(page,offset,name+f' paused keyboard cycle{i}')
        check(not page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' paused new output reveals jump pill')
        check(fits(rect(page,'#jump'),page.evaluate('__vv815.snapshot()')),name+' jump pill is visible above keyboard')
        check(rect(page,'#jump')['bottom']<=rect(page,'#composer')['y']+1,name+' jump pill clears actual composer height')
    page.screenshot(path=str(out/'paused-keyboard-anchor.png'))
    page.locator('#jump').click();settle(page);visible_controls(page,name+' jump reattachment',tail=True)
    # Native window resize/rotation, then explicit shifted landscape keyboard.
    page.set_viewport_size({'width':844,'height':390});page.evaluate('__vv815.native()');settle(page,160)
    visible_controls(page,name+' native landscape rotation',tail=True)
    page.locator('#input').focus()
    viewport(page,{'offsetLeft':20,'offsetTop':10,'width':804,'height':260,'scale':1})
    visible_controls(page,name+' simulated landscape keyboard',tail=True)
    palette_check(page,'/he',name+' landscape slash');palette_check(page,'@fi',name+' landscape mention')
    page.screenshot(path=str(out/'keyboard-landscape.png'))
    page.set_viewport_size({'width':390,'height':844});page.evaluate('__vv815.native()');settle(page,160)
    visible_controls(page,name+' native portrait rotation',tail=True)
    # Pinch is distinct from keyboard occlusion. Layout width/root text must not
    # be inversely scaled; controls may float within the smaller visual window.
    base=page.evaluate('({font:getComputedStyle(document.documentElement).fontSize,width:document.querySelector("#app").offsetWidth})')
    if name=='chromium':
        cdp=context.new_cdp_session(page);cdp.send('Emulation.setPageScaleFactor',{'pageScaleFactor':2});settle(page,180)
        native=page.evaluate('__vv815.real()')
        check(native and abs(native['scale']-2)<.02,name+' native CDP pinch retains scale2')
        check(page.evaluate('getComputedStyle(document.documentElement).fontSize')==base['font'],name+' pinch preserves root font size')
        check(page.locator('#app').evaluate('(e)=>e.offsetWidth')==base['width'],name+' pinch preserves main layout width')
        visible_controls(page,name+' native pinch controls',tail=True)
        cdp.send('Emulation.setPageScaleFactor',{'pageScaleFactor':1});settle(page,150)
    else:
        viewport(page,{'offsetLeft':32,'offsetTop':40,'width':260,'height':450,'scale':1.5})
        check(page.evaluate('getComputedStyle(document.documentElement).fontSize')==base['font'],name+' simulated pinch preserves root font size')
        check(page.locator('#app').evaluate('(e)=>e.offsetWidth')==base['width'],name+' simulated pinch preserves main layout width')
        visible_controls(page,name+' simulated pinch controls',tail=True)
    page.screenshot(path=str(out/'pinch.png'))
    # At200% a long draft may need local composer scrolling. Each native
    # control must be reachable without moving the document or losing follow.
    viewport(page,{'offsetLeft':0,'offsetTop':0,'width':390,'height':844,'scale':1})
    page.evaluate('__mobile815.setReading(2);__mobile815.scrollToBottom(true)')
    page.locator('#input').fill('\n'.join('Long draft line '+str(i) for i in range(12)));page.locator('#input').focus()
    viewport(page,{'offsetLeft':0,'offsetTop':0,'width':390,'height':300,'scale':1})
    check(fits(rect(page,'#composer'),page.evaluate('__vv815.snapshot()')),name+'200% long draft composer stays in short viewport')
    for selector in ['#input','#send-btn']:
        page.locator(selector).evaluate('(e)=>e.scrollIntoView({block:"nearest",inline:"nearest"})');settle(page)
        check(fits(rect(page,selector),page.evaluate('__vv815.snapshot()')),name+'200% long draft '+selector+' is reachable by local scrolling')
    page.locator('#input').focus();settle(page)
    tail=page.locator('.stream-tail').evaluate('''e=>{const w=document.createTreeWalker(e,NodeFilter.SHOW_TEXT);let n,last;while(n=w.nextNode())if(n.length)last=n;if(!last)return null;const r=document.createRange();r.setStart(last,last.length-1);r.setEnd(last,last.length);const x=r.getBoundingClientRect(),t=document.querySelector('#transcript').getBoundingClientRect(),c=document.querySelector('#composer').getBoundingClientRect(),v=__vv815.snapshot();return {top:x.top,bottom:x.bottom,min:Math.max(t.top,v.top),max:Math.min(t.bottom,c.top,v.bottom)};}''')
    check(page.evaluate('__mobile815.S.follow') and tail and tail['top']>=tail['min']-1 and tail['bottom']<=tail['max']+1,name+'200% long draft keeps latest tail line above keyboard')
    page.screenshot(path=str(out/'composer-200-long-draft.png'))
    page.evaluate('''()=>{document.documentElement.style.setProperty('--safe-top','24px');document.documentElement.style.setProperty('--safe-bottom','22px');void __mobile815.ask({title:'Large text viewport fixture',input:true,value:'Visible native field'});}''')
    viewport(page,{'offsetLeft':0,'offsetTop':0,'width':390,'height':200,'scale':1});settle(page,100)
    field=rect(page,'#ask-input');surface=rect(page,'#ask-form')
    check(fits(field,page.evaluate('__vv815.snapshot()')) and field['y']>=surface['y']-1 and field['bottom']<=surface['bottom']+1,name+'200% modal field is fully visible through local scrolling')
    check(page.locator('#ask-input').evaluate('(e)=>e===document.activeElement'),name+'200% modal retains native field focus')
    page.screenshot(path=str(out/'modal-200-short-viewport.png'));page.keyboard.press('Escape')
    page.evaluate('''()=>{document.documentElement.style.removeProperty('--safe-top');document.documentElement.style.removeProperty('--safe-bottom');__mobile815.setReading(1);document.querySelector('#input').value='';__mobile815.autosize();__vv815.native();}''');settle(page)



def run_engine(p,name,url,out):
    start=PASSES;reports=[]
    browser=p.chromium.launch(executable_path='/usr/bin/chromium',headless=True,args=['--no-sandbox']) if name=='chromium' else p.webkit.launch(executable_path=p.webkit.executable_path,headless=True)
    for coarse in [True,False]:
        context=browser.new_context(viewport={'width':390,'height':844} if coarse else {'width':1280,'height':900},is_mobile=coarse,has_touch=coarse,reduced_motion='reduce')
        context.add_init_script(INIT);context.route('**/app.js',expose_script)
        page=context.new_page();page.set_default_timeout(5000);errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
        page.goto(url);page.wait_for_function('!!__mobile815');fixture(page)
        label=name+(' coarse' if coarse else ' fine')
        input_checks(page,coarse,label)
        if coarse:
            geometry_checks(page,name,out,context)
        check(not errors,label+' no page errors: '+repr(errors))
        reports.append({'pointer':'coarse' if coarse else 'fine','errors':errors})
        context.close()
    report={'engine':name,'version':browser.version,'assertions':PASSES-start,'contexts':reports,'physicalKeyboard':False,'nativePinch':name=='chromium','hybridHardware':False,'pauseGesture':'native Home key' if name=='webkit' else 'native wheel'}
    (out/'report.json').write_text(json.dumps(report,indent=2));browser.close()
    print('ENGINE',json.dumps(report),flush=True)
    return report


def run():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('artifacts',nargs='?',default='/tmp/formless-815-browser');parser.add_argument('--browser',choices=['all','chromium','webkit'],default='all')
    args=parser.parse_args();out=pathlib.Path(args.artifacts);out.mkdir(parents=True,exist_ok=True)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHandler,directory=str(ROOT/'web')));threading.Thread(target=server.serve_forever,daemon=True).start();reports=[]
    try:
        with sync_playwright() as p:
            for name in ['chromium','webkit'] if args.browser=='all' else [args.browser]:
                engine_out=out/name;engine_out.mkdir(exist_ok=True);reports.append(run_engine(p,name,f'http://127.0.0.1:{server.server_port}/',engine_out))
    finally:server.shutdown()
    (out/'report.json').write_text(json.dumps({'complete':args.browser=='all','results':reports},indent=2));print(f'PASS total:{PASSES}; engines:{args.browser}; artifacts:{out}',flush=True)


if __name__=='__main__':run()
