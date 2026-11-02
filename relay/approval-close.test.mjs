import { installEventRenderFixture } from './test-web-render.mjs';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
const fn=name=>{const start=source.search(new RegExp('  (?:async )?function '+name+'\\('));assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);};
function setup(){
  const notes=[];
  const c=vm.createContext({S:{current:'s',running:new Set(['s']),approvals:[],questions:[],liveTools:[]},note:text=>notes.push(text),notifyFor(){},branchSummaryEvent(){},renderApproval(){},renderStatus(){},renderComposer(){},showQuestion(){},cancelAsk(){},toast(){},pickedPaths:()=>({}),isBroad:()=>false,isRunning:()=>true,setActivity(){},prefs:{set(){}},addApproval(ev){if(!c.S.approvals.some(a=>a.id===ev.approval_id))c.S.approvals.push({kind:'tool',id:ev.approval_id});},queueQuestion(){}});
  installEventRenderFixture(c, fn);
  vm.runInContext(['onEvent','applySessionLiveState','decide'].map(fn).join('\n'),c);
  return{c,notes};
}
test('global close retires only matching tool prompt and never reports grant or denial',()=>{
  const{c,notes}=setup();c.S.approvals=[{kind:'tool',id:'close'},{kind:'tool',id:'other'},{kind:'promotion',id:'close'}];
  c.onEvent({type:'approval_closed',approval_id:'close'});
  assert.deepEqual(Array.from(c.S.approvals,a=>a.kind+':'+a.id),['tool:other','promotion:close']);
  assert.equal(notes.length,1);assert.doesNotMatch(notes[0],/granted|denied|approved/);
  c.onEvent({type:'approval_closed',approval_id:'close'});assert.equal(notes.length,1);
});
test('closed approval in racing event cannot reappear from stale reconnect attention',()=>{
  const{c}=setup();const held={type:'approval_requested',approval_id:'close',session_id:'s'};
  c.S.approvals=[{kind:'tool',id:'close'}];
  c.applySessionLiveState('s',{running:true,pending_events:[held]},[{type:'approval_closed',approval_id:'close'}]);
  assert.equal(c.S.approvals.length,0);
});
test('cancel-first close while decision is in flight never turns false ACK into success',async()=>{
  const{c,notes}=setup();let resolve;c.request=()=>new Promise(r=>resolve=r);
  c.S.approvals=[{kind:'tool',id:'close',tool_name:'writer',capabilities:[]}];
  const answering=c.decide(true,'once');c.onEvent({type:'approval_closed',approval_id:'close'});resolve({resolved:false});await answering;
  assert.equal(c.S.approvals.length,0);assert.ok(notes.some(n=>n.includes('no decision was applied')));assert.ok(!notes.some(n=>/^approved |^denied |approval granted/.test(n)));
});
test('answer-first ACK records one decision even when matching close arrives late',async()=>{
  const{c,notes}=setup();c.request=async()=>({resolved:true});c.S.approvals=[{kind:'tool',id:'close',tool_name:'writer',capabilities:[]}];
  await c.decide(true,'once');c.onEvent({type:'approval_closed',approval_id:'close'});
  assert.equal(notes.filter(n=>n==='approved writer once').length,1);assert.ok(!notes.some(n=>n.includes('denied')));
});
test('lost acknowledgement is uncertain and cannot claim transport never delivered a decision',async()=>{
  const{c,notes}=setup();c.request=async()=>{throw new Error('reply connection lost');};c.S.approvals=[{kind:'tool',id:'held',tool_name:'writer',capabilities:[]}];
  await c.decide(true,'once');
  assert.equal(c.S.approvals.length,1);
  assert.ok(notes.some(n=>/not confirmed.*Inspect current approvals or grants/.test(n)));
  assert.ok(!notes.some(n=>/not delivered|^approved |^denied /.test(n)));
});
