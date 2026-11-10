import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
// #1735: with no hover (touch), every Session row shows its actions button;
// desktop keeps hover/focus/active/expanded reveal, and tapping it opens the
// menu without opening the Session.
const css = readFileSync(new URL('./web/app.css', import.meta.url), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
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
const rulesFor = (rules, sel) => rules.filter(r => r.selectors.includes(sel));
const noHover = media.filter(m => /^@media \(hover: ?none\)$/.test(m.query)).flatMap(m => m.rules);
test('desktop: Session actions stay hidden until hover, focus-within, active or expanded', () => {
  assert.deepEqual(rulesFor(top, '.session .menu').map(r => r.body), ['visibility: hidden;']);
  const reveal = top.find(r => r.selectors.includes('.session:hover .menu'));
  assert.ok(reveal);
  assert.deepEqual(reveal.selectors, ['.session:hover .menu', '.session:focus-within .menu', '.session.active .menu', '.session .menu[aria-expanded="true"]']);
  assert.equal(reveal.body, 'visibility: visible;');
});
test('hover:none reveals every Session row menu and still reveals the message menu', () => {
  assert.ok(rulesFor(noHover, '.session .menu').some(r => /visibility: visible/.test(r.body)), '.session .menu not revealed under hover:none');
  assert.ok(rulesFor(noHover, '.msg-menu').some(r => r.body === 'opacity: .7;'), '.msg-menu touch rule changed');
});
test('the touch reveal does not leak outside the hover:none query', () => {
  for (const r of top) for (const s of r.selectors)
    if (/^\.session\b.*\.menu\b/.test(s) && /visibility: visible/.test(r.body))
      assert.ok(/:hover|:focus-within|\.active|\[aria-expanded/.test(s), 'unconditional reveal: ' + s);
  for (const m of media) if (!/hover: ?none/.test(m.query))
    assert.equal(rulesFor(m.rules, '.session .menu').length, 0, 'session menu rule in ' + m.query);
});
test('tapping Session actions opens its menu without opening the Session', () => {
  const start = source.indexOf('class: "icon-btn menu", type: "button", "aria-label": "Session actions"');
  assert.ok(start >= 0, 'Session actions button');
  const line = source.slice(start, source.indexOf('}, "⋯")', start)).split('\n').find(l => /^\s*onclick:/.test(l));
  assert.ok(line && /stopPropagation/.test(line), 'onclick with stopPropagation');
  const calls = { menu: 0, open: 0 };
  const ctx = vm.createContext({ menu: { synthetic: 'button' }, s: { id: 'synthetic' },
    sessionMenu: () => { calls.menu++; }, openSession: () => { calls.open++; } }); // inert stubs
  const onclick = vm.runInContext('(' + line.trim().replace(/^onclick:\s*/, '').replace(/,\s*$/, '') + ')', ctx);
  // Synthetic bubble: the row's own click (openSession) runs only if not stopped.
  let stopped = false;
  const event = { stopPropagation() { stopped = true; } };
  onclick(event);
  if (!stopped) ctx.openSession('synthetic');
  assert.deepEqual(calls, { menu: 1, open: 0 });
});
