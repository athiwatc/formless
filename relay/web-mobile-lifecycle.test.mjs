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
const relayLifecycle = source.slice(source.indexOf('  let localJsonSerializationSeq = 0;'), source.indexOf('\n  function showPair()'));
const wakeLockStart = source.indexOf('  let wakeLockEnabled = false;');
const wakeLockEnd = source.indexOf('\n\n  /** Step mode', wakeLockStart);
const wakeLockLifecycle = source.slice(wakeLockStart, wakeLockEnd);
const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };

function relayHarness() {
  const requests = [], opened = [], closes = [], notes = [];
  let probe;
  const ws = { readyState: 1, close: (...args) => closes.push(args) };
  const S = { channel: {}, local: false, relayReady: true, userClosed: false, ws, current: 'session-1', reconnect: null };
  const document = { hidden: false };
  const navigator = { onLine: true };
  const context = vm.createContext({
    S, document, navigator,
    Blob: class { constructor(parts, options) { this.parts = parts; this.options = options; } },
    downloadBlob() {},
    setInterval: (callback, ms) => { context.interval = { callback, ms }; return 1; },
    request: (req, timeout) => { requests.push({ type: req.type, timeout }); return probe(); },
    openSession: async (...args) => opened.push(args), loadAll: async () => opened.push(['all']),
    openRelay: () => {
      context.reconnects = (context.reconnects || 0) + 1;
      S.ws = { readyState: 0 };
      S.channel = null;
    },
    retryNow() { context.retries = (context.retries || 0) + 1; },
    note: (...args) => notes.push(args),
  });
  probe = () => Promise.resolve({});
  vm.runInContext(relayLifecycle, context);
  vm.runInContext(fn('download'), context);
  return { context, S, document, navigator, ws, requests, opened, closes, notes, setProbe: value => { probe = value; } };
}

test('foreground return probes the encrypted relay and reloads the current session', async () => {
  const h = relayHarness();
  let resolve;
  h.setProbe(() => new Promise(done => { resolve = done; }));
  const recovery = h.context.resumeRelayAfterReturn();
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].type, 'ping');
  assert.equal(h.requests[0].timeout, 5000);
  resolve({});
  await recovery;
  assert.deepEqual(h.opened, [['session-1', true]]);
});

test('failed relay probe replaces the zombie socket without waiting for a close event', async () => {
  const h = relayHarness();
  h.setProbe(() => Promise.reject(new Error('probe timed out')));
  await h.context.resumeRelayAfterReturn();
  assert.equal(h.context.reconnects, 1);
  assert.notEqual(h.S.ws, h.ws);
  assert.equal(h.S.channel, null);
  assert.equal(h.closes.length, 1);
  assert.equal(h.closes[0][0], 4003);
  assert.equal(h.opened.length, 0);
  assert.match(h.notes[0][0], /reconnecting/);
});

test('a stale failed probe cannot replace or close the newer relay socket', async () => {
  const h = relayHarness();
  let reject;
  h.setProbe(() => new Promise((_, fail) => { reject = fail; }));
  const recovery = h.context.resumeRelayAfterReturn();
  const newerSocket = { readyState: 1 };
  h.S.ws = newerSocket;
  h.S.channel = {};
  reject(new Error('probe timed out'));
  await recovery;
  assert.equal(h.context.reconnects || 0, 0);
  assert.equal(h.closes.length, 0);
});

test('a ping timeout during local JSON export does not replace the healthy socket', async () => {
  const h = relayHarness();
  let reject;
  h.setProbe(() => new Promise((_, fail) => { reject = fail; }));
  const probe = h.context.probeRelay();
  h.context.act = async () => ({
    name: 'large export', format: 'json',
    messages: { toJSON() { reject(new Error('probe reply was delayed during serialization')); return ['large export']; } },
  });
  h.context.slug = value => value.replaceAll(' ', '-');
  vm.runInContext(fn('exportSession'), h.context);
  await h.context.exportSession('session-1', 'json');
  assert.equal(await probe, false);
  assert.equal(h.context.reconnects || 0, 0);
  assert.equal(h.closes.length, 0);
});

test('foreground resync failures are reported without an unhandled rejection', async () => {
  for (const [session, method] of [['session-1', 'openSession'], [null, 'loadAll']]) {
    const h = relayHarness();
    h.S.current = session;
    h.context[method] = async () => { throw new Error('snapshot failed'); };
    await h.context.resumeRelayAfterReturn();
    assert.match(h.notes.at(-1)[0], /Could not refresh the Session/);
  }
});

