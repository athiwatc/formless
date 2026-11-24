import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

const app = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
const html = readFileSync(new URL('./web/index.html', import.meta.url), 'utf8');

function fn(name) {
  const regular = app.indexOf('  function ' + name + '(');
  const async = app.indexOf('  async function ' + name + '(');
  const start = regular < 0 ? async : async < 0 ? regular : Math.min(regular, async);
  assert.ok(start >= 0, name);
  return app.slice(start, app.indexOf('\n  }', start) + 4);
}

function fixture() {
  let now = 0;
  let nextTimer = 0;
  const timers = new Map();
  const root = { children: [], append(child) { this.children.push(child); child.parent = this; } };
  const announcer = { textContent: '' };
  function node(tag, props, children) {
    return {
      tag, props: props || {}, children: children.flat(Infinity).filter(child => child != null),
      disabled: false, removed: false,
      remove() { this.removed = true; this.parent?.children.splice(this.parent.children.indexOf(this), 1); },
      contains(target) { return target === this || this.children.some(child => child?.contains?.(target)); },
    };
  }
  const c = vm.createContext({
    S: { current: 'session', messages: [] },
    $: id => id === 'toasts' ? root : id === 'chat-announcer' ? announcer : null,
    h: (tag, props, ...children) => node(tag, props, children),
    Date: { now: () => now },
    setTimeout: (fn, ms) => { const id = ++nextTimer; timers.set(id, { at: now + ms, fn }); return id; },
    clearTimeout: id => timers.delete(id),
    request: async () => { throw new Error('offline'); },
    note: () => {},
    summarizeArgs: () => 'args',
    elapsed: () => '2s',
    uiFixed: value => value.toFixed(1),
    uiFormat: String,
  });
  const announceStart = app.indexOf('  const chatAnnouncement =');
  const announceEnd = app.indexOf('\n\n  // -------------------------------------------------------- ask dialogs', announceStart);
  vm.runInContext(fn('updateText') + fn('toast') + fn('act') + fn('recordedDuration') + app.slice(announceStart, announceEnd), c);
  function advance(ms) {
    now += ms;
    while (true) {
      const due = [...timers].filter(([, timer]) => timer.at <= now).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      due[1].fn();
    }
  }
  return { c, root, announcer, timers, advance, node };
}

test('transcript is not live and chat has a separate polite status announcer', () => {
  assert.match(html, /id="transcript"[^>]*tabindex="0"/);
  assert.doesNotMatch(html.match(/<div id="transcript"[^>]*>/)?.[0] || '', /aria-live|role="status"/);
  assert.match(html, /id="chat-announcer" class="sr-only" role="status" aria-atomic="true"/);
  assert.doesNotMatch(html.match(/<div id="toasts"[^>]*>/)?.[0] || '', /aria-live/);
  const eventHandler = app.slice(app.indexOf('  function onEvent(ev)'));
  for (const [event, phrase] of [
    ['run_started', 'Run started'], ['tool_call_started', 'Running '],
    ['run_finished', 'Answer finished'], ['approval_requested', 'Approval needed'],
  ]) {
    assert.match(eventHandler, new RegExp('case "' + event + '"[\\s\\S]{0,4000}announceChat\\("' + phrase));
  }
});

test('coarse chat announcements coalesce bursts and completed text is read once', () => {
  const { c, announcer, advance } = fixture();
  c.announceChat('Running read_file', 'session');
  c.announceChat('Running search', 'session');
  assert.equal(announcer.textContent, '');
  advance(500);
  assert.equal(announcer.textContent, 'Running search');
  c.announceChat('Running another', 'session');
  advance(499);
  assert.equal(announcer.textContent, 'Running search');
  c.announceChat('Answer finished. Complete answer', 'session', true);
  advance(1);
  assert.equal(announcer.textContent, 'Answer finished. Complete answer');
  assert.equal(c.completedRunAnswer([
    { role: 'assistant', seq: 2, content: 'old answer' },
    { role: 'assistant', seq: 4, content: 'tool step', meta: '{"tool_calls":[{"id":"c"}]}' },
    { role: 'tool', seq: 5, content: 'result' },
    { role: 'assistant', seq: 6, content: 'final answer' },
  ], 3), 'final answer');
  assert.equal(c.completedRunAnswer([{ role: 'assistant', seq: 4, content: 'old answer' }], 3), 'old answer');
  assert.equal(c.completedRunAnswer([{ role: 'assistant', seq: 2, content: 'old answer' }], 3), '');
});

