import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  const W = ');
const end = source.indexOf('\n  function javascriptNumberEnd(', start);
assert.ok(start >= 0 && end > start);
const context = vm.createContext({ h: (tag, props, text) => ({ tag, props, text }) });
vm.runInContext(source.slice(start, end), context);

function tokens(code, lang) {
  const nodes = Array.from(context.highlight(code, lang));
  assert.equal(nodes.map(n => typeof n === 'string' ? n : n.text).join(''), code);
  return nodes.filter(n => typeof n !== 'string').map(n => [n.props.class, n.text]);
}

test('MySQL hash comments consume block markers and do not swallow following SQL', () => {
  assert.deepEqual(tokens('SELECT 1;# keep /* false\nSELECT id FROM t;', 'mysql'), [
    ['tok-kw', 'SELECT'], ['tok-num', '1'], ['tok-com', '# keep /* false'], ['tok-kw', 'SELECT'], ['tok-kw', 'FROM'],
  ]);
});

test('MySQL double dash requires whitespace, a control character, or end of input', () => {
  for (const suffix of ['', ' comment', '\tcomment', '\nSELECT', '\vcomment']) {
    const result = tokens('SELECT 1--' + suffix, 'mysql');
    assert.equal(result.find(t => t[0] === 'tok-com')?.[1], '--' + suffix.split('\n')[0]);
  }
  assert.deepEqual(tokens('SELECT 1--2 AS n;', 'mysql'), [
    ['tok-kw', 'SELECT'], ['tok-num', '1'], ['tok-num', '2'], ['tok-kw', 'AS'],
  ]);
});

test('MySQL quoted markers stay strings and block comments still work', () => {
  for (const quoted of ["'# -- /*'", '"# -- /*"', '`# -- /*`', '"first\n# continued"']) {
    assert.deepEqual(tokens('SELECT ' + quoted + ' # actual', 'mysql'), [
      ['tok-kw', 'SELECT'], ['tok-str', quoted], ['tok-com', '# actual'],
    ]);
  }
  assert.deepEqual(tokens('`a\\`# comment', 'mysql'), [
    ['tok-str', '`a\\`'], ['tok-com', '# comment'],
  ]);
  assert.deepEqual(tokens('/* open\n# -- */ SELECT', 'mysql'), [
    ['tok-com', '/* open\n# -- */'], ['tok-kw', 'SELECT'],
  ]);
});

test('generic SQL and other aliases retain their existing comment rules', () => {
  for (const lang of ['sql', 'sqlite', 'postgres', 'postgresql']) {
    assert.deepEqual(tokens('SELECT 1--2 AS n;', lang), [
      ['tok-kw', 'SELECT'], ['tok-num', '1'], ['tok-com', '--2 AS n;'],
    ]);
    assert.deepEqual(tokens('# drop table', lang), [['tok-kw', 'drop'], ['tok-kw', 'table']]);
  }
});
