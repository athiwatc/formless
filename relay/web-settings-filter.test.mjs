// #1731: the Settings filter's visibility and highlights fold the same way,
// and repeated filtering leaves the label text whole. Synthetic labels only;
// a minimal DOM stands in for the browser (no Settings values or controls).
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const fn = name => {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
};

class Node {
  constructor() { this.parentNode = null; this.childNodes = []; }
  replaceWith(...nodes) {
    const p = this.parentNode, list = nodes.map(n => n instanceof Node ? n : new Text(String(n)));
    for (const n of list) n.parentNode = p;
    p.childNodes.splice(p.childNodes.indexOf(this), 1, ...list);
    this.parentNode = null;
  }
}
class Text extends Node {
  constructor(data) { super(); this.nodeType = 3; this.data = data; }
  get textContent() { return this.data; }
}
class Element extends Node {
  constructor(tag) { super(); this.nodeType = 1; this.tagName = tag.toUpperCase(); this.className = ''; this.title = ''; this.listeners = []; this.style = {}; }
  get classList() {
    const names = () => this.className.split(/\s+/).filter(Boolean);
    return {
      contains: c => names().includes(c),
      toggle: (c, on) => { const s = new Set(names()); (on ?? !s.has(c)) ? s.add(c) : s.delete(c); this.className = [...s].join(' '); },
    };
  }
  setAttribute(k, v) { if (k === 'class') this.className = v; else this[k] = v; }
  getAttribute(k) { return this[k] ?? null; }
  addEventListener(type, f) { this.listeners.push([type, f]); }
  dispatch(type) { for (const [t, f] of this.listeners) if (t === type) f({ type, target: this }); }
  append(...kids) { for (const k of kids) { const n = k instanceof Node ? k : new Text(String(k)); n.parentNode = this; this.childNodes.push(n); } }
  get textContent() { return this.childNodes.map(n => n.textContent).join(''); }
  get nextElementSibling() { const s = this.parentNode.childNodes; return s.slice(s.indexOf(this) + 1).find(n => n.nodeType === 1) ?? null; }
  normalize() {
    const out = [];
    for (const n of this.childNodes) {
      if (n.nodeType === 3) {
        const last = out.at(-1);
        if (!n.data || last?.nodeType === 3) { if (n.data) last.data += n.data; n.parentNode = null; continue; }
      } else n.normalize();
      out.push(n);
    }
    this.childNodes = out;
  }
  matches(selector) {
    const simple = (el, s) => { const [tag, cls] = s.split('.'); return el?.nodeType === 1 && (!tag || el.tagName === tag.toUpperCase()) && (!cls || el.classList.contains(cls)); };
    return selector.split(',').some(part => {
      const steps = part.trim().split(/\s*>\s*/);
      let el = this;
      for (let i = steps.length - 1; i >= 0; i--, el = el?.parentNode) if (!simple(el, steps[i])) return false;
      return true;
    });
  }
  querySelectorAll(selector) {
    const out = [], walk = n => { for (const k of n.childNodes) if (k.nodeType === 1) { if (k.matches(selector)) out.push(k); walk(k); } };
    walk(this);
    return out;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null; }
}
const el = (tag, props, ...kids) => { const n = new Element(tag); Object.assign(n, props); n.append(...kids); return n; };
const texts = root => { const out = []; const walk = n => { for (const k of n.childNodes) k.nodeType === 3 ? out.push(k) : walk(k); }; walk(root); return out; };

