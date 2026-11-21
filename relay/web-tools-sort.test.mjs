// Tools-table header direction and filtered ordering from the shipped
// toolsTable / visibleTools. Glyph and aria-sort share one direction.
// This VM fixture does not cover browser layout or native button Enter activation.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const DASH = '—';

function fn(name) {
  const start = source.indexOf('  function ' + name + '(');
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
function constStatement(name) {
  const start = source.indexOf('  const ' + name + ' = ');
  assert.ok(start >= 0, name);
  const line = source.slice(start, source.indexOf('\n', start));
  assert.ok(line.endsWith(';'), line);
  return line;
}

const tools = [
  { id: 'beta', name: 'ベータ', current_version: 2, invocations: 10, success_rate: 0.5, updated_at: '2024-01-02T00:00:00Z', last_used_at: '2024-03-01T00:00:00.123+00:00', tags: ['計測'], description: '測定器', capabilities: ['fs:read', '計測'] },
  { id: 'ecl', name: 'éclair', current_version: 10, invocations: 3, success_rate: null, updated_at: '2024-03-01T00:00:00Z', tags: ['pastry'], description: 'dessert' },
  { id: 'zebra', name: 'ゼブラ', current_version: 1, invocations: 10, success_rate: 0.95, updated_at: '2023-12-01T00:00:00Z', last_used_at: '2024-03-01T00:00:00+00:00', tags: ['計測'], description: 'しまうま', needs_attention: true },
  { id: 'retired', name: '引退', current_version: 4, invocations: 1, success_rate: 0.2, updated_at: '2024-06-01T00:00:00Z', last_used_at: '2024-01-01T00:00:00+00:00', tags: ['旧'], description: '終了', retired: true },
  { id: 'zero', name: 'fennel', current_version: 3, invocations: 0, success_rate: 0, updated_at: '2022-05-05T00:00:00Z', last_used_at: null, tags: ['herb'], description: 'zero successes' },
  { id: 'alpha', name: 'alpha', current_version: 8, invocations: 7, success_rate: 0.8, updated_at: '2024-02-02T00:00:00Z', last_used_at: '2024-03-01T00:00:00+00:00', tags: ['latin'], description: 'first' },
];
const byId = new Map(tools.map((tool) => [tool.id, tool]));
const COLUMNS = [
  { key: 'name', label: 'Tool', title: 'Sort by tool', natural: 'ascending' },
  { key: 'version', label: 'Ver', title: 'Sort by ver', natural: 'ascending' },
  { key: 'calls', label: 'Calls', title: 'Sort by calls', natural: 'descending', numeric: true },
  { key: 'rate', label: 'Success', title: 'Sort by success', natural: 'descending', numeric: true },
  { key: 'used', label: 'Last used', title: 'Sort by last used', natural: 'descending' },
];
const VERSION_NATURAL = ['zebra', 'beta', 'zero', 'retired', 'alpha', 'ecl'];
const CALLS_NATURAL = ['beta', 'zebra', 'alpha', 'ecl', 'retired', 'zero'];
const RATE_NATURAL = ['zebra', 'alpha', 'beta', 'retired', 'zero', 'ecl'];
const USED_NATURAL = ['beta', 'alpha', 'zebra', 'retired', 'ecl', 'zero'];
const USED_REVERSE = ['retired', 'alpha', 'zebra', 'beta', 'ecl', 'zero'];
const ORDER = {
  version: VERSION_NATURAL,
  calls: CALLS_NATURAL,
  rate: RATE_NATURAL,
};

class Node {
  constructor(tag) {
    this.tagName = String(tag).toUpperCase();
    this.attrs = {};
    this.children = [];
    this.className = '';
    this.style = { cssText: '' };
    this.listeners = {};
    const node = this;
    this.classList = { contains(token) { return node.className.split(' ').includes(token); } };
  }
  setAttribute(key, value) { this.attrs[key] = String(value); }
  getAttribute(key) { return Object.prototype.hasOwnProperty.call(this.attrs, key) ? this.attrs[key] : null; }
  hasAttribute(key) { return this.getAttribute(key) !== null; }
  addEventListener(type, fn) {
    if (!this.listeners[type]) this.listeners[type] = [];
    this.listeners[type].push(fn);
  }
  append(...items) { this.children.push(...items); }
  replaceChildren(...items) { this.children = [...items]; }
  get textContent() {
    return this.children.map((child) => typeof child === 'string' ? child : child.textContent).join('');
  }
}

function fixture() {
  const controls = {
    'tools-filter': { value: '' },
    'tools-quick': { value: '' },
    'tools-sort': { value: 'name', options: ['name', 'rate', 'used', 'calls'].map((value) => ({ value })) },
    'tools-count': { textContent: '' },
    'tools-table': new Node('table'),
    'tool-detail': new Node('div'),
  };
  const context = vm.createContext({
    Node,
    document: { createElement: (tag) => new Node(tag) },
    S: { tools, modules: [], toolSort: { key: 'name', reverse: false }, selectedTool: null },
    $: (id) => controls[id],
    uiFormat: (value) => String(value),
    tr: text => text,
    rateCell: (rate) => rate == null ? DASH : String(rate),
    relTime: (ts) => String(ts || ''),
    bar: (rate) => 'bar:' + String(rate),
    selectTool: () => {},
    toolDetailFailure: () => null,
  });
  vm.runInContext([
    constStatement('TOOL_COLS'),
    ...['configureTextInput', 'h', 'fuzzyMatch', 'toolStatusDot', 'toolLabel', 'recentCalls', 'toolRecent', 'toolsTable', 'visibleTools', 'renderToolsTable', 'renderTools'].map(fn),
  ].join('\n'), context);
  return { context, controls };
}
function clearFilters(setup) {
  setup.controls['tools-filter'].value = '';
  setup.controls['tools-quick'].value = '';
}
function nameOf(row) {
  return row.children[0].children.filter((child) => typeof child === 'string').join('');
}
function idsOf(table) {
  return table.children[1].children.map((row) => {
    const tool = tools.find((item) => item.name === nameOf(row));
    assert.ok(tool, nameOf(row));
    return tool.id;
  });
}
function headersOf(table) {
  assert.equal(table.children[0].tagName, 'THEAD');
  assert.equal(table.children[1].tagName, 'TBODY');
  return table.children[0].children[0].children;
}
function directionFor(key, reverse) {
  const column = COLUMNS.find((item) => item.key === key);
  if (!column) return null;
  if (!reverse) return column.natural;
  return column.natural === 'ascending' ? 'descending' : 'ascending';
}
function glyphFor(direction) {
  return direction === 'ascending' ? ' ▴' : ' ▾';
}
function expectHeaders(table, key, reverse) {
  const headers = headersOf(table);
  assert.equal(headers.length, 6);
  const direction = directionFor(key, reverse);
  COLUMNS.forEach((column, index) => {
    const th = headers[index];
    const button = th.children[0];
    assert.equal(button.tagName, 'BUTTON');
    assert.equal(th.children.length, 1);
    assert.equal(button.getAttribute('type'), 'button');
    assert.equal(button.className, 'th-sort');
    assert.equal(button.getAttribute('title'), column.title);
    assert.equal(th.className, column.numeric ? 'num' : '');
    if (column.key === key) {
      assert.equal(th.getAttribute('aria-sort'), direction);
      assert.equal(button.textContent, column.label + glyphFor(direction));
    } else {
      assert.equal(th.getAttribute('aria-sort'), null);
      assert.equal(button.textContent, column.label);
      assert.equal(/[▴▾]/.test(button.textContent), false);
    }
  });
  const capabilities = headers[5];
  assert.equal(capabilities.tagName, 'TH');
  assert.equal(capabilities.className, '');
  assert.equal(capabilities.getAttribute('aria-sort'), null);
  assert.deepEqual(capabilities.children, ['Capabilities']);
}
function renderFiltered(setup) {
  const filteredTools = setup.context.visibleTools();
  const table = setup.context.document.createElement('table');
  setup.context.toolsTable(table, filteredTools, {
    selected: setup.context.S.selectedTool, onselect() {}, sortable: true,
  });
  if (filteredTools.length) assert.deepEqual(idsOf(table), filteredTools.map((tool) => tool.id));
  return { table, filteredTools };
}
function idsByName(list, reverse) {
  const ids = list.slice().sort((a, b) => a.name.localeCompare(b.name)).map((tool) => tool.id);
  return reverse ? ids.slice().reverse() : ids;
}
function expectedIds(key, reverse) {
  if (key === 'used') return reverse ? USED_REVERSE : USED_NATURAL;
  const natural = key === 'name' ? idsByName(tools, false) : ORDER[key];
  return reverse ? natural.slice().reverse() : natural.slice();
}
function proveOrders() {
  const every = tools.map((tool) => tool.id).sort();
  for (const ids of [VERSION_NATURAL, CALLS_NATURAL, RATE_NATURAL, USED_NATURAL]) {
    assert.deepEqual(ids.slice().sort(), every);
  }
  assert.deepEqual(VERSION_NATURAL.map((id) => byId.get(id).current_version), [1, 2, 3, 4, 8, 10]);
  assert.deepEqual(CALLS_NATURAL.map((id) => byId.get(id).invocations), [10, 10, 7, 3, 1, 0]);
  assert.deepEqual(tools.filter((tool) => tool.invocations === 10).map((tool) => tool.id), ['beta', 'zebra']);
  assert.deepEqual(RATE_NATURAL.map((id) => byId.get(id).success_rate ?? -1), [0.95, 0.8, 0.5, 0.2, 0, -1]);
  const stamps = USED_NATURAL.map((id) => byId.get(id).last_used_at).filter(Boolean);
  assert.deepEqual(stamps, stamps.slice().sort().reverse());
  const collated = tools.map((tool) => tool.name).sort((a, b) => a.localeCompare(b));
  assert.notDeepEqual(collated, tools.map((tool) => tool.name).sort());
  assert.ok(collated.indexOf('alpha') < collated.indexOf('éclair') && collated.indexOf('éclair') < collated.indexOf('fennel'));
}
function paint(setup) {
  const table = setup.context.document.createElement('table');
  const draw = () => setup.context.toolsTable(table, setup.context.visibleTools(), {
    selected: setup.context.S.selectedTool, onselect(id) { setup.context.S.selectedTool = id; }, sortable: true,
  });
  setup.context.renderToolsTable = draw;
  draw();
  return table;
}
function activate(table, index) {
  const button = headersOf(table)[index].children[0];
  assert.equal(button.listeners.click.length, 1);
  button.listeners.click[0]({});
}

test('sortable headers show one shared direction for every column and both reversals', () => {
  proveOrders();
  const setup = fixture();
  clearFilters(setup);
  const sourceIds = tools.map((tool) => tool.id);
  for (const column of COLUMNS) {
    for (const reverse of [false, true]) {
      setup.context.S.toolSort = { key: column.key, reverse };
      const { table, filteredTools } = renderFiltered(setup);
      expectHeaders(table, column.key, reverse);
      assert.deepEqual(filteredTools.map((tool) => tool.id), expectedIds(column.key, reverse));
    }
  }
  assert.deepEqual(tools.map((tool) => tool.id), sourceIds);
  const scrambled = tools.slice().reverse();
  const table = setup.context.document.createElement('table');
  setup.context.S.toolSort = { key: 'name', reverse: false };
  setup.context.toolsTable(table, scrambled, { selected: null, onselect() {}, sortable: true });
  assert.deepEqual(idsOf(table), scrambled.map((tool) => tool.id));
  expectHeaders(table, 'name', false);
});

test('filtered-empty Tools keep count and selection while an empty registry keeps its guidance', () => {
  const { context, controls } = fixture();
  context.S.selectedTool = 'alpha';
  controls['tools-filter'].value = 'no matching tool';
  context.renderTools();
  assert.equal(controls['tools-count'].textContent, '(0/6)');
  assert.match(controls['tools-table'].textContent, /No Tools match the current filters/);
  assert.equal(context.S.selectedTool, 'alpha');
  controls['tools-filter'].value = '';
  context.renderTools();
  const selected = controls['tools-table'].children[1].children.find(row => row.className === 'selected');
  assert.equal(nameOf(selected), 'alpha');
  context.S.tools = [];
  context.renderTools();
  assert.match(controls['tools-table'].textContent, /No Tools yet — the agent builds them with create_tool/);
  const metrics = new Node('table');
  context.toolsTable(metrics, [], { bars: true });
  assert.match(metrics.textContent, /No Tools yet — the agent builds them with create_tool/);
});

test('a header click sorts naturally and the next click on that header reverses', () => {
  const setup = fixture();
  clearFilters(setup);
  setup.context.S.toolSort = null;
  setup.context.S.selectedTool = 'alpha';
  setup.controls['tools-sort'].value = 'calls';
  const table = paint(setup);
  assert.equal(setup.context.S.toolSort, null);
  assert.deepEqual(idsOf(table), CALLS_NATURAL);
  expectHeaders(table, null, false);
  const sourceIds = tools.map((tool) => tool.id);
  const steps = [
    [1, 'version', false, 'calls', VERSION_NATURAL],
    [1, 'version', true, 'calls', VERSION_NATURAL.slice().reverse()],
    [0, 'name', false, 'name', expectedIds('name', false)],
    [0, 'name', true, 'name', expectedIds('name', true)],
    [0, 'name', false, 'name', expectedIds('name', false)],
    [2, 'calls', false, 'calls', CALLS_NATURAL],
    [2, 'calls', true, 'calls', CALLS_NATURAL.slice().reverse()],
    [3, 'rate', false, 'rate', RATE_NATURAL],
    [3, 'rate', true, 'rate', RATE_NATURAL.slice().reverse()],
    [4, 'used', false, 'used', USED_NATURAL],
    [4, 'used', true, 'used', USED_REVERSE],
  ];
  for (const [index, key, reverse, select, ids] of steps) {
    activate(table, index);
    assert.equal(setup.context.S.toolSort.key, key);
    assert.equal(setup.context.S.toolSort.reverse, reverse);
    assert.equal(setup.controls['tools-sort'].value, select);
    expectHeaders(table, key, reverse);
    assert.deepEqual(idsOf(table), ids);
    assert.equal(setup.context.S.selectedTool, 'alpha');
  }
  assert.deepEqual(tools.map((tool) => tool.id), sourceIds);
});

test('filters and non-ASCII fields keep production order in the rendered table', () => {
  const setup = fixture();
  const sourceIds = tools.map((tool) => tool.id);
  clearFilters(setup);
  setup.context.S.selectedTool = 'alpha';
  setup.context.S.toolSort = { key: 'version', reverse: false };
  const version = renderFiltered(setup);
  assert.deepEqual(version.filteredTools.map((tool) => tool.id), VERSION_NATURAL);
  const selected = version.table.children[1].children.filter((row) => row.className === 'selected');
  assert.equal(selected.length, 1);
  assert.equal(nameOf(selected[0]), 'alpha');
  assert.ok(version.table.children[1].children.every((row) => row.getAttribute('tabindex') === '0'));
  assert.deepEqual(version.table.children[1].children[0].children.map((cell) => cell.className), ['', '', 'num', 'num', 'muted', 'caps']);
  assert.deepEqual(version.table.children[1].children.map((row) => row.children.map((cell, index) => index === 0
    ? nameOf(row) : cell.textContent)), [
    ['ゼブラ', 'v1', '10', '0.95', '2024-03-01T00:00:00+00:00', DASH],
    ['ベータ', 'v2', '10', '0.5', '2024-03-01T00:00:00.123+00:00', 'fs:read, 計測'],
    ['fennel', 'v3', '0', '0', DASH, DASH],
    ['引退', 'v4', '1', '0.2', '2024-01-01T00:00:00+00:00', DASH],
    ['alpha', 'v8', '7', '0.8', '2024-03-01T00:00:00+00:00', DASH],
    ['éclair', 'v10', '3', DASH, DASH, DASH],
  ]);
  assert.ok(version.table.children[1].children[3].children[0].textContent.includes('(retired)'));

  clearFilters(setup);
  setup.context.S.selectedTool = null;
  setup.controls['tools-quick'].value = 'active';
  setup.context.S.toolSort = { key: 'used', reverse: false };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['beta', 'alpha', 'zebra', 'ecl', 'zero']);
  setup.context.S.toolSort = { key: 'name', reverse: false };
  const activeNames = renderFiltered(setup).filteredTools.map((tool) => tool.name);
  assert.deepEqual(activeNames, activeNames.slice().sort((a, b) => a.localeCompare(b)));
  assert.equal(activeNames.includes('引退'), false);
  assert.ok(activeNames.indexOf('alpha') < activeNames.indexOf('éclair') && activeNames.indexOf('éclair') < activeNames.indexOf('fennel'));

  clearFilters(setup);
  setup.controls['tools-quick'].value = 'retired';
  setup.context.S.toolSort = { key: 'calls', reverse: true };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['retired']);
  setup.controls['tools-quick'].value = 'failing';
  setup.context.S.toolSort = { key: 'version', reverse: false };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['beta', 'zero', 'retired']);
  setup.controls['tools-quick'].value = 'attention';
  setup.context.S.toolSort = { key: 'rate', reverse: true };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['zebra']);

  clearFilters(setup);
  setup.controls['tools-filter'].value = ' #計測';
  setup.context.S.toolSort = { key: 'calls', reverse: false };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['beta', 'zebra']);
  setup.context.S.toolSort = { key: 'calls', reverse: true };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['zebra', 'beta']);
  setup.controls['tools-filter'].value = ' しまう ';
  setup.context.S.toolSort = { key: 'name', reverse: false };
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.name), ['ゼブラ']);
  setup.controls['tools-filter'].value = ' #旧';
  assert.deepEqual(renderFiltered(setup).filteredTools.map((tool) => tool.id), ['retired']);

  clearFilters(setup);
  setup.controls['tools-quick'].value = 'active';
  setup.context.S.toolSort = { key: 'unknown', reverse: true };
  assert.deepEqual(setup.context.visibleTools().map((tool) => tool.id), ['beta', 'ecl', 'zebra', 'zero', 'alpha']);
  clearFilters(setup);
  setup.controls['tools-filter'].value = 'zzz';
  setup.context.S.toolSort = { key: 'rate', reverse: false };
  const empty = renderFiltered(setup);
  assert.deepEqual(empty.filteredTools, []);
  assert.equal(empty.table.children[1].children.length, 1);
  assert.equal(empty.table.children[1].children[0].children[0].getAttribute('colspan'), '6');
  assert.ok(empty.table.children[1].children[0].textContent.includes('No Tools yet'));
  expectHeaders(empty.table, 'rate', false);
  assert.deepEqual(tools.map((tool) => tool.id), sourceIds);
});

