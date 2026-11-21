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
function fixture(language) {
  const context = vm.createContext({ uiLang: () => language, tr: value => value });
  vm.runInContext('Date.now = () => 2000000000000;\n' + ['uiLocale', 'uiFormat', 'elapsed', 'activityText'].map(fn).join('\n'), context);
  return context;
}

test('elapsed preserves sub-hour strings and rolls long durations into hours', () => {
  for (const language of ['en', 'th']) {
    const c = fixture(language);
    for (const [seconds, expected] of [[0,'0s'],[59,'59s'],[60,'1m00s'],[65,'1m05s'],[3599,'59m59s'],[3600,'1h00m00s'],[3601,'1h00m01s'],[3661,'1h01m01s'],[5400,'1h30m00s'],[86399,'23h59m59s'],[86400,'24h00m00s'],[3600000,'1,000h00m00s']]) {
      const since = 2000000000000 - seconds * 1000;
      assert.equal(c.elapsed(since), expected, `${language}: ${seconds}`);
      assert.equal(c.activityText({label:'thinking', since}), 'thinking… ' + expected);
    }
    assert.equal(c.elapsed(2000000000000 - 3599999), '59m59s');
    assert.equal(c.elapsed(2000000000000 - 3600999), '1h00m00s');
  }
});