function fixture() {
  let clicks = 0;
  const button = el('button', {}, 'Edit');
  button.addEventListener('click', () => clicks++);
  const model = el('dt', { title: 'provider.model' }, 'Default model');
  const modelValue = el('dd', {}, el('code', {}, 'fixture-model'), ' ', button);
  const root = el('div', {},
    el('section', {}, el('h3', {}, 'Provider'), el('dl', { className: 'kv' },
      model, modelValue,
      el('dt', { title: 'provider.max_retries' }, 'Retries'), el('dd', {}, '3'))),
    el('section', {}, el('h3', {}, 'Budget'), el('dl', { className: 'kv' },
      el('dt', { title: 'limits.daily_usd' }, 'Daily limit'), el('dd', {}, 'USD 5'))),
    el('section', {}, el('h3', {}, 'Places'), el('dl', { className: 'kv' },
      el('dt', {}, 'Caf\u00e9 NFC'), el('dd', {}, 'one'),
      el('dt', {}, 'Cafe\u0301 NFD'), el('dd', {}, 'two'),
      el('dt', {}, '\u0130stanbul office'), el('dd', {}, 'three'),
      el('dt', {}, '\u039f\u0394\u039f\u03a3 label'), el('dd', {}, 'four'))),
    el('section', {}, el('h3', {}, 'Memory'), el('table', {}, el('tbody', {},
      el('tr', {}, el('td', {}, 'Caf\u00e9 latte'), el('td', {}, 'kept')),
      el('tr', {}, el('td', {}, 'Re\u0301sume\u0301 path'), el('td', {}, 'nfd')),
      el('tr', {}, el('td', {}, 'Tea'), el('td', {}, 'other'))))),
    el('section', {}, el('h3', {}, 'Notes'), el('p', {}, 'Nothing about rows')));
  const input = el('input', { value: '' });
  const ids = { settings: root, 'settings-filter': input };
  const c = vm.createContext({ Node, $: id => ids[id], document: { createElement: tag => new Element(tag) }, uiFormat: String });
  vm.runInContext(['h', 'configureTextInput', 'foldMap', 'markWords', 'filterSettings'].map(fn).join('\n'), c);
  const original = root.textContent;
  const filter = q => {
    input.value = q;
    c.filterSettings();
    // Invariants after every keystroke: no empty or adjacent text nodes,
    // marks only directly inside dt labels, text content unchanged.
    for (const t of texts(root)) {
      assert.ok(t.data.length, `empty text node after ${JSON.stringify(q)}`);
      const sib = t.parentNode.childNodes;
      assert.notEqual(sib[sib.indexOf(t) + 1]?.nodeType, 3, `adjacent text nodes after ${JSON.stringify(q)}`);
    }
    for (const m of root.querySelectorAll('mark.find')) assert.equal(m.parentNode.tagName, 'DT');
    assert.equal(root.textContent, original);
  };
  const dts = () => root.querySelectorAll('dl.kv > dt');
  const dt = label => dts().find(d => d.textContent === label);
  const marks = node => node.querySelectorAll('mark.find').map(m => m.textContent);
  const visible = () => [...dts(), ...root.querySelectorAll('tbody > tr')].filter(n => !n.classList.contains('hidden')).map(n => n.textContent);
  const sections = () => root.querySelectorAll('section').filter(s => !s.classList.contains('hidden')).map(s => s.querySelector('h3').textContent);
  return { root, model, modelValue, button, clicks: () => clicks, filter, dt, marks, visible, sections, original };
}

test('typing and deleting a word keeps its highlight at every step, with whole text nodes', () => {
  const f = fixture();
  const steps = ['m', 'mo', 'mod', 'mode', 'model'];
  for (const q of [...steps, ...steps.slice(0, -1).reverse()]) {
    f.filter(q);
    assert.deepEqual(f.marks(f.model), [q], q);
    assert.deepEqual(f.model.childNodes.map(n => n.textContent), ['Default ', q, 'model'.slice(q.length)].filter(Boolean), q);
  }
  // Re-applying the same query is idempotent; clearing restores one text node.
  f.filter('model');
  const once = f.model.childNodes.map(n => [n.nodeType, n.textContent]);
  f.filter('model');
  assert.deepEqual(f.model.childNodes.map(n => [n.nodeType, n.textContent]), once);
  // Extending past the old mark's end and starting inside it both match.
  f.filter('odel');
  assert.deepEqual(f.marks(f.model), ['odel']);
  f.filter('');
  assert.equal(f.root.querySelectorAll('mark.find').length, 0);
  assert.deepEqual(f.model.childNodes.map(n => n.textContent), ['Default model']);
  assert.equal(f.visible().length, 10);
  assert.deepEqual(f.sections(), ['Provider', 'Budget', 'Places', 'Memory', 'Notes']);
});

