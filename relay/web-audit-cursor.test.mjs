import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const load = source.slice(source.indexOf('  let auditSeq = 0;'), source.indexOf('  /** Settings → Providers & Roles'));
function fixture(audit) {
  const calls = [], paints = [];
  const context = vm.createContext({ S: {audit, auditFilter: '*'}, renderView: () => paints.push(true), request: async req => { calls.push(req); return [{ cursor: 'audit:v1:next', created_at: 'same', detail: 'older' }]; } });
  vm.runInContext(load, context);
  return { context, calls, paints, run: older => vm.runInContext(`loadAudit(${older})`, context) };
}
test('Older forwards the opaque cursor and appends the returned page', async () => {
  const f = fixture([{cursor:'audit:v1:previous',created_at:'same',detail:'newer'}]);
  await f.run(true);
  assert.equal(f.calls[0].before,'audit:v1:previous');
  assert.equal(f.calls[0].type,'list_audit');
  assert.equal(f.calls[0].limit,50);
  assert.equal(f.calls[0].filter,'*');
  assert.equal(f.context.S.audit.length,2);
  assert.equal(f.context.S.audit[0].detail,'newer');
  assert.equal(f.context.S.audit[1].detail,'older');
  assert.equal(f.paints.length,1);
});
test('Older falls back to a legacy timestamp and fresh loads reset the boundary', async () => {
  const f = fixture([{created_at:'legacy'}]);
  await f.run(true);
  assert.equal(f.calls[0].before,'legacy');
  await f.run(false);
  assert.equal(f.calls[1].before,null);
  assert.equal(f.context.S.audit.length,1);
  assert.equal(f.context.S.auditPages,1);
});
