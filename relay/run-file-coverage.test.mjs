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
function setup() {
  const notes = [], appended = [];
  const h = (tag, props, ...children) => ({ tag, props, children: children.flat(Infinity).filter(x => x != null), isConnected: true, addEventListener() {}, focus() {}, remove() {} });
  const context = vm.createContext({ h, uiFormat: String, tr: x => x, S: {current:'s'}, note: x => notes.push(x), branchSummaryEvent() {}, notifyFor() {}, renderHistory() {}, renderStatus() {}, renderComposer() {}, renderLive() {},
    ask: async () => true, document: {activeElement:null,body:{append: x => appended.push(x)}},
    // Keep mounting observable for the content/coverage assertions; keyboard
    // focus and inertness use the browser regression suite's real DOM.
    openDialog: node => appended.push(node), closeDialog: node => node.remove(),
    $: () => ({focus(){}}), act: async () => context.reply });
  installEventRenderFixture(context, fn);
  vm.runInContext(['runFilesCaveat','runFilesText','showRunFiles','currentRunTarget','ensureRunTarget','recordedDuration','recapBlock','onEvent'].map(fn).join('\n'), context);
  return { context, notes, appended };
}
function text(node) {
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  return node?.children?.map(text).join(' ') || '';
}
test('actual run event warns with no tracked paths and counts partial tracking', () => {
  const {context:c,notes} = setup();
  c.onEvent({type:'run_files_changed',session_id:'s',files:[],untracked_writes_possible:true});
  assert.match(notes[0], /^No tracked path changes\./);
  assert.match(notes[0], /Untracked writes are possible.*MCP tools/);
  c.onEvent({type:'run_files_changed',session_id:'s',files:[{path:'x',status:'modified'},{path:'y',status:'unchanged'}],untracked_writes_possible:true});
  assert.match(notes[1], /^1 tracked path\(s\) changed/);
  assert.match(notes[1], /attempts safe restoration/);
  c.onEvent({type:'run_files_changed',session_id:'other',files:[],untracked_writes_possible:true});
  assert.equal(notes.length,2);
  assert.doesNotMatch(c.runFilesText([],false), /Untracked writes/);
  assert.equal(c.runFilesText([{path:'x',status:'unchanged'}],false), 'No current changes among tracked paths.');
});
test('files dialog displays durable coverage and specific skip reasons as text', async () => {
  const {context:c,appended} = setup();
  const reason = 'symlink <script>target</script> is not tracked';
  c.reply = {run_id:'r',untracked_writes_possible:true,files:[{path:'<img>.txt',status:'modified',revertible:false,revert_reason:reason}]};
  await c.showRunFiles();
  assert.match(text(appended[0]), /Paths tracked by the Run \(1\)/);
  assert.match(text(appended[0]), /Untracked writes are possible.*MCP tools/);
  assert.ok(text(appended[0]).includes(reason));
  const head = appended[0].children[0].children.find(x => x.tag === 'div' && x.props?.class === 'files-body');
  assert.ok(text(head).includes('<img>.txt'));
  assert.ok(!JSON.stringify(appended[0]).includes('innerHTML'));
});
test('recap summary and expanded content both warn even with no tracked paths', () => {
  const {context:c} = setup();
  const recap = c.recapBlock({steps:4,duration_ms:1,used:[],built:[],files:[],steered:0,untracked_writes_possible:true});
  assert.match(text(recap.children[0]), /Untracked writes are possible.*MCP tools/);
  assert.match(text(recap.children.slice(1).find(x => x.tag === 'p')), /Untracked writes are possible/);
});
test('expired recovery remains listed and a skipped rewind never reports success', async () => {
  const {context:c,appended,notes} = setup();
  const reason = 'Snapshot expired by retention policy';
  c.reply = {run_id:'expired',untracked_writes_possible:true,files:[{path:'old.txt',status:'modified',revertible:false,revert_reason:reason}]};
  await c.showRunFiles();
  const dialog = appended[0];
  assert.ok(text(dialog).includes('old.txt'));
  assert.ok(text(dialog).includes(reason));
  function nodes(node) { return node && typeof node === 'object' ? [node, ...node.children.flatMap(nodes)] : []; }
  assert.equal(nodes(dialog).filter(x => x.tag === 'button' && text(x) === 'Revert').length, 0);
  const all = nodes(dialog).find(x => x.tag === 'button' && text(x) === 'Revert all');
  c.reply = {restored:[],skipped:[{path:'old.txt',reason}]};
  all.props.onclick();
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(notes.some(x => x === 'not reverted: old.txt (' + reason + ')'));
  assert.ok(!notes.some(x => x.startsWith('reverted ')));
});
