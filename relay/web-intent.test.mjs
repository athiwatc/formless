// Run with node --test relay/web-intent.test.mjs. Exercise the shipped handlers
// with controlled daemon replies, without a browser or network dependency.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const fn = (name) => {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  const end = source.indexOf('\n  }', start);
  return source.slice(start, end + 4);
};
const deferred = () => { let resolve; const promise = new Promise(r => resolve = r); return { promise, resolve }; };
function setup(names, extras = {}) {
  const elements = new Map();
  const $ = id => {
    if (!elements.has(id)) elements.set(id, { value: '', style: {}, textContent: '', classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } }, replaceChildren() {}, select() {}, focus() {} });
    return elements.get(id);
  };
  const context = vm.createContext({ $, S: { current: 'A', chips: {}, messages: [], sessions: [], view: 'chat', tools: [] },
    setTimeout: f => f(), rememberFocus() {}, restoreFocus() {}, renderChips() {}, note() {}, autosize() {},
    ...extras });
  vm.runInContext(names.map(fn).join('\n'), context);
  return context;
}
test('malformed fragment pairs preserve independently valid pairs and plus signs', () => {
  const c = setup(['hashParams'], { location: { hash: '' } });
  for (const hash of ['#k=%E0%A4%A', '#s=%', '#local=%zz', '#k=abc%&s=valid']) {
    c.location.hash = hash;
    const r = c.hashParams();
    assert.equal(r.malformed, true);
    if (hash.includes('valid')) assert.equal(r.s, 'valid');
  }
  c.location.hash = '#k=a+b%3D&s=session';
  assert.equal(c.hashParams().k, 'a+b=');
});
test('init and hashchange recover malformed links through pairing guidance', () => {
  for (const kind of ['init', 'hashchange']) {
    let shown = 0, handler;
    const c = setup(['hashParams'], { location: { hash: '#local=%zz', pathname: '/' },
      history: { replaceState() {} }, showPair() { shown++; },
      window: { addEventListener(_, f) { handler = f; } } });
    if (kind === 'init') {
      vm.runInContext(source.slice(source.indexOf('  (function init()'), source.lastIndexOf('\n})();')), c);
    } else {
      const start = source.indexOf('  window.addEventListener("hashchange"');
      vm.runInContext(source.slice(start, source.indexOf('\n  });', start) + 6), c);
      handler();
    }
    assert.equal(shown, 1);
    assert.match(c.$('pair-error').textContent, /does not look right/);
  }
});
test('overlapping asks preserve typed text and both settle in FIFO order', async () => {
  const c = setup(['ask', 'showNextAsk', 'closeAsk']);
  vm.runInContext('let askResolve = null; const askQueue = [];', c);
  const first = c.ask({ title: 'Rename', input: true });
  c.$('ask-input').value = 'typed name';
  const second = c.ask({ title: 'Agent question', input: true });
  assert.equal(c.$('ask-title').textContent, 'Rename');
  assert.equal(c.$('ask-input').value, 'typed name');
  c.closeAsk('typed name');
  assert.equal(await first, 'typed name');
  assert.equal(c.$('ask-title').textContent, 'Agent question');
  c.closeAsk('answer');
  assert.equal(await second, 'answer');
});
test('file upload remains in original Session after switching', async () => {
  const wait = deferred(), calls = [];
  const c = setup(['attachFiles'], { uploadFile: async (sid, file) => { calls.push(sid); await wait.promise; return { name: file.name }; } });
  c.S.chips.B = [{ name: 'B-file' }];
  const pending = c.attachFiles([{ name: 'one' }, { name: 'two' }]);
  c.S.current = 'B'; wait.resolve(); await pending;
  assert.deepEqual(calls, ['A', 'A']);
  assert.equal(c.S.chips.A.length, 2);
  assert.equal(c.S.chips.B[0].name, 'B-file');
});
test('slow mention attachment does not cross-post or overwrite new Session composer', async () => {
  const wait = deferred(), calls = [];
  const c = setup(['submit'], { isRunning: () => true, askNotifyPermission() {}, closeMentions() {},
    mentionsIn: () => ['file'], request: async x => { calls.push(x); await wait.promise; return { name: 'file' }; },
    act: async x => { calls.push(x); return {}; } });
  vm.runInContext('const mentionAttached = {}; const attachmentDrafts = {};', c);
  c.$('input').value = 'please read @file';
  const pending = c.submit(false);
  c.S.current = 'B'; c.$('input').value = 'B draft'; c.S.chips.B = [{ name: 'B-file' }];
  wait.resolve(); await pending;
  assert.equal(calls[0].session_id, 'A');
  assert.equal(calls.length, 1); // fresh attachments must be reviewed before sending
  assert.equal(c.S.chips.A[0].name, 'file');
  assert.equal(c.S.chips.B[0].name, 'B-file');
  assert.equal(c.$('input').value, 'B draft');
  assert.equal(vm.runInContext('attachmentDrafts.A', c), 'please read @file');
});
test('send completion clears only the original Session attachments', async () => {
  const wait = deferred(), calls = [];
  const c = setup(['submit'], { isRunning: () => false, askNotifyPermission() {}, closeMentions() {},
    mentionsIn: () => [], crypto: { randomUUID: () => 'key' }, act: async x => { calls.push(x); await wait.promise; return {}; } });
  vm.runInContext('const mentionAttached = {}; const attachmentDrafts = {};', c);
  c.$('input').value = 'hello'; c.S.chips.A = [{}]; c.S.chips.B = [{}];
  const pending = c.submit(false); c.S.current = 'B'; wait.resolve(); await pending;
  assert.equal(calls[0].session_id, 'A'); assert.equal(calls[0].mode, 'send');
  assert.equal(c.S.chips.A, undefined); assert.equal(c.S.chips.B.length, 1);
});
test('tool, search and audit ignore out-of-order resolutions', async () => {
  for (const name of ['selectTool', 'search', 'loadAudit']) {
    const replies = [deferred(), deferred()]; let call = 0; const renders = [];
    const c = setup([name], { act: () => replies[call++].promise, request: () => replies[call++].promise,
      renderView() {}, renderSearch: (...x) => renders.push(x) });
    vm.runInContext('let toolSeq = 0, searchSeq = 0, auditSeq = 0;', c);
    let a, b;
    if (name === 'loadAudit') { c.S.auditFilter = 'A'; a = c.loadAudit(false); c.S.auditFilter = 'B'; b = c.loadAudit(false); }
    else { a = c[name]('A'); b = c[name]('B'); }
    replies[1].resolve(name === 'selectTool' ? { id: 'B' } : ['B']); await b;
    replies[0].resolve(name === 'selectTool' ? { id: 'A' } : ['A']); await a;
    if (name === 'selectTool') assert.equal(c.S.toolDetail.id, 'B');
    if (name === 'loadAudit') assert.equal(c.S.audit[0], 'B');
    if (name === 'search') assert.equal(renders.at(-1)[1], 'B');
  }
});
test('clearing search invalidates an in-flight reply', async () => {
  const reply = deferred(), renders = [];
  const c = setup(['search'], { act: () => reply.promise, renderSearch: x => renders.push(x) });
  vm.runInContext('let searchSeq = 0;', c);
  const a = c.search('old'); await c.search(''); reply.resolve(['old']); await a;
  assert.deepEqual(renders, [null]);
});
test('opening Session clears old messages and merges live appended messages into snapshot', async () => {
  const wait = deferred();
  const noop = () => {};
  const c = setup(['openSession', 'onEvent'], { notifyFor: noop, renderComposer: noop, act: () => wait.promise, request: async () => ({}), prefs: { set() {} },
    rendered: new Map(), closeSidebar: noop, renderHistory: noop, renderRunTools: noop, renderStatus: noop,
    renderQueue: noop, renderPlan: noop, renderStep: noop, renderRunBanner: noop, renderAll: noop, isRunning: () => false });
  vm.runInContext('let sessionSeq = 0; const CLIENT_LABEL = "test"; const attachmentDrafts = {};', c);
  c.S.messages = [{ id: 'old' }];
  const a = c.openSession('B'); assert.equal(c.S.messages.length, 0);
  c.onEvent({ type: 'message_appended', session_id: 'B', message: { id: 'duplicate', role: 'user' } });
  c.onEvent({ type: 'message_appended', session_id: 'B', message: { id: 'live', role: 'user' } });
  wait.resolve({ messages: [{ id: 'snapshot' }, { id: 'duplicate' }] }); await a;
  assert.deepEqual(Array.from(c.S.messages, m => m.id), ['snapshot', 'duplicate', 'live']);
});
