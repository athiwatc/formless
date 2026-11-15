import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.indexOf('  function ' + name + '(');
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
function fixture(fallback = false) {
  const c = vm.createContext({ Intl: fallback ? {} : Intl,
    h: (tag, attrs, ...children) => ({tag, attrs, children}),
    payload: text => ({text}), artifactRefs: () => [] });
  const start = source.indexOf('  const summarySegments =');
  vm.runInContext(source.slice(start, source.indexOf('  function rateCell', start)), c);
  vm.runInContext(fn('toolResultChip'), c);
  return c;
}
test('summary retains ASCII, normalized whitespace and fitting Unicode', () => {
  const c = fixture();
  assert.equal(c.truncate('  a\n b  ', 10), 'a b');
  assert.equal(c.truncate('abcdef', 5), 'abcd…');
  assert.equal(c.truncate('é👩🏽‍💻', 10), 'é👩🏽‍💻');
  assert.equal(c.truncate('x', 0), '');
});
test('summary never divides a grapheme at its UTF-16 budget', () => {
  const c = fixture();
  for (const grapheme of ['😀', 'e\u0301', '👍🏽', '👩🏽‍💻', '🇹🇭']) {
    const input = 'a'.repeat(158) + grapheme + 'x';
    const output = c.truncate(input, 160);
    assert.equal(output, 'a'.repeat(158) + '…', grapheme);
    assert.ok(output.length <= 160);
    assert.equal(c.truncate(input, 158 + grapheme.length + 1), input);
  }
});
test('compatibility fallback retains code points within the same budget', () => {
  const c = fixture(true);
  assert.equal(c.truncate('a'.repeat(158) + '😀x', 160), 'a'.repeat(158) + '…');
  assert.equal(c.truncate('abcdef', 5), 'abcd…');
});
test('logical lines follow str::lines for LF, CRLF and extra blank lines', () => {
  const c = fixture();
  for (const [text, expected] of [['',0],['a',1],['a\n',1],['a\nb',2],['a\nb\n',2],['\n',1],['a\n\n',2],['\n\n',2],['a\r',1]]) {
    assert.equal(c.logicalLineCount(text), expected, JSON.stringify(text));
    assert.equal(c.logicalLineCount(text.replaceAll('\n','\r\n')), expected);
  }
});
test('actual result chip omits one-line badges and preserves expanded text', () => {
  const c = fixture();
  for (const [text, badge] of [['',null],['a\n',null],['a\r\n',null],['a\nb\n','2 lines'],['a\n\n','2 lines']]) {
    const chip = c.toolResultChip(text);
    const stat = chip.children[0].children[2];
    assert.equal(stat?.children[0] ?? null, badge);
    assert.equal(chip.children[1].text, text);
  }
});
