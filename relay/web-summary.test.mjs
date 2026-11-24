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
function accessibleText(node) {
  if (node == null || node.attrs?.['aria-hidden'] === 'true') return '';
  if (typeof node === 'string') return node;
  return (node.children || []).map(accessibleText).join('');
}
function fixture(fallback = false) {
  const c = vm.createContext({ Intl: fallback ? {} : Intl,
    h: (tag, attrs, ...children) => ({tag, attrs, children}),
    payload: text => ({text}), artifactRefs: () => [], summarizeArgs: () => 'arguments', uiFormat: String });
  const start = source.indexOf('  const summarySegments =');
  vm.runInContext(source.slice(start, source.indexOf('  function rateCell', start)), c);
  vm.runInContext(fn('recordedDuration'), c);
  vm.runInContext(fn('toolCallChip'), c);
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
test('stored outcome, not tool payload text, controls the historical result status', () => {
  const c = fixture();
  const cases = [
    ['{"ok":false}', '{"tool_call_id":"call-1","name":"read_file"}', false, 'read_file · done'],
    ['Error: sample content', '{"is_error":false}', false, 'Tool · done'],
    ['command exited with status 128', '{"is_error":true}', true, 'Tool · failed'],
    ['{"ok":false}', null, false, 'Tool · done'],
    ['error response read from a file', '{broken', false, 'Tool · done'],
    ['result', 'null', false, 'Tool · done'],
    ['result', '{"is_error":"true"}', false, 'Tool · done'],
  ];
  assert.match(source, /m\.role === "tool"[\s\S]{0,170}toolResultChip\(m\.content \|\| "", m\.meta\)/);
  for (const [text, meta, failed, label] of cases) {
    const chip = c.toolResultChip(text, meta);
    assert.equal(chip.attrs.class.endsWith(' failed'), failed, meta);
    const summary = chip.children[0];
    assert.equal(summary.tag, 'summary');
    assert.equal(summary.children[0].attrs['aria-label'], undefined);
    assert.equal(accessibleText(summary.children[0]), label);
    assert.ok(accessibleText(summary).startsWith(label));
    assert.equal(chip.children[1].text, text);
  }
});

test('historical tool result names include the stored outcome and duration', () => {
  const c = fixture();
  const chip = c.toolResultChip('output', '{"name":"read_file","is_error":true,"elapsed_ms":1234}');
  const summary = chip.children[0];
  const name = summary.children[0];
  assert.equal(summary.tag, 'summary');
  assert.equal(name.attrs['aria-label'], undefined);
  assert.equal(accessibleText(name), 'read_file · failed · duration 1.2s');
  assert.ok(accessibleText(summary).startsWith('read_file · failed · duration 1.2s'));
});

test('tool call summary exposes the tool name as content rather than a generic span label', () => {
  const c = fixture();
  const chip = c.toolCallChip({function: {name: 'read_file', arguments: '{}'}});
  const summary = chip.children[0];
  const name = summary.children[0];
  assert.equal(summary.tag, 'summary');
  assert.equal(name.attrs['aria-label'], undefined);
  assert.equal(accessibleText(name), 'read_file');
  assert.ok(accessibleText(summary).startsWith('read_file'));
});
test('approval expiration shows whole hours and remaining minutes without rounding up', () => {
  const now = Date.parse('2026-11-20T04:00:00Z');
  const c = vm.createContext({ Date: { parse: Date.parse, now: () => now }, uiFormat: String });
  vm.runInContext(fn('untilTime'), c);
  for (const [seconds, output] of [
    [-60, 'ended'], [0, 'ended'], [59, 'in less than 1m'], [60, 'in 1m'],
    [3599, 'in 59m'], [3600, 'in 1h'], [5340, 'in 1h 29m'], [5400, 'in 1h 30m'],
    [7199, 'in 1h 59m'], [9000, 'in 2h 30m'], [86400, 'in 24h'],
  ]) assert.equal(c.untilTime(new Date(now + seconds * 1000).toISOString()), output, seconds);
  assert.equal(c.untilTime('bad'), 'bad');
  assert.equal(c.untilTime(''), '');
});