test('inactive headers and unsortable columns expose no direction', () => {
  const setup = fixture();
  setup.context.S.toolSort = { key: 'calls', reverse: true };
  const metrics = setup.context.document.createElement('table');
  setup.context.toolsTable(metrics, [], { bars: true, selected: null, onselect() {} });
  const headers = headersOf(metrics);
  assert.deepEqual(headers.map((th) => th.textContent), ['Tool', 'Ver', 'Calls', 'Success', 'Last used', '']);
  for (const th of headers) {
    assert.equal(th.getAttribute('aria-sort'), null);
    assert.equal(th.children.some((child) => child && child.tagName === 'BUTTON'), false);
    assert.equal(/[▴▾]/.test(th.textContent), false);
  }
  assert.equal(headers[2].className, 'num');
  assert.equal(headers[3].className, 'num');
  assert.equal(metrics.children[1].children[0].children[0].className, 'empty');
  assert.equal(metrics.children[1].children[0].children[0].getAttribute('colspan'), '6');

  setup.context.S.toolSort = null;
  clearFilters(setup);
  const table = paint(setup);
  expectHeaders(table, null, false);
  assert.equal(headersOf(table)[5].textContent, 'Capabilities');
});

test('row selection follows the rendered tool', () => {
  const setup = fixture();
  clearFilters(setup);
  setup.context.S.toolSort = { key: 'name', reverse: false };
  const picks = [];
  const table = setup.context.document.createElement('table');
  const rows = setup.context.visibleTools();
  setup.context.toolsTable(table, rows, {
    selected: 'alpha', sortable: true, onselect: (id) => picks.push(id),
  });
  const row = (id) => table.children[1].children[idsOf(table).indexOf(id)];
  assert.equal(row('alpha').className, 'selected');
  assert.equal(table.children[1].children.filter((item) => item.className === 'selected').length, 1);
  assert.equal(row('ecl').listeners.click.length, 1);
  row('ecl').listeners.click[0]({});
  assert.equal(row('zebra').listeners.keydown.length, 1);
  row('zebra').listeners.keydown[0]({ key: 'Enter' });
  row('beta').listeners.keydown[0]({ key: 'Escape' });
  assert.deepEqual(picks, ['ecl', 'zebra']);
  assert.equal(row('alpha').className, 'selected');
  assert.deepEqual(idsOf(table), rows.map((tool) => tool.id));
});


