import { installEventRenderFixture } from './test-web-render.mjs';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const fn = name => {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
};
function setup(actualRefresh = false) {
  const notes = [], box = { classList: { toggle() {} }, children: [], replaceChildren(...children) { this.children = children; } };
  const h = (tag, props, ...children) => ({
    tag, props, children: children.flat(Infinity).filter(x => x != null),
    classList: { toggle() {} },
    append(...nodes) { this.children.push(...nodes); },
    replaceChildren(...nodes) { this.children = nodes; },
    get firstElementChild() { return this.children[0]; },
    get lastElementChild() { return this.children.at(-1); },
  });
  const c = vm.createContext({S:{current:'s',running:new Set(),sessions:[],liveTools:[],invocations:[],ws:{},conn:1,channel:{}}, h, $:() => box,
    forecastRevision:0, scheduleForecast() {}, note: text => notes.push(text), branchSummaryEvent() {}, notifyFor() {}, renderLive() {}, renderStatus() {}, renderComposer() {}, renderRunBanner() {}, renderRunTools() {}, renderPlan() {}, renderHistory() {}, setActivity() {}, isRunning:() => true,
    document: { activeElement: null }, runningRefresh: null, reconcileChildren: (parent, nodes) => parent.replaceChildren(...nodes),
    request: async () => [], Date});
  installEventRenderFixture(c, fn);
  vm.runInContext(['runningTool','reconcileInvocationProgress','progressDiagnostic','isRunningConnection',
    'refreshRunning','drainRunningRefresh','runningRow','renderRunningList','onEvent'].map(fn).join('\n'),c);
  if (!actualRefresh) c.refreshRunning = () => {};
  return {c, notes, box};
}
const event = (type, id, extra = {}) => ({type,invocation_id:id,session_id:'s',run_id:'r',tool_name:'same',depth:0,version:1,arguments:'{}',...extra});
const row = (id, extra = {}) => ({invocation_id:id,handle:'h-'+id,tool_name:'same',session_id:'s',elapsed_ms:100,silent_ms:30000,stalled:true,last_fraction:0.5,last_label:'stuck-'+id,...extra});
const text = node => typeof node === 'string' ? node : node?.children?.map(text).join(' ') || '';
test('shipped events keep two identical parallel progress bars and finish the exact call', () => {
  const {c} = setup();
  for (const id of ['a','b']) c.onEvent(event('tool_call_started',id));
  c.onEvent(event('tool_progress','a',{fraction:0.2,label:'first'}));
  c.onEvent(event('tool_progress','b',{fraction:0.8,label:'second'}));
  assert.equal(c.S.liveTools[0].progress.fraction,0.2);
  assert.equal(c.S.liveTools[1].progress.fraction,0.8);
  for (const id of [null,'unknown']) {
    c.onEvent(event('tool_progress',id,{fraction:1,label:'wrong'}));
    c.onEvent(event('tool_call_finished',id,{ok:true,elapsed_ms:1}));
  }
  assert.ok(c.S.liveTools.every(x => x.status === 'running'));
  assert.equal(c.S.liveTools[0].progress.label,'first');
  c.onEvent(event('tool_call_finished','a',{ok:true,elapsed_ms:1}));
  assert.equal(c.S.liveTools[0].status,'ok');
  assert.equal(c.S.liveTools[1].status,'running');
  c.onEvent(event('tool_progress',null,{fraction:0.9,label:'legacy unique'}));
  assert.equal(c.S.liveTools[1].progress.fraction,0.9);
  c.onEvent(event('tool_call_started','b'));
  assert.equal(c.S.liveTools.length,2,'duplicate start cannot make identity ambiguous');
});
test('pending handles, stall diagnostics, resumed activity and completion stay on their ID', () => {
  const {c,notes} = setup();
  for (const id of ['a','b']) {
    c.onEvent(event('tool_call_started',id));
    c.onEvent(event('invocation_pending',id,{handle:'h-'+id}));
    c.onEvent(event('invocation_stalled',id,{handle:'h-'+id,silent_secs:30,last_fraction:0.5,last_label:'stuck-'+id}));
  }
  assert.equal(c.S.liveTools[0].handle,'h-a');
  assert.equal(c.S.liveTools[1].handle,'h-b');
  assert.ok(notes.some(x => /last progress 50% \(stuck-a\)/.test(x)));
  c.onEvent(event('invocation_activity','a',{handle:'h-a'}));
  assert.equal(c.S.liveTools[0].stalledSecs,null);
  assert.equal(c.S.liveTools[1].stalledSecs,30);
  c.onEvent(event('invocation_activity','unknown',{handle:'h-b'}));
  c.onEvent(event('invocation_activity','b',{session_id:'other',handle:'h-b'}));
  assert.equal(c.S.liveTools[1].stalledSecs,30);
  c.onEvent(event('invocation_activity',null,{handle:'h-b'}));
  assert.equal(c.S.liveTools[1].stalledSecs,null,'legacy activity uses unique associated handle');
  c.onEvent(event('invocation_completed','a',{handle:'h-a',ok:true}));
  assert.equal(c.S.liveTools[0].status,'ok');
  assert.equal(c.S.liveTools[1].status,'running');
});
test('actual reconnect refresh restores distinct bars and diagnostic labels from IDs', async () => {
  const {c,box} = setup(true);
  c.request = async () => [row('a',{last_fraction:0.2}),row('b'),row('foreign',{session_id:'other'})];
  await c.refreshRunning();
  assert.equal(c.S.liveTools.length,2);
  assert.equal(c.S.liveTools[0].progress.fraction,0.2);
  assert.equal(c.S.liveTools[1].progress.label,'stuck-b');
  assert.ok(box.children.some(x => text(x).includes('last progress 50% (stuck-b)')));
  await c.refreshRunning();
  assert.equal(c.S.liveTools.length,2);
  c.S.liveTools[0].status = 'ok';
  await c.refreshRunning();
  assert.equal(c.S.liveTools.length,2);
  assert.equal(c.S.liveTools[0].status,'ok','older snapshot cannot revive a completed ID');
});
test('refreshes coalesce, preserve event invalidation, and reject old connection replies', async () => {
  const {c} = setup(true), resolvers = [];
  c.request = () => new Promise(resolve => resolvers.push(resolve));
  const pending = c.refreshRunning(false);
  assert.equal(c.refreshRunning(false), pending);
  assert.equal(c.refreshRunning(), pending);
  assert.equal(c.refreshRunning(), pending);
  assert.equal(resolvers.length, 1);
  resolvers[0]([row('a')]);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(c.S.liveTools.length, 0, 'invalidated snapshot did not restore old progress');
  assert.equal(resolvers.length, 2);
  resolvers[1]([row('b')]);
  await pending;
  assert.equal(c.S.liveTools.length,1);
  assert.equal(c.S.liveTools[0].invocationId,'b');
  const old = c.refreshRunning();
  c.S.channel = {};
  const current = c.refreshRunning(false);
  resolvers[3]([row('b', { last_label: 'new connection' })]);
  await current;
  resolvers[2]([row('a')]);
  await old;
  assert.equal(c.S.liveTools.length,1);
  assert.equal(c.S.liveTools[0].invocationId,'b');
  assert.equal(c.S.liveTools[0].progress.label,'new connection');
});
test('pending before start retains handle, progress and stall while start fills exact metadata', () => {
  const {c} = setup();
  c.onEvent(event('run_started',null));
  c.onEvent(event('invocation_pending','early',{handle:'h-early'}));
  assert.equal(c.S.liveTools.length,1);
  assert.equal(c.S.liveTools[0].metadataPending,true);
  const began = c.S.liveTools[0].started;
  c.onEvent(event('tool_progress','early',{depth:3,fraction:0.4,label:'before start'}));
  c.onEvent(event('invocation_stalled','early',{handle:'h-early',silent_secs:30,last_fraction:0.4,last_label:'before start'}));
  c.onEvent(event('tool_call_started','early',{depth:3,version:7,arguments:'{"x":1}'}));
  assert.equal(c.S.liveTools.length,1);
  const call = c.S.liveTools[0];
  assert.equal(call.metadataPending,false);
  assert.equal(call.handle,'h-early');
  assert.equal(call.depth,3);
  assert.equal(call.version,7);
  assert.equal(call.arguments,'{"x":1}');
  assert.equal(call.progress.fraction,0.4);
  assert.equal(call.progress.label,'before start');
  assert.equal(call.stalledSecs,30);
  assert.equal(call.started,began);
  c.onEvent(event('tool_call_finished','early',{ok:true,elapsed_ms:1}));
  c.onEvent(event('tool_call_started','early'));
  c.onEvent(event('invocation_pending','early',{handle:'h-late'}));
  assert.equal(c.S.liveTools.length,1);
  assert.equal(call.status,'ok');
  assert.equal(call.handle,'h-early');
  c.onEvent(event('run_finished',null,{status:'completed'}));
  c.onEvent(event('invocation_pending','late',{handle:'h-late'}));
  assert.equal(c.S.liveTools.length,0,'terminal Run cannot recreate a pending bar');
  c.onEvent(event('run_started',null,{run_id:'r2'}));
  c.onEvent(event('invocation_pending','late',{handle:'h-late'}));
  c.onEvent(event('tool_call_started','late'));
  c.onEvent(event('run_finished',null,{status:'completed'}));
  assert.equal(c.S.liveTools.length,0,'previous Run lifecycle cannot touch new Run');
  assert.equal(c.S.liveRun.id,'r2');
  assert.equal(c.S.liveRun.ended,false);
});
test('reconnect placeholders accept metadata and pending allocation stays bounded', () => {
  const {c} = setup();
  c.reconcileInvocationProgress([row('restored')]);
  c.onEvent(event('tool_call_started','restored',{depth:2,version:8,arguments:'[]'}));
  assert.equal(c.S.liveTools.length,1);
  assert.equal(c.S.liveTools[0].metadataPending,false);
  assert.equal(c.S.liveTools[0].version,8);
  assert.equal(c.S.liveTools[0].depth,2);
  assert.equal(c.S.liveTools[0].handle,'h-restored');
  assert.equal(c.S.liveTools[0].progress.label,'stuck-restored');
  for (let index=0;index<140;index++) c.onEvent(event('invocation_pending','pending-'+index,{handle:'h-'+index}));
  assert.equal(c.S.liveTools.filter(t => t.metadataPending).length,128);
  c.onEvent(event('invocation_pending','foreign',{session_id:'other',handle:'h-foreign'}));
  assert.ok(!c.S.liveTools.some(t => t.invocationId === 'foreign'));
});
test('authoritative background ID receives progress and start metadata across newer Run scope', () => {
  const {c} = setup();
  c.onEvent(event('run_started',null,{run_id:'r2'}));
  c.onEvent(event('tool_call_started','new',{run_id:'r2'}));
  c.reconcileInvocationProgress([row('background')]);
  c.onEvent(event('tool_progress','background',{fraction:0.6,label:'still running'}));
  c.onEvent(event('tool_call_started','background',{depth:2,version:9,arguments:'[]'}));
  assert.equal(c.S.liveTools[1].version,9);
  assert.equal(c.S.liveTools[1].depth,2);
  assert.equal(c.S.liveTools[1].progress.fraction,0.6);
  assert.equal(c.S.liveTools[1].handle,'h-background');
  c.onEvent(event('invocation_stalled','background',{handle:'h-background',silent_secs:40,last_fraction:0.6,last_label:'still running'}));
  assert.equal(c.S.liveTools[1].stalledSecs,40);
  assert.equal(c.S.liveTools[0].progress,undefined);
  c.onEvent(event('run_finished',null,{run_id:'r2',status:'completed'}));
  assert.equal(c.S.liveTools.length,1,'background call survives foreground completion');
  c.onEvent(event('tool_progress','background',{run_id:'',fraction:0.7,label:'manual context'}));
  assert.equal(c.S.liveTools[0].progress.fraction,0.7);
  c.onEvent(event('tool_call_finished','background',{ok:true,elapsed_ms:1}));
  c.onEvent(event('tool_call_started','background'));
  c.onEvent(event('invocation_pending','background',{handle:'h-again'}));
  c.onEvent(event('invocation_pending','unknown-old',{handle:'h-old'}));
  c.onEvent(event('tool_call_started','unknown-old'));
  assert.equal(c.S.liveTools.length,1);
  assert.equal(c.S.liveTools[0].status,'ok');
});
