#!/usr/bin/env python3
"""Issue #816 streaming/scroll regressions in real Chromium and Playwright WebKit.

Run: python relay/web-streaming.browser-test.py [artifact-directory]
Both engines are required by default; --browser chromium is explicitly partial
validation. Requires Python Playwright, /usr/bin/chromium, and a supported
Playwright WebKit install. PLAYWRIGHT_BROWSERS_PATH defaults to the workspace
install. WebKit uses its official executable_path and normal headless WPE
wrapper. Native dependencies must already be available through that runtime;
no dependencies, wrappers, or system settings are changed by this test.

The complete shipped UI runs with test-only closure hooks and local read-only
fixture replies. RAF and interval callbacks are controlled for deterministic
burst/timer checks. Real wheel/key/scroll/toggle/image layout runs in each engine.
A controlled SVG reply exercises actual diagram completion/image-load handlers.
Playwright WebKit is engine evidence, not physical iOS/macOS Safari evidence.
"""
import argparse
import functools
import http.server
import json
import os
import pathlib
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


INIT = r'''(() => {
  const nativeRAF=window.requestAnimationFrame.bind(window);
  const nativeCancel=window.cancelAnimationFrame.bind(window);
  let held=false, seq=1000000;
  const pending=new Map(), canceled=new Map(), intervals=new Map();
  window.requestAnimationFrame=cb=>{if(!held)return nativeRAF(cb);const id=++seq;pending.set(id,cb);return id;};
  window.cancelAnimationFrame=id=>{if(pending.has(id)){canceled.set(id,pending.get(id));pending.delete(id);}else nativeCancel(id);};
  window.setInterval=(fn,ms,...args)=>{const id=++seq;intervals.set(id,{fn,ms,args});return id;};
  window.clearInterval=id=>intervals.delete(id);
  window.__raf816={
    hold(){held=true;}, release(){held=false;},
    count(){return pending.size;},
    flush(includeCanceled=false){const jobs=[...pending.values()];pending.clear();if(includeCanceled){jobs.push(...canceled.values());canceled.clear();}for(const cb of jobs)cb(performance.now());return jobs.length;},
    tick(ms){for(const task of intervals.values())if(task.ms===ms)task.fn(...task.args);},
    frame(){return new Promise(resolve=>nativeRAF(()=>resolve()));},
  };
})();'''


def expose_script(route):
    source = (ROOT / 'web/app.js').read_text()
    end = source.rfind('\n})();')
    assert end > 0
    hooks = r'''
    const fixture816={S, onEvent, openSession, setView, renderChrome, renderHistory,
      renderLive, scrollToBottom, resetReadingSession, withTranscriptLayout,
      scheduleEventRender, cancelEventRender, updateElapsedTimers, setThinking, streamingNode, splitStreaming, markdown, MD_FORMAT_CHARS,
      stats:{history:0,live:0,status:0,composer:0}, diagrams:new Map(), snapshots:new Map(), calls:[]};
    const painted816={renderHistory,renderLive,renderStatus,renderComposer};
    renderHistory=(...args)=>{fixture816.stats.history++;return painted816.renderHistory(...args);};
    renderLive=(...args)=>{fixture816.stats.live++;return painted816.renderLive(...args);};
    renderStatus=(...args)=>{fixture816.stats.status++;return painted816.renderStatus(...args);};
    renderComposer=(...args)=>{fixture816.stats.composer++;return painted816.renderComposer(...args);};
    fixture816.paintHistory=(...args)=>renderHistory(...args);
    fixture816.paintLive=(...args)=>renderLive(...args);
    fixture816.resetStats=()=>{for(const key of Object.keys(fixture816.stats))fixture816.stats[key]=0;};
    const originalMermaid816=renderMermaid;
    renderMermaid=src=>{
      if(!src.includes('fixture_delayed_diagram'))return originalMermaid816(src);
      return new Promise(resolve=>fixture816.diagrams.set(src,()=>{
        const svg='<svg xmlns="http://www.w3.org/2000/svg" width="500" height="640" viewBox="0 0 500 640"><rect width="500" height="640" fill="#dae8e0"/><path d="M20 20L480 620" stroke="#176039" stroke-width="4"/></svg>';
        resolve({url:'data:image/svg+xml;charset=utf-8,'+encodeURIComponent(svg),svg,width:500,theme:'default'});
      }));
    };
    fixture816.messages=(sid,count=30)=>{
      const result=[];
      for(let i=0;i<count;i++)result.push({id:sid+'-m'+i,seq:i,role:i%2?'assistant':'user',
        content:'Message '+i+' '+('Stable history text for reading while new output arrives. '.repeat(18)),created_at:'2026-11-02T12:00:00Z',meta:'{}'});
      result[1].meta=JSON.stringify({tool_calls:[{function:{name:'fixture_display_chip',arguments:JSON.stringify({lines:'Expandable display-only payload\n'.repeat(80)})}}]});
      result[3].content='```mermaid\nfixture_delayed_diagram_'+sid+'\n```';
      return result;
    };
    const reply816=async req=>{
      fixture816.calls.push(req.type);
      if(req.type==='get_session'&&fixture816.snapshots.has(req.id))return fixture816.snapshots.get(req.id);
      if(req.type==='get_session')return {session:S.sessions.find(s=>s.id===req.id),messages:fixture816.messages(req.id,24),queued_items:[],plan_items:[]};
      if(req.type==='subscribe')return {conn:'fixture-browser'};
      if(req.type==='context_usage')return {used_tokens:100,context_tokens:10000};
      if(req.type==='session_usage')return {runs:0,total:{}};
      if(req.type==='list_tools'||req.type==='list_modules')return [];
      return null;
    };
    act=reply816;request=reply816;
    window.__stream816=fixture816;
    '''
    route.fulfill(status=200, content_type='text/javascript', body=source[:end]+hooks+source[end:])


