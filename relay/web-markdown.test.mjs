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
    ['- a\n\nParagraph',['ul','p']],['- a\n\n- - -\n\n- b',['ul','hr','ul']],['* a\n\n* * *\n\n* b',['ul','hr','ul']],['1. a\n\n## Heading',['ol','h4']],
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

test('table recognition shares logical header and delimiter cell counts with lookahead', () => {
  const c=fixture();
  for(const source of ['A | B\n---','| a | b |\n|---|','a | b\n--- | --- | ---','a | b\n---\\|---']) {
    c.caseLines=source.split('\n');
    assert.equal(vm.runInContext('tableStart(caseLines,0)',c),false,source);
    assert.equal(vm.runInContext('startsBlock(caseLines,0)',c),false,source);
    assert.equal(all(c.markdown(source)).some(n=>n.tag==='table'),false,source);
    const following=c.markdown('Leading paragraph\n'+source);
    assert.equal(all(following).some(n=>n.tag==='table'),false,source);
    assert.ok(plain(following.children[0]).includes('Leading paragraph'));
    assert.ok(plain(following.children[0]).includes(source.split('\n')[0]),'lookahead must not split at a rejected table');
  }
});

test('valid tables keep edge pipes, escaped header pipes, alignment and body padding', () => {
  const c=fixture();
  for(const [header,delimiter] of [
    ['a | b','--- | :---:'],['| a | b |','|:---|---:|'],
    ['a\\|b | c','--- | ---'],['| a\\|b | c |','|---|---|'],
  ]) {
    c.caseLines=[header,delimiter];
    assert.equal(vm.runInContext('tableStart(caseLines,0)',c),true);
    assert.equal(vm.runInContext('startsBlock(caseLines,0)',c),true);
    const root=c.markdown(header+'\n'+delimiter+'\none |\nleft | right | extra');
    const table=all(root).find(n=>n.tag==='table');assert.ok(table);
    const rows=all(table).filter(n=>n.tag==='tbody')[0].children;
    assert.equal(rows[0].children.length,2);assert.equal(plain(rows[0].children[1]),'');
    assert.equal(rows[1].children.length,2);assert.equal(plain(rows[1]),'leftright');
  }
});

test('streamed table prefixes use the same predicate and completed block shapes', () => {
  const c=fixture();
  for(const source of ['A | B\n---','A | B\n--- | ---\na | b','a\\|b | c\n--- | ---']) {
    for(let end=0;end<=source.length;end++) {
      const prefix=source.slice(0,end);c.caseLines=prefix.split('\n');
      const recognized=vm.runInContext('tableStart(caseLines,0)',c);
      assert.equal(all(c.markdown(prefix)).some(n=>n.tag==='table'),recognized,prefix);
    }
    const text='Earlier paragraph\n\n'+source+'\n\nFinal paragraph';
    const [stable,tail]=c.splitStreaming(text);
    assert.deepEqual([...c.markdown(stable).children,...c.markdown(tail).children].map(shape),c.markdown(text).children.map(shape));
  }
});

test('Setext paragraphs share logical levels with ATX and retain multiline inline content', () => {
  const c=fixture();
  for(const [underline,atx] of [['===','#'],['---','##'],['=','#'],['-','##']]) {
    const title='ไทย **Title** [link](https://example.invalid)';
    assert.deepEqual(shape(c.markdown(title+'\n'+underline).children[0]),shape(c.markdown(atx+' '+title).children[0]));
    const heading=c.markdown('First line\n'+title+'\n  '+underline+'  ').children[0];
    assert.equal(heading.tag,c.markdown(atx+' title').children[0].tag);
    assert.equal(plain(heading),'First line'+title);
    assert.ok(heading.children.some(child=>child.tag==='br'));
  }
});

test('Setext precedence preserves standalone rules, fences, tables and nested block handling', () => {
  const c=fixture();
  assert.deepEqual(c.markdown('Title\n\n---').children.map(n=>n.tag),['p','hr']);
  assert.deepEqual(c.markdown('---').children.map(n=>n.tag),['hr']);
  assert.deepEqual(c.markdown('```text\nTitle\n===\n```').children.map(n=>n.tag),['pre']);
  assert.equal(plain(c.markdown('```text\nTitle\n---\n```')),'Title\n---');
  assert.equal(all(c.markdown('A | B\n--- | ---')).some(n=>n.tag==='table'),true);
  const rejected=c.markdown('A | B\n---');
  assert.equal(rejected.children[0].tag,c.markdown('## title').children[0].tag);
  assert.equal(plain(rejected),'A | B');
  assert.equal(c.markdown('Title\n    ===').children[0].tag,'p','four-space underline is not Setext');
  const quote=c.markdown('> Quoted title\n> ===').children[0];
  assert.equal(quote.tag,'blockquote');assert.equal(quote.children[0].tag,c.markdown('# title').children[0].tag);
  const list=c.markdown('- item\n  title\n  ===').children[0];
  assert.equal(list.tag,'ul');assert.ok(all(list).some(n=>n.tag===c.markdown('# title').children[0].tag));
});

test('Setext incremental tails settle to the same completed blocks', () => {
  const c=fixture();
  for(const text of ['Title\n===','First line\nSecond line\n---','A | B\n---','A | B\n--- | ---\nx | y']) {
    for(let end=0;end<=text.length;end++) {
      const prefix='Earlier paragraph\n\n'+text.slice(0,end);
      const [stable,tail]=c.splitStreaming(prefix);
      assert.deepEqual([...c.markdown(stable).children,...c.markdown(tail).children].map(shape),c.markdown(prefix).children.map(shape),prefix);
    }
  }
});

test('all six Markdown heading depths remain distinct below the Session title', () => {
  const c=fixture();
  const headings=c.markdown(Array.from({length:6},(_,i)=>'#'.repeat(i+1)+' ไทย **depth '+(i+1)+'**').join('\n\n')).children;
  assert.equal(headings.length,6);
  for(const [index,heading] of headings.entries()) {
    assert.equal(heading.attrs.role,'heading');
    assert.equal(heading.attrs['aria-level'],index+3);
    assert.equal(heading.attrs['data-heading-level'],index+1);
    assert.ok(heading.attrs.class.includes('md-heading-'+(index+1)));
    assert.equal(plain(heading),'ไทย **depth '+(index+1)+'**');
  }
  assert.equal(new Set(headings.map(h=>h.attrs['aria-level'])).size,6);
});
