import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const app = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const section = app.slice(app.indexOf('  let runningRefresh ='), app.indexOf('  function sessionRows('));
function harness() {
  const deferred = [], microtasks = [], paints = [], requests = [];
  const layout = { rect: true, hidden: false, visibility: 'visible' };
  const sidebar = {
    getClientRects: () => layout.rect ? [{}] : [],
    closest: () => layout.hidden ? sidebar : null,
  };
  const S = { view: 'chat', ws: { readyState: 1 }, local: true, channel: {}, invocations: [] };
  const document = { hidden: false, addEventListener(event, callback) { assert.equal(event, 'visibilitychange'); this.change = callback; } };
  const context = vm.createContext({
    S, document, $: id => { assert.equal(id, 'sidebar'); return sidebar; },
    getComputedStyle: () => layout,
    queueMicrotask: callback => microtasks.push(callback),
    setInterval: (callback, ms) => { assert.equal(ms, 2000); context.tick = callback; },
    request(req) {
      assert.equal(req.type, 'list_invocations');
      requests.push(req.type);
      return new Promise((resolve, reject) => deferred.push({ resolve, reject }));
    },
    reconcileInvocationProgress: () => {},
    paints,
  });
  vm.runInContext(section + '\nrenderRunning = list => paints.push(list);', context);
  const flush = () => { while (microtasks.length) microtasks.shift()(); };
  const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
  const reply = async list => { deferred.shift().resolve(list); await settle(); };
  const fail = async () => { deferred.shift().reject(new Error('offline')); await settle(); };
  return { S, document, context, layout, paints, requests, flush, reply, fail };
}

test('periodic work stops outside the actual exposed sidebar, independent events still refresh', async () => {
  const { S, document, context, layout, requests, reply } = harness();
  S.invocations = [{}];
  document.hidden = true; context.tick(); document.hidden = false;
  S.view = 'metrics'; context.tick(); S.view = 'chat';
  layout.rect = false; context.tick(); layout.rect = true;
  layout.hidden = true; context.tick(); layout.hidden = false;
  layout.visibility = 'hidden'; context.tick();
  assert.equal(requests.length, 0);
  context.refreshRunning();
  assert.equal(requests.length, 1);
  await reply([{}]);
  layout.visibility = 'visible'; context.tick();
  assert.equal(requests.length, 2);
  context.tick();
  assert.equal(requests.length, 2, 'slow read is not overlapped by the interval');
});

test('opening an empty sidebar coalesces exposures and picks up later events', async () => {
  const { S, document, context, layout, requests, flush, reply, paints, fail } = harness();
  document.hidden = true;
  layout.visibility = 'hidden';
  document.change(); flush(); context.tick();
  assert.equal(requests.length, 0);
  document.hidden = false;
  layout.visibility = 'visible';
  document.change(); context.syncRunningExposure(); context.syncRunningExposure(); flush();
  assert.equal(requests.length, 1);
  context.refreshRunning(); context.refreshRunning();
  await fail();
  assert.equal(requests.length, 2, 'events during a failed read retain one fresh request');
  const active = [{ handle: 'active', silent_ms: 0 }];
  await reply(active);
  assert.deepEqual(paints, [active]);
  layout.visibility = 'hidden'; context.syncRunningExposure(); flush();
  context.refreshRunning();
  await reply([]);
  assert.deepEqual(S.invocations, []);
  assert.deepEqual(paints, [active, []]);
});

test('quick hides cancel pending exposure and old connections cannot paint', async () => {
  const { S, document, context, layout, requests, flush, reply, paints } = harness();
  context.syncRunningExposure();
  S.view = 'tools'; context.syncRunningExposure(); flush();
  assert.equal(requests.length, 0);
  S.view = 'chat'; context.syncRunningExposure(); flush();
  S.channel = null; document.hidden = true; document.change();
  S.ws = { readyState: 1 }; S.channel = {};
  document.hidden = false; layout.visibility = 'visible'; document.change(); flush();
  assert.equal(requests.length, 2, 'new connection does not wait on the old read');
  await reply([{ handle: 'stale' }]);
  assert.deepEqual(paints, []);
  S.conn = 'new subscription';
  await reply([{ handle: 'old subscription' }]);
  assert.equal(requests.length, 3);
  assert.deepEqual(paints, []);
  const fresh = [{ handle: 'fresh' }];
  await reply(fresh);
  assert.deepEqual(paints, [fresh]);
});

test('drawer layout and completion hooks use the same refresh logic', () => {
  assert.match(app, /function syncSidebar\(\)[\s\S]{0,450}syncRunningExposure\(\);/);
  assert.match(app, /new ResizeObserver\(syncSidebar\)/);
  assert.match(app, /case "invocation_completed": \{\n        refreshRunning\(\);/);
  assert.match(section, /cancel_invocation[\s\S]+then\(\(\) => refreshRunning\(\)\)/);
  assert.match(section, /abandon_invocation[\s\S]+then\(\(\) => refreshRunning\(\)\)/);
});
