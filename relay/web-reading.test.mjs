import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
function fn(name){const start=source.indexOf('  function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
const text='A distinct streamed paragraph. '.repeat(20);
const stamp=()=>({block:1,length:text.length,head:text.slice(0,128),tail:text.slice(-128),afterSeq:10});
function fixture(){const roots=[],live={querySelectorAll:()=>[]};const box={contains:n=>roots.some(r=>r===n||r.blocks.includes(n)),querySelector:()=>live,querySelectorAll:()=>roots};
 const c=vm.createContext({S:{messages:[]},reading:{session:'A',anchor:null,saved:new Map()},$:()=>box,anchorBlocks:'p'});
 vm.runInContext(['readingStreamMatches','remapReadingStream','commitReadingStream','resolveReadingBlock'].map(fn).join('\n'),c);return{c,roots,live};}
test('missing semantic block never resolves to an empty live or history root',()=>{
 const{c,roots,live}=fixture();const history={dataset:{messageId:'old'},blocks:[],querySelectorAll(){return this.blocks;}};roots.push(history);
 assert.equal(c.resolveReadingBlock({live:true,block:4}),undefined);
 assert.equal(c.resolveReadingBlock({live:false,id:'old',block:4}),undefined);
 assert.equal(c.resolveReadingBlock({live:true,block:-1}),live);
 assert.equal(c.resolveReadingBlock({live:false,id:'old',block:-1}),history);
});
test('saved live bookmark remaps to its first matching committed step, excluding old and unrelated messages',()=>{
 const{c,roots}=fixture();const blocks=[{},{}];const history={dataset:{messageId:'step'},blocks,querySelectorAll(){return this.blocks;}};roots.push(history);
 c.S.messages=[{id:'old',role:'assistant',seq:9,content:text},{id:'other',role:'assistant',seq:11,content:'unrelated'},
  {id:'step',role:'assistant',seq:12,content:text+'Later output.'},{id:'next',role:'assistant',seq:14,content:text}];
 const candidate={live:true,block:8,node:null,stream:stamp()};assert.equal(c.resolveReadingBlock(candidate),blocks[1]);
 assert.equal(candidate.id,'step');assert.equal(candidate.live,false);assert.equal(candidate.block,1);
});
test('background commit maps only that Session bookmark and never retargets an already committed anchor',()=>{
 const{c}=fixture();const a={live:true,stream:stamp()},b={live:true,stream:stamp()};c.reading.session='B';c.reading.anchor={candidates:[b]};c.reading.saved.set('A',{anchor:{candidates:[a]}});
 c.commitReadingStream('A',{id:'step',role:'assistant',seq:11,content:text});assert.equal(a.id,'step');assert.equal(b.live,true);
 c.commitReadingStream('A',{id:'next',role:'assistant',seq:12,content:text});assert.equal(a.id,'step');
 assert.equal(c.readingStreamMatches({role:'tool',seq:11,content:text},stamp()),false);
 assert.equal(c.readingStreamMatches({role:'assistant',seq:11,content:'x'+text.slice(1)},stamp()),false);
});
test('hover-only visibility does not schedule recovery; actual removal is coalesced and respects intervening focus',()=>{
 const body={},frames=[],returns=[];const c=vm.createContext({document:{body,activeElement:body},requestAnimationFrame:cb=>{frames.push(cb);return frames.length;},focusReturn:target=>returns.push(target)});
 vm.runInContext('let focusRecoveryFrame=0,lastFocusTarget=null;\n'+['focusRemovedByState','scheduleFocusRecovery'].map(fn).join('\n')+'\nfunction target(value){lastFocusTarget=value;}',c);
 const hover={isConnected:true,closest:()=>null,getClientRects(){throw new Error('unexpected layout read');}};c.target(hover);c.scheduleFocusRecovery();assert.equal(frames.length,0);
 const removed={isConnected:false};c.target(removed);c.scheduleFocusRecovery();c.scheduleFocusRecovery();assert.equal(frames.length,1);
 c.document.activeElement={};frames.shift()();assert.equal(returns.length,0);
 c.document.activeElement=body;c.scheduleFocusRecovery();frames.shift()();assert.equal(returns[0],removed);
});