def settle(page, milliseconds=70):
    page.wait_for_timeout(milliseconds)


def flush(page, include_canceled=False):
    return page.evaluate('(c)=>__raf816.flush(c)', include_canceled)


def drain(page):
    for _ in range(8):
        jobs=flush(page)
        settle(page,20)
        if not jobs and page.evaluate('__raf816.count()')==0:
            return
    check(page.evaluate('__raf816.count()')==0,'RAF work settles within eight frames')


def fixture(page):
    page.evaluate(r'''() => {
      const d=__stream816;d.S.current='A';d.S.view='chat';d.S.channel={};d.S.daemonOnline=true;
      d.S.sessions=[{id:'A',name:'Streaming fixture A',updated_at:'2026-11-02T12:00:00Z'},{id:'B',name:'Streaming fixture B',updated_at:'2026-11-01T12:00:00Z'}];
      d.S.approvalLater=false;d.S.messages=d.messages('A');d.S.running.add('A');
      d.S.liveRun={id:'run-A',ended:false};d.S.activity={label:'responding',since:Date.now()-4000};
      d.S.liveTools=[{invocationId:'fixture-call',name:'fixture_display_work',version:1,arguments:'{"message":"Display-only fixture"}',status:'running',started:Date.now()-5000,depth:0,progress:{fraction:.1,label:'starting'}}];
      d.S.streaming='First complete paragraph.\n\nTail base ';d.S.thinking='Initial reasoning';
      document.querySelector('#app').classList.remove('hidden');document.querySelector('#pair').classList.add('hidden');
      d.resetReadingSession('A');d.renderChrome();d.paintHistory();d.scrollToBottom(true);
    }''')
    settle(page)


def anchor(page, deep=False):
    return page.evaluate(r'''(deep) => {
      const box=document.querySelector('#transcript'),r=box.getBoundingClientRect();
      const messages=[...box.querySelectorAll('.msg[data-message-id]')];
      const candidates=deep?messages.flatMap(m=>[...m.querySelectorAll('p,pre,summary,h1,h2,h3,h4')]):messages;
      const node=candidates.find(e=>{const x=e.getBoundingClientRect();return x.top>=r.top+5&&x.top<r.bottom-20;})
        ||candidates.find(e=>{const x=e.getBoundingClientRect();return x.bottom>r.top+10&&x.top<r.bottom;});
      if(!node)throw new Error('No stable visible message anchor');
      window.anchor816=node;window.anchorTop816=node.getBoundingClientRect().top;
      return {id:node.closest('[data-message-id]').dataset.messageId,top:window.anchorTop816,scrollTop:box.scrollTop};
    }''', deep)


def unchanged_anchor(page, before, label):
    current=page.evaluate(r'''() => ({connected:anchor816.isConnected,top:anchor816.getBoundingClientRect().top,id:anchor816.closest('[data-message-id]').dataset.messageId,follow:__stream816.S.follow})''')
    check(current['connected'] and current['id']==before['id'],label+' retains exact message node')
    check(current['top']==before['top'],label+f' pixel-identical anchor ({before["top"]} → {current["top"]})')
    check(current['follow'] is False,label+' leaves follow disabled')


def detach(page):
    page.locator('#transcript').hover()
    page.mouse.wheel(0,-1100)
    try:
        page.wait_for_function('__stream816.S.follow===false')
    except Exception as exc:
        state=page.evaluate('''() => {const b=document.querySelector('#transcript'),r=b.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);return {current:__stream816.S.current,view:__stream816.S.view,follow:__stream816.S.follow,scrollTop:b.scrollTop,scrollHeight:b.scrollHeight,clientHeight:b.clientHeight,rect:{x:r.x,y:r.y,width:r.width,height:r.height},hit:hit?.outerHTML.slice(0,180),pending:__raf816.count()};}''')
        raise AssertionError('Real user wheel failed to pause follow: '+json.dumps(state)) from exc
    # WebKit dispatches a native kinetic wheel animation over several frames.
    # Capture the reading anchor only after that intentional user movement ends.
    page.evaluate('''async () => {let last=-1,stable=0;for(let i=0;i<120;i++){
      await __raf816.frame();const top=document.querySelector('#transcript').scrollTop;
      stable=top===last?stable+1:0;last=top;if(stable>=8)return;
    }throw new Error('Native wheel did not settle within120frames');}''')


