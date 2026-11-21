import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
const cursor = id => ({ cursor: id });
function fixture() {
  const calls = [], S = { ws: { readyState: 1 }, channel: {}, local: true, view: 'settings' };
  const c = vm.createContext({
    S,
    $: () => ({ classList: { contains: () => false } }),
    prefs: { get: (_key, fallback) => fallback },
    request(req) {
      if (req.type !== 'list_audit') return Promise.resolve([]);
      return new Promise((resolve, reject) => calls.push({ req, resolve, reject }));
    },
    act: async () => ({}),
    refreshDescriptionReviews: async () => {},
    applyMotion() {}, applyLanguage() {}, renderView() {}, renderStatus() {}, renderApproval() {},
  });
  vm.runInContext([
    'let auditRequest = null, auditOwner = null;',
    source.slice(source.indexOf('  const settingsLists ='), source.indexOf('  function settingsConnectionReady')),
    ...['approvalHelpSeen', 'ownsAudit', 'loadAudit', 'settingsConnectionReady', 'isSettingsConnection',
      'refreshSettings', 'requestSettingsRefresh', 'drainSettingsRefresh', 'loadSettingsDomain'].map(fn),
  ].join('\n'), c);
  const ids = () => Array.from(S.audit || [], e => e.cursor);
  const refresh = async () => { const run = c.refreshSettings(); await tick(); return { run }; };
  async function seed(id = 'first') {
    const ran = c.loadAudit(false);
    calls.at(-1).resolve([cursor(id)]);
    await ran;
  }
  return { c, S, calls, ids, refresh, seed };
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test('explicit Older keeps first page ownership ahead of background refresh and duplicate clicks', async () => {
  const f = fixture(); await f.seed();
  const older = f.c.loadAudit(true);
  assert.equal(f.calls.at(-1).req.before, 'first');
  await f.c.loadAudit(true);
  await f.c.refreshSettings();
  assert.equal(f.calls.length, 2);
  f.calls[1].resolve([cursor('older')]); await older;
  assert.deepEqual(f.ids(), ['first','older']); assert.equal(f.S.auditPages, 2);
  await f.c.refreshSettings(); assert.equal(f.calls.length, 2);
});

test('explicit Older can supersede a pending background head result', async () => {
  const f = fixture(); await f.seed();
  const { run: refresh } = await f.refresh();
  const older = f.c.loadAudit(true);
  f.calls[2].resolve([cursor('older')]); await older;
  f.calls[1].resolve([cursor('stale-head')]); await refresh;
  assert.deepEqual(f.ids(), ['first','older']);
});

test('failed Older preserves the cursor and leaves another attempt available', async () => {
  const f = fixture(); await f.seed();
  const older = f.c.loadAudit(true);
  f.calls[1].reject(new Error('timeout')); await older;
  assert.deepEqual(f.ids(), ['first']); assert.equal(f.S.auditPages, 1);
  const retry = f.c.loadAudit(true);
  assert.equal(f.calls[2].req.before, 'first');
  f.calls[2].resolve([cursor('retry')]); await retry;
  assert.deepEqual(f.ids(), ['first','retry']);
});

test('explicit filter owns its head result; cannot paginate rows from another filter', async () => {
  const f = fixture(); await f.seed();
  const older = f.c.loadAudit(true);
  f.S.auditFilter = 'filtered';
  const newFilter = f.c.loadAudit(false);
  assert.equal(f.calls[2].req.filter, 'filtered');
  await f.c.loadAudit(true);
  await f.c.refreshSettings();
  assert.equal(f.calls.length, 3);
  f.calls[2].resolve([cursor('filter-head')]); await newFilter;
  f.calls[1].resolve([cursor('stale-old')]); await older;
  assert.deepEqual(f.ids(), ['filter-head']); assert.equal(f.S.auditPages, 1);
});

test('changed socket/channel cannot paginate old rows, can reseed even after page 2', async () => {
  const f = fixture(); await f.seed();
  const pageTwo = f.c.loadAudit(true);
  f.calls[1].resolve([cursor('second')]); await pageTwo;
  const pageThree = f.c.loadAudit(true);
  f.S.channel = {};
  await f.c.loadAudit(true); assert.equal(f.calls.length, 3);
  const { run: refresh } = await f.refresh();
  assert.equal(f.calls[3].req.before, null);
  f.calls[2].resolve([cursor('stale-third')]); await pageThree;
  f.calls[3].resolve([cursor('fresh')]); await refresh;
  assert.deepEqual(f.ids(), ['fresh']); assert.equal(f.S.auditPages, 1);
});

test('an old Settings traversal cannot start an audit request after reconnect', async () => {
  const f = fixture(); let finish;
  const request = f.c.request;
  f.c.request = req => req.type === 'list_mcp_servers'
    ? new Promise(resolve => { finish = resolve; }) : request(req);
  const refresh = f.c.refreshSettings();
  await tick();
  f.S.ws = {};
  finish([]);
  await refresh;
  assert.equal(f.calls.length, 0);
});

test('audit is global: switching the open chat Session does not invalidate an audit page', async () => {
  const f = fixture(); await f.seed();
  const older = f.c.loadAudit(true);
  f.S.current = 'another-chat';
  f.S.conn = 'updated-subscription';
  f.calls[1].resolve([cursor('older')]); await older;
  assert.deepEqual(f.ids(), ['first','older']);
});
