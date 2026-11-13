// Presentation dependencies for the intent/data VM fixtures. The production
// scheduler runs unchanged; tests explicitly flush its deterministic RAF queue.
// Real layout/ResizeObserver/scroll anchoring use the browser regression suite.
import vm from 'node:vm';
import assert from 'node:assert/strict';

export function installEventRenderFixture(context, fn) {
  // Keep presentation dependencies real when reducers render localized values.
  context.navigator ??= { language: 'en-US' };
  vm.runInContext(['uiLang', 'uiLocale', 'uiFormat', 'uiFixed', 'uiMoney'].map(fn).join('\n'), context);
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
  context.reading ??= { session: null, anchor: null, saved: new Map() };
  vm.runInContext(['readingStreamMatches', 'remapReadingStream', 'commitReadingStream'].map(fn).join('\n'), context);
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
  // Intent fixtures omit diagram/layout DOM; real lifecycle runs in Chromium.
  context.refreshDiagrams ??= () => {};
  vm.runInContext('const historyRender = { session: undefined, turns: new Map(), live: null, welcome: null, banner: null };\n'
    + ['reconcileChildren', 'updateText'].map(fn).join('\n'), context);
}
