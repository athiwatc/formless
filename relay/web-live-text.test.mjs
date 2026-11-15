// Incremental state against the pre-cache parser, independent of browser timing.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
function fn(name) {const start=source.indexOf('  function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
function fixture() {
  const c=vm.createContext({S:{current:'A',liveRun:{id:'r1'},streaming:'',thinking:''}});
  vm.runInContext(source.slice(source.indexOf('  const FENCE'),source.indexOf('  function splitRow')),c);
  vm.runInContext(['splitStreaming','thinkingTail','currentLiveText','clearLiveText','appendLiveText','cachedLiveText'].map(fn).join('\n')+'\nconst liveText={session:undefined,run:undefined,answer:{},thinking:{},expanded:false};',c);
  return c;
}
// Exact completed-prefix rule before #1537: the incomplete final line is
// provisional. The incremental parser must agree on every observable cut.
function reference(text,c) {
  let fence=null,math=false,cut=0,at=0;
  for(const line of text.split('\n')) {
    const next=at+line.length+1;
    if(math)math=c.mathBlockOpen(line,true);
    else if(fence) {if(c.fenceCloses(line,fence))fence=null;}
    else {const marker=line.match(/^\s*(`{3,}|~{3,})\s*([^\s`{,]*)/);if(marker)fence=marker[1];else if(/^\s*\$\$/.test(line))math=c.mathBlockOpen(line);else if(!line.trim()&&next<=text.length)cut=next;}
    at=next;
  }
  return [text.slice(0,cut),text.slice(cut)];
}
const reasoning=text=>{const lines=text.split('\n');return lines.length>6?'… '+lines.slice(-6).join('\n'):text;};

test('each partial delimiter, newline and replacement agrees with the original split rule',()=>{
  const c=fixture();
  const corpus=[
    'Before\n\n```js\n\ncode\n```\n\nAfter\n\n',
    'Before\r\n\r\n~~~~text\r\n$$\r\n\r\n~~~\r\n\r\n~~~~\r\n\r\nTail',
    '$$\n```\n\nx\n```\n\ny $$ trailing\n\nAfter\n\n',
    '$$x$$\n\nA\n\n```\ncode\n```js\n\nnot closed\n```\n\n',
    '- one\n\n * * *\n\n- two\n\nTail',
    'ไทย 👩‍💻\n \t\n\n$$\nx\n\ny\n$$\n\n',
    'head\n\n````\ncode\n```\n\nnever closed',
    '\n\n\n', '', 'no newline', '  $$partial',
  ];
  for(const text of corpus)for(const step of [1,2,7,64]) {
    const state={};
    for(let end=0;end<text.length+step;end+=step) {
      const prefix=text.slice(0,end),value=c.splitStreaming(prefix,state);
      assert.deepEqual([...value],reference(prefix,c),JSON.stringify(prefix));
      assert.strictEqual(c.splitStreaming(prefix,state),value,'unchanged result is reused');
    }
    for(const replacement of ['different\n\nbody','a','',text,'$$\n\n'])
      assert.deepEqual([...c.splitStreaming(replacement,state)],reference(replacement,c));
  }
});

test('seeded mixed delimiter streams retain exact cuts at arbitrary append boundaries',()=>{
  const c=fixture();let seed=17;
  const next=()=>seed=(Math.imul(seed,1664525)+1013904223)>>>0;
  const parts=['text','\n','\n\n','```','~~~~','$$','  ','ไทย','\r\n','- item'];
  for(let trial=0;trial<100;trial++) {
    let text='';for(let i=0;i<40;i++)text+=parts[next()%parts.length];
    const state={};let end=0;
    while(end<text.length){end+=1+next()%11;const prefix=text.slice(0,end);assert.deepEqual([...c.splitStreaming(prefix,state)],reference(prefix,c));}
  }
});

test('reasoning cache keeps exactly the last six logical lines and resets on replacement',()=>{
  const c=fixture();const state={};
  const text='first\n\nไทย\n👩‍💻\n4\n5\n6\n7\n\nlast';
  for(let end=0;end<=text.length;end++) {
    const prefix=text.slice(0,end);
    assert.equal(c.thinkingTail(prefix,state),reasoning(prefix));
    assert.ok(state.lines.length<=6);
  }
  for(const replacement of ['replacement','\n','a\nb\nc\nd\ne\nf\n',''])
    assert.equal(c.thinkingTail(replacement,state),reasoning(replacement));
});

test('completed-line scans cover appended suffixes; unchanged renders do zero scanner work',()=>{
  const c=fixture();
  vm.runInContext(`const scan={newlineCharacters:0,prefixVerificationCharacters:0};
    const originalIndexOf=String.prototype.indexOf,originalStartsWith=String.prototype.startsWith;
    String.prototype.indexOf=function(needle,start=0){const result=originalIndexOf.call(this,needle,start);if(needle==='\\n')scan.newlineCharacters+=(result<0?this.length:result+1)-start;return result;};
    String.prototype.startsWith=function(prefix,...rest){scan.prefixVerificationCharacters+=prefix.length;return originalStartsWith.call(this,prefix,...rest);};`,c);
  const state={},tail={};let text='',oldFullScanCharacters=0;
  for(let i=0;i<1000;i++){text+='line '+i+'\n\n';oldFullScanCharacters+=text.length;c.splitStreaming(text,state);c.thinkingTail(text,tail);}
  const metrics=vm.runInContext('({...scan})',c);
  assert.equal(metrics.newlineCharacters,text.length*2,'answer and reasoning each search only appended suffixes');
  const before=JSON.stringify(metrics);
  for(let i=0;i<100;i++){c.splitStreaming(text,state);c.thinkingTail(text,tail);}
  assert.equal(JSON.stringify(vm.runInContext('({...scan})',c)),before);
  vm.runInContext('scan.newlineCharacters=0;scan.prefixVerificationCharacters=0',c);
  for(let i=0;i<1000;i++){
    c.appendLiveText('streaming','line '+i+'\n\n');c.appendLiveText('thinking','line '+i+'\n\n');
    c.cachedLiveText('answer',c.S.streaming);c.cachedLiveText('thinking',c.S.thinking);
  }
  const eventMetrics=vm.runInContext('({...scan})',c);
  assert.equal(eventMetrics.newlineCharacters,text.length*2);
  assert.equal(eventMetrics.prefixVerificationCharacters,0,'verified reducer appends do not compare growing prefixes');
  c.S.streaming='replacement\n\n';c.appendLiveText('streaming','tail');
  assert.deepEqual([...c.cachedLiveText('answer',c.S.streaming)],reference(c.S.streaming,c));
  console.log(JSON.stringify({frames:1000,sourceCharacters:text.length,legacyAnswerFullScanCharacters:oldFullScanCharacters,genericFallback:metrics,eventAppends:eventMetrics}));
  // Prefix verification, string slicing/concatenation and Markdown/DOM work
  // are separate costs: this test does not establish total linear rendering.
});

test('same-Session new answers, retries, Run changes and Session switches reset caches',()=>{
  const c=fixture();let state=c.currentLiveText(),answer=state.answer,tail=state.thinking;
  c.splitStreaming('old\n\n',answer);c.thinkingTail('old',tail);state.expanded=true;
  c.S.streaming='old';c.S.thinking='old';c.clearLiveText();
  state=c.currentLiveText();assert.notStrictEqual(state.answer,answer);assert.notStrictEqual(state.thinking,tail);assert.equal(state.expanded,false);assert.equal(c.S.streaming,'');assert.equal(c.S.thinking,'');
  answer=state.answer;c.S.liveRun={id:'r2'};assert.notStrictEqual(c.currentLiveText().answer,answer);
  answer=c.currentLiveText().answer;c.S.current='B';assert.notStrictEqual(c.currentLiveText().answer,answer);
  answer=c.currentLiveText().answer;c.S.current='A';assert.notStrictEqual(c.currentLiveText().answer,answer);
});
