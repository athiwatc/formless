import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.indexOf('  function ' + name + '(');
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
function fixture() {
  const requests = [], timers = [];
  const S = { channel: {}, ws: {}, conn: 1, selectedTool: 'a', toolDetail: { tool: { id: 'a', current_version: 2 }, versions: [{version:1},{version:2}] } };
  const context = vm.createContext({ S, document: { activeElement: {} }, setTimeout: callback => timers.push(callback),
    h: (tag, attrs, ...children) => ({ tag, attrs, children, isConnected:true, open:false, textContent:'',
      addEventListener(name, callback) { this[name] = callback; }, replaceChildren(...next) { this.children = next; } }),
    renderDiffLines: diff => [diff],
    request: args => new Promise((resolve,reject) => requests.push({args,resolve,reject})),
  });
  vm.runInContext(fn('toolVersionChanges') + '\n' + fn('focusToolEditor'), context);
  const create = () => context.toolVersionChanges('a',1,2);
  return { context, S, requests, timers, create };
}
async function open(node) { node.open = true; return node.toggle(); }

test('diff is lazy and deduplicated in-flight, settled, and across same-pair renders', async () => {
  const f = fixture(), first = f.create();
  assert.equal(f.requests.length,0);
  await first.toggle();
  assert.equal(f.requests.length,0);
  const pending = open(first), second = f.create(), also = open(second);
  assert.equal(f.requests.length,1);
  assert.equal(f.requests[0].args.type,'diff_tool_versions');
  assert.equal(f.requests[0].args.from,1);
  assert.equal(f.requests[0].args.to,2);
  f.requests[0].resolve({diff:'synthetic diff'});
  await Promise.all([pending,also]);
  assert.equal(first.children[1].children[0],'synthetic diff');
  assert.equal(second.children[1].children[0],'synthetic diff');
  await open(first);
  assert.equal(f.requests.length,1);
});

test('selection, version, connection and detached nodes reject stale replies', async () => {
  for (const invalidate of [
    f => { f.S.selectedTool = 'b'; },
    f => { f.S.toolDetail.tool.current_version = 3; },
    f => { f.S.toolDetail.versions[0].version = 0; },
    f => { f.S.channel = {}; },
    f => { f.S.ws = {}; },
    f => { f.S.conn = 2; },
    f => { f.S.toolVersionDiff = null; },
    (f,node) => { node.isConnected = false; },
  ]) {
    const f = fixture(), node = f.create(), pending = open(node);
    invalidate(f,node);
    f.requests[0].resolve({diff:'stale'});
    await pending;
    assert.notEqual(node.children[1].children[0],'stale');
  }
});

test('new connection or version pair starts a new bounded cache; errors do not repeat on toggle', async () => {
  const f = fixture(), first = f.create(), pending = open(first);
  f.requests[0].reject(new Error('synthetic failure'));
  await pending;
  assert.equal(first.children[1].textContent,'synthetic failure');
  await open(first);
  assert.equal(f.requests.length,1);
  f.S.channel = {};
  const second = f.create(), next = open(second);
  assert.equal(f.requests.length,2);
  f.requests[1].resolve({diff:'new connection'}); await next;
  f.S.toolDetail = {tool:{id:'a',current_version:3},versions:[{version:2},{version:3}]};
  const third = f.context.toolVersionChanges('a',2,3), newest = open(third);
  assert.equal(f.requests.length,3);
  f.requests[2].resolve({diff:'new version'}); await newest;
  assert.equal(f.S.toolVersionDiff.from,2);
  assert.equal(f.S.toolVersionDiff.to,3);
});

test('editor focus is consumed once and respects intervening user focus and disconnected nodes', () => {
  const f = fixture(); let focuses = 0;
  const area = {isConnected:true,focus:()=>focuses++};
  f.S.editingCode = {focusPending:true};
  f.context.focusToolEditor(area); f.timers.shift()();
  assert.equal(focuses,1);
  f.context.focusToolEditor(area); assert.equal(f.timers.length,0);
  f.S.editingCode = {focusPending:true};
  f.context.focusToolEditor(area); f.context.document.activeElement = {}; f.timers.shift()();
  assert.equal(focuses,1);
  f.S.editingCode = {focusPending:true};
  f.context.focusToolEditor(area); area.isConnected = false; f.timers.shift()();
  assert.equal(focuses,1);
});

test('a detached toggle does not request a diff; an incomplete result is safe', async () => {
  const f = fixture(), old = f.create();
  old.isConnected = false;
  await open(old);
  assert.equal(f.requests.length, 0);
  const current = f.create(), waiting = open(current);
  f.requests[0].resolve({});
  await waiting;
  assert.equal(current.children[1].textContent, 'No version diff returned.');
});
