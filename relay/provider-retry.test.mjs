import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
const fn=name=>{const start=source.search(new RegExp('  (?:async )?function '+name+'\\('));assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);};
const command=source.split('\n').find(line=>line.includes('{ name: "resend"')).trim().replace(/,$/,'');
function retrySetup(){const notes=[],calls=[];const c=vm.createContext({S:{current:'s'},note:text=>notes.push(text),request:async request=>{calls.push(request);return{retrying:false};}});vm.runInContext(fn('retryProviderRequest')+'\nglobalThis.command='+command+';',c);return{c,notes,calls};}
test('actual resend command reports accepted and no-op replies without optimistic success',async()=>{
  for(const retrying of [false,true]){
    const{c,notes,calls}=retrySetup();let resolve;c.request=request=>{calls.push(request);return new Promise(r=>resolve=r);};
    const pending=c.command.run();assert.equal(notes.length,0);assert.equal(calls[0].type,'retry_provider_request');assert.equal(calls[0].session_id,'s');
    resolve({retrying});await pending;assert.match(notes[0],retrying?/^Retry requested/:/^Nothing was retried/);assert.doesNotMatch(notes[0],/Re-sending the request/);
  }
});
test('retry lost acknowledgement stays uncertain and refers to original Session',async()=>{
  const{c,notes}=retrySetup();let reject;c.request=()=>new Promise((_,r)=>reject=r);const pending=c.command.run();c.S.current='other';reject(new Error('reply lost'));await pending;
  assert.match(notes[0],/^Session s: Provider retry was not confirmed/);assert.doesNotMatch(notes[0],/not delivered|Retry requested/);
});
test('retry missing acknowledgement cannot fabricate success or future retry',async()=>{
  const{c,notes}=retrySetup();c.request=async()=>({});await c.command.run();assert.match(notes[0],/did not confirm/);assert.doesNotMatch(notes[0],/^Retry requested/);
});
test('actual Session attach keeps persisted stuck reason visible with Resume guidance',async()=>{
  const elements=new Map();const h=(tag,props,...children)=>({tag,props,children:children.flat(Infinity).filter(x=>x!=null),classList:{toggle(){}},replaceChildren(...kids){this.children=kids.flat(Infinity);}});const $=id=>{if(!elements.has(id))elements.set(id,h('div',{}));return elements.get(id);};
  const c=vm.createContext({S:{current:'s',view:'chat',sessions:[],messages:[],running:new Set(),approvals:[],questions:[]},h,$,tr:x=>x,truncate:(s,n)=>s.slice(0,n),invalidateBranchSummary(){},closeSidebar(){},renderHistory(){},renderRunTools(){},renderStatus(){},renderQueue(){},renderPlan(){},renderStep(){},renderChips(){},renderAll(){},rendered:new Map(),prefs:{set(){}},request:async()=>({}),applySessionLiveState(){},act:async()=>({messages:[],running:false,plan:[],pending_events:[],unfinished_run:{run_id:'r',status:'paused',error:'stuck: repeated invalid tool arguments; fix the call before Resume'}}),isRunning:()=>false});
  vm.runInContext(['restoreSessionRunState','renderRunBanner','openSession'].map(fn).join('\n')+'\nlet sessionSeq=0;const attachmentDrafts={};const CLIENT_LABEL="fixture";',c);await c.openSession('s');
  const text=node=>typeof node==='string'?node:(node?.children||[]).map(text).join(' ');const banner=text(elements.get('run-banner'));assert.match(banner,/stuck: repeated invalid tool arguments/);assert.match(banner,/Resume/);
});
