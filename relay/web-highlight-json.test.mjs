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
for (const lang of ['jsonc', 'json5']) {
  test(`${lang}: comments consume embedded strings/literals and block markers`, () => {
    assert.deepEqual(tokens('// keep "draft": true /*\n{"ok": false}', lang), [
      ['tok-com', '// keep "draft": true /*'], ['tok-str', '"ok"'], ['tok-lit', 'false'],
    ]);
    assert.deepEqual(tokens('/* "draft": true\n// keep */ null', lang), [
      ['tok-com', '/* "draft": true\n// keep */'], ['tok-lit', 'null'],
    ]);
    assert.deepEqual(tokens('/* outer /* inner */ true', lang), [
      ['tok-com', '/* outer /* inner */'], ['tok-lit', 'true'],
    ]);
    assert.deepEqual(tokens('/* unfinished\nfalse', lang), [['tok-com', '/* unfinished\nfalse']]);
  });
  test(`${lang}: quoted comment markers and escapes remain literal`, () => {
    const quoted = '"https://example.com/ /* \\" true"';
    assert.deepEqual(tokens(quoted + ' // false', lang), [['tok-str', quoted], ['tok-com', '// false']]);
  });
}
test('JSON5 accepts single-quoted strings without enabling them in JSONC or JSON', () => {
  const quoted = "'it\\'s // /* true'";
  assert.deepEqual(tokens(quoted + ' false', 'json5'), [['tok-str', quoted], ['tok-lit', 'false']]);
  for (const lang of ['json', 'jsonc']) assert.deepEqual(tokens("'formless'", lang), []);
});
test('JSON5: escaped line breaks keep comment markers inside strings', () => {
  for (const quote of ['"', "'"]) {
    const continuation = '\\' + '\n';
    const quoted = quote + 'first' + continuation + '// continued ' + continuation + '/* literal */' + quote;
    assert.deepEqual(tokens(quoted + ', ok: true // comment', 'json5'), [
      ['tok-str', quoted], ['tok-lit', 'true'], ['tok-com', '// comment'],
    ]);
  }
});
test('plain JSON remains comment-free and double-quoted strings stay intact', () => {
  const actual = tokens('// "draft": true\n/* false */ "// literal"', 'json');
  assert.deepEqual(actual, [['tok-str', '"draft"'], ['tok-lit', 'true'], ['tok-lit', 'false'], ['tok-str', '"// literal"']]);
});
