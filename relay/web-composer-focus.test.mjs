import { readFileSync } from 'node:fs';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1743: the composer textarea keeps a visible keyboard-focus outline, also in
// forced-colors mode where the focused and resting border can look the same.
const css = readFileSync(new URL('./web/app.css', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const norm = (s) => s.trim().replace(/\s+/g, ' ');
/** Top-level rules and the inner rules of each @media block. */
function parse(text) {
  const top = [], media = [];
  let i = 0;
  while (i < text.length) {
    const open = text.indexOf('{', i);
    if (open < 0) break;
    const prelude = norm(text.slice(i, open));
    if (prelude.startsWith('@')) {
      let depth = 1, j = open + 1;
      for (; j < text.length && depth; j++) depth += text[j] === '{' ? 1 : text[j] === '}' ? -1 : 0;
      media.push({ query: prelude, rules: parse(text.slice(open + 1, j - 1)).top });
      i = j;
    } else {
      const close = text.indexOf('}', open);
      top.push({ selectors: prelude.split(',').map(norm), body: norm(text.slice(open + 1, close)) });
      i = close + 1;
    }
  }
  return { top, media };
}
const { top, media } = parse(css);
const decl = (body, prop) => { const m = body.match(new RegExp('(?:^|;)\\s*' + prop + '\\s*:\\s*([^;]+)')); return m && m[1].trim(); };
const focusSel = (s) => /textarea/.test(s) && /:focus/.test(s) && /\.composer|#composer|#input/.test(s);
const SYSTEM = /\b(Highlight|CanvasText|ButtonText|LinkText|AccentColor|SelectedItem|Mark|ActiveText|FieldText)\b/;
test('(a) no unconditional composer textarea focus rule removes the outline', () => {
  for (const r of [...top, ...media.flatMap(m => m.rules)]) for (const s of r.selectors) {
    if (!focusSel(s)) continue;
    const o = decl(r.body, 'outline') ?? decl(r.body, 'outline-style');
    if (o && /^(none|0)\b/.test(o)) assert.match(s, /:not\(:focus-visible\)/, 'outline removed by ' + s);
  }
});
test('(b) keyboard focus on the composer textarea sets a visible outline in normal themes', () => {
  const rules = top.filter(r => r.selectors.some(s => focusSel(s) && !/:not\(/.test(s)));
  const o = rules.map(r => decl(r.body, 'outline')).filter(Boolean).pop();
  assert.ok(o && !/^(none|0)\b/.test(o) && /\bsolid\b/.test(o), 'normal-theme outline: ' + o);
});
test('(c, d) forced-colors gives the focused textarea a system-colour outline, without opting out', () => {
  const fc = media.filter(m => /^@media \(forced-colors: ?active\)$/.test(m.query)).flatMap(m => m.rules);
  const r = fc.find(r => r.selectors.some(focusSel));
  assert.ok(r, 'no forced-colors focus rule for the composer textarea');
  const o = decl(r.body, 'outline');
  assert.ok(o && /\bsolid\b/.test(o) && SYSTEM.test(o), 'forced-colors outline: ' + o);
  assert.doesNotMatch(css, /forced-color-adjust\s*:\s*none/);
});
test('(e) the global :focus-visible ring is unchanged', () => {
  assert.deepEqual(top.filter(r => r.selectors.includes(':focus-visible')).map(r => r.body),
    ['outline: 2px solid var(--accent); outline-offset: 2px;']);
});
