// Presentation preference validation without a browser; layout/print coverage is
// in web-display.browser-test.py and runs against the complete shipped UI.
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

test('reading preference preserves endpoints and recovers malformed persisted values', () => {
  const c = vm.createContext({});
  vm.runInContext(fn('readingScale'), c);
  for (const [input, expected] of [[0.1, .85], [.85, .85], [1, 1], [1.95, 1.95], [2, 2], [3, 2]]) {
    assert.equal(c.readingScale(input), expected);
  }
  for (const input of [null, undefined, '2', {}, NaN, Infinity]) assert.equal(c.readingScale(input), 1);
});

test('reading setting changes root scale, persists it, and retires old menu geometry', () => {
  const root = { style: { setProperty: (key, value) => { root[key] = value; } }, dataset: {} };
  const saved = new Map();
  let closed = 0;
  const c = vm.createContext({ document: { documentElement: root }, S: { readScale: 1, view: 'chat' },
    prefs: { set: (k, v) => saved.set(k, v) }, closePopover: () => closed++ });
  vm.runInContext(['readingScale', 'setReading', 'applyReading'].map(fn).join('\n'), c);
  c.setReading(2.2, 'compact');
  assert.equal(root['--read-scale'], '2');
  assert.equal(saved.get('readScale'), 2);
  assert.equal(root.dataset.density, 'compact');
  c.setReading(.1, 'comfortable');
  assert.equal(root['--read-scale'], '0.85');
  assert.equal(root.dataset.density, undefined);
  assert.equal(closed, 2);
});