def long_message_case(page,name,out):
    # One long message exercises a visible paragraph anchor inside the same
    # message as the delayed image, rather than only anchors in later messages.
    page.evaluate(r'''() => {const d=__stream816,n=String.fromCharCode(10);d.S.current='LONG';d.S.view='chat';d.S.sessions.push({id:'LONG',name:'Long message fixture',updated_at:'2026-11-02T12:00:00Z'});
      d.S.messages=[{id:'LONG-message',seq:0,role:'assistant',created_at:'2026-11-02T12:00:00Z',content:'Before delayed image.'+n+n+'```mermaid'+n+'fixture_delayed_diagram_LONG'+n+'```'+n+n+Array.from({length:45},(_,i)=>'Long paragraph '+i+' '+('Stable visible paragraph text. '.repeat(24))).join(n+n),meta:JSON.stringify({tool_calls:[{function:{name:'long_display_chip',arguments:JSON.stringify({message:'Expanded long-message payload '.repeat(35)})}}]})}];
      d.S.running.clear();d.S.running.add('LONG');d.S.streaming='';d.S.thinking='';d.S.liveTools=[];d.S.liveRun={id:'run-LONG',ended:false};d.S.activity={label:'responding',since:Date.now()};d.resetReadingSession('LONG');d.renderChrome();d.paintHistory();d.scrollToBottom(true);
    }''')
    settle(page)
    detach(page);long_anchor=anchor(page,deep=True)
    check(page.evaluate('anchor816.tagName')=='P',name+' long-message anchor is an actual visible paragraph')
    page.evaluate('''() => {const entry=[...__stream816.diagrams].find(([src])=>src.includes('fixture_delayed_diagram_LONG'));if(!entry)throw new Error('Missing long-message diagram');entry[1]();}''')
    page.wait_for_function('document.querySelector("[data-message-id=LONG-message] .diagram-img")?.complete')
    settle(page,140)
    unchanged_anchor(page,long_anchor,name+' delayed layout inside a single long message')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' long-message image layout alone creates no unseen pill')
    page.screenshot(path=str(out/'long-message-anchor.png'))
    summary=page.locator('[data-message-id=LONG-message] details.chip summary')
    summary.focus();settle(page)
    check(page.evaluate('__stream816.S.follow') is False,name+' programmatic disclosure focus does not resume follow')
    page.keyboard.press('Space');settle(page)
    check(summary.evaluate('(e)=>e.parentElement.open'),name+' native Space opens focused Tool disclosure')
    check(page.evaluate('__stream816.S.follow') is False,name+' Space disclosure opening does not count as user scroll-to-bottom')
    page.keyboard.press('Space');settle(page)
    check(not summary.evaluate('(e)=>e.parentElement.open'),name+' native Space closes focused Tool disclosure')
    check(page.evaluate('__stream816.S.follow') is False,name+' Space disclosure closing does not reattach follow')


def markdown_block_cases(page, name):
    result=page.evaluate(r'''() => {
      const d=__stream816;
      const corpus=[
        ['ordered loose','Intro.\n\n3. first\n\n4. second\n\n5. third\n\n',true],
        ['unordered loose','Intro.\n\n- first\n\n- second\n\n- third\n\n',true],
        ['nested loose','- parent\n\n  - child one\n\n  - child two\n\n- sibling\n\n',true],
        ['item paragraphs','- first\n\n  second paragraph\n\n- sibling\n\nAfter.\n\n',true],
        ['different kinds','- first\n\n1. second\n\nAfter.\n\n',true],
        ['list rule boundary','- first\n\n- - -\n\n- second\n\n',true],
        ['display math blank','Before\n\n$$\na = 1\n\nb = 2\n$$\n\nAfter paragraph.\n\n',true],
        ['math fence-looking text','Before\n\n$$\n```js\n\na = 1\n```\n\nb = 2\n$$\n\nAfter.\n\n',true],
        ['inline math close','Before\n\n$$ a = 1\n\nb = 2 $$ trailing text\n\nAfter.\n\n',true],
        ['single line math','Before\n\n$$a = 1$$\n\nAfter.\n\n',true],
        ['math inside fence','Before\n\n````text\n$$\n\n```\n\nstill code\n````\n\nAfter.\n\n',true],
        ['tilde fence with math','Before\n\n~~~~text\n$$\n\n~~~\n\nstill code\n~~~~\n\nAfter.\n\n',true],
        ['fence language-looking close','Before\n\n```js\ncode\n```js\n\nstill code\n```\n\nAfter.\n\n',true],
        ['incomplete math','Before\n\n$$\na = 1\n\nb = 2\n\n',false],
      ];
      const cases=[];
      for(const [label,text,closed] of corpus){
        let failure=null;
        for(const size of [1,2,5,13,64]){
          d.S.current='markdown-'+label+'-'+size;
          for(let at=1;at<text.length+size;at+=size){
            const prefix=text.slice(0,Math.min(at,text.length)), node=d.streamingNode(prefix);
            const [done,tail]=d.splitStreaming(prefix), full=d.markdown(done);
            if(node.querySelector('.md').innerHTML!==full.innerHTML||node.querySelector('.stream-tail').textContent!==tail){
              failure={size,at,done,actual:node.querySelector('.md').innerHTML,expected:full.innerHTML};break;
            }
            if(at>=text.length)break;
          }
          if(failure)break;
          const node=d.streamingNode(text),[done,tail]=d.splitStreaming(text);
          if(closed&&(tail!==''||node.querySelector('.md').innerHTML!==d.markdown(text).innerHTML))failure={size,complete:true,done,tail};
          if(!closed&&(!tail.includes('$$')||done!=='Before\n\n'))failure={size,incomplete:true,done,tail};
        }
        cases.push({label,failure});
      }
      d.S.current='loose-list-identity';
      const initial='Intro.\n\n3. first\n\n', node=d.streamingNode(initial);
      const paragraph=node.querySelector('p'), list=node.querySelector('ol'), item=list.firstElementChild, tail=node.querySelector('.stream-tail').firstChild;
      d.streamingNode(initial+'4. second\n\n');const second=list.children[1];
      d.streamingNode(initial+'4. second\n\n5. third\n\n');
      const identities={paragraph:paragraph===node.querySelector('p'),list:list===node.querySelector('ol'),
        items:item===list.children[0]&&second===list.children[1],tail:tail===node.querySelector('.stream-tail').firstChild,
        structure:node.querySelectorAll('ol').length===1&&list.children.length===3&&list.classList.contains('loose')&&list.start===3};
      d.S.current='nested-list-identity';
      const nestedPrefix='- parent\n\n  - child one\n\n', nestedNode=d.streamingNode(nestedPrefix);
      const outer=nestedNode.querySelector('ul'),parent=outer.firstElementChild,parentText=parent.querySelector('p'),nested=parent.querySelector('ul'),child=nested.firstElementChild;
      d.streamingNode(nestedPrefix+'  - child two\n\n');
      Object.assign(identities,{nestedRoot:outer===nestedNode.querySelector('ul'),nestedParent:parent===outer.firstElementChild,
        nestedList:nested===parent.querySelector('ul'),nestedContent:child===nested.children[0]&&parentText===parent.querySelector('p')});
      const math=corpus.find(row=>row[0]==='display math blank')[1];d.S.current='math-final';const mathNode=d.streamingNode(math);
      const mathElements=mathNode.querySelectorAll('.math.block');
      return {cases,identities,math:mathElements.length===1&&mathElements[0].textContent==='a = 1\n\nb = 2'};
    }''')
    for case in result['cases']:
        check(case['failure'] is None,name+' streaming/final block parity across five burst sizes: '+case['label']+' '+json.dumps(case['failure']))
    for key,value in result['identities'].items():
        check(value,name+' loose-list sibling append preserves '+key)
    check(result['math'],name+' completed display math has one nonempty box and both equation lines')


