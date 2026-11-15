import { installEventRenderFixture } from './test-web-render.mjs';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const fn = name => {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
};
const fixture = () => {
  const context = vm.createContext({ S: { current: 's', liveRun: { id: 'r' }, streaming: '', thinking: '' } });
  installEventRenderFixture(context, fn);
  return context;
};

test('event fixture installs real append/reset helpers with VM-local cache state', () => {
  const c = fixture(), other = fixture();
  c.appendLiveText('streaming', 'answer');
  c.appendLiveText('thinking', 'thought');
  assert.equal(c.S.streaming, 'answer');
  assert.equal(c.S.thinking, 'thought');
  assert.equal(c.currentLiveText().answerSource, 'answer');
  assert.equal(c.currentLiveText().thinkingSource, 'thought');
  assert.equal(other.S.streaming, '');
  assert.equal(other.currentLiveText().answerSource, undefined);
  c.clearLiveText();
  assert.equal(c.S.streaming, '');
  assert.equal(c.S.thinking, '');
  assert.equal(c.currentLiveText().answerSource, undefined);
  assert.equal(c.currentLiveText().thinkingSource, undefined);
});
