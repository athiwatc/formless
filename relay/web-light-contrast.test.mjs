import { readFileSync } from 'node:fs';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1744: in both light palettes (explicit data-theme="light" and system light),
// enabled primary-button text and current-tab text meet WCAG AA for normal text
// (4.5:1), also while a primary button is hovered (filter: brightness(1.08)).
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
const decls = (body) => Object.fromEntries(body.split(';').map(d => d.trim()).filter(Boolean)
  .map(d => { const k = d.indexOf(':'); return [d.slice(0, k).trim(), d.slice(k + 1).trim()]; }));
const only = (rules, sel) => { const r = rules.filter(r => r.selectors.length === 1 && r.selectors[0] === sel); assert.equal(r.length, 1, sel); return r[0]; };
const dark = decls(only(top, ':root').body);
const explicitLight = decls(only(top, ':root[data-theme="light"]').body);
const systemMedia = media.filter(m => m.query === '@media (prefers-color-scheme: light)');
const systemLight = decls(only(systemMedia.flatMap(m => m.rules), ':root:not([data-theme="dark"])').body);
const rule = (sel) => decls(only(top, sel).body);

// WCAG 2.x relative luminance and contrast ratio over 8-bit sRGB.
const rgb = (hex) => { const h = hex.replace('#', ''); const f = h.length === 3 ? [...h].map(c => c + c).join('') : h; return [0, 2, 4].map(i => parseInt(f.slice(i, i + 2), 16)); };
const lum = (c) => { const [r, g, b] = c.map(v => { v /= 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
// filter: brightness(k) multiplies each sRGB channel of the painted element (text and background) and clamps.
const bright = (c, k) => c.map(v => Math.min(255, Math.round(v * k)));
/** Resolve var(--x[, fallback]) against a palette. */
function resolve(value, pal) {
  const m = value.match(/^var\((--[\w-]+)\s*(?:,\s*(.+))?\)$/);
  if (!m) return value;
  if (pal[m[1]] !== undefined) return resolve(pal[m[1]], pal);
  assert.ok(m[2], 'unresolved ' + value);
  return resolve(m[2].trim(), pal);
}
const AA = 4.5;

test('explicit light and system light define the same accent tokens', () => {
  for (const k of ['--accent', '--accent-ink', '--accent-soft', '--accent-strong']) {
    assert.ok(explicitLight[k], k + ' missing from explicit light');
    assert.equal(systemLight[k], explicitLight[k], k + ' differs between explicit and system light');
  }
  assert.equal(explicitLight['--accent-strong'], '#127040');
});

test('shared brand, status and dark tokens are unchanged', () => {
  for (const pal of [explicitLight, systemLight]) {
    assert.deepEqual([pal['--accent'], pal['--accent-ink'], pal['--accent-soft']], ['#1f9d5a', '#ffffff', '#dff3e8']);
    assert.deepEqual([pal['--warn'], pal['--err'], pal['--err-soft'], pal['--user'], pal['--think']],
      ['#a87700', '#c33a2c', '#fbe4e1', '#1f7fa8', '#8a4aa3']);
  }
  assert.deepEqual([dark['--accent'], dark['--accent-ink'], dark['--accent-soft']], ['#57c785', '#07130c', '#1d3127']);
  assert.equal(dark['--accent-strong'], undefined, 'dark keeps its accent via the var() fallback');
});

test('only enabled primary buttons and the current tab use the strong accent', () => {
  assert.deepEqual(rule('.btn.primary'), { background: 'var(--accent)', color: 'var(--accent-ink)', 'border-color': 'transparent' });
  assert.deepEqual(rule('.btn:disabled'), { opacity: '.45', cursor: 'default' });
  assert.deepEqual(rule('.btn.primary:not(:disabled)'), { background: 'var(--accent-strong, var(--accent))' });
  assert.deepEqual(rule('.tabs button[aria-current="page"]'), { color: 'var(--accent-strong, var(--accent))', background: 'var(--accent-soft)' });
  const uses = [...top, ...media.flatMap(m => m.rules)].filter(r => r.body.includes('var(--accent-strong'));
  assert.deepEqual(uses.flatMap(r => r.selectors).sort(), ['.btn.primary:not(:disabled)', '.tabs button[aria-current="page"]']);
  // Same specificity as .tabs button:hover, later in the sheet: a hovered current tab keeps its colours.
  assert.ok(css.indexOf('.tabs button[aria-current="page"]') > css.indexOf('.tabs button:hover'));
});

test('light primary button and current tab text reach AA, normal and hovered', () => {
  const k = Number(rule('.btn.primary:hover').filter.match(/^brightness\(([\d.]+)\)$/)[1]);
  assert.equal(k, 1.08);
  for (const pal of [explicitLight, systemLight]) {
    const p = rule('.btn.primary:not(:disabled)'), base = rule('.btn.primary'), tab = rule('.tabs button[aria-current="page"]');
    const bg = rgb(resolve(p.background, pal)), ink = rgb(resolve(base.color, pal));
    const tabInk = rgb(resolve(tab.color, pal)), tabBg = rgb(resolve(tab.background, pal));
    const got = {
      primary: ratio(ink, bg),
      primaryHover: ratio(bright(ink, k), bright(bg, k)),
      tab: ratio(tabInk, tabBg),
    };
    for (const [name, r] of Object.entries(got)) assert.ok(r >= AA, `${name} ${r.toFixed(3)}:1 < ${AA}:1`);
    assert.ok(Math.abs(got.primary - 6.145) < 0.01 && Math.abs(got.primaryHover - 5.457) < 0.01 && Math.abs(got.tab - 5.303) < 0.01, JSON.stringify(got));
  }
  // The issue's baseline, for the arithmetic: white on #1f9d5a and #1f9d5a on #dff3e8.
  assert.ok(Math.abs(ratio(rgb('#ffffff'), rgb('#1f9d5a')) - 3.483) < 0.001);
  assert.ok(Math.abs(ratio(rgb('#1f9d5a'), rgb('#dff3e8')) - 3.006) < 0.001);
});