def committed_stream_cases(page, name):
    for mode in ['complete','tool-step','background-event','background-snapshot']:
        sid='commit-'+mode
        page.evaluate(r'''sid=>{const d=__stream816;__raf816.release();d.S.current=sid;d.S.view='chat';d.S.channel={};d.S.daemonOnline=true;
          d.S.sessions=[{id:sid,name:'Reading commit fixture',updated_at:'2026-11-02T12:00:00Z'},{id:'reading-away',name:'Away fixture',updated_at:'2026-11-02T12:00:00Z'}];
          d.S.messages=[{id:sid+'-user',seq:1,role:'user',content:'Read this long streamed answer',meta:'{}',created_at:'2026-11-02T12:00:00Z'}];
          d.S.running.clear();d.S.running.add(sid);d.S.liveRun={id:sid+'-run',ended:false};d.S.thinking='Harmless visible thinking';d.S.showThinking=true;
          d.S.liveTools=[{invocationId:'reading-display-call',name:'fixture_display_work',version:1,arguments:'{}',status:'running',started:Date.now(),depth:0}];
          d.S.activity={label:'responding',since:Date.now()};
          d.S.streaming=Array.from({length:45},(_,i)=>'Unique paragraph '+i+': '+('Keep this paragraph at the same screen position while the answer is persisted. '.repeat(8))).join('\n\n')+'\n\n';
          window.commitText816=d.S.streaming;window.commitUser816=d.S.messages[0];
          d.resetReadingSession(sid);d.renderChrome();d.paintHistory();d.scrollToBottom(true);
        }''',sid)
        settle(page);detach(page)
        before=page.evaluate(r'''()=>{const box=document.querySelector('#transcript').getBoundingClientRect();
          const block=[...document.querySelectorAll('#live .streaming p')].find(e=>{const r=e.getBoundingClientRect();return r.top>=box.top+5&&r.bottom<box.bottom-10;});
          if(!block)throw new Error('No visible middle paragraph in live answer');
          window.commitBlock816=block;return {text:block.textContent,top:block.getBoundingClientRect().top};}''')
        check(page.evaluate('__stream816.S.follow') is False,name+' '+mode+' genuinely pauses in live answer')
        check(page.evaluate('commitBlock816.closest("[data-live-root]")!=null'),name+' '+mode+' anchor starts inside live root')
        if mode.startswith('background'):
            page.evaluate('(sid)=>__stream816.openSession("reading-away")',sid);settle(page)
            check(page.evaluate('__stream816.S.current')=='reading-away',name+' '+mode+' actually switches away before commit')
        page.evaluate(r'''({sid,mode})=>{const d=__stream816;
          const meta=mode==='tool-step'?JSON.stringify({tool_calls:[{id:'display-call',function:{name:'fixture_display_only',arguments:'{}'}}]}):'{}';
          const message={id:sid+'-step',seq:2,role:'assistant',content:commitText816,meta,created_at:'2026-11-02T12:01:00Z'};
          d.snapshots.set(sid,{session:d.S.sessions.find(s=>s.id===sid),messages:[commitUser816,message],queued_items:[],plan_items:[]});
          if(mode!=='background-snapshot')d.onEvent({type:mode==='tool-step'?'message_appended':'message_complete',session_id:sid,message});
        }''',{'sid':sid,'mode':mode})
        if mode.startswith('background'):
            page.evaluate('(sid)=>__stream816.openSession(sid)',sid)
        settle(page,150)
        after=page.evaluate(r'''({sid,text})=>{const message=[...document.querySelectorAll('[data-message-id]')].find(e=>e.dataset.messageId===sid+'-step');
          const block=message&&[...message.querySelectorAll('p')].find(e=>e.textContent===text);
          return {exists:!!block,top:block?.getBoundingClientRect().top,follow:__stream816.S.follow,oldConnected:commitBlock816.isConnected,current:__stream816.S.current};}''',{'sid':sid,'text':before['text']})
        check(after['exists'] and after['current']==sid,name+' '+mode+' maps to matching persisted paragraph')
        check(after['top']==before['top'],name+' '+mode+f' pixel-identical live-to-history anchor ({before["top"]} → {after["top"]})')
        check(after['follow'] is False,name+' '+mode+' preserves paused follow state')
        check(not after['oldConnected'],name+' '+mode+' really replaced the old live paragraph')
        if mode=='tool-step':
            check(page.evaluate('__stream816.S.running.has(__stream816.S.current)'),name+' Tool-call step commit is tested before Run completion')


