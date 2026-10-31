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
const deferred = () => { let resolve, reject; const promise = new Promise((r, e) => { resolve = r; reject = e; }); return { promise, resolve, reject }; };
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
test('approval decisions stay pending until confirmed; failures remain retryable', async () => {
  const waits = [deferred(), deferred()]; let calls = 0; const notes = [], errors = [];
  const c = setup(['decide'], { request: () => waits[calls++].promise, renderApproval() {}, renderStatus() {},
    toast: x => errors.push(x), note: x => notes.push(x), isBroad: () => false, pickedPaths: () => ({}),
    prefs: { set() {} }, isRunning: () => false });
  c.S.approvals = [{ kind: 'tool', id: 'one', tool_name: 'reader', capabilities: [], batch: { batch_id: 'b', index: 1 } }];
  const first = c.decide(true, 'once');
  await c.decide(false); // a repeated key cannot send a second request
  assert.equal(calls, 1); assert.equal(c.S.approvals.length, 1); assert.equal(notes.length, 0);
  waits[0].reject(new Error('offline')); await first;
  assert.equal(c.S.approvals.length, 1); assert.equal(c.S.approvals[0].deciding, false);
  assert.match(errors[0], /not delivered/); assert.equal(c.S.batchAnswers, undefined);
  const retry = c.decide(true, 'once'); waits[1].resolve({ resolved: true }); await retry;
  assert.equal(c.S.approvals.length, 0); assert.equal(c.S.batchAnswers.b[1], 'once');
  assert.equal(notes.filter(x => x.startsWith('approved ')).length, 1);
});
test('a stale approval response cannot be reported as an accepted denial', async () => {
  const notes = [];
  const c = setup(['decide'], { request: async () => ({ resolved: false }), renderApproval() {}, renderStatus() {},
    note: x => notes.push(x), isBroad: () => false, pickedPaths: () => ({}) });
  c.S.approvals = [{ kind: 'tool', id: 'gone', tool_name: 'reader' }];
  await c.decide(false);
  assert.equal(c.S.approvals.length, 0); assert.equal(notes.some(x => x.startsWith('denied ')), false);
});
function node(tag, attrs, ...children) {
  return { tag, attrs: attrs || {}, children: children.flat(Infinity), classList: { add() {} },
    append(...kids) { this.children.push(...kids.flat(Infinity)); }, replaceChildren(...kids) { this.children = kids.flat(Infinity); },
    setAttribute(k, v) { this.attrs[k] = v; } };
}
const all = (n) => typeof n === 'object' && n ? [n, ...n.children.flatMap(all)] : [];
const plainText = n => typeof n === 'string' ? n : (n?.children || []).map(plainText).join('');
test('hostile persisted Markdown has bounded nesting and preserves readable text', () => {
  const c = setup(['link', 'inline', 'splitRow', 'blocks', 'list', 'markdown'], { h: node });
  vm.runInContext(source.slice(source.indexOf('  const SAFE_URL'), source.indexOf('  function link')), c);
  vm.runInContext(source.slice(source.indexOf('  const FENCE'), source.indexOf('  function splitRow')), c);
  vm.runInContext('const MD_DEPTH = 32, MD_INLINE_CHARS = 4096, MD_FORMAT_CHARS = 128 * 1024;', c);
  const corpus = [
    '> '.repeat(5000) + 'quote end',
    Array.from({ length: 1500 }, (_, i) => ' '.repeat(i * 2) + '- item').join('\n'),
    '**a '.repeat(20000) + ' **'.repeat(20000),
    '['.repeat(20000) + 'brackets' + ']'.repeat(20000),
    Array.from({ length: 250 }, (_, i) => ' '.repeat(i * 2) + '- item').join('\n'),
  ];
  for (const text of corpus) {
    const at = performance.now(), rendered = c.markdown(text);
    assert.ok(performance.now() - at < 1000, 'render exceeded 1 second');
    assert.ok(all(rendered).length < 1000, 'excessive DOM expansion');
    assert.ok(plainText(rendered).length > 0);
  }
  const normal = c.markdown('## Heading\n\n**bold** and [link](https://example.com)\n\n- item');
  assert.ok(all(normal).some(n => n.tag === 'strong'));
  assert.ok(all(normal).some(n => n.tag === 'a'));
  assert.ok(all(normal).some(n => n.tag === 'ul'));
});
test('live MessageComplete → RunRecap → RunFinished updates recap/rating outside cached body', async () => {
  const transcript = node('div'), cached = new Map();
  const c = setup(['renderHistory', 'messageBody', 'rateButtons', 'onEvent'], {
    h: node, notifyFor() {}, renderPlan() {}, refreshSessions() {}, renderStatus() {}, renderComposer() {}, renderLive() {}, scrollToBottom() {}, renderRunBanner() {}, setActivity() {},
    markdown: text => node('p', null, text), speaker: () => 'assistant', clock: () => '',
    recapBlock: r => node('recap', null, r.label), tr: x => x,
    act: async () => ({ rating: 'good' }), note() {}, isRunning: () => c.S.running.has(c.S.current),
  });
  c.$ = id => id === 'transcript' ? transcript : null;
  c.S.daemonOnline = true; c.S.running = new Set(['A']);
  c.rendered = cached;
  vm.runInContext('const rendered = globalThis.rendered; const WHO = { assistant: "Formless" };', c);
  c.onEvent({ type: 'message_complete', session_id: 'A', message: { id: 'answer', role: 'assistant', content: 'answer' } });
  const originalBody = cached.get('answer');
  assert.equal(all(transcript).filter(n => n.attrs.class === 'rate').length, 0);
  c.onEvent({ type: 'run_recap', session_id: 'A', recap: { label: 'summary' } });
  c.onEvent({ type: 'run_finished', session_id: 'A', status: 'completed' });
  assert.equal(cached.get('answer'), originalBody);
  assert.equal(all(transcript).filter(n => n.tag === 'recap').length, 1);
  const rate = all(transcript).find(n => n.attrs.class === 'rate');
  assert.ok(rate);
  await rate.children[0].attrs.onclick();
  assert.ok(all(transcript).some(n => n.attrs['aria-pressed'] === 'true'));
  c.S.running.add('A'); c.S.messages.push({ id: 'next', role: 'assistant', content: 'next' });
  c.renderHistory(); assert.equal(all(transcript).filter(n => n.attrs.class === 'rate').length, 0);
  c.S.running.delete('A'); c.renderHistory();
  assert.equal(all(transcript).filter(n => n.attrs.class === 'rate').length, 1);
  assert.equal(all(originalBody).filter(n => n.attrs.class === 'rate').length, 0);
});
