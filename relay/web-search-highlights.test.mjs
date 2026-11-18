import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  function renderSearch(');
const end = source.indexOf('\n  // ---', start);
const fold = source.indexOf('  function foldMap(');
const foldEnd = source.indexOf('\n  }', fold) + 4;
assert.ok(start >= 0 && end > start && fold >= 0 && foldEnd > fold);

function renderSnippet(text, query) {
  const h = (tag, props, ...children) => ({
    tag, props, children,
    append(...parts) { this.children.push(...parts); },
    replaceChildren() { this.children.length = 0; },
    classList: { add() {}, remove() {} },
  });
  const results = h('div');
  const c = vm.createContext({ h, $: () => results });
  vm.runInContext(source.slice(start, end) + source.slice(fold, foldEnd), c);
  c.renderSearch([{ session_name: 'Fixture', seq: 1, role: 'user', snippet: text }], query);
  const snippet = results.children[1].children[1];
  const segments = snippet.children;
  const marks = segments.filter(part => typeof part !== 'string');
  assert.ok(marks.every(mark => mark.tag === 'mark' && typeof mark.children[0] === 'string'));
  assert.equal(segments.map(part => typeof part === 'string' ? part : part.children[0]).join(''), text);
  return marks.map(mark => mark.children[0]);
}

test('search marks present term prefixes in snippet order, once per term', () => {
  assert.deepEqual(renderSnippet('upstaging, staging was deployed; staging again', 'deploy stag'), ['stag', 'deploy']);
  assert.deepEqual(renderSnippet('staging was completed', 'deploy stag'), ['stag']);
  assert.deepEqual(renderSnippet('app apple Apple', 'app apple APP'), ['app', 'apple']);
  assert.deepEqual(renderSnippet('Apple later Apple', 'app apple'), ['Apple']);
  assert.deepEqual(renderSnippet('upstaging only', 'stag deploy'), []);
  assert.deepEqual(renderSnippet('…staging only', 'stag'), []);
  assert.deepEqual(renderSnippet('…staging but then staging', 'stag'), ['stag']);
  assert.deepEqual(renderSnippet('… staging with a known boundary', 'stag'), ['stag']);
});

test('search retains original accent and case in NFC and decomposed snippets', () => {
  assert.deepEqual(renderSnippet('café cafe', 'cafe'), ['café']);
  assert.deepEqual(renderSnippet('cafe\u0301 cafe', 'cafe'), ['cafe\u0301']);
  assert.deepEqual(renderSnippet('🚀 İ was in İstanbul; later in Zürich', 'istanbul zurich'), ['İstanbul', 'Zürich']);
  assert.deepEqual(renderSnippet('각, 가', '가'), ['가']);
});

test('punctuation-only queries retain literal matching and plain-text markup', () => {
  const text = '🚀 <b> ::: </b> :::';
  assert.deepEqual(renderSnippet(text, ' ::: '), [':::']);
  assert.deepEqual(renderSnippet(text, '>>>'), []);
  assert.deepEqual(renderSnippet(text, '   '), []);
});