test('Tools and Metrics rows display retained use and treat old or empty history as unknown', () => {
  for (const bars of [false, true]) {
    const setup = fixture();
    const table = new Node('table');
    const retained = byId.get('beta');
    const legacy = byId.get('ecl');
    const pruned = byId.get('zero');
    setup.context.toolsTable(table, [retained, legacy, pruned], { bars, onselect: () => {} });
    const stamps = table.children[1].children.map((row) => row.children[4].textContent);
    assert.deepEqual(stamps, [retained.last_used_at, DASH, DASH]);
    assert.notEqual(retained.last_used_at, retained.updated_at);
    assert.ok(legacy.invocations > 0 && !Object.hasOwn(legacy, 'last_used_at'));
  }
});

test('Tools and Metrics rows show the same retained sample without changing lifetime success', () => {
  for (const bars of [false, true]) {
    const setup = fixture();
    const table = new Node('table');
    const tool = { ...tools[0], recent: 's'.repeat(20) + 'sfff'.repeat(5) };
    setup.context.toolsTable(table, [tool], { bars, onselect: () => {} });
    const cell = table.children[1].children[0].children[3];
    assert.equal(cell.children[0], '0.5');
    const recent = cell.children[1];
    assert.equal(recent.attrs.role, 'img');
    assert.match(recent.attrs['aria-label'], /Succeeded: 5; Failed: 15; Recent sample trend: down/);
    assert.equal(recent.children[0].children.length, 20);
    assert.equal(recent.children[1].textContent, '5/20 ✓ ↓');
  }
});


