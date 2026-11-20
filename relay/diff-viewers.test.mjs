import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  function renderDiffLines(');
assert.ok(start >= 0);
const context = vm.createContext({
  h: (tag, props, text) => ({ tag, props, text }),
});
vm.runInContext(source.slice(start, source.indexOf('\n  }', start) + 4), context);
const render = (diff, tag) => Array.from(context.renderDiffLines(diff, tag));
for (const tag of ['div', 'span']) {
  test(tag + ': file/hunk headers are metadata, actual changes retain their colors', () => {
    const lines = ['--- a/main.rs', '+++ b/main.rs', '@@ -1,2 +1,3 @@', ' unchanged',
      '-old', '+new', '+extra', '\\ No newline at end of file'];
    const nodes = render(lines.join('\n'), tag);
    assert.deepEqual(nodes.map(n => n.props.class),
      ['muted', 'muted', 'muted', null, 'del', 'add', 'add', null]);
    assert.deepEqual(nodes.map(n => n.text), lines.map(l => l + (tag === 'span' ? '\n' : '')));
    assert.ok(nodes.every(n => n.tag === tag));
  });
  test(tag + ': header-looking hunk content stays a real change across files', () => {
    const lines = ['--- a/one', '+++ b/one', '@@ -1 +1 @@', '--- removed text',
      '+++ added text', '--- a/two', '+++ b/two', '@@ -0,0 +1 @@', '+++i;'];
    assert.deepEqual(render(lines.join('\n'), tag).map(n => n.props.class),
      ['muted', 'muted', 'muted', 'del', 'add', 'muted', 'muted', 'muted', 'add']);
  });
  test(tag + ': deletion-only hunks and no-newline markers preserve counts', () => {
    const lines = ['@@ -1 +0,0 @@', '-gone', '\\ No newline at end of file',
      '--- a/next', '+++ b/next', '@@ -1 +1 @@', '-old',
      '\\ No newline at end of file', '+new'];
    assert.deepEqual(render(lines.join('\n'), tag).map(n => n.props.class),
      ['muted', 'del', null, 'muted', 'muted', 'muted', 'del', null, 'add']);
  });
  test(tag + ': text remains literal and headerless diffs retain changes', () => {
    const lines = ['+<script>not executable</script>', '-old', ' context', ''];
    assert.deepEqual(render(lines.join('\n'), tag).map(n => n.text),
      lines.map(l => l + (tag === 'span' ? '\n' : '')));
    assert.deepEqual(render(lines.join('\n'), tag).map(n => n.props.class),
      ['add', 'del', null, null]);
  });
}
test('both shipped diff viewers use the renderer without changing fenced highlights', () => {
  assert.match(source, /pre\.replaceChildren\(\.\.\.renderDiffLines\(r\.diff, "div"\)\)/);
  assert.match(source, /h\("pre", \{ class: "diff" \}, renderDiffLines\(f\.diff, "span"\)\)/);
});
test('run-file dialog uses metadata styling while preserving literal diff text', async () => {
  const begin = source.indexOf('  async function showRunFiles(');
  const diff = '--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new';
  let dialog;
  const c = vm.createContext({
    document: { activeElement: {} }, S: { current: 'session' },
    act: async () => ({ run_id: 'run', files: [{ path: 'file', status: 'modified',
      revertible: false, diff }] }),
    h: (tag, props, ...children) => ({ tag, props, children: children.flat(Infinity).filter(x => x != null) }),
    openDialog: node => { dialog = node; }, closeDialog() {}, note() {},
  });
  vm.runInContext(source.slice(start, source.indexOf('\n  }', start) + 4) + '\n' +
    source.slice(begin, source.indexOf('\n  }', begin) + 4), c);
  await c.showRunFiles();
  const all = [];
  const visit = n => { if (typeof n === 'object') { all.push(n); n.children?.forEach(visit); } };
  visit(dialog);
  const pre = all.find(n => n.tag === 'pre');
  assert.deepEqual(Array.from(pre.children, n => n.props.class), ['muted', 'muted', 'muted', 'del', 'add']);
  assert.equal(pre.children.map(n => n.children.join('')).join(''), diff + '\n');
});
