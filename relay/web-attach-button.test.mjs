import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1740: Attach must be a keyboard-reachable native button that opens the
// hidden file input from its click (the user gesture) and never sends.
const html = readFileSync(new URL('./web/index.html', import.meta.url), 'utf8');
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const tag = (id) => { const m = html.match(new RegExp('<(\\w+)\\b[^>]*\\bid="' + id + '"[^>]*>')); assert.ok(m, id); return m; };
test('Attach is a native type=button named "Attach a file", with the multi-file input outside it', () => {
  const [btn, name] = tag('attach-btn');
  assert.equal(name, 'button');
  assert.match(btn, /\btype="button"/);
  assert.match(btn, /\baria-label="Attach a file"/);
  assert.doesNotMatch(btn, /\btabindex="-/);
  const [input] = tag('attach-input');
  assert.match(input, /\btype="file"/);
  assert.match(input, /\bmultiple\b/);
  const before = html.slice(0, html.indexOf(input));
  for (const el of ['label', 'button']) {
    const open = (before.match(new RegExp('<' + el + '\\b', 'g')) || []).length;
    const close = (before.match(new RegExp('</' + el + '>', 'g')) || []).length;
    assert.equal(open, close, 'attach-input sits inside a <' + el + '>');
  }
});
test('clicking Attach opens the picker exactly once and never submits the composer', () => {
  const line = source.split('\n').find(l => /\$\("attach-btn"\)\.addEventListener\("click"/.test(l));
  assert.ok(line, 'attach-btn click wiring');
  assert.match(line, /\$\("attach-input"\)\.click\(\)/);
  const listeners = {}, calls = { picker: 0, submit: 0 };
  const els = {
    'attach-btn': { addEventListener: (t, f) => { (listeners[t] ||= []).push(f); } },
    'attach-input': { click: () => { calls.picker++; } }, // inert picker stub
    composer: { requestSubmit: () => { calls.submit++; }, submit: () => { calls.submit++; } },
  };
  vm.runInNewContext(line, { $: (id) => { assert.ok(els[id], 'unexpected $(' + id + ')'); return els[id]; } });
  assert.equal(listeners.click.length, 1);
  listeners.click[0]({ type: 'click', preventDefault() {} });
  assert.deepEqual(calls, { picker: 1, submit: 0 });
});
test('the existing change handler still attaches every chosen file and resets the input', () => {
  assert.match(source, /\$\("attach-input"\)\.addEventListener\("change", \(e\) => \{ attachFiles\(\[\.\.\.e\.target\.files\]\); e\.target\.value = ""; \}\);/);
});
