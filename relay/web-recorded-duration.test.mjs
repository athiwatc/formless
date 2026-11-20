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
  const notices = [];
  let now = 100_000;
  const c = vm.createContext({
    Date: { now: () => now },
    uiFormat: (x, options) => x.toLocaleString('en-US', options),
    S: { settings: { tui: { notify_min_secs: 1 } } }, runStarts: new Map(),
    sessionName: () => 'Synthetic', alertUser: (...notice) => notices.push(notice),
    h: (tag, props, ...children) => ({ tag, props, children }), tr: x => x,
  });
  vm.runInContext(['recordedDuration', 'notifyFor', 'recapBlock'].map(fn).join('\n'), c);
  return { c, notices, advance: ms => { now += ms; } };
}
test('recorded duration retains subsecond values and never prematurely crosses a unit', () => {
  const { c } = fixture();
  for (const [ms, expected] of [
    [0, '0s'], [499, '499ms'], [1000, '1s'], [1175, '1.1s'], [59999, '59.9s'],
    [60000, '1m00s'], [3599999, '59m59s'], [3600000, '1h00m00s'],
    [7200000, '2h00m00s'], [undefined, '—'], [-1, '—'],
  ]) assert.equal(c.recordedDuration(ms), expected, String(ms));
});
test('recap actually displays the subsecond duration', () => {
  const { c } = fixture();
  const recap = c.recapBlock({ steps: 1, duration_ms: 499, status: 'completed', used: [], built: [], files: [], steered: 0 });
  assert.match(recap.children[0].children[0], /499ms/);
});
test('notifications preserve the threshold and describe long finished runs', () => {
  const { c, notices, advance } = fixture();
  const start = () => c.notifyFor({ type: 'run_started', session_id: 's' });
  const finish = status => c.notifyFor({ type: 'run_finished', session_id: 's', status });
  start(); advance(999); finish('completed');
  start(); advance(7200000); finish('interrupted');
  finish('completed');
  assert.equal(notices.length, 0);
  start(); advance(1000); finish('completed');
  start(); advance(7200000); finish('completed');
  assert.deepEqual(notices, [
    ['Run completed', 'Synthetic · 1s'],
    ['Run completed', 'Synthetic · 2h00m00s'],
  ]);
});