test('filter, quick filter and menu sort leave the selected detail and editor untouched', () => {
  const { context, controls } = fixture();
  for (const id of ['tools-filter', 'tools-quick', 'tools-sort']) {
    controls[id].listeners = {};
    controls[id].addEventListener = Node.prototype.addEventListener;
  }
  const detail = controls['tool-detail'];
  const editor = new Node('textarea');
  editor.value = 'unsaved code';
  editor.selectionStart = 3;
  const tags = new Node('input');
  tags.value = 'unsaved tags';
  const changes = new Node('details');
  changes.open = true;
  detail.append(editor, tags, changes);
  context.S.selectedTool = 'alpha';
  context.S.toolDetail = { tool: { id: 'alpha' } };
  context.renderToolDetail = () => { throw new Error('detail rebuilt: would request diff and focus editor'); };
  context.document.activeElement = controls['tools-filter'];
  let paints = 0;
  const paintTable = context.toolsTable;
  context.toolsTable = (...args) => { paints++; return paintTable(...args); };
  const start = source.indexOf('  for (const id of ["tools-filter"');
  const end = source.indexOf('  for (const id of ["proposals-filter"', start);
  assert.ok(start >= 0 && end > start);
  vm.runInContext(source.slice(start, end), context);
  for (const text of ['a', 'al', 'alp']) {
    controls['tools-filter'].value = text;
    for (const listener of controls['tools-filter'].listeners.input) listener();
  }
  controls['tools-quick'].value = '';
  for (const listener of controls['tools-quick'].listeners.input) listener();
  controls['tools-sort'].value = 'calls';
  assert.equal(controls['tools-sort'].listeners.input.length, 1);
  for (const listener of controls['tools-sort'].listeners.input) listener();
  assert.equal(paints, 5);
  assert.equal(context.S.toolSort.key, 'calls');
  assert.deepEqual(detail.children, [editor, tags, changes]);
  assert.equal(editor.value, 'unsaved code');
  assert.equal(editor.selectionStart, 3);
  assert.equal(tags.value, 'unsaved tags');
  assert.equal(changes.open, true);
  assert.equal(context.document.activeElement, controls['tools-filter']);
  expectHeaders(controls['tools-table'], 'calls', false);
});