def run_engine(p, name, url, out):
    start=PASSES
    browser=p.chromium.launch(executable_path='/usr/bin/chromium',headless=True,args=['--no-sandbox']) if name=='chromium' else p.webkit.launch(executable_path=p.webkit.executable_path,headless=True)
    context=browser.new_context(viewport={'width':1280,'height':900},reduced_motion='reduce')
    context.add_init_script(INIT)
    context.route('**/app.js',expose_script)
    page=context.new_page();page.set_default_timeout(5000)
    errors=[];page.on('pageerror',lambda e:errors.append(str(e)))
    page.goto(url);page.wait_for_function('!!window.__stream816')
    parsed=page.evaluate(r'''() => {
      const d=__stream816;d.S.current='parser-fixture';
      const first=d.streamingNode('Independent paragraph.\n\n'),paragraph=first.querySelector('.md').firstChild;
      d.streamingNode('Independent paragraph.\n\nSecond paragraph.\n\nTail');
      const appended=!!paragraph && paragraph===first.querySelector('.md').firstChild;
      d.streamingNode('- one\n\n');const loose='- one\n\n  continuation\n\nmore',node=d.streamingNode(loose),done=d.splitStreaming(loose)[0];
      const list=node.querySelector('.md').textContent===d.markdown(done).textContent && node.querySelectorAll('.md li').length===1;
      const large='Initial paragraph.\n\n'+'x'.repeat(d.MD_FORMAT_CHARS)+'\n\n';
      d.streamingNode('Initial paragraph.\n\n');const formatted=first.querySelector('.md p');d.streamingNode(large);
      const plain=first.querySelector('.md-plain'),text=plain?.firstChild;
      const cap=!!plain && plain.textContent===d.splitStreaming(large)[0] && !!formatted && !first.contains(formatted);
      d.streamingNode(large+'extra completed text\n\n');
      return {appended,list,cap,plainStable:text===first.querySelector('.md-plain')?.firstChild};
    }''')
    check(parsed['appended'],name+' independent completed Markdown chunks preserve prior paragraph')
    check(parsed['list'],name+' indented loose-list continuation matches full Markdown parsing')
    check(parsed['cap'],name+' crossing Markdown formatting bound replaces old formatted content')
    check(parsed['plainStable'],name+' large plain-text stream retains its Text node')
    markdown_block_cases(page,name)
    fixture(page)
    check(page.locator('.msg[data-message-id]').count()==30,name+' renders actual persisted fixture messages')
    supported=page.evaluate('CSS.supports("overflow-anchor","none")')
    if supported:
        check(page.locator('#transcript').evaluate('(e)=>getComputedStyle(e).overflowAnchor')=='none',name+' disables native overflow anchoring')
    else:
        check('overflow-anchor: none' in (ROOT/'web/app.css').read_text(),name+' uses explicit anchor code with no native overflow-anchor support')
    page.evaluate(r'''() => {
      const live=document.querySelector('#live'),chip=live.querySelector('details');chip.open=true;
      window.refs816={live,chip,summary:chip.querySelector('summary'),stream:live.querySelector('.streaming'),tail:live.querySelector('.stream-tail'),tailText:live.querySelector('.stream-tail').firstChild,
        completed:live.querySelector('.streaming .md')?.firstChild,thinking:live.querySelector('.thinking'),first:document.querySelector('.msg[data-message-id="A-m0"]'),who:document.querySelector('.msg').closest('.turn').querySelector('.who')};
    }''')
    settle(page)
    check(page.evaluate('!!refs816.completed'),name+' fixture contains a real completed Markdown block')
    page.evaluate('__raf816.hold();__stream816.resetStats()')
    page.evaluate(r'''() => {const d=__stream816;for(let i=0;i<200;i++){
      d.onEvent({type:'message_delta',session_id:'A',run_id:'run-A',delta:'x'});
      d.onEvent({type:'reasoning_delta',session_id:'A',run_id:'run-A',delta:'r'});
      d.onEvent({type:'tool_progress',session_id:'A',run_id:'run-A',tool_name:'fixture_display_work',invocation_id:'fixture-call',fraction:i/200,label:'batch '+i});
    }}''')
    check(page.evaluate('__stream816.S.streaming.endsWith("x".repeat(200)) && __stream816.S.thinking.endsWith("r".repeat(200))'),name+' reducer receives every burst delta')
    check(page.evaluate('__stream816.stats.live===0 && __stream816.stats.history===0 && __stream816.stats.status===0'),name+' burst performs no synchronous paint before RAF')
    check(page.evaluate('__raf816.count()')>0,name+' burst schedules animation-frame work')
    flush(page)
    check(page.evaluate('__stream816.stats.live')==1,name+' 600 delta/progress events coalesce into one live paint')
    check(page.evaluate('__stream816.stats.history')==0,name+' text burst does not repaint persisted history')
    check(page.evaluate('__stream816.stats.status<=1 && __stream816.stats.composer===0'),name+' mixed burst coalesces status and leaves composer untouched')
    check(page.evaluate('refs816.live===document.querySelector("#live") && refs816.chip===document.querySelector("#live details") && refs816.summary===document.querySelector("#live details summary")'),name+' live/tool/summary nodes retain identity')
    check(page.evaluate('refs816.chip.open'),name+' burst preserves open Tool disclosure')
    check(page.evaluate('refs816.stream===document.querySelector(".streaming") && refs816.tail===document.querySelector(".stream-tail") && refs816.tailText===document.querySelector(".stream-tail").firstChild'),name+' unfinished stream updates existing Text node')
    check(page.evaluate('refs816.thinking===document.querySelector("#live .thinking")'),name+' reasoning block retains identity')
    page.evaluate(r'''__stream816.onEvent({type:'message_delta',session_id:'A',run_id:'run-A',delta:'\n\nSecond complete paragraph.\n\nNew tail'})''')
    flush(page)
    check(page.evaluate('refs816.completed===document.querySelector(".streaming .md")?.firstChild'),name+' completed stream prefix retains old DOM blocks')
    drain(page)
    page.evaluate(r'''() => {window.timer816=document.querySelector('[data-live-timer]')?.firstChild;if(!timer816)throw new Error('Missing live timer Text');window.timerData816=timer816.data;window.timerMutations816=[];
      window.timerObserver816=new MutationObserver(items=>timerMutations816.push(...items));timerObserver816.observe(document.querySelector('#transcript'),{childList:true,subtree:true,characterData:true});
      __stream816.resetStats();window.realNow816=Date.now;Date.now=()=>realNow816()+3000;__raf816.tick(1000);Date.now=realNow816;
    }''')
    settle(page,25)
    check(page.evaluate('timer816===document.querySelector("[data-live-timer]").firstChild && timer816.data!==timerData816'),name+' elapsed interval updates existing time Text')
    check(page.evaluate('__stream816.stats.history===0 && __stream816.stats.live===0'),name+' elapsed interval does not repaint transcript')
    check(page.evaluate('!timerMutations816.some(m=>m.type==="childList")'),name+' elapsed interval changes only text, not structure')
    page.evaluate('timerObserver816.disconnect();__raf816.release()')
    settle(page)

    # Pause above live output. Programmatic disclosure/image changes above the
    # viewport must compensate layout against the same stable message anchor.
    detach(page);before=anchor(page)
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' merely scrolling away creates no unseen-output pill')
    page.evaluate('__raf816.tick(1000)');settle(page)
    unchanged_anchor(page,before,name+' elapsed tick while paused')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' paused timer tick creates no unseen output')
    page.evaluate('__raf816.hold();__stream816.resetStats()')
    page.evaluate(r'''() => {for(let i=0;i<50;i++)__stream816.onEvent({type:'tool_progress',session_id:'A',run_id:'run-A',tool_name:'fixture_display_work',invocation_id:'fixture-call',fraction:i/50,label:'progress '+i});}''')
    flush(page);drain(page)
    unchanged_anchor(page,before,name+' progress-only update while paused')
    check(page.evaluate('__stream816.stats.live===1 && __stream816.stats.status===0 && __stream816.stats.composer===0'),name+' progress burst touches live display only')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' progress-only metadata creates no unseen-output pill')
    page.evaluate('__raf816.release()')
    page.evaluate('__stream816.setThinking(false);__raf816.hold();__stream816.resetStats();__stream816.onEvent({type:"reasoning_delta",session_id:"A",run_id:"run-A",delta:" Hidden reasoning fixture"})')
    flush(page);drain(page)
    unchanged_anchor(page,before,name+' hidden reasoning update')
    check(page.locator('#live .thinking').count()==0,name+' hidden reasoning never creates visible transcript content')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' hidden reasoning creates no unseen-output pill')
    page.evaluate('__raf816.release()')
    page.evaluate('document.querySelector("[data-message-id=A-m1] details").open=true')
    settle(page,120)
    unchanged_anchor(page,before,name+' Tool chip expansion above viewport')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' pure disclosure layout creates no unseen output')
    page.evaluate('''() => {const d=__stream816;const entry=[...d.diagrams].find(([src])=>src.includes('fixture_delayed_diagram_A'));if(!entry)throw new Error('No pending diagram');entry[1]();}''')
    page.wait_for_selector('[data-message-id=A-m3] .diagram-img')
    page.wait_for_function('document.querySelector("[data-message-id=A-m3] .diagram-img").complete')
    settle(page,140)
    unchanged_anchor(page,before,name+' delayed diagram finish above viewport')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' pure diagram layout creates no unseen output')
    page.screenshot(path=str(out/'paused-anchor.png'))

    page.evaluate('__raf816.hold();__stream816.resetStats()')
    page.evaluate(r'''() => {const d=__stream816;for(let i=0;i<3;i++)d.onEvent({type:'message_appended',session_id:'A',message:{id:'A-tail-'+i,seq:30+i,role:'user',content:'Appended while reading '+i,meta:'{}',created_at:'2026-11-02T13:00:00Z'}});}''')
    check(page.evaluate('__stream816.stats.history')==0,name+' persisted burst waits for RAF')
    flush(page);drain(page)
    check(page.evaluate('__stream816.stats.history')==1,name+' persisted tail batch patches history once')
    check(page.evaluate('refs816.first===document.querySelector("[data-message-id=A-m0]") && refs816.who===document.querySelector("[data-message-id=A-m0]").closest(".turn").querySelector(".who")'),name+' tail patch retains old message and speaker wrappers')
    unchanged_anchor(page,before,name+' persisted message append')
    check(not page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' new persisted content below viewport shows jump pill')
    page.evaluate('__raf816.release()');page.locator('#jump').click();settle(page)
    check(page.evaluate('__stream816.S.follow'),name+' explicit jump tap resumes follow')
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' jump tap acknowledges unseen output')
    check(page.locator('#transcript').evaluate('(e)=>Math.abs(e.scrollHeight-e.clientHeight-e.scrollTop)<=1'),name+' jump tap reaches actual transcript bottom')

    # A correction of an already-persisted body below the viewport is new
    # output even though its stable message wrapper is not newly inserted.
    detach(page);patched_anchor=anchor(page)
    check(page.locator('[data-message-id=A-m29]').evaluate('(e)=>e.getBoundingClientRect().top>document.querySelector("#transcript").getBoundingClientRect().bottom'),name+' persisted body patch target is below viewport')
    page.evaluate('window.oldBodyWrapper816=document.querySelector("[data-message-id=A-m29]");__raf816.hold()')
    page.evaluate(r'''__stream816.onEvent({type:'message_complete',session_id:'A',message:{id:'A-m29',seq:29,role:'assistant',content:'BODY_PATCH_BELOW_VIEWPORT '+('new material '.repeat(30)),meta:'{}',created_at:'2026-11-02T13:00:00Z'}})''')
    flush(page);drain(page)
    check(page.evaluate('oldBodyWrapper816===document.querySelector("[data-message-id=A-m29]") && oldBodyWrapper816.textContent.includes("BODY_PATCH_BELOW_VIEWPORT")'),name+' persisted body correction reuses its message wrapper')
    unchanged_anchor(page,patched_anchor,name+' persisted body correction below viewport')
    check(not page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' corrected persisted body below viewport shows pill')
    page.evaluate('__raf816.release()');page.locator('#jump').click();settle(page)
    page.evaluate('__raf816.hold();__stream816.onEvent({type:"message_delta",session_id:"A",run_id:"run-A",delta:"Fresh streaming tail"})')
    flush(page);drain(page);page.evaluate('__raf816.release()');settle(page)

    # A same-height delta below the viewport is still new output. Conversely,
    # visible text or timer/layout changes alone must not create a false pill.
    detach(page);before=anchor(page)
    old_height=page.locator('#transcript').evaluate('(e)=>e.scrollHeight')
    page.evaluate('__raf816.hold();__stream816.onEvent({type:"message_delta",session_id:"A",run_id:"run-A",delta:"Z"})')
    flush(page);drain(page)
    check(page.locator('#transcript').evaluate('(e)=>e.scrollHeight')==old_height,name+' single tail delta leaves scrollHeight unchanged')
    unchanged_anchor(page,before,name+' same-height streaming update')
    check(not page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' same-height unseen tail update shows pill')
    page.evaluate('__raf816.release();document.querySelector("#transcript").scrollTop=document.querySelector("#transcript").scrollHeight')
    settle(page)
    check(page.evaluate('__stream816.S.follow') is False,name+' programmatic scroll-to-bottom does not resume follow')
    page.evaluate('__raf816.hold();__stream816.onEvent({type:"message_delta",session_id:"A",run_id:"run-A",delta:"Y"})')
    flush(page);drain(page)
    check(page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' text already visible at bottom creates no unseen pill')
    page.evaluate('__raf816.release()')
    page.locator('#transcript').focus();page.keyboard.press('End');settle(page)
    check(page.evaluate('__stream816.S.follow'),name+' explicit user End-to-bottom resumes follow')

    page.evaluate('__raf816.hold();window.tailWrapper816=document.querySelector("[data-message-id=A-tail-2]")')
    page.evaluate(r'''__stream816.onEvent({type:'message_complete',session_id:'A',message:{id:'A-tail-2',seq:32,role:'user',content:'Patched persisted tail',meta:'{}',created_at:'2026-11-02T13:00:00Z'}})''')
    flush(page);drain(page)
    check(page.evaluate('tailWrapper816===document.querySelector("[data-message-id=A-tail-2]") && tailWrapper816.textContent.includes("Patched persisted tail")'),name+' existing persisted tail patches its stable message wrapper')
    check(page.evaluate('refs816.first===document.querySelector("[data-message-id=A-m0]") && refs816.live===document.querySelector("#live")'),name+' tail correction preserves prior history and live roots')
    page.evaluate('__stream816.resetStats();for(let i=0;i<100;i++)__stream816.onEvent({type:"message_delta",session_id:"offscreen",run_id:"other-run",delta:"ignored"})')
    check(page.evaluate('__raf816.count()===0 && __stream816.stats.live===0 && __stream816.stats.history===0'),name+' off-Session delta burst schedules no empty RAF work')
    page.evaluate('__raf816.release()')

    # Queued rendering is scoped to the Session/view that owns it. Late diagram
    # completion must not move or reattach the newly selected Session.
    page.evaluate(r'''() => {__stream816.onEvent({type:'message_appended',session_id:'A',message:{id:'A-late-diagram',seq:100,role:'assistant',content:'```mermaid\nfixture_delayed_diagram_A_late\n```',meta:'{}',created_at:'2026-11-02T13:00:00Z'}});}''')
    settle(page)
    page.wait_for_function('[...__stream816.diagrams.keys()].some(src=>src.includes("fixture_delayed_diagram_A_late"))')
    page.evaluate('__raf816.hold();__stream816.resetStats();__stream816.onEvent({type:"message_delta",session_id:"A",run_id:"run-A",delta:" queued-A"})')
    page.evaluate('() => __stream816.openSession("B")')
    drain(page)
    check(page.evaluate('__stream816.S.current')=='B',name+' actual Session switch selects B')
    check(page.locator('[data-message-id^="B-m"]').count()==24,name+' Session switch paints only B history')
    page.evaluate('__raf816.release()')
    detach(page);b_anchor=anchor(page)
    settle(page,1300)
    page.evaluate('document.querySelector("#transcript").scrollTop=document.querySelector("#transcript").scrollHeight')
    settle(page)
    check(page.evaluate('__stream816.S.follow') is False,name+' programmatic scroll after user-intent window cannot resume follow')
    unchanged_anchor(page,b_anchor,name+' delayed programmatic scroll while paused')
    page.evaluate('''() => {const d=__stream816;const entry=[...d.diagrams].find(([src])=>src.includes('fixture_delayed_diagram_A_late'));if(!entry)throw new Error('Missing pending stale Session A diagram');entry[1]();}''')
    flush(page,True);drain(page)
    unchanged_anchor(page,b_anchor,name+' stale Session A frame/image callbacks')
    check('queued-A' not in page.locator('#transcript').inner_text(),name+' stale Session A content never paints into B')
    page.evaluate('__raf816.release();__stream816.S.running.add("B");__stream816.S.liveRun={id:"run-B",ended:false};__stream816.S.activity={label:"responding",since:Date.now()};__stream816.paintLive()')
    settle(page)
    page.evaluate('__raf816.hold();__stream816.resetStats();__stream816.onEvent({type:"message_delta",session_id:"B",run_id:"run-B",delta:"B view-scoped delta"});__stream816.setView("tools")')
    flush(page,True);drain(page)
    check(page.evaluate('__stream816.stats.live===0 && __stream816.stats.history===0'),name+' stale Chat RAF does not paint after switching views')
    page.evaluate('__stream816.setView("chat");__raf816.release()')
    settle(page)
    check('B view-scoped delta' in page.locator('#transcript').inner_text(),name+' returning to Chat shows latest reduced stream state')
    check(page.evaluate('__stream816.S.follow') is False,name+' returning to Chat preserves paused follow state')
    check(not page.locator('#jump').evaluate('(e)=>e.classList.contains("hidden")'),name+' returning to Chat reports output received below the paused viewport')

    long_message_case(page,name,out)
    committed_stream_cases(page,name)
    check(not errors,name+' no browser page errors: '+repr(errors))
    page.screenshot(path=str(out/'final-state.png'))
    report={'engine':name,'version':browser.version,'assertions':PASSES-start,'nativeOverflowAnchor':supported,'physicalSafari':False,'errors':errors}
    (out/'report.json').write_text(json.dumps(report,indent=2))
    context.close();browser.close()
    print('ENGINE',json.dumps(report),flush=True)
    return report


def run():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('artifacts',nargs='?',default='/tmp/formless-816-browser')
    parser.add_argument('--browser',choices=['all','chromium','webkit'],default='all',help='all is required for complete validation; a single engine is partial')
    args=parser.parse_args()
    out=pathlib.Path(args.artifacts);out.mkdir(parents=True,exist_ok=True)
    server=http.server.ThreadingHTTPServer(('127.0.0.1',0),functools.partial(QuietHandler,directory=str(ROOT/'web')))
    threading.Thread(target=server.serve_forever,daemon=True).start()
    reports=[]
    try:
        with sync_playwright() as p:
            names=['chromium','webkit'] if args.browser=='all' else [args.browser]
            for name in names:
                engine_out=out/name;engine_out.mkdir(exist_ok=True)
                reports.append(run_engine(p,name,f'http://127.0.0.1:{server.server_port}/',engine_out))
    finally:
        server.shutdown()
    (out/'report.json').write_text(json.dumps({'complete':args.browser=='all','results':reports},indent=2))
    print(f'PASS total: {PASSES}; engines: {args.browser}; artifacts: {out}',flush=True)


if __name__=='__main__':
    run()
