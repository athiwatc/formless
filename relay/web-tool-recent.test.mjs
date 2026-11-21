// Exercise shipped helpers without a browser, provider, or daemon.
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
const context = vm.createContext({
  tr: text => text, uiFormat: String,
  h: (tag, attrs, ...children) => ({ tag, attrs, children: children.flat() }),
});
vm.runInContext(['recentCalls', 'toolRecent'].map(fn).join('\n'), context);
const sample = text => JSON.parse(JSON.stringify(context.recentCalls(text)));

test('matches TUI falling, flat and insufficient earlier-window rules', () => {
  assert.equal(sample('s'.repeat(20) + 'sfff'.repeat(5)).trend, 'down');
  assert.equal(sample('s'.repeat(40)).trend, 'none');
  assert.equal(sample('ffffff').trend, 'none');
  assert.equal(sample('f'.repeat(4) + 's'.repeat(20)).trend, 'none');
  assert.equal(sample('f'.repeat(5) + 's'.repeat(20)).trend, 'up');
  assert.equal(sample('s'.repeat(10) + 'f'.repeat(10) + 's'.repeat(13) + 'f'.repeat(7)).trend, 'up');
  assert.equal(sample('s'.repeat(10) + 'f'.repeat(10) + 's'.repeat(11) + 'f'.repeat(9)).trend, 'none');
});

test('bounds sample to 40 and graphic to latest 20, oldest first', () => {
  const recent = 's'.repeat(100) + 'f'.repeat(40);
  assert.deepEqual(sample(recent), { now: 'f'.repeat(20), succeeded: 0, failed: 20, trend: 'none' });
  const rendered = context.toolRecent('s'.repeat(20) + 'sfff'.repeat(5));
  const [bars, counts] = rendered.children;
  assert.equal(bars.children.length, 20);
  assert.equal(bars.children[0].attrs.class, 'rate-good success');
  assert.equal(bars.children[1].attrs.class, 'rate-bad failure');
  assert.equal(counts.children[0], '5/20 ✓ ↓');
});

test('accessible non-hover summary includes counts and descriptive trend', () => {
  const rendered = context.toolRecent('s'.repeat(20) + 'sfff'.repeat(5));
  assert.equal(rendered.attrs.role, 'img');
  assert.equal(rendered.attrs['aria-label'], 'Recent retained calls: 20; Succeeded: 5; Failed: 15; Recent sample trend: down');
  assert.ok(rendered.children.every(child => child.attrs['aria-hidden'] === 'true'));
  context.tr = text => 'localized:' + text;
  assert.match(context.toolRecent('sf').attrs['aria-label'], /localized:Recent retained calls/);
  context.tr = text => text;
});

test('missing, empty or malformed retained samples do not invent outcomes', () => {
  for (const value of ['', null, undefined, 42, {}, 'sx', 's f']) {
    assert.equal(context.recentCalls(value), null);
    assert.equal(context.toolRecent(value), null);
  }
});

test('shared Tools/Metrics renderer uses recent data without extra requests', () => {
  const table = fn('toolsTable');
  assert.match(table, /rateCell\(t.success_rate\), toolRecent\(t.recent\)/);
  assert.doesNotMatch(fn('toolRecent') + fn('recentCalls'), /\brpc\b|fetch\(|setInterval/);
});
