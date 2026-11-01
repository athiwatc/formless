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
    forecastRevision: 0, scheduleForecast() {}, setTimeout: f => f(), rememberFocus() {}, restoreFocus() {}, renderChips() {}, note() {}, autosize() {},
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
  vm.runInContext('let askResolve = null; let askTag = null; const askQueue = [];', c);
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
test('secret prompts mask their field and ordinary prompts reset the input type', async () => {
  const c = setup(['ask', 'showNextAsk', 'closeAsk']);
  vm.runInContext('let askResolve = null; let askTag = null; const askQueue = [];', c);
  const secret = c.ask({ title: 'Connection URL', input: true, secret: true });
  assert.equal(c.$('ask-input').type, 'password');
  c.closeAsk('hidden'); await secret;
  const plain = c.ask({ title: 'Name', input: true });
  assert.equal(c.$('ask-input').type, 'text'); assert.equal(c.$('ask-input').value, '');
  c.closeAsk('name'); await plain;
});
test('passwordless PostgreSQL argv authenticates through the actual pg constructor fallback', () => {
  const base = new URL('../crates/formless-core/tests/fixtures/postgres-env/', import.meta.url);
  const cases = JSON.parse(readFileSync(new URL('cases.json', base), 'utf8'));
  const parserModule = { exports: {} };
  vm.runInNewContext(readFileSync(new URL('pg-connection-string.cjs', base), 'utf8'), {
    module: parserModule, URL,
    require: () => { throw new Error('fixture must not read files or connect'); },
  });
  for (const fixture of cases) {
    const env = { PGPASSWORD: fixture.daemon_password };
    if (fixture.literal_password !== null) env.PGPASSWORD = fixture.literal_password;
    const module = { exports: {} };
    vm.runInNewContext(readFileSync(new URL('pg-connection-parameters.cjs', base), 'utf8'), {
      module, process: { env },
      require: name => {
        if (name === 'pg-connection-string') return parserModule.exports;
        if (name === './defaults') return { user: 'default-user', database: null, host: 'localhost', port: 5432, password: null, ssl: false };
        if (name === 'dns') return { lookup() { throw new Error('no network in constructor fixture'); } };
        throw new Error('unexpected dependency ' + name);
      },
    });
    // The pinned MCP package passes argv[0] to new pg.Pool({connectionString}).
    // A fake Pool observes that exact constructor boundary without starting it.
    class FakePool {
      constructor(config) { this.parameters = new module.exports(config); }
    }
    const fakePackage = argv => new FakePool({ connectionString: argv[0] });
    const pool = fakePackage([fixture.argument]);
    assert.equal(new URL(fixture.argument).password, '');
    assert.equal(pool.parameters.password, fixture.effective_password);
    assert.equal(pool.parameters.user, 'u');
    assert.equal(pool.parameters.database, 'db');
    assert.equal(pool.parameters.host, 'h');
  }
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
    mentionsIn: () => [], crypto: { randomUUID: () => 'key' }, request: async x => { calls.push(x); await wait.promise; return {}; } });
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
  const c = setup(['openSession', 'onEvent'], { applySessionLiveState: noop, notifyFor: noop, renderComposer: noop, act: () => wait.promise, request: async () => ({}), prefs: { set() {} },
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
  const c = setup(['renderHistory', 'messageBody', 'rateButtons', 'onEvent', 'assistantStopNote'], {
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
test('parallel questions queue across Sessions; closing queued question preserves active settings text', async () => {
  const c = setup(['ask', 'showNextAsk', 'closeAsk', 'cancelAsk', 'queueQuestion', 'removeQuestion', 'showQuestion'], {
    renderStatus() {}, questionForm: () => ({ querySelector: () => null }),
    readQuestionForm: () => [{ selected: [], text: 'answer' }], act: async () => ({ answered: true }),
  });
  vm.runInContext('let askResolve = null; let askTag = null; const askQueue = [];', c);
  const settings = c.ask({ title: 'Setting', input: true }); c.$('ask-input').value = 'unsaved';
  c.queueQuestion({ question_id: 'one', session_id: 'A', question: 'first' });
  c.queueQuestion({ question_id: 'two', session_id: 'B', question: 'second' });
  assert.equal(c.S.pendingQuestion.id, 'one'); assert.equal(c.S.questions.length, 2);
  c.removeQuestion('one'); await Promise.resolve();
  assert.equal(c.$('ask-title').textContent, 'Setting'); assert.equal(c.$('ask-input').value, 'unsaved');
  c.closeAsk('saved'); assert.equal(await settings, 'saved');
  assert.equal(c.$('ask-title').textContent, 'The agent asks'); assert.equal(c.S.pendingQuestion.id, 'two');
  c.closeAsk(true); await new Promise(r => setImmediate(r));
  assert.equal(c.S.questions.length, 0); assert.equal(c.S.pendingQuestion, null);
});
test('local and relay reconnect rebuild pending prompts/running and reject stale snapshot state', async () => {
  for (const transport of ['local', 'relay']) {
    let reply = deferred();
    const c = setup(['openSession', 'onEvent', 'clearQuestions', 'clearLiveState', 'applySessionLiveState', 'addApproval', 'queueQuestion', 'removeQuestion'], {
      act: () => reply.promise, request: async () => ({}), prefs: { set() {} }, rendered: new Map(),
      closeSidebar() {}, renderHistory() {}, renderRunTools() {}, renderStatus() {}, renderQueue() {}, renderPlan() {}, renderStep() {}, renderRunBanner() {}, renderAll() {},
      renderApproval() {}, renderComposer() {}, showQuestion() {}, cancelAsk() {}, notifyFor() {}, setActivity() {},
      isRunning: () => c.S.running.has(c.S.current),
    });
    vm.runInContext('let sessionSeq = 0; const CLIENT_LABEL = "test"; const attachmentDrafts = {};', c);
    c.S.local = transport === 'local'; c.S.running = new Set(['A']);
    c.S.approvals = [{ kind: 'tool', id: 'answered-offline' }];
    c.S.questions = [{ id: 'stale' }]; c.S.pendingQuestion = c.S.questions[0];
    c.S.streaming = 'stale partial'; c.S.liveTools = [{}];
    c.clearLiveState();
    assert.equal(c.S.approvals.length, 0); assert.equal(c.S.questions.length, 0);
    assert.equal(c.S.streaming, ''); assert.equal(c.S.liveTools.length, 0);
    const loading = c.openSession('A');
    reply.resolve({ messages: [], running: true, pending_events: [
      { type: 'approval_requested', approval_id: 'raised-offline', session_id: 'A', capabilities: [] },
      { type: 'user_question', question_id: 'new-question', session_id: 'A', question: 'which?' },
    ] });
    await loading;
    assert.equal(c.S.running.has('A'), true);
    assert.deepEqual(Array.from(c.S.approvals, a => a.id), ['raised-offline']);
    assert.equal(c.S.pendingQuestion.id, 'new-question');
    reply = deferred(); const refresh = c.openSession('A');
    c.onEvent({ type: 'approval_answered', approval_id: 'raised-offline', approved: true });
    c.onEvent({ type: 'approval_requested', approval_id: 'arrived-live', session_id: 'A', capabilities: [] });
    c.onEvent({ type: 'user_question_closed', question_id: 'new-question' });
    reply.resolve({ messages: [], running: false, pending_events: [
      { type: 'approval_requested', approval_id: 'raised-offline', session_id: 'A', capabilities: [] },
      { type: 'user_question', question_id: 'new-question', session_id: 'A', question: 'which?' },
    ] });
    await refresh;
    assert.equal(c.S.running.has('A'), false);
    assert.deepEqual(Array.from(c.S.approvals, a => a.id), ['arrived-live']);
    assert.equal(c.S.pendingQuestion, null);
  }
});

function liveSetup(extras = {}) {
  const wait = deferred();
  const c = setup(['openSession', 'onEvent', 'clearQuestions', 'clearLiveState', 'applySessionLiveState', 'addApproval', 'queueQuestion', 'removeQuestion'], {
    act: () => wait.promise, request: async () => ({}), prefs: { set() {} }, rendered: new Map(),
    closeSidebar() {}, renderHistory() {}, renderRunTools() {}, renderStatus() {}, renderQueue() {}, renderPlan() {}, renderStep() {}, renderRunBanner() {}, renderAll() {},
    renderApproval() {}, renderComposer() {}, showQuestion() {}, cancelAsk() {}, notifyFor() {}, setActivity() {},
    isRunning: () => c.S.running.has(c.S.current), ...extras,
  });
  vm.runInContext('let sessionSeq = 0; const CLIENT_LABEL = "test"; const attachmentDrafts = {};', c);
  c.S.running = new Set(); c.S.approvals = []; c.S.questions = []; c.S.liveTools = [];
  return { c, wait };
}
test('global attention snapshot preserves another Session question and entered dialog', async () => {
  const cancelled = [], { c, wait } = liveSetup({ cancelAsk: tag => cancelled.push(tag) });
  c.onEvent({ type: 'user_question', question_id: 'qa', session_id: 'A', question: 'A?' });
  const question = c.S.questions[0];
  c.$('ask-input').value = 'typed answer';
  const load = c.openSession('B');
  wait.resolve({ messages: [], running: false, pending_events: [
    { type: 'user_question', question_id: 'qa', session_id: 'A', question: 'A?' },
    { type: 'user_question', question_id: 'qb', session_id: 'B', question: 'B?' },
  ] });
  await load;
  assert.deepEqual(Array.from(c.S.questions, q => q.id), ['qa', 'qb']);
  assert.equal(c.S.questions[0], question);
  assert.equal(c.$('ask-input').value, 'typed answer');
  assert.deepEqual(cancelled, []);
});
test('snapshot reduction retains racing stream state and does not notify twice', async () => {
  let notifications = 0;
  const { c, wait } = liveSetup({ notifyFor: () => notifications++ });
  const load = c.openSession('A');
  c.onEvent({ type: 'run_started', session_id: 'A' });
  c.onEvent({ type: 'message_delta', session_id: 'A', delta: 'partial answer' });
  c.onEvent({ type: 'reasoning_delta', session_id: 'A', delta: 'thinking' });
  c.onEvent({ type: 'tool_call_started', session_id: 'A', tool_name: 'reader', arguments: '{}' });
  const liveTools = c.S.liveTools;
  const before = notifications;
  wait.resolve({ messages: [], running: false, pending_events: [] }); await load;
  assert.equal(c.S.streaming, 'partial answer'); assert.equal(c.S.thinking, 'thinking');
  assert.equal(c.S.liveTools, liveTools); assert.equal(c.S.liveTools.length, 1);
  assert.equal(c.S.running.has('A'), true); assert.equal(notifications, before);
});
test('promotion resolved by another client cannot reappear from a delayed snapshot', async () => {
  const { c, wait } = liveSetup();
  const load = c.openSession('A');
  c.onEvent({ type: 'promotion_approval_resolved', promotion_id: 'p' });
  wait.resolve({ messages: [], running: false, pending_events: [
    { type: 'promotion_approval_requested', promotion_id: 'p', tool_name: 'reader', capabilities: [] },
  ] }); await load;
  assert.equal(c.S.approvals.length, 0);
});
test('relay daemon_offline clears live state and rejects pending requests without socket close', async () => {
  const { c } = liveSetup({ clearTimeout() {}, note() {}, renderChrome() {} });
  const socket = {}; c.S.ws = socket; c.S.daemonOnline = true; c.S.channel = {};
  c.S.running.add('A'); c.S.approvals = [{ id: 'old' }]; c.S.streaming = 'old partial';
  let rejected = 0; c.S.pending = new Map([[1, { reject: e => { assert.match(e.message, /offline/); rejected++; } }]]);
  const start = source.indexOf('    async function onFrame(ws, frame) {');
  vm.runInContext(source.slice(start, source.indexOf('\n    ws.onclose =', start)), c);
  await c.onFrame(socket, '{"relay":"daemon_offline"}');
  assert.equal(c.S.channel, null); assert.equal(c.S.approvals.length, 0);
  assert.equal(c.S.running.size, 0); assert.equal(c.S.streaming, '');
  assert.equal(rejected, 1); assert.equal(c.S.pending.size, 0);
});

test('catalog Add masks credentials and waits for command and exposure confirmation', async () => {
  let table; const calls = [], prompts = [];
  const c = setup(['showMcpCatalog'], {
    h: node, modal: (_, body) => { table = body; return () => {}; }, refreshSettings() {},
    act: async req => {
      calls.push(req.type);
      if (req.type === 'mcp_catalog') return [{ id: 'postgres', title: 'Postgres', description: '', target: 'npx -y package@1.2.3 {url}', execution_warning: 'Downloads and runs code from npm/PyPI on first use.', params: [{ name: 'url', kind: 'secret', description: 'Connection URL' }] }];
      if (req.type === 'preview_mcp_catalog') return { target: 'npx -y package@1.2.3 postgresql://u:REDACTED@h/db', execution_warning: 'Downloads and runs code from npm/PyPI on first use.', warnings: ['canonical folder exposes credentials'] };
      assert.equal(prompts.length, 3); assert.equal(req.params.confirm_run, true); assert.equal(req.params.confirm_exposure, true);
      return { added: 'postgres', tools: 1 };
    },
    ask: async opts => { prompts.push(opts); return opts.input ? 'postgresql://u:pw@h/db' : true; },
  });
  await c.showMcpCatalog();
  assert.match(plainText(table), /package@1.2.3/); assert.match(plainText(table), /Downloads and runs code/);
  await all(table).find(n => n.tag === 'button').attrs.onclick();
  assert.equal(prompts[0].secret, true);
  assert.match(prompts[1].text, /REDACTED/); assert.equal(prompts[2].title, 'Confirm folder exposure');
  assert.deepEqual(calls, ['mcp_catalog', 'preview_mcp_catalog', 'add_mcp_from_catalog']);
});

test('import dialog discloses plaintext storage and sends only a reference plus skip choice', async () => {
  for (const skipSecrets of [true, false]) {
    let table, imported;
    const c = setup(['showMcpImport'], {
      h: (tag, attrs, ...kids) => Object.assign(node(tag, attrs, ...kids), tag === 'input' ? { checked: !!attrs.checked } : {}),
      modal: (_, body) => { table = body; }, refreshSettings() {},
      act: async req => {
        if (req.type === 'mcp_import_candidates') return [{ candidate_id: 'opaque', name: 'db', source: 'Cursor', target: 'db-command', env: ['K'], headers: ['Authorization'], missing_env: [] }];
        imported = req; return { added: 'db', disabled: true };
      },
      ask: async opts => {
        assert.match(plainText(opts.body), /plaintext config.toml/); assert.match(plainText(opts.body), /Skip secrets/);
        all(opts.body).find(n => n.tag === 'input').checked = skipSecrets; return true;
      },
    });
    await c.showMcpImport();
    await all(table).find(n => n.tag === 'button').attrs.onclick({ target: { replaceWith() {} } });
    assert.equal(imported.type, 'import_mcp_candidate'); assert.equal(imported.candidate_id, 'opaque');
    assert.equal(imported.skip_secrets, skipSecrets); assert.equal(imported.server, undefined);
  }
});

test('persisted assistant stop reasons render explanatory notes when Session is reopened', () => {
  const c = setup(['assistantStopNote']);
  for (const [reason, expected] of [
    ['max_tokens', /output token limit/], ['model_context_window_exceeded', /context window is full/],
    ['pause_turn', /paused its turn/], ['refusal', /refused/], ['content_filter', /content filter/], ['other_reason', /ended early \(other_reason\)/],
  ]) {
    assert.match(c.assistantStopNote({ meta: JSON.stringify({ stop_reason: reason }) }), expected);
  }
  assert.match(c.assistantStopNote({ meta: '{"truncated":true}' }), /output token limit/);
  assert.equal(c.assistantStopNote({ meta: '{"placeholder":true,"stop_reason":"refusal"}' }), null);
  assert.equal(c.assistantStopNote({ meta: '{}' }), null);
  assert.equal(c.assistantStopNote({ meta: 'invalid' }), null);

  const transcript = node('div');
  const view = setup(['renderHistory', 'messageBody', 'rateButtons', 'assistantStopNote'], {
    h: node, isRunning: () => false, markdown: text => node('p', null, text), speaker: () => 'assistant', clock: () => '',
    renderLive() {}, scrollToBottom() {}, tr: x => x,
  });
  view.$ = () => transcript;
  vm.runInContext('const rendered = new Map(); const WHO = { assistant: "Formless" };', view);
  view.S.daemonOnline = true;
  view.S.messages = [{ id: 'persisted', role: 'assistant', content: 'partial output', meta: '{"truncated":true,"stop_reason":"max_tokens"}' }];
  view.renderHistory();
  assert.match(plainText(transcript), /partial output/);
  assert.match(plainText(transcript), /answer was cut off/);
  assert.equal(all(transcript).filter(n => n.attrs.class === 'muted stop-note').length, 1);
});

test('dropped updates refresh durable usage and mark transient progress unavailable', async () => {
  const replies = [], notes = [];
  let running = 0;
  const c = setup(['reconcileDropped'], {
    S: { current: 'A', usage: { A: { steps: 1 } }, liveTools: [{ status: 'running', progress: { fraction: 0.9, label: 'stale' } }] },
    note: s => notes.push(s), sessionName: id => id, renderStatus() {}, renderLive() {},
    refreshRunning() { running++; }, setActivity(s) { notes.push(s); },
    request(r) { const d = deferred(); replies.push({ r, d }); return d.promise; },
  });
  const gap = { session_id: 'A', run_id: 'run', kinds: ['run_usage', 'tool_progress', 'invocation_activity'] };
  c.reconcileDropped(gap);
  c.reconcileDropped(gap);
  assert.equal(c.S.usage.A, undefined);
  assert.equal(c.S.liveTools[0].progress.fraction, null);
  assert.match(c.S.liveTools[0].progress.label, /skipped/);
  assert.equal(replies.length, 1, 'duplicate usage refresh is coalesced');
  assert.equal(running, 2);
  replies[0].d.resolve({ last_run: { steps: 3 } });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(c.S.usage.A.steps, 3);
  c.reconcileDropped(gap);
  c.S.usage.A = { steps: 9 }; // New streamed state wins over older snapshot.
  replies[1].d.resolve({ last_run: { steps: 4 } });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(c.S.usage.A.steps, 9);
  c.reconcileDropped({ session_id: '', run_id: '', kinds: ['model_pull_progress'], model: 'model' });
  assert.ok(notes.some(n => /download progress unavailable for model/.test(n)));
  assert.ok(notes.some(n => /live text, reasoning or progress updates were skipped/.test(n)));
});

function forecastContext(names, extras = {}) {
  const timers = new Map(); let next = 1;
  const c = setup(names, { isRunning: () => false, clearTimeout: id => timers.delete(id),
    setTimeout: fn => { const id = next++; timers.set(id, fn); return id; }, h: () => ({}),
    ...extras });
  vm.runInContext('let forecastTimer = null; let forecastSequence = 0; let forecastStampLast = "";', c);
  c.S.daemonOnline = true; c.S.hello = 'connection-one';
  c.S.sessions = [{ id: 'A', model: 'model', cwd: '/project' }];
  c.fire = async () => { const pending = [...timers.values()]; timers.clear(); for (const fn of pending) await fn(); };
  return c;
}
test('forecast hints debounce and reject stale draft, model, Session and attachments', async () => {
  for (const change of ['draft', 'model', 'session', 'files']) {
    const wait = deferred(), reads = [];
    const c = forecastContext(['forecastStamp', 'scheduleForecast'], { request: req => { reads.push(req); return wait.promise; } });
    c.$('input').value = 'old'; c.scheduleForecast();
    c.$('input').value = 'current'; c.scheduleForecast();
    assert.equal(reads.length, 0, 'typing never immediately requests a forecast');
    const fire = c.fire(); assert.equal(reads.length, 1); assert.equal(reads[0].input.draft, 'current');
    if (change === 'draft') c.$('input').value = 'newer';
    if (change === 'model') c.S.sessions[0].model = 'other';
    if (change === 'session') c.S.current = 'B';
    if (change === 'files') c.S.chips.A = [{id:'new-file'}];
    wait.resolve({ hint: 'stale', warnings: [] }); await fire;
    assert.notEqual(c.$('forecast-hint').textContent, 'stale');
  }
});
test('current forecast hint shows compaction without changing the draft', async () => {
  const c = forecastContext(['forecastStamp', 'scheduleForecast'], { request: async () => ({hint:'~$0.04–0.11 · ~6 steps · will compact', warnings:['N comparable Runs']}) });
  c.$('input').value = 'keep my draft'; c.scheduleForecast(); await c.fire();
  assert.match(c.$('forecast-hint').textContent, /will compact/); assert.equal(c.$('input').value, 'keep my draft');
});
test('forecast confirmation cannot cross a changed draft, Session, model or connection', async () => {
  for (const change of ['draft','model','session','connection']) {
    const dialog = deferred(), calls = [];
    const c = forecastContext(['forecastStamp','confirmForecast'], { request: async req => {calls.push(req); return {confirmation_required:true,confirmation_id:'preview',hint:'estimate',warnings:[]};}, ask: () => dialog.promise });
    c.$('input').value = 'task'; const result = c.confirmForecast('A','task'); await new Promise(resolve => setImmediate(resolve));
    if (change === 'draft') c.$('input').value = 'newer';
    if (change === 'model') c.S.sessions[0].model = 'other';
    if (change === 'session') c.S.current = 'B';
    if (change === 'connection') c.S.hello = 'connection-two';
    dialog.resolve(true); assert.equal(await result, false); assert.deepEqual(calls.map(c => c.type), ['forecast_run']);
  }
});
test('declined consent retains composer and allocates no send intent', async () => {
  const calls = [];
  const c = forecastContext(['forecastStamp','confirmForecast','submit'], { ask: async () => false, request: async req => {calls.push(req); return {confirmation_required:true,confirmation_id:'preview',hint:'estimate',warnings:[]};},
    askNotifyPermission() {}, closeMentions() {}, mentionsIn:()=>[], crypto:{randomUUID:()=>{throw Error('intent must not be allocated');}} });
  vm.runInContext('const mentionAttached = {}; const attachmentDrafts = {};', c);
  c.S.settings={limits:{forecast_confirm_usd:0}};c.$('input').value='task';await c.submit(false);
  assert.equal(c.$('input').value,'task');assert.deepEqual(calls.map(c=>c.type),['forecast_run']);
});
test('a definite gate rejection retries after consent with a fresh intent; uncertain sends keep their key', async () => {
  for (const definite of [false,true]) {
    const calls = [];let intent = 0, sends = 0;
    const c = forecastContext(['forecastStamp','confirmForecast','submit'], { ask:async()=>true, request:async req=>{
      calls.push(req);if(req.type==='forecast_run')return {confirmation_required:true,confirmation_id:'preview',hint:'estimate',warnings:[]};
      if(req.type==='confirm_forecast')return {confirmed:true};
      if(req.type==='send_message' && ++sends===1){const error=new Error(definite?'stale forecast':'lost response');error.daemonResponse=definite;throw error;}
      return {started:true};
    }, toast(){},askNotifyPermission(){},closeMentions(){},mentionsIn:()=>[],crypto:{randomUUID:()=>`intent-${++intent}`} });
    vm.runInContext('const mentionAttached = {}; const attachmentDrafts = {};', c);
    c.S.settings={limits:{forecast_confirm_usd:0}};c.$('input').value='task';await c.submit(false);assert.equal(c.$('input').value,'task');
    await c.submit(false);const sent=calls.filter(c=>c.type==='send_message');assert.equal(sent.length,2);
    assert.equal(sent[0].client_msg_id===sent[1].client_msg_id,!definite);
    assert.equal(c.S.unsent,null);assert.equal(c.$('input').value,'');
  }
});

test('forecast generation invalidates history, Tool and settings revisions without draft edits', async () => {
  for (const type of ['message_appended','messages_edited','compaction_notice','tool_updated','settings_changed']) {
    const pending=deferred();
    const c=forecastContext(['forecastStamp','scheduleForecast'], {request:()=>pending.promise});
    c.$('input').value='task'; c.scheduleForecast(); const finishing=c.fire();
    // Exercise the exact shipped discrete-event invalidation prefix without
    // needing unrelated browser rendering for every event kind.
    const start=fn('onEvent');
    vm.runInContext(start.slice(0,start.indexOf('    if (S.sessionSnapshot'))+'\n  }',c);
    c.onEvent({type,session_id:'A'});
    pending.resolve({hint:'outdated'}); await finishing;
    assert.equal(c.$('forecast-hint').textContent,'',type);
  }
});
test('resume and retry require prospective consent before sending their commands', async () => {
  for (const action of ['resume','retry']) {
    for (const accepted of [false,true]) {
      const calls=[];
      const c=forecastContext(['forecastStamp','confirmForecast',action==='resume'?'resumeRun':'retryRun'], {
        request:async r=>{calls.push(r);return r.type==='forecast_run'?{confirmation_required:true,confirmation_id:'quote',hint:'$2',warnings:[]}:{confirmed:true};},
        ask:async()=>accepted,act:async r=>{calls.push(r);return {};},renderRunBanner(){},refreshSessions(){},
      });
      c.S.settings={limits:{forecast_confirm_usd:1}};c.$('input').value='another draft';
      if(action==='resume')await c.resumeRun();else await c.retryRun('specific-model');
      assert.equal(calls[0].type,'forecast_run');
      assert.equal(calls[0].for_send,true);
      assert.equal(calls[0].input[action],true);
      assert.equal(calls[0].input.draft,'');
      assert.equal(calls.at(-1).type,accepted?action+'_run':'forecast_run');
      if(accepted)assert.equal(calls[1].type,'confirm_forecast');
      assert.equal(c.$('input').value,'another draft');
    }
  }
});
