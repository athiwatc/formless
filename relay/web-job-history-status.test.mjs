import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const start = source.indexOf('  async function showJobHistory(');
assert.notEqual(start, -1);
const implementation = source.slice(start, source.indexOf('\n  }', start) + 4);

async function history(response) {
  let shown;
  const requests = [];
  const h = (tag, attrs, ...children) => ({
    tag, attrs, children: children.flat(),
    replaceChildren(...next) { this.children = next.flat(); },
  });
  const context = vm.createContext({
    h, uiFormat: String, uiMoney: String,
    request: async request => { requests.push(request); if (response instanceof Error) throw response; return response; },
    ask: options => { shown = options; return Promise.resolve(); },
  });
  vm.runInContext(implementation, context);
  await context.showJobHistory({ id: 'synthetic-job' });
  assert.equal(requests.length, 1);
  assert.equal(requests[0].type, 'job_runs');
  assert.equal(requests[0].id, 'synthetic-job');
  assert.equal(shown.title, 'Job history');
  assert.equal(shown.ok, 'Close');
  assert.equal(shown.hideCancel, true);
  return shown.body;
}

test('Job history reserves error styling for failures, not running or skipped firings', async () => {
  const statuses = ['completed', 'running', 'skipped', 'failed'];
  const body = await history(statuses.map(status => ({ status, started_at: '2026-11-20T00:00:00Z', trigger: 'schedule', detail: 'detail: ' + status })));
  const table = body.children[0].children[0];
  assert.deepEqual(table.children[0].children[0].children.map(cell => cell.children[0]), ['Started', 'Trigger', 'Status', 'Duration', 'Cost', 'Session', 'Detail']);
  const rows = table.children[1].children;
  assert.deepEqual(rows.map(row => row.children[2].children[0]), statuses);
  assert.deepEqual(rows.map(row => row.children[2].attrs.class), ['', '', 'warn', 'error']);
  assert.deepEqual(rows.map(row => row.children[6].children[0]), statuses.map(status => 'detail: ' + status));
});

test('Job history retains distinct empty and failed-load presentations', async () => {
  const empty = await history([]);
  assert.equal(empty.children[0].children[0], 'It has not run yet.');
  assert.equal(empty.children[0].attrs.class, 'muted');
  const failed = await history(new Error('synthetic read failure'));
  assert.equal(failed.children[0].attrs.role, 'alert');
  assert.equal(failed.children[0].attrs.class, 'error');
  assert.equal(failed.children[0].children[1], 'synthetic read failure');
  assert.equal(failed.children[1].children[0], 'Retry');
});
