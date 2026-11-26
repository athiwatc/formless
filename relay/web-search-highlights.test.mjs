import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  function renderSearch(');
const end = source.indexOf('\n  // ---', start);
const fold = source.indexOf('  function foldMap(');
const foldEnd = source.indexOf('\n  }', fold) + 4;
const searchStart = source.indexOf('  let searchSeq = 0;');
const searchEnd = source.indexOf('  function autosize()', searchStart);
assert.ok(start >= 0 && end > start && fold >= 0 && foldEnd > fold);

function renderResults(hits, query, searchedLatest) {
  const h = (tag, props, ...children) => ({
    tag, props, children,
    append(...parts) { this.children.push(...parts); },
    replaceChildren() { this.children.length = 0; },
    classList: { add() {}, remove() {} },
  });
  const results = h('div');
  const c = vm.createContext({ h, $: () => results });
  vm.runInContext(source.slice(start, end) + source.slice(fold, foldEnd), c);
  c.renderSearch(hits, query, searchedLatest);
  return results;
}

function renderSnippet(text, query) {
  const results = renderResults([{ session_name: 'Fixture', seq: 1, role: 'user', snippet: text }], query);
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
  assert.deepEqual(renderSnippet('…staging only', 'stag'), ['stag']);
});

test('empty bounded results still show the searched message limit', () => {
  const results = renderResults([], '??', 10000);
  assert.equal(results.children[1].children[0], 'Searched latest 10000 messages.');
  assert.equal(renderResults([], 'hello').children.length, 1);
});

test('search falls back only for an older daemon rejecting the scoped request', async () => {
  const calls = [];
  const rendered = [];
  const errors = [];
  let code = 'unsupported';
  const c = vm.createContext({
    request: async (req) => {
      calls.push(req.type);
      throw Object.assign(new Error(code), { code, daemonResponse: true });
    },
    act: async (req) => { calls.push(req.type); return []; },
    renderSearch: (...args) => rendered.push(args),
    toast: (error) => errors.push(error),
    note: () => {},
  });
  vm.runInContext(source.slice(searchStart, searchEnd), c);
  await c.search('??');
  assert.deepEqual(calls, ['search_sessions_with_scope', 'search_sessions']);
  assert.deepEqual(rendered, [[[], '??']]);
  assert.deepEqual(errors, []);
  code = 'bad_request';
  await c.search('hello');
  assert.deepEqual(calls.slice(2), ['search_sessions_with_scope']);
  assert.deepEqual(errors, ['bad_request']);
});

test('search retains original accent and case in NFC and decomposed snippets', () => {
  assert.deepEqual(renderSnippet('café cafe', 'cafe'), ['café']);
  assert.deepEqual(renderSnippet('cafe\u0301 cafe', 'cafe'), ['cafe\u0301']);
  assert.deepEqual(renderSnippet('🚀 İ was in İstanbul; later in Zürich', 'istanbul zurich'), ['İstanbul', 'Zürich']);
  assert.deepEqual(renderSnippet('각, 가', '가'), ['가']);
  assert.deepEqual(renderSnippet('αγάπη αγαπη', 'αγαπη'), ['αγαπη']);
  assert.deepEqual(renderSnippet('ΚΌΣΜΟΣ', 'κόσμος'), ['ΚΌΣΜΟΣ']);
  assert.deepEqual(renderSnippet('foo\u034fbar', 'bar'), ['bar']);
  assert.deepEqual(renderSnippet('foo\u0301bar', 'bar'), []);
});

test('punctuation-only queries retain literal matching and plain-text markup', () => {
  const text = '🚀 <b> ::: </b> :::';
  assert.deepEqual(renderSnippet(text, ' ::: '), [':::']);
  assert.deepEqual(renderSnippet(text, '>>>'), []);
  assert.deepEqual(renderSnippet(text, '   '), []);
});
