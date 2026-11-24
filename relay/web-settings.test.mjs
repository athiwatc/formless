import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
function fixture() {
  const calls = [], notes = [], errors = [];
  const c = vm.createContext({
    uiFormat: value => String(value), calls, notes, errors,
    h: (tag, attrs, ...kids) => ({tag, attrs, kids}),
    act: async req => { calls.push(JSON.parse(JSON.stringify(req))); return { applies: req.key === 'tui.on_start' ? 'on_tui_start' : 'now' }; },
    note: text => notes.push(text), toast: text => errors.push(text), refreshSettings: () => {},
  });
  vm.runInContext(['settingsRows', 'settingDisplay', 'parseSettingInput', 'namingDownloadInfo', 'editSetting'].map(fn).join('\n'), c);
  return c;
}
const plain = value => JSON.parse(JSON.stringify(value));

test('documented values generate all rows and kinds without a hardcoded key list', () => {
  const c = fixture();
  const values = { 'future.flag': false, 'provider.max_retries': 0, 'limits.daily_usd': null,
    'provider.fallback_models': ['a'], 'tui.pane_widths': [22, 26], 'provider.prices': {},
    'tui.theme': 'auto', 'naming.model': 'fixture' };
  const docs = Object.fromEntries(Object.keys(values).map(key => [key, { description: 'Meaning', default: null }]));
  const rows = plain(c.settingsRows({ docs, values, choices: { 'tui.theme': ['auto', 'dark'] }, read_only: { 'hooks': '0 configured' } }));
  const find = key => rows.find(row => row.key === key);
  assert.equal(rows.length, 9);
  for (const [key, kind] of [['future.flag','bool'], ['provider.max_retries','number'], ['limits.daily_usd','json'],
    ['provider.fallback_models','list'], ['tui.pane_widths','list'], ['provider.prices','json'], ['tui.theme','choice'], ['naming.model','text']]) {
    assert.equal(find(key).kind, kind, key); assert.equal(find(key).editable, true, key);
    assert.deepEqual(find(key).value, values[key]);
  }
  assert.deepEqual(find('tui.theme').choices, ['auto', 'dark']);
  assert.equal(find('hooks').editable, false);
});

test('docs alone cannot grant edits and old summaries remain read-only', () => {
  const c = fixture();
  const docs = { 'security.secure': {}, 'provider.extra_headers': {}, 'provider.model': {} };
  const rows = plain(c.settingsRows({ docs, provider: { model: 'legacy' }, tui: { timestamps: true }, security: { secure: true } }));
  assert.ok(rows.length > 0); assert.ok(rows.every(row => row.editable === false));
  assert.ok(!rows.some(row => row.key === 'provider.extra_headers'));
  const projected = plain(c.settingsRows({ docs, values: { 'undocumented.key': 1, 'provider.model': 'new' }, read_only: { 'provider.model': 'stale', 'security.secure': true } }));
  assert.ok(!projected.some(row => row.key === 'undocumented.key'));
  assert.equal(projected.find(row => row.key === 'provider.model').value, 'new');
  assert.equal(projected.find(row => row.key === 'security.secure').editable, false);
});

test('typed input preserves JSON maps, numeric lists and literal commas; empty resets', () => {
  const c = fixture();
  for (const kind of ['number', 'list', 'json', 'text', 'optional']) assert.equal(c.parseSettingInput('  ', kind), null);
  assert.equal(c.parseSettingInput('0', 'number'), 0);
  assert.equal(c.parseSettingInput('2.5', 'number'), 2.5);
  assert.throws(() => c.parseSettingInput('Infinity', 'number'), /not a number/);
  assert.throws(() => c.parseSettingInput('oops', 'number'), /not a number/);
  assert.deepEqual(plain(c.parseSettingInput('a, b', 'list')), ['a','b']);
  assert.deepEqual(plain(c.parseSettingInput('["a,b", "c"]', 'list')), ['a,b','c']);
  assert.deepEqual(plain(c.parseSettingInput('[22,26]', 'list')), [22,26]);
  assert.throws(() => c.parseSettingInput('[broken', 'list'));
  assert.deepEqual(plain(c.parseSettingInput('{"model":{"input":0.000001}}', 'json')), { model: { input: 0.000001 } });
  assert.equal(c.parseSettingInput('true', 'json'), true);
  assert.equal(c.parseSettingInput('null', 'json'), null);
  assert.throws(() => c.parseSettingInput('{broken}', 'json'));
  assert.equal(c.settingDisplay(false), 'off'); assert.equal(c.settingDisplay(0), '0');
});

