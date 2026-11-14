// Issue #1734: the status bar's Session, also-open and usage text spans wrap
// inside the bar instead of overflowing it. Static CSS checks; rendered
// narrow/enlarged-font geometry is verified separately in a browser.
import { readFileSync } from 'node:fs';
import test from 'node:test';
import assert from 'node:assert/strict';

const css = readFileSync(new URL('./web/app.css', import.meta.url), 'utf8');
const html = readFileSync(new URL('./web/index.html', import.meta.url), 'utf8');
const plain = css.replace(/\/\*[\s\S]*?\*\//g, '');

// Every top-level-or-nested rule as [selector, body], in source order.
function rules(source) {
  const out = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m;
  while ((m = re.exec(source))) out.push([m[1].trim().replace(/\s+/g, ' '), m[2].trim()]);
  return out;
}
const all = rules(plain);
const TEXT = ['#status-session', '#status-others', '#status-usage'];
const decls = (body) => Object.fromEntries(body.split(';').map((d) => d.trim()).filter(Boolean)
  .map((d) => [d.slice(0, d.indexOf(':')).trim(), d.slice(d.indexOf(':') + 1).trim()]));

test('the three ordinary status text spans are still plain spans in the status bar', () => {
  const bar = html.slice(html.indexOf('<footer class="statusbar">'), html.indexOf('</footer>'));
  assert.ok(bar.includes('<span id="status-session" class="strong"></span>'));
  assert.ok(bar.includes('<span id="status-others" class="muted hidden"></span>'));
  assert.ok(bar.includes('<span id="status-usage" class="muted"></span>'));
});

test('one rule targets exactly those spans and lets their text wrap', () => {
  const mine = all.filter(([sel]) => TEXT.some((id) => sel.split(',').some((s) => s.includes(id))));
  assert.equal(mine.length, 1, JSON.stringify(mine));
  const [sel, body] = mine[0];
  assert.deepEqual(sel.split(',').map((s) => s.trim()), TEXT);
  assert.deepEqual(decls(body), {
    'white-space': 'normal', 'overflow-wrap': 'anywhere', 'min-width': '0', 'max-width': '100%',
  });
  // Wrapping, not truncation: the full text stays visible without hover.
  assert.doesNotMatch(body, /text-overflow|ellipsis|nowrap|overflow\s*:|display|padding|margin|flex/);
});

test('the rule overrides the bar nowrap without touching neighbours', () => {
  const at = (needle) => { const i = css.indexOf(needle); assert.ok(i >= 0, needle); return i; };
  assert.ok(at('#status-session, #status-others, #status-usage {') > at('.statusbar { display: flex;'));
  const expected = [
    '.statusbar { display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem; padding: 0.3125rem 1rem; border-top: 1px solid var(--line); background: var(--panel); font-size: 0.78125rem; min-width: 0; white-space: nowrap; }',
    '.statusbar .sep { color: var(--line); }',
    '.statusbar .note { overflow: hidden; text-overflow: ellipsis; min-width: 0; }',
    '.statusbar .note.err { color: var(--err); }',
    '.statusbar .err { color: var(--err); font-weight: 650; }',
    '#status-context.clickable { cursor: pointer; }',
    '#status-model.clickable { cursor: pointer; }',
    '.spacer { flex: 1; }',
    '.hidden, [hidden] { display: none !important; }',
    '  .statusbar { padding: 0.3125rem 0.75rem; }',
  ];
  for (const line of expected) assert.ok(css.includes(line + '\n'), line);
  const hide = '  #status-model, #status-model + .sep, .statusbar .sep:nth-of-type(2) { display: none; }\n';
  assert.equal(css.split(hide).length - 1, 2, 'both model-hiding breakpoints unchanged');
});