test('periodic relay heartbeat runs every 15 seconds with a five-second dead-connection timeout', async () => {
  const h = relayHarness();
  let reject;
  h.setProbe(() => new Promise((_, fail) => { reject = fail; }));
  assert.equal(h.context.interval.ms, 15000);
  h.context.interval.callback();
  assert.equal(h.requests[0].type, 'ping');
  assert.equal(h.requests[0].timeout, 5000);
  reject(new Error('timeout'));
  await settle();
  assert.equal(h.context.reconnects, 1);
  assert.equal(h.closes[0][0], 4003);
});

test('foreground lifecycle skips relay work while hidden and retries an already scheduled reconnect', async () => {
  const h = relayHarness();
  h.document.hidden = true;
  await h.context.resumeRelayAfterReturn();
  assert.equal(h.requests.length, 0);
  h.document.hidden = false;
  h.S.reconnect = {};
  await h.context.resumeRelayAfterReturn();
  assert.equal(h.context.retries, 1);
  assert.equal(h.requests.length, 0);
});

test('foreground return immediately replaces an already closed relay socket', async () => {
  const h = relayHarness();
  h.S.key = {};
  h.ws.readyState = 3;
  await h.context.resumeRelayAfterReturn();
  assert.equal(h.context.reconnects, 1);
  assert.equal(h.requests.length, 0);
});

test('approval badges reflect the pending count and clear after resolution', async () => {
  const operations = [];
  const context = vm.createContext({
    S: { approvals: [{}, {}] },
    navigator: {
      setAppBadge: count => { operations.push(['set', count]); return Promise.resolve(); },
      clearAppBadge: () => { operations.push(['clear']); return Promise.resolve(); },
    },
  });
  vm.runInContext(fn('syncAppBadge'), context);
  context.syncAppBadge();
  context.S.approvals = [];
  context.syncAppBadge();
  await settle();
  assert.deepEqual(operations, [['set', 2], ['clear']]);
  assert.match(fn('renderApproval'), /syncAppBadge\(\);/);
});

test('screen wake lock is opt-in, released when hidden or no longer watching, and reacquired on return', async () => {
  const locks = [], released = [];
  const button = {
    hidden: true, textContent: '', attrs: {},
    setAttribute(key, value) { this.attrs[key] = value; },
  };
  const S = { view: 'chat', current: 'session-1', running: new Set(['session-1']) };
  const context = vm.createContext({
    S, document: { hidden: false },
    navigator: { wakeLock: { request: async type => {
      assert.equal(type, 'screen');
      const lock = { release: () => { released.push(lock); return Promise.resolve(); }, addEventListener() {} };
      locks.push(lock);
      return lock;
    } } },
    $: id => { assert.equal(id, 'keep-awake'); return button; },
    isRunning: () => S.running.has(S.current), note() {},
  });
  vm.runInContext(wakeLockLifecycle, context);
  context.renderWakeLock();
  assert.equal(button.hidden, false);
  assert.equal(locks.length, 0, 'wake lock remains off until the user opts in');
  button.onclick();
  await settle();
  assert.equal(button.attrs['aria-pressed'], 'true');
  assert.equal(locks.length, 1);
  context.document.hidden = true;
  context.renderWakeLock();
  await settle();
  assert.equal(released.length, 1);
  assert.equal(button.attrs['aria-pressed'], 'true');
  context.document.hidden = false;
  context.renderWakeLock();
  await settle();
  assert.equal(locks.length, 2);
  S.running.clear();
  context.renderWakeLock();
  await settle();
  assert.equal(button.hidden, true);
  assert.equal(button.attrs['aria-pressed'], 'false');
  assert.equal(released.length, 2);
});

test('wake-lock rejection resets the button without retrying automatically', async () => {
  let requests = 0;
  const button = {
    hidden: true, textContent: '', attrs: {},
    setAttribute(key, value) { this.attrs[key] = value; },
  };
  const S = { view: 'chat', current: 'session-1', running: new Set(['session-1']) };
  const context = vm.createContext({
    S, document: { hidden: false },
    navigator: { wakeLock: { request: async () => { requests++; throw new Error('permission denied'); } } },
    $: id => { assert.equal(id, 'keep-awake'); return button; },
    isRunning: () => S.running.has(S.current), note() {},
  });
  vm.runInContext(wakeLockLifecycle, context);
  context.renderWakeLock();
  button.onclick();
  await settle();
  assert.equal(requests, 1);
  assert.equal(button.attrs['aria-pressed'], 'false');
  assert.equal(button.textContent, 'Keep awake');
  context.renderWakeLock();
  await settle();
  assert.equal(requests, 1);
});
