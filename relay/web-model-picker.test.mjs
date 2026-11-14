import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1730: the model picker's loading/testing labels end on failure, and an
// empty listing hints at free-form entry. `act` is an inert stub whose
// replies the tests resolve by hand; no provider or model is contacted.
const source = readFileSync(new URL(process.env.APP_JS || './web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
class El {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.kids = []; this.attrs = {}; this.cls = new Set(); this.listeners = {}; this.value = ''; }
  set className(v) { this.cls = new Set(String(v).split(/\s+/).filter(Boolean)); }
  get classList() { const c = this.cls; return { add: x => c.add(x), remove: x => c.delete(x), contains: x => c.has(x) }; }
  get children() { return this.kids.filter(k => k instanceof El); }
  get textContent() { return this.kids.map(k => (k instanceof El ? k.textContent : k)).join(''); }
  set textContent(v) { this.kids = v === '' ? [] : [String(v)]; }
  append(...k) { this.kids.push(...k); }
  replaceChildren(...k) { this.kids = k; }
  setAttribute(k, v) { this.attrs[k] = v; }
  addEventListener(k, f) { this.listeners[k] = f; }
  scrollIntoView() {}
}
function h(tag, props, ...kids) {
  const node = new El(tag);
  for (const [k, v] of Object.entries(props || {})) if (v != null) k === 'class' ? (node.className = v) : node.setAttribute(k, v);
  for (const kid of kids.flat(Infinity)) if (kid != null && kid !== false) node.append(kid);
  return node;
}
function fixture() {
  const calls = [], applied = [];
  const c = vm.createContext({
    h, calls, applied, S: { sessions: [{ id: 's', model: '' }], current: 's' },
    act: req => new Promise(resolve => calls.push({ req, resolve })),
    openDialog: node => { node.isConnected = true; c.dialog = node; },
    closeDialog: node => { node.isConnected = false; },
    setSessionModel: (m, e) => applied.push([m, e]), note: () => {}, uiMoney: v => String(v),
    fuzzyMatch: (q, id) => (id.includes(q.trim()) ? [1, []] : null),
  });
  vm.runInContext(['modelDetail', 'modelTestText', 'openModelPicker'].map(fn).join('\n'), c);
  c.done = vm.runInContext('openModelPicker()', c);
  const find = (n, pred) => (pred(n) ? n : n.children.map(k => find(k, pred)).find(Boolean));
  c.status = () => find(c.dialog, n => n.cls.has('modal-actions')).children[0];
  c.button = text => find(c.dialog, n => n.tagName === 'BUTTON' && n.textContent === text);
  c.input = () => find(c.dialog, n => n.tagName === 'INPUT');
  c.list = () => find(c.dialog, n => n.tagName === 'UL');
  return c;
}
const tick = () => new Promise(r => setImmediate(r));
const tested = { ok: true, model: 'm1', first_token_ms: 5, total_ms: 9, streaming: true, reasoning: false, tool_calling: true };

test('a failed listing ends the loading label; reopening can still list', async () => {
  const c = fixture();
  assert.equal(c.status().textContent, 'loading models…');
  c.calls[0].resolve(null); await c.done;
  assert.equal(c.status().textContent, 'could not load models — type a model id');
  assert.ok(c.status().cls.has('err'));
  c.input().value = 'typed-model'; c.input().oninput();
  c.button('Use in this Session').onclick();
  assert.deepEqual(c.applied, [['typed-model', null]], 'free-form entry still applies');
  const again = fixture();
  again.calls[0].resolve({ models: ['m1', 'm2'] }); await again.done;
  assert.equal(again.status().textContent, '2 of 2');
});

test('an empty listing hints free-form entry; a populated one is unchanged', async () => {
  const c = fixture();
  c.calls[0].resolve({ models: [], aliases: {}, roles: [], recent: [] }); await c.done;
  assert.equal(c.status().textContent, 'no models listed — type a model id');
  assert.ok(!c.status().cls.has('err'));
  const e = fixture();
  e.calls[0].resolve({ models: [], error: 'boom' }); await e.done;
  assert.equal(e.status().textContent, 'could not list models: boom');
  assert.ok(e.status().cls.has('err'));
  const p = fixture();
  p.calls[0].resolve({ models: ['m1', 'm2', 'm3'], current: { model: 'm2' } }); await p.done;
  assert.equal(p.status().textContent, '3 of 3');
  assert.equal(p.list().children.length, 3);
  assert.equal(p.list().children[1].attrs['aria-selected'], 'true');
});

test('a failed test ends the testing label; a later success is unchanged', async () => {
  const c = fixture();
  c.calls[0].resolve({ models: ['m1'] }); await c.done;
  c.button('Test').onclick(); await tick();
  assert.equal(c.status().textContent, 'testing m1…');
  c.calls[1].resolve(null); await tick();
  assert.equal(c.status().textContent, '✗ m1: test did not complete');
  assert.doesNotMatch(c.status().textContent, /testing/);
  c.button('Test').onclick(); await tick();
  c.calls[2].resolve(tested); await tick();
  // Unchanged: success records the result on the row, then draw() redraws
  // the count over modelTestText (pre-existing behavior, not #1730's).
  assert.equal(c.status().textContent, '1 of 1');
  assert.match(c.list().children[0].textContent, /✓ tested/);
});

test('closing first, or newer status text, is never overwritten by a late failure', async () => {
  const c = fixture();
  c.button('Cancel').onclick();
  c.calls[0].resolve(null); await c.done;
  assert.equal(c.status().textContent, 'loading models…', 'closed picker is left alone');
  const t = fixture();
  t.button('Test').onclick(); await tick(); // nothing typed yet: a no-op
  t.input().value = 'free'; t.button('Test').onclick(); await tick(); // list still pending
  assert.equal(t.status().textContent, 'testing free…');
  t.calls[0].resolve(null); await t.done;
  assert.equal(t.status().textContent, 'testing free…', 'list failure keeps the newer label');
  t.calls[1].resolve(null); await tick();
  assert.equal(t.status().textContent, '✗ free: test did not complete');
});