test('onEvent announces no streaming tokens or timer ticks and reads the final answer once', () => {
  const { c, announcer, advance } = fixture();
  const writes = [];
  let announcedText = '';
  Object.defineProperty(announcer, 'textContent', {
    get: () => announcedText,
    set: value => { announcedText = value; writes.push(value); },
  });
  const announceCalls = [];
  const announce = c.announceChat;
  c.announceChat = (...args) => { announceCalls.push(args[0]); announce(...args); };
  c.S = {
    current: 'session', messages: [], running: new Set(), liveTools: [], liveRun: null,
    activity: null, sessionSnapshot: null, showThinking: false,
  };
  c.branchSummaryEvent = () => {};
  c.scheduleForecast = () => {};
  c.forecastRevision = 0;
  c.notifyFor = () => {};
  c.renderRunBanner = () => {};
  c.renderRunTools = () => {};
  c.renderPlan = () => {};
  c.clearLiveText = () => { c.S.streaming = ''; c.S.thinking = ''; };
  c.appendLiveText = (kind, delta) => { c.S[kind] = (c.S[kind] || '') + delta; };
  c.commitReadingStream = () => {};
  c.setActivity = label => { c.S.activity = label ? { label } : null; };
  c.scheduleEventRender = () => {};
  c.withTranscriptLayout = render => render();
  c.statusRender = { text: null };
  const liveElement = {};
  const row = { node: { isConnected: true }, tool: { elapsedMs: null, started: 0 }, timer: { data: '' } };
  c.liveRender = { session: 'session', root: liveElement, activity: { isConnected: false }, rows: new Map([['tool', row]]) };
  c.$ = id => id === 'chat-announcer' ? announcer : id === 'live' ? liveElement : null;
  let ticks = 0;
  c.elapsed = () => `${++ticks}s`;
  vm.runInContext(fn('onEvent') + fn('updateElapsedTimers'), c);

  c.onEvent({ type: 'run_started', session_id: 'session', run_id: 'run' });
  c.onEvent({ type: 'message_delta', session_id: 'session', delta: 'A token' });
  c.onEvent({ type: 'tool_call_started', session_id: 'session', run_id: 'run', invocation_id: 'call', tool_name: 'read_file', arguments: '{}', depth: 0 });
  c.onEvent({ type: 'message_delta', session_id: 'session', delta: ' at a time' });
  c.updateElapsedTimers();
  advance(1000);
  c.updateElapsedTimers();
  assert.equal(row.timer.data, '2s', 'the live timer rerenders while the run is streaming');
  assert.deepEqual(writes, ['Run started', 'Running read_file']);

  c.onEvent({ type: 'message_delta', session_id: 'session', delta: ' without announcements' });
  c.updateElapsedTimers();
  advance(1000);
  c.updateElapsedTimers();
  assert.deepEqual(writes, ['Run started', 'Running read_file'], 'tokens and timer ticks do not update the announcer');

  c.onEvent({ type: 'message_complete', session_id: 'session', message: { id: 'answer', role: 'assistant', seq: 1, content: 'The complete answer', meta: '{}' } });
  const finished = { type: 'run_finished', session_id: 'session', run_id: 'run', status: 'completed' };
  c.onEvent(finished);
  c.onEvent(finished);
  assert.deepEqual(writes, ['Run started', 'Running read_file', 'Answer finished. The complete answer']);
  assert.deepEqual(announceCalls, ['Run started', 'Running read_file', 'Answer finished. The complete answer']);
});

