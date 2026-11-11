import { readFileSync } from 'node:fs';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1708: the Running panel (div#running) must not style every `.running`
// element; Session activity marks and the composer's running state reuse it.
const css = readFileSync(new URL('./web/app.css', import.meta.url), 'utf8');
const html = readFileSync(new URL('./web/index.html', import.meta.url), 'utf8');
const selectors = css.replace(/\/\*[\s\S]*?\*\//g, '').match(/[^{}]+(?=\{)/g)
  .flatMap(s => s.split(',')).map(s => s.trim().replace(/\s+/g, ' ')).filter(Boolean);
test('Running panel rules are scoped to #running, not the shared .running class', () => {
  assert.match(html, /<div id="running"[^>]*>/);
  for (const s of selectors) assert.ok(!/^\.running(?![\w-])/.test(s), 'unscoped Running-panel selector: ' + s);
  assert.ok(selectors.includes('#running'));
  assert.ok(selectors.includes('#running h4'));
});
test('Session marks, composer state and running items keep their own rules', () => {
  for (const s of ['.session .running', '.composer.running textarea', '.running-item', '.running-item .linklike', '.running-item.stalled .linklike'])
    assert.ok(selectors.includes(s), 'missing ' + s);
});
