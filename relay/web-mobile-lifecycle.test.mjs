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
const relayLifecycle = source.slice(source.indexOf('  const RELAY_PROBE_INTERVAL_MS = '), source.indexOf('\n  function showPair()'));
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
    setInterval: (callback, ms) => { context.interval = { callback, ms }; return 1; },
    request: (req, timeout, extendOnRelayActivity) => { requests.push({ type: req.type, timeout, extendOnRelayActivity }); return probe(); },
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
  return { context, S, document, navigator, ws, requests, opened, closes, notes, setProbe: value => { probe = value; } };
}

test('foreground return probes the encrypted relay and reloads the current session', async () => {
  const h = relayHarness();
  let resolve;
  h.setProbe(() => new Promise(done => { resolve = done; }));
  const recovery = h.context.resumeRelayAfterReturn();
  assert.equal(h.requests.length, 1);
  assert.equal(h.requests[0].type, 'ping');
  assert.equal(h.requests[0].timeout, 15000);
  assert.equal(h.requests[0].extendOnRelayActivity, true);
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

test('probe timeout uses only the remaining idle budget after Relay progress', async () => {
  const timers = new Map();
  let timerID = 0, now = 10000;
  const ws = { readyState: 1, send() {} };
  const S = { ws, channel: {}, local: true, relayReady: true, pending: new Map(), nextId: 1, sendChain: Promise.resolve(), relayActivityAt: 0 };
  const context = vm.createContext({
    S, PROTOCOL: 1,
    performance: { now: () => now },
    setTimeout: (callback, ms) => { const id = ++timerID; timers.set(id, { callback, ms, at: now + ms }); return id; },
    clearTimeout: id => timers.delete(id),
  });
  vm.runInContext(fn('request'), context);
  const advance = async ms => {
    const target = now + ms;
    for (;;) {
      const next = [...timers].sort((a, b) => a[1].at - b[1].at)[0];
      if (!next || next[1].at > target) break;
      now = next[1].at;
      timers.delete(next[0]);
      next[1].callback();
      await settle();
    }
    now = target;
    await settle();
  };

  const response = context.request({ type: 'ping' }, 15000, true);
  const settled = response.then(value => ({ value }), error => ({ error }));
  await settle();
  assert.equal(timers.size, 1);
  const first = timers.values().next().value;
  assert.equal(first.ms, 15000);
  await advance(100);
  S.relayActivityAt = now;
  await advance(14900);
  assert.equal(S.pending.size, 1);
  assert.equal(timers.size, 1);
  assert.equal(timers.values().next().value.at, 25100, 'do not grant a fresh 15-second timeout at the old deadline');
  await advance(99);
  assert.equal(S.pending.size, 1);
  await advance(1);
  assert.match((await settled).error.message, /daemon did not answer/);
  assert.equal(S.pending.size, 0);
  assert.equal(timers.size, 0);

  S.relayActivityAt = now;
  const idleResponse = context.request({ type: 'ping' }, 15000, true).then(
    value => ({ value }), error => ({ error }),
  );
  await settle();
  await advance(15000);
  assert.match((await idleResponse).error.message, /daemon did not answer/);
  assert.equal(S.pending.size, 0);
});

test('a replacement relay reauthenticates after daemon_online', async () => {
  const sockets = [], controls = [], badgeUpdates = [];
  let now = 0, endpointDisposed = false;
  const S = {
    ws: { readyState: 1 }, local: false, daemonOnline: true, relayReady: true, channel: {}, room: 'room',
    approvals: [{ id: 'stale-approval' }], questions: [{ id: 'stale-question' }], pendingQuestion: { id: 'stale-question' },
    relayLink: { ws: null, endpoint: { dispose() { endpointDisposed = true; } } }, pending: new Map(),
  };
  S.relayLink.ws = S.ws;
  class FakeWebSocket {
    constructor(url) {
      assert.equal(S.approvals.length, 0, 'replacement clears stale approvals before opening');
      assert.equal(S.questions.length, 0, 'replacement clears stale questions before opening');
      this.url = url; this.readyState = 1; this.sent = []; sockets.push(this);
    }
    send(frame) { this.sent.push(JSON.parse(frame)); }
    close() {}
  }
  class FakeWireEndpoint {
    constructor(options) { this.options = options; this.closed = false; }
    receive(frame) {
      const packet = JSON.parse(frame);
      if (packet.relay === 'control') controls.push(this.options.control(packet.f));
    }
    dispose() { this.closed = true; }
  }
  Object.assign(S, { relayModule: { wireBytes: value => value.length, LIMITS: { plaintext: 1024 }, WireEndpoint: FakeWireEndpoint } });
  const context = vm.createContext({
    S, location: { protocol: 'https:', host: 'relay.test' }, WebSocket: FakeWebSocket,
    FRAME_VERSION: 2, HANDSHAKE_MS: 8000, HANDSHAKE_TRIES: 3,
    crypto: { getRandomValues: value => { value.fill(7); return value; } },
    bytesToB64: () => 'nonce', performance: { now: () => now }, setTimeout: () => 1, clearTimeout() {},
    $: () => ({ classList: { add() {}, remove() {} } }),
    clearLiveState: () => { S.approvals = []; S.questions = []; S.pendingQuestion = null; badgeUpdates.push(S.approvals.length); },
    renderAll() {}, renderChrome() {}, renderHistory() {}, note() {},
  });
  vm.runInContext(fn('sendHello'), context);
  vm.runInContext(fn('startHandshake'), context);
  vm.runInContext(fn('resetRelayConnection'), context);
  vm.runInContext(fn('openRelay'), context);
  context.openRelay();
  assert.equal(endpointDisposed, true);
  assert.deepEqual(badgeUpdates, [0]);
  assert.equal(S.daemonOnline, false, 'new socket starts offline until its daemon_online transition');
  const ws = sockets[0], generation = 'a'.repeat(32);
  const sendControl = value => ws.onmessage({ data: JSON.stringify({
    relay: 'control', g: generation, f: JSON.stringify(value),
  }) });
  now = 1;
  sendControl({ relay: 'transport', v: 1, g: generation });
  await settle();
  assert.equal(S.relayActivityAt, 1);
  now = 2;
  sendControl({ relay: 'daemon_offline' });
  await settle();
  assert.equal(S.approvals.length, 0);
  assert.equal(S.questions.length, 0);
  now = 3;
  sendControl({ relay: 'daemon_online' });
  await settle();
  assert.equal(S.daemonOnline, true);
  assert.equal(ws.sent.length, 1);
  assert.deepEqual(ws.sent[0], { v: 2, hello: 'nonce' }, 'new generation sends its authentication hello');
  assert.equal(S.relayActivityAt, 3);
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

test('periodic relay heartbeat runs every 10 seconds with a 15-second no-progress timeout', async () => {
  const h = relayHarness();
  let reject;
  h.setProbe(() => new Promise((_, fail) => { reject = fail; }));
  assert.equal(h.context.interval.ms, 10000);
  h.context.interval.callback();
  assert.equal(h.requests[0].type, 'ping');
  assert.equal(h.requests[0].timeout, 15000);
  assert.equal(h.requests[0].extendOnRelayActivity, true);
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