test('row visibility folds accents, NFC/NFD and İ exactly as the highlights do', () => {
  const f = fixture();
  for (const q of ['cafe', 'CAF\u00c9', 'cafe\u0301']) {
    f.filter(q);
    assert.deepEqual(f.visible(), ['Caf\u00e9 NFC', 'Cafe\u0301 NFD', 'Caf\u00e9 lattekept'], q);
    assert.deepEqual(f.marks(f.dt('Caf\u00e9 NFC')), ['Caf\u00e9'], q);
    assert.deepEqual(f.marks(f.dt('Cafe\u0301 NFD')), ['Cafe\u0301'], q);
    assert.deepEqual(f.sections(), ['Places', 'Memory'], q);
  }
  f.filter('r\u00e9sum\u00e9');
  assert.deepEqual(f.visible(), ['Re\u0301sume\u0301 pathnfd']);
  for (const q of ['istanbul', '\u0130STANBUL', 'I\u0307stanbul office']) {
    f.filter(q);
    assert.deepEqual(f.visible(), ['\u0130stanbul office'], q);
    assert.deepEqual(f.dt('\u0130stanbul office').childNodes.map(n => [n.nodeType, n.textContent]),
      q.includes('office') ? [[1, '\u0130stanbul'], [3, ' '], [1, 'office']] : [[1, '\u0130stanbul'], [3, ' office']], q);
  }
  // A dt shown by its own label always carries a highlight.
  f.filter('caf');
  for (const label of ['Caf\u00e9 NFC', 'Cafe\u0301 NFD']) assert.equal(f.marks(f.dt(label)).length, 1);
});

test('all words must match within one target; section titles reveal whole sections', () => {
  const f = fixture();
  f.filter('daily limit');
  assert.deepEqual(f.visible(), ['Daily limit']);
  assert.deepEqual(f.marks(f.dt('Daily limit')), ['Daily', 'limit']);
  f.filter('daily usd'); // both in the key (dt.title) only
  assert.deepEqual(f.visible(), ['Daily limit']);
  f.filter('limit 5'); // label has "limit", value has "5": never combined
  assert.deepEqual(f.visible(), []);
  assert.deepEqual(f.sections(), []);
  f.filter('model edit'); // both in the value
  assert.deepEqual(f.visible(), ['Default model']);
  f.filter('default edit'); // label + value: no
  assert.deepEqual(f.visible(), []);
  f.filter('latte kept'); // one table row
  assert.deepEqual(f.visible(), ['Caf\u00e9 lattekept']);
  f.filter('PROVIDER');
  assert.deepEqual(f.sections(), ['Provider']);
  assert.deepEqual(f.visible(), ['Default model', 'Retries']);
  f.filter('memory');
  assert.deepEqual(f.visible(), ['Caf\u00e9 lattekept', 'Re\u0301sume\u0301 pathnfd', 'Teaother']);
  f.filter('rows'); // a section without rows matches on its own text
  assert.deepEqual(f.sections(), ['Notes']);
});

test('table cells are never highlighted, and elements and listeners survive filtering', () => {
  const f = fixture();
  const elements = f.root.querySelectorAll('*').filter(n => n.tagName !== 'MARK');
  for (const q of ['latte', 'caf', 'model', 'mo', 'fixture', '\u0130', '']) {
    f.filter(q);
    assert.equal(f.root.querySelector('table').querySelectorAll('mark.find').length, 0, q);
  }
  f.filter('model');
  assert.deepEqual(f.root.querySelectorAll('*').filter(n => n.tagName !== 'MARK'), elements);
  assert.equal(f.model.nextElementSibling, f.modelValue);
  assert.equal(f.modelValue.childNodes.at(-1), f.button);
  assert.deepEqual(f.marks(f.modelValue), []);
  f.button.dispatch('click');
  f.filter('');
  f.button.dispatch('click');
  assert.equal(f.clicks(), 2);
});
