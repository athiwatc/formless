import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
const fn=name=>{const start=source.search(new RegExp('  (?:async )?function '+name+'\\('));assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);};
const deferred=()=>{let resolve;const promise=new Promise(r=>resolve=r);return{promise,resolve};};
const metadata=(run,status,time='2026-11-02T01:00:00Z')=>({run_id:run,updated_at:time,run_status:status,previous_task:false,origin:'live'});
const recap=(status,count)=>({status,total_calls:count,unfinished_calls:2,steps:4,duration_ms:1000,used:['same ×'+Math.max(0,count-2)],built:['created example v1'],files:[],steered:1,untracked_writes_possible:true});
const snapshot=run=>({session:{id:'s'},messages:[],queued_items:[],running:false,pending_events:[],plan:[{content:'original step',status:'in_progress'}],plan_state:metadata(run,'paused'),latest_run:{run_id:run,status:'paused'},last_recap:{run_id:run,recap:recap('paused',7)}});
const text=node=>typeof node==='string'?node:(node?.children||[]).map(text).join(' ');
function setup(){
  const elements=new Map();const h=(tag,props,...children)=>({tag,props,children:children.flat(Infinity).filter(x=>x!=null),replaceChildren(...kids){this.children=kids.flat(Infinity);},classList:{toggle(){}}});
  const $=id=>{if(!elements.has(id))elements.set(id,h('div',{}));return elements.get(id);};
  let renders=0;
  const c=vm.createContext({S:{current:'s',view:'chat',sessions:[],messages:[],liveTools:[],running:new Set(),approvals:[],questions:[]},h,$,tr:x=>x,forecastRevision:0,scheduleForecast(){},branchSummaryEvent(){},notifyFor(){},note(){},invalidateBranchSummary(){},closeSidebar(){},renderHistory(){renders++;},renderRunTools(){},renderStatus(){},renderComposer(){},renderQueue(){},renderChips(){},renderStep(){},renderRunBanner(){},renderAll(){},renderLive(){},renderApproval(){},showQuestion(){},cancelAsk(){},setActivity(){},clearQuestions(){},addApproval(){},queueQuestion(){},rendered:new Map(),prefs:{set(){}},request:async()=>({}),act:async()=>snapshot('r1'),isRunning:()=>c.S.running.has(c.S.current)});
  vm.runInContext(['planStateLabel','renderPlan','runFilesCaveat','recapBlock','restoreSessionRunState','applySessionLiveState','openSession','onEvent','clearLiveState'].map(fn).join('\n')+'\nlet sessionSeq=0;const CLIENT_LABEL="fixture";const attachmentDrafts={};',c);
  return{c,elements,renders:()=>renders};
}
test('actual openSession restores exact terminal recap and lifecycle; clear and legacy remain honest',async()=>{
  const{c,elements}=setup();await c.openSession('s');
  assert.equal(c.S.recapRunId,'r1');assert.equal(c.S.recap.total_calls,7);assert.equal(c.S.planState.run_status,'paused');
  assert.match(text(elements.get('plan')),/paused/);assert.match(text(elements.get('plan')),/original step/);
  const block=c.recapBlock(c.S.recap);assert.match(text(block.children[0]),/paused.*7 calls.*2 unfinished calls/);assert.match(text(block),/programs.*MCP tools/);
  c.act=async()=>({...snapshot('r2'),last_recap:null,plan_state:null});await c.openSession('s');assert.equal(c.S.recap,null);assert.equal(c.S.planState,null);
});
test('snapshot races preserve newest plan and recap without replaying or duplicate rendering',async()=>{
  const{c,renders}=setup(),wait=deferred();c.act=()=>wait.promise;
  const loading=c.openSession('s');
  c.onEvent({type:'run_started',session_id:'s',run_id:'r2'});
  c.onEvent({type:'plan_updated',session_id:'s',items:[{content:'new task',status:'in_progress'}],plan_state:metadata('r2','running','2026-11-02T02:00:00Z')});
  c.onEvent({type:'run_recap',session_id:'s',run_id:'r2',recap:recap('failed',42)});
  c.onEvent({type:'run_recap',session_id:'s',run_id:'r1',recap:recap('completed',1)});
  c.onEvent({type:'run_finished',session_id:'s',run_id:'r1',status:'completed'});
  wait.resolve(snapshot('r1'));await loading;
  assert.equal(c.S.plan[0].content,'new task');assert.equal(c.S.planState.run_id,'r2');assert.equal(c.S.recapRunId,'r2');assert.equal(c.S.recap.total_calls,42);assert.ok(c.S.running.has('s'));
  const before=renders();c.onEvent({type:'run_recap',session_id:'s',run_id:'r2',recap:recap('failed',42)});assert.equal(renders(),before);
});
test('older owned plan event cannot overwrite current state; copied interrupted plan stays visible',()=>{
  const{c,elements}=setup();c.S.liveRun={id:'r2',ended:false};
  c.onEvent({type:'plan_updated',session_id:'s',items:[{content:'current',status:'pending'}],plan_state:metadata('r2','running','2026-11-02T02:00:00Z')});
  c.onEvent({type:'plan_updated',session_id:'s',items:[{content:'old',status:'in_progress'}],plan_state:metadata('r1','paused')});assert.equal(c.S.plan[0].content,'current');
  c.S.plan=[{content:'copied step',status:'completed'}];c.S.planState={...metadata(null,'interrupted'),origin:'copied',previous_task:true};c.renderPlan();
  assert.match(text(elements.get('plan')),/previous task.*interrupted.*copied/);assert.match(text(elements.get('plan')),/copied step/);
});
test('connection replacement discards delayed stored recap and plan',async()=>{
  const{c}=setup(),wait=deferred();c.act=()=>wait.promise;const loading=c.openSession('s');c.clearLiveState();wait.resolve(snapshot('r1'));await loading;assert.equal(c.S.recap,null);assert.equal(c.S.planState,null);
});
test('all unfinished calls are a subset of total and never labeled finished outcomes',()=>{
  const{c}=setup();
  const block=c.recapBlock({...recap('paused',5),unfinished_calls:5,used:[],built:[]});
  assert.match(text(block.children[0]),/5 calls.*5 unfinished calls/);
  assert.doesNotMatch(text(block),/5 finished calls|success/);
  assert.ok(!block.children.some(node=>text(node)==='Used'));
});
