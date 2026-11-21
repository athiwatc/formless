import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const match = new RegExp('  (?:async )?function ' + name + '\\(').exec(source);
  return match ? source.slice(match.index, source.indexOf('\n  }', match.index) + 4) : '';
}

test('Daemon status preserves seconds below an hour and shows hours thereafter', async () => {
  for (const [seconds, expected] of [[0, '0m 0s'], [45, '0m 45s'], [90, '1m 30s'], [3599, '59m 59s'], [3600, '1h 0m'], [7500, '2h 5m'], [86400, '24h 0m'], [null, '?'], [undefined, '?']]) {
    const trigger = {};
    let shown;
    const context = vm.createContext({
      document: { activeElement: trigger },
      act: async request => { assert.equal(request.type, 'status'); return { resources: { uptime_secs: seconds }, running_sessions: [] }; },
      h: (tag, attrs, ...children) => ({ tag, attrs, children: children.flat() }),
      humanBytes: () => '0 bytes', uiFixed: String,
      modal: (title, body, options) => { shown = { title, body, options }; },
    });
    vm.runInContext(fn('daemonUptime') + '\n' + fn('showStatus'), context);
    await context.showStatus();
    const row = shown.body.children[0].children.find(row => row.children[0].children[0] === 'uptime');
    assert.equal(row.children[1].children[0], expected, String(seconds));
    assert.equal(shown.title, 'Daemon');
    assert.equal(shown.options.trigger, trigger);
  }
});
