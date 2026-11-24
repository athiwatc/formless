// Presentation dependencies for the intent/data VM fixtures. The production
// scheduler runs unchanged; tests explicitly flush its deterministic RAF queue.
// Real layout/ResizeObserver/scroll anchoring use the browser regression suite.
import vm from 'node:vm';
import assert from 'node:assert/strict';

export function installEventRenderFixture(context, fn) {
  // Event/session reducers use the production incremental live-text helpers.
  // Keep their state scoped to this VM, rather than stubbing cache resets.
  vm.runInContext('const liveText = { session: undefined, run: undefined, answer: {}, thinking: {}, answerSource: undefined, thinkingSource: undefined, expanded: false };\n'
    + ['currentLiveText', 'appendLiveText', 'clearLiveText'].map(fn).join('\n'), context);
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
  context.renderSessions ??= () => {};
  if (context.S && !context.S.drafts) context.S.drafts = {};
  context.saveDraft ??= () => true;
  context.autosize ??= () => {};
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
  context.tr ??= text => text;
  vm.runInContext('const historyRender = { session: undefined, turns: new Map(), live: null, welcome: null, banner: null };\n'
    + ['reconcileChildren', 'updateText', 'sessionLoadNotice', 'turnTimestamp'].map(fn).join('\n'), context);
}
