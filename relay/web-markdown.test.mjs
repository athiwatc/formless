// Markdown block semantics from the shipped parser. Real DOM/stream identity
// and incremental layout run in web-streaming.browser-test.py.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.indexOf('  function '+name+'(');
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start)+4);
}
function node(tag, attrs, ...children) {
  const element = { tag, attrs: { ...attrs }, children: [],
    append(...items) { this.children.push(...items.flat(Infinity).filter(item => item != null)); },
    setAttribute(key, value) { this.attrs[key] = value; },
  };
  element.classList = { add(value) { element.attrs.class = [element.attrs.class, value].filter(Boolean).join(' '); } };
  element.append(...children);
  return element;
}
function fixture() {
  const c = vm.createContext({ h: node, inline: text => [text],
    codeBlock: text => node('pre', { class: 'codeblock' }, text),
    diagramBlock: text => node('pre', { class: 'diagram' }, text),
  });
  vm.runInContext(source.slice(source.indexOf('  const FENCE'), source.indexOf('  function splitRow')), c);
  vm.runInContext('const MD_DEPTH=32, MD_FORMAT_CHARS=128*1024;\n'+['splitRow','blocks','list','markdown','splitStreaming'].map(fn).join('\n'), c);
  return c;
}
const plain = n => typeof n === 'string' ? n : n.children.map(plain).join('');
const all = n => typeof n === 'string' ? [] : [n,...n.children.flatMap(all)];
const shape = n => typeof n === 'string' ? n : {tag:n.tag,attrs:n.attrs,children:n.children.map(shape)};

test('blank-separated siblings form one loose ordered or unordered list', () => {
  const c=fixture();
  for (const [text, tag, start, count] of [
    ['1. First step\n\n2. Second step\n\n3. Third step','ol',undefined,3],
    ['3. Third\n\n4. Fourth\n\n5. Fifth','ol','3',3],
    ['- a\n\n- b\n\n- c','ul',undefined,3],
    ['+ a\n\n\n* b\n\n- c','ul',undefined,3],
    ['1) a\n\n2) b','ol',undefined,2],
  ]) {
    const root=c.markdown(text), list=root.children[0];
    assert.equal(root.children.length,1,text);assert.equal(list.tag,tag,text);
    assert.equal(list.attrs.start,start,text);assert.equal(list.attrs.class,'loose',text);
    assert.equal(list.children.length,count,text);
    assert.ok(list.children.every(child=>child.tag==='li'));
  }
});

test('different list kinds or a following paragraph still terminate a list', () => {
  const c=fixture();
  for (const [text, tags] of [
    ['- a\n\n1. b',['ul','ol']],['1. a\n\n- b',['ol','ul']],
    ['- a\n\nParagraph',['ul','p']],['- a\n\n- - -\n\n- b',['ul','hr','ul']],['* a\n\n* * *\n\n* b',['ul','hr','ul']],['1. a\n\n## Heading',['ol','h3']],
  ]) assert.deepEqual(c.markdown(text).children.map(child=>child.tag),tags,text);
  const tight=c.markdown('1. a\n2. b').children[0];
  assert.equal(tight.attrs.class,undefined);assert.equal(tight.children.length,2);
});

test('nested loose lists and blank-separated item paragraphs retain their structure', () => {
  const c=fixture();
  const root=c.markdown('- parent\n\n  - child a\n\n  - child b\n\n- next');
  const lists=all(root).filter(n=>n.tag==='ul');
  assert.equal(lists.length,2);assert.equal(lists[0].children.length,2);assert.equal(lists[1].children.length,2);
  assert.equal(lists[0].attrs.class,'loose');assert.equal(lists[1].attrs.class,'loose');
  const paragraphs=c.markdown('- item\n\n  second paragraph\n\n- next').children[0];
  assert.equal(paragraphs.children.length,2);assert.equal(paragraphs.children[0].children.length,2);
  assert.equal(plain(paragraphs.children[0]),'itemsecond paragraph');
  assert.equal(plain(c.markdown('- lazy\ncontinuation\n\n- next').children[0].children[0]),'lazycontinuation');
});

test('stream boundaries never split open display math at internal blank lines', () => {
  const c=fixture(), before='Before\n\n';
  const cases=[
    '$$\na = 1\n\nb = 2\n$$',
    '$$ a = 1\n\nb = 2 $$',
    '$$\n```js\n\na = 1\n```\n\nb = 2\n$$',
    '$$x$$', '$$$$', '$$\na = 1\n\nb = 2 $$ trailing text',
  ];
  for(const body of cases) {
    const text=before+body+'\n\nAfter paragraph.\n\n';
    for(let at=before.length;at<before.length+body.length;at++) {
      const prefix=text.slice(0,at), [done]=c.splitStreaming(prefix);
      assert.ok(done===before||done==='',`open math committed early at${at}: ${JSON.stringify(body)}`);
    }
    const [done, tail]=c.splitStreaming(text);
    assert.equal(done,text);assert.equal(tail,'');
    const rendered=c.markdown(done);
    assert.equal(all(rendered).filter(n=>n.attrs?.class==='math block').length,1,text);
    assert.deepEqual(shape(rendered),shape(c.markdown(text)));
  }
  // Preserve the stored parser's existing inline closing/trailing-text rule.
  const trailing=c.markdown('$$\na = 1\n\nb = 2 $$ trailing text\n\nAfter.');
  assert.equal(plain(trailing.children[0]),'a = 1\n\nb = 2 $$ trailing text');
  assert.equal(trailing.children[1].tag,'p');assert.equal(plain(trailing.children[1]),'After.');
});

test('math and fence state follow parser precedence and closing-length rules', () => {
  const c=fixture(), before='Before\n\n';
  for(const body of ['$$\na\n\nb', '```js\n$$\n\ncontent', '~~~~\n$$\n\n~~~\n\nstill code']) {
    const text=before+body+'\n\n';
    assert.equal(c.splitStreaming(text)[0],before,body);
    assert.equal(c.splitStreaming(text)[1],body+'\n\n',body);
  }
  const fenced='````js\n$$\n\n```\n\nstill code\n````\n\nAfter\n\n';
  assert.equal(c.splitStreaming(fenced)[0],fenced);
  assert.equal(all(c.markdown(fenced)).filter(n=>n.attrs?.class==='math block').length,0);
  const languageLikeClose='```js\ncode\n```js\n\nnot closed';
  assert.equal(c.splitStreaming(languageLikeClose)[0],'');
});
