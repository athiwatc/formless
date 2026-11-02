// Presentation dependencies for the intent/data VM fixtures. The production
// scheduler runs unchanged; tests explicitly flush its deterministic RAF queue.
// Real layout/ResizeObserver/scroll anchoring use the browser regression suite.
import vm from 'node:vm';
import assert from 'node:assert/strict';

export function installEventRenderFixture(context, fn) {
  const frames = new Map();
  let nextFrame = 0;
  context.requestAnimationFrame = callback => {
    const id = ++nextFrame;
    frames.set(id, callback);
    return id;
  };
  context.cancelAnimationFrame = id => frames.delete(id);
  context.sessionSeq ??= 0;
  // These fixtures isolate reducer/intent behavior rather than geometry.
  context.withTranscriptLayout ??= update => update();
  context.resetReadingSession ??= () => {};
  context.flushEventRender = () => {
    for (let turn = 0; frames.size; turn++) {
      assert.ok(turn < 10, 'presentation RAF queue must settle');
      const pending = [...frames.keys()];
      for (const id of pending) {
        const callback = frames.get(id);
        frames.delete(id);
        if (callback) callback(0);
      }
    }
  };
  vm.runInContext('let eventPaint = null, eventPaintEpoch = 0;\n'
    + ['cancelEventRender', 'scheduleEventRender'].map(fn).join('\n'), context);
}

export function installHistoryFixture(context, fn) {
  vm.runInContext('const historyRender = { session: undefined, turns: new Map(), live: null, welcome: null, banner: null };\n'
    + ['reconcileChildren', 'updateText'].map(fn).join('\n'), context);
}
