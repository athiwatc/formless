import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  const W = (s) =>');
const end = source.indexOf('\n  function javascriptNumberEnd(', start);
assert.ok(start >= 0 && end > start);
const context = vm.createContext({ h: (tag, props, text) => ({ cls: props.class, text }) });
vm.runInContext(source.slice(start, end), context);

function tokens(lang, text) {
  const out = context.highlight(text, lang);
  assert.equal(out.map(x => typeof x === 'string' ? x : x.text).join(''), text);
  return Array.from(out.filter(x => typeof x !== 'string'), x => [x.cls, x.text]);
}

test('PHP hash comments stop before the next line even with block openers', () => {
  for (const lang of ['php', 'PHP']) {
    for (const first of ['# note: /* not a block', 'foo();# /* note']) {
      const out = tokens(lang, first + '\nreturn "hi";\nclass User {}');
      assert.deepEqual(out.filter(x => x[0] === 'tok-com'), [['tok-com', first.slice(first.indexOf('#'))]]);
      assert.deepEqual(out.filter(x => x[0] === 'tok-kw'), [['tok-kw', 'return'], ['tok-kw', 'class']]);
    }
  }
});

test('PHP attributes and quoted hashes remain code and strings', () => {
  const out = tokens('php', '#[Route("/users")]\nfunction route() {}\n"# not a comment"');
  assert.deepEqual(out.filter(x => x[0] === 'tok-com'), []);
  assert.ok(out.some(x => x[0] === 'tok-str' && x[1] === '"# not a comment"'));
  assert.deepEqual(tokens('php', '// return'), [['tok-com', '// return']]);
  assert.deepEqual(tokens('php', '/* block\n# still block */'), [['tok-com', '/* block\n# still block */']]);
});

test('PHP line comments end before a closing tag', () => {
  for (const opener of ['#', '//']) {
    const source = '<?php foo();' + opener + ' note ?> <p>outside</p>';
    assert.deepEqual(tokens('php', source).filter(x => x[0] === 'tok-com'), [
      ['tok-com', opener + ' note '],
    ]);
  }
});

test('other languages retain their hash boundaries and comments', () => {
  for (const lang of ['c', 'cpp', 'java', 'rust']) {
    assert.deepEqual(tokens(lang, '#include <stdio.h>\n#define VALUE 1').filter(x => x[0] === 'tok-com'), []);
  }
  assert.deepEqual(tokens('python', 'x=1# note').filter(x => x[0] === 'tok-com'), [['tok-com', '# note']]);
  for (const lang of ['yaml', 'env', 'sh']) {
    assert.deepEqual(tokens(lang, 'key: foo#bar').filter(x => x[0] === 'tok-com'), []);
    assert.deepEqual(tokens(lang, 'value # note').filter(x => x[0] === 'tok-com'), [['tok-com', '# note']]);
  }
});
