// Issue #1677: a long unbroken token in a proposal's text or rationale wraps
// inside its card instead of widening the Proposals grid. Static CSS checks
// plus the real renderer in a minimal DOM; rendered geometry (narrow/wide,
// enlarged reading size, light/dark) is verified separately in a browser.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const css = readFileSync(new URL('./web/app.css', import.meta.url), 'utf8');
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const plain = css.replace(/\/\*[\s\S]*?\*\//g, '');
const fn = (name) => {
  const start = source.indexOf('  function ' + name + '(');
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
};

function rules(text) {
  const out = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m;
  while ((m = re.exec(text))) out.push([m[1].trim().replace(/\s+/g, ' '), m[2].trim()]);
  return out;
}
const decls = (body) => Object.fromEntries(body.split(';').map((d) => d.trim()).filter(Boolean)
  .map((d) => [d.slice(0, d.indexOf(':')).trim(), d.slice(d.indexOf(':') + 1).trim()]));
const all = rules(plain);
const bySelector = (sel) => all.filter(([s]) => s.split(',').map((x) => x.trim()).includes(sel)).map(([, b]) => decls(b));

test('proposal text keeps its newlines and gains emergency wrapping', () => {
  assert.deepEqual(bySelector('.proposal .content'), [{ 'white-space': 'pre-wrap', 'overflow-wrap': 'anywhere' }]);
});

test('the rationale paragraph wraps too, without changing its whitespace handling', () => {
  assert.deepEqual(bySelector('.proposal > p'), [{ 'overflow-wrap': 'anywhere' }]);
});

test('the card may shrink below its content so the grid track sets its width', () => {
  const cards = bySelector('.proposal');
  assert.equal(cards.length, 2, JSON.stringify(cards));
  assert.deepEqual(cards[0], { border: '1px solid var(--line)', 'border-radius': 'var(--radius)', background: 'var(--panel)', padding: '0.875rem 1rem' });
  assert.deepEqual(cards[1], { 'min-width': '0' });
});

test('the fix stays local: list, head, actions and the view are unchanged', () => {
  assert.deepEqual(bySelector('.proposal-list'), [{ display: 'grid', gap: '0.625rem' }]);
  assert.deepEqual(bySelector('.proposal-head'), [{ display: 'flex', 'align-items': 'center', gap: '0.5rem', 'flex-wrap': 'wrap', 'margin-bottom': '0.5rem' }]);
  assert.deepEqual(bySelector('.proposal .actions'), [{ display: 'flex', gap: '0.375rem', 'margin-top': '0.625rem' }]);
  assert.deepEqual(bySelector('.view')[0], { flex: '1', 'min-height': '0', overflow: 'auto', padding: '1.25rem 1.5rem' });
  // No proposal rule clips, hides, truncates or breaks words in the header.
  for (const [sel, body] of all.filter(([s]) => /\.proposal\b/.test(s))) {
    const d = decls(body);
    for (const k of ['overflow', 'overflow-x', 'text-overflow', 'word-break', 'max-width', 'width', 'display']) {
      if (sel === '.proposal-list' && k === 'display') continue;
      if (sel.startsWith('.proposal-head') || sel === '.proposal .actions') continue;
      assert.ok(!(k in d), sel + ' sets ' + k);
    }
  }
});

test('the renderer still puts full text and rationale in those elements', () => {
  const render = fn('renderProposals');
  assert.ok(render.includes('h("div", { class: "content" }, p.content)'));
  assert.ok(render.includes('p.rationale ? h("p", { class: "muted", style: "margin:0.5rem 0 0" }, p.rationale) : null'));
  assert.ok(render.includes('box.append(h("article", { class: "proposal" },'));
});

// A minimal DOM: just enough for h() and renderProposals.
class El {
  constructor(tag) { this.tagName = tag.toUpperCase(); this.className = ''; this.style = {}; this.attrs = {}; this.children = []; this.listeners = []; this.value = ''; }
  setAttribute(k, v) { this.attrs[k] = v; }
  getAttribute(k) { return this.attrs[k] ?? null; }
  hasAttribute(k) { return k in this.attrs; }
  addEventListener(t, f) { this.listeners.push([t, f]); }
  append(...kids) { this.children.push(...kids); }
  replaceChildren() { this.children = []; }
  get textContent() { return this.children.map((c) => typeof c === 'string' ? c : c.textContent).join(''); }
  set textContent(v) { this.children = [String(v)]; }
}

test('the real renderer keeps every character and invokes no action', () => {
  const ids = { proposals: new El('div'), 'proposals-count': new El('span'), 'proposals-filter': new El('input'), 'proposals-quick': new El('select') };
  ids['proposals-quick'].value = 'all';
  const calls = [];
  const content = 'Line one\n\n  indented ' + 'x'.repeat(2000) + '\nไทย 漢字 é 👩‍👩‍👧 https://example.invalid/' + 'a/'.repeat(300);
  const rationale = 'Because ' + 'ซ'.repeat(800) + ' and e\u0301 👍🏽';
  const c = vm.createContext({
    document: { getElementById: (id) => ids[id], createElement: (t) => new El(t) },
    Node: El, S: { proposals: [
      { id: 'p1', kind: 'prompt', status: 'measured', content, rationale, baseline_score: 0.5, candidate_score: 0.75, eval_total: 8, est_tokens: 512, created_at: 'invalid' },
      { id: 'p2', kind: 'memory', status: 'applied', content: 'short', rationale: '', est_tokens: 3, created_at: 'invalid' },
    ] },
    configureTextInput: () => {}, uiFormat: (v) => String(v), pct: (r) => Math.round(r * 100) + '%', tr: (s) => s,
    fuzzyMatch: () => [1, []], relTime: (t) => t,
    decideProposal: (...a) => calls.push(['decide', ...a]), act: (...a) => calls.push(['act', ...a]),
  });
  vm.runInContext('const $ = (id) => document.getElementById(id);\n' + fn('h') + '\n' + fn('renderProposals') + '\nrenderProposals();', c);
  const cards = ids.proposals.children;
  assert.equal(cards.length, 2);
  const [head, body, why, actions] = cards[0].children;
  assert.equal(cards[0].className, 'proposal');
  assert.equal(head.className, 'proposal-head');
  assert.equal(body.className, 'content');
  assert.equal(body.textContent, content);
  assert.equal(why.tagName, 'P');
  assert.equal(why.textContent, rationale);
  assert.deepEqual(actions.children.map((b) => b.textContent), ['Apply', 'Reject']);
  assert.deepEqual(cards[1].children.filter(Boolean).at(-1).children.map((b) => b.textContent), ['Roll back']);
  assert.deepEqual(calls, []);
});