test('Edit submits existing SetSetting requests with typed values and correct startup notice', async () => {
  const c = fixture();
  await c.editSetting('tui.timestamps', true, 'bool');
  c.ask = async () => '7'; await c.editSetting('provider.max_retries', 5, 'number');
  c.ask = async () => ''; await c.editSetting('limits.daily_usd', 2.5, 'number');
  c.ask = async () => '[22,26]'; await c.editSetting('tui.pane_widths', [20,20], 'list');
  c.ask = async () => '{"fixture":{"input":1,"output":2}}'; await c.editSetting('provider.prices', {}, 'json');
  c.h = (tag, attrs) => { const node = { value: '', tag }; if (tag === 'select') c.select = node; return node; };
  c.ask = async () => { c.select.value = 'picker'; return true; };
  await c.editSetting('tui.on_start', 'last', 'choice', ['last','new','picker']);
  assert.deepEqual(c.calls.map(req => req.value), [false, 7, null, [22,26], { fixture: { input: 1, output: 2 } }, 'picker']);
  assert.ok(c.calls.every(req => req.type === 'set_setting'));
  assert.match(c.notes.at(-1), /when the TUI next starts/);
});

test('cancelled or malformed edits never submit requests', async () => {
  const c = fixture();
  c.ask = async () => null; await c.editSetting('provider.model', 'old', 'text');
  for (const [kind, current, invalid] of [['number',5,'NaN'], ['json',{},'{bad'], ['list',[],'[bad']]) {
    const prompts=[];const replies=[invalid,null];
    c.ask = async opts => {prompts.push(opts);return replies.shift();};
    await c.editSetting('fixture.value',current,kind);
    assert.equal(prompts[1].value,invalid);
    assert.equal(prompts[1].body.attrs.role,'alert');
    assert.ok(prompts[1].body.kids[0]);
  }
  assert.deepEqual(c.calls, []); assert.equal(c.errors.length, 0);
});

test('enabling naming auto-download asks consent before saving', async () => {
  const c = fixture();
  let prompt;
  c.ask = async options => { prompt = options; return false; };
  const settings = { naming: { model: 'qwen2.5-0.5b', model_size: '409 MiB' } };
  await c.editSetting('naming.auto_download', false, 'bool', [], settings);
  assert.match(prompt.title, /download/i);
  assert.match(prompt.text, /qwen2\.5-0\.5b/);
  assert.match(prompt.text, /409 MiB/);
  assert.match(prompt.text, /heuristic/);
  const info = c.namingDownloadInfo(settings.naming);
  assert.match(info.help, /qwen2\.5-0\.5b/);
  assert.match(info.help, /409 MiB/);
  assert.deepEqual(c.calls, []);
  c.ask = async () => true;
  await c.editSetting('naming.auto_download', false, 'bool', [], settings);
  assert.deepEqual(c.calls, [{ type: 'set_setting', key: 'naming.auto_download', value: true }]);
});

test('unsupported naming models show heuristic-only consent without a fabricated size', async () => {
  for (const naming of [
    { model: '', model_size: '—', status: 'heuristic only' },
    { model: 'not-in-catalog', model_size: '—', status: 'unknown model not-in-catalog; titles use a heuristic' },
  ]) {
    const c = fixture();
    let prompt;
    c.ask = async options => { prompt = options; return false; };
    await c.editSetting('naming.auto_download', false, 'bool', [], { naming });
    assert.equal(prompt.title, 'No supported session-naming model');
    assert.equal(prompt.ok, 'Save setting');
    assert.match(prompt.text, /heuristic/);
    assert.doesNotMatch(prompt.text, /will download|about |409 MiB/i);
    const info = c.namingDownloadInfo(naming);
    assert.equal(info.supported, false);
    assert.match(info.help, /heuristic/);
    assert.doesNotMatch(info.help, /will download|about |409 MiB/i);
    if (naming.model) assert.match(info.help, /not supported/);
    else assert.match(info.help, /No supported naming model/);
    assert.deepEqual(c.calls, []);
  }
});

test('invalid Settings text can be corrected before the only update request', async () => {
  const c=fixture();const replies=['  {bad  ','{"fixed":[1,2]}'];const prompts=[];
  c.ask=async opts=>{assert.equal(c.calls.length,0);prompts.push(opts);return replies.shift();};
  await c.editSetting('fixture.value',{},'json');
  assert.equal(prompts[1].value,'  {bad  ');
  assert.deepEqual(plain(c.calls),[{type:'set_setting',key:'fixture.value',value:{fixed:[1,2]}}]);
});