test('live tool status name includes completion state and duration without timer text', () => {
  const { c } = fixture();
  const attribute = () => {
    const values = {};
    return { hidden: false, getAttribute: key => values[key] ?? null, setAttribute: (key, value) => { values[key] = value; } };
  };
  const tool = { status: 'running', name: 'read_file', version: 1, arguments: '{}', depth: 0, started: 0 };
  const row = {
    tool, node: { className: '', style: {}, dataset: {} }, summary: { firstChild: attribute() },
    mark: { className: '', style: {}, textContent: '' }, name: { data: '' }, args: { data: '' },
    status: { data: '' }, folded: { hidden: true, textContent: '' },
    progressBox: { hidden: true }, progress: { hidden: true }, progressText: { data: '' },
    stalled: { hidden: true, textContent: '' }, cancel: { hidden: true }, transcript: { hidden: true },
    timer: { data: '' }, payloadInitialized: true, payloadArgs: '{}', payloadSubagent: undefined,
  };
  vm.runInContext(fn('updateLiveTool'), c);
  c.updateLiveTool(row, 0);
  assert.equal(row.status.data, 'Running · 2s');
  tool.status = 'failed';
  tool.elapsedMs = 1234;
  c.updateLiveTool(row, 0);
  assert.equal(row.status.data, 'Failed · 1.2s');
  assert.equal(row.timer.data, '1.2s');
  assert.match(app, /const statusText = document\.createTextNode\(""\)/);
  assert.match(app, /h\("span", \{ class: "sr-only", "data-live-status": "" \}, statusText\)/);
  assert.match(app, /const mark = h\("span", \{ "aria-hidden": "true" \}\)/);
});

test('toast dismissal, timer pause, and retry actions are accessible', async () => {
  const { c, root, timers, advance } = fixture();
  c.toast('offline', true, { label: 'Retry', run: () => {} });
  const error = root.children.at(-1);
  assert.equal(error.props.role, 'alert');
  assert.equal(timers.size, 0, 'errors persist until a user dismisses them');
  const actions = error.children[1].children;
  assert.equal(actions[0].children[0], 'Retry');
  assert.equal(actions[1].props['aria-label'], 'Dismiss notification');
  actions[1].props.onclick();
  assert.equal(error.removed, true);

  c.toast('Saved', false);
  const success = root.children.at(-1);
  success.props.onmouseenter();
  advance(5000);
  assert.equal(success.removed, false, 'hover pauses auto-dismiss');
  success.props.onmouseleave();
  advance(3499);
  assert.equal(success.removed, false);
  advance(1);
  assert.equal(success.removed, true);

  c.toast('Saved again', false);
  const focused = root.children.at(-1);
  focused.props.onfocusin();
  advance(4000);
  assert.equal(focused.removed, false, 'focus pauses auto-dismiss');
  focused.props.onfocusout({ relatedTarget: focused.children[0] });
  advance(1000);
  assert.equal(focused.removed, false, 'moving focus within the toast keeps it paused');
  focused.props.onfocusout({ relatedTarget: null });
  advance(3499);
  assert.equal(focused.removed, false);
  advance(1);
  assert.equal(focused.removed, true);

  c.toast('Focus remains while pointer leaves', false);
  const focusThenHover = root.children.at(-1);
  focusThenHover.props.onfocusin();
  focusThenHover.props.onmouseenter();
  advance(5000);
  focusThenHover.props.onmouseleave();
  advance(1000);
  assert.equal(focusThenHover.removed, false, 'mouseleave does not resume while focus remains');
  focusThenHover.props.onfocusout({ relatedTarget: null });
  advance(3499);
  assert.equal(focusThenHover.removed, false);
  advance(1);
  assert.equal(focusThenHover.removed, true);

  c.toast('Pointer remains while focus leaves', false);
  const hoverThenFocus = root.children.at(-1);
  hoverThenFocus.props.onmouseenter();
  hoverThenFocus.props.onfocusin();
  advance(5000);
  hoverThenFocus.props.onfocusout({ relatedTarget: null });
  advance(1000);
  assert.equal(hoverThenFocus.removed, false, 'focusout does not resume while pointer remains');
  hoverThenFocus.props.onmouseleave();
  advance(3499);
  assert.equal(hoverThenFocus.removed, false);
  advance(1);
  assert.equal(hoverThenFocus.removed, true);

  c.toast('Undo available', false, { label: 'Undo', run: () => {} });
  const actionToast = root.children.at(-1);
  advance(3499);
  assert.equal(actionToast.removed, false, 'action toast defaults to the normal timeout');
  advance(1);
  assert.equal(actionToast.removed, true);

  let retries = 0;
  await c.act({ type: 'get_tool' }, null, undefined, () => { retries++; });
  const retryToast = root.children.at(-1);
  assert.equal(retryToast.props.role, 'alert');
  assert.equal(retryToast.children[1].children[0].children[0], 'Retry');
  retryToast.children[1].children[0].props.onclick();
  assert.equal(retries, 1);
});
