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

function accessibleText(element) {
  if (element == null || element.hidden || element.props?.hidden || element.props?.['aria-hidden'] === true || element.props?.['aria-hidden'] === 'true') return '';
  if (typeof element === 'string') return element;
  if (typeof element.props?.['aria-label'] === 'string') return element.props['aria-label'];
  if (typeof element.data === 'string') return element.data;
  return (element.children || []).map(accessibleText).join('');
}

function findElement(element, predicate) {
  if (element == null || typeof element !== 'object') return null;
  if (predicate(element)) return element;
  for (const child of element.children || []) {
    const found = findElement(child, predicate);
    if (found) return found;
  }
  return null;
}

function liveRendererFixture(tools) {
  const { c, announcer, node } = fixture();
  const outside = node('button', {}, []);
  const fallback = node('div', {}, []);
  const movedFocusedNodes = [];
  c.document = { activeElement: outside, body: fallback, createTextNode: data => ({ data }) };
  const domNode = (tag, props, children) => {
    const element = node(tag, props, children);
    const listeners = new Map();
    for (const child of element.children) if (child && typeof child === 'object') child.parent = element;
    Object.defineProperties(element, {
      firstChild: { get() { return this.children[0] || null; } },
      nextSibling: { get() {
        const index = this.parent?.children.indexOf(this) ?? -1;
        return index < 0 ? null : this.parent.children[index + 1] || null;
      } },
    });
    element.insertBefore = function (child, reference) {
      if (child === reference) return child;
      if (child.parent) {
        if (child.contains?.(c.document.activeElement)) movedFocusedNodes.push(child);
        child.remove();
      }
      const index = reference == null ? this.children.length : this.children.indexOf(reference);
      if (index < 0) throw new Error('reference is not a child');
      this.children.splice(index, 0, child);
      child.parent = this;
      return child;
    };
    element.remove = function () {
      if (!this.parent) return;
      if (this.contains?.(c.document.activeElement)) c.document.activeElement = c.document.body;
      const siblings = this.parent.children;
      const index = siblings.indexOf(this);
      if (index >= 0) siblings.splice(index, 1);
      this.parent = null;
    };
    element.addEventListener = (type, listener) => {
      const registered = listeners.get(type) || [];
      registered.push(listener);
      listeners.set(type, registered);
    };
    element.dispatchEvent = event => {
      for (const listener of listeners.get(event.type) || []) listener.call(element, event);
    };
    return element;
  };
  const live = domNode('div', {}, []);
  c.h = (tag, props, ...children) => domNode(tag, props, children);
  c.S = {
    current: 'session', messages: [], running: new Set(['session']), liveTools: tools,
    liveRun: { id: 'run', ended: false }, streaming: '', thinking: '', showThinking: false, activity: null,
    sessionSnapshot: null, invocations: [], planState: null, unfinished: null,
  };
  c.$ = id => id === 'live' ? live : id === 'chat-announcer' ? announcer : null;
  c.liveRender = { session: 'session', root: live, who: domNode('div', {}, ['Formless']), rows: new Map() };
  c.currentLiveText = () => ({ expanded: false });
  c.isRunning = () => c.S.running.has(c.S.current);
  c.withTranscriptLayout = render => render();
  const animationFrames = [];
  c.requestAnimationFrame = callback => { animationFrames.push(callback); return animationFrames.length; };
  c.flushAnimationFrames = () => { for (const callback of animationFrames.splice(0)) callback(0); };
  c.payload = () => null;
  c.renderSessions = () => {};
  c.note = () => {};
  c.branchSummaryEvent = () => {};
  c.scheduleForecast = () => {};
  c.notifyFor = () => {};
  c.renderPlan = () => {};
  c.renderRunBanner = () => {};
  c.clearLiveText = () => {};
  c.setActivity = () => {};
  c.runningTool = (_name, invocationId) => c.S.liveTools.find(tool => tool.invocationId === invocationId && tool.status === 'running');
  const announcements = [];
  c.announceChat = text => announcements.push(text);
  c.scheduleEventRender = flags => { if (flags.live) c.renderLive({ newContent: !!flags.newContent }); };
  c.forecastRevision = 0;
  c.announcedRun = null;
  c.lastAnnouncedFinishedRun = null;
  c.completedRunAnswer = () => '';
  vm.runInContext(fn('reconcileChildren') + fn('liveToolRow') + fn('updateLiveTool') + fn('focusedLiveTools') + fn('liveTree') + fn('renderLive') + fn('onEvent'), c);
  const createLiveToolRow = c.liveToolRow;
  c.liveToolRow = tool => {
    const row = createLiveToolRow(tool);
    row.node.style = {};
    row.node.dataset = {};
    const depthMarker = row.summary.firstChild;
    depthMarker.getAttribute = key => depthMarker.props[key] ?? null;
    depthMarker.setAttribute = (key, value) => { depthMarker.props[key] = value; };
    row.mark.style = {};
    return row;
  };
  return { c, live, outside, announcements, movedFocusedNodes };
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
  const row = { node: { isConnected: true }, tool: { status: 'running', elapsedMs: null, started: 0 }, status: { data: '' }, timer: { data: '' } };
  c.liveRender = { session: 'session', root: liveElement, activity: { isConnected: false }, rows: new Map([['tool', row]]) };
  c.$ = id => id === 'chat-announcer' ? announcer : id === 'live' ? liveElement : null;
  let elapsed = '1s';
  c.elapsed = () => elapsed;
  vm.runInContext(fn('focusedLiveTools') + fn('onEvent') + fn('updateElapsedTimers'), c);

  c.onEvent({ type: 'run_started', session_id: 'session', run_id: 'run' });
  c.onEvent({ type: 'message_delta', session_id: 'session', delta: 'A token' });
  c.onEvent({ type: 'tool_call_started', session_id: 'session', run_id: 'run', invocation_id: 'call', tool_name: 'read_file', arguments: '{}', depth: 0 });
  c.onEvent({ type: 'message_delta', session_id: 'session', delta: ' at a time' });
  c.updateElapsedTimers();
  assert.equal(row.status.data, 'Running · 1s');
  elapsed = '2s';
  advance(1000);
  c.updateElapsedTimers();
  assert.equal(row.status.data, 'Running · 2s', 'the accessible duration refreshes on elapsed ticks');
  assert.equal(row.timer.data, '2s', 'the live timer rerenders while the run is streaming');
  assert.deepEqual(writes, ['Run started', 'Running read_file']);

  c.onEvent({ type: 'message_delta', session_id: 'session', delta: ' without announcements' });
  elapsed = '3s';
  c.updateElapsedTimers();
  elapsed = '4s';
  advance(1000);
  c.updateElapsedTimers();
  assert.equal(row.status.data, 'Running · 4s');
  assert.deepEqual(writes, ['Run started', 'Running read_file'], 'tokens and timer ticks do not update the announcer');

  c.onEvent({ type: 'message_complete', session_id: 'session', message: { id: 'answer', role: 'assistant', seq: 1, content: 'The complete answer', meta: '{}' } });
  const finished = { type: 'run_finished', session_id: 'session', run_id: 'run', status: 'completed' };
  c.onEvent(finished);
  c.onEvent(finished);
  assert.deepEqual(writes, ['Run started', 'Running read_file', 'Answer finished. The complete answer']);
  assert.deepEqual(announceCalls, ['Run started', 'Running read_file', 'Answer finished. The complete answer']);
});

test('artifact Load more retry reopens its viewer when the original dialog has closed', async () => {
  const { c } = fixture();
  const id = '12345678-1234-1234-1234-123456789012';
  const responses = [
    { text: 'first', end: 5, total_chars: 10, bytes: 10 },
    new Error('offline'),
    { text: 'fresh', end: 5, total_chars: 10, bytes: 10 },
  ];
  const requests = [];
  const viewers = [];
  c.document = { activeElement: null };
  c.request = async request => {
    requests.push(request);
    const response = responses.shift();
    if (response instanceof Error) throw response;
    return response;
  };
  c.openDialog = viewer => { viewer.isConnected = true; viewers.push(viewer); };
  c.closeDialog = viewer => { viewer.isConnected = false; };
  vm.runInContext(fn('openArtifact'), c);
  await c.openArtifact(id);
  const descendants = node => [node, ...node.children.flatMap(child => typeof child === 'object' ? descendants(child) : [])];
  const button = (viewer, label) => descendants(viewer).find(node => node.tag === 'button' && node.children[0] === label);
  const firstViewer = viewers[0];
  const firstPre = descendants(firstViewer).find(node => node.tag === 'pre');
  firstPre.textContent = 'first';
  let retry;
  c.toast = (_message, _error, action) => { retry = action.run; };
  await button(firstViewer, 'Load more').onclick();
  assert.equal(typeof retry, 'function');
  button(firstViewer, 'Close').onclick();
  await retry();
  assert.equal(viewers.length, 2);
  assert.equal(firstViewer.isConnected, false);
  assert.equal(viewers[1].isConnected, true);
  assert.equal(firstPre.textContent, 'first', 'retry does not mutate the detached viewer');
  assert.equal(descendants(viewers[1]).find(node => node.tag === 'pre').children[0], 'fresh');
  assert.deepEqual(requests.map(request => request.offset), [0, 5, 0]);
});

test('live tool disclosure labels arguments and subagent progress separately in every state', () => {
  const { c } = fixture();
  c.document = { createTextNode: data => ({ data }) };
  c.liveRender = { rows: new Map() };
  vm.runInContext(fn('liveToolRow'), c);
  vm.runInContext(fn('updateLiveTool'), c);
  function prepareRow(tool) {
    const row = c.liveToolRow(tool);
    row.node.style = {};
    row.node.dataset = {};
    row.summary.firstChild = row.summary.children[0];
    const depthMarker = row.summary.firstChild;
    depthMarker.getAttribute = key => depthMarker.props[key] ?? null;
    depthMarker.setAttribute = (key, value) => { depthMarker.props[key] = value; };
    row.mark.style = {};
    row.payloadInitialized = true;
    row.payloadArgs = tool.arguments;
    row.payloadSubagent = tool.subagent;
    return row;
  }
  const tool = { status: 'running', name: 'check_timer', version: 0, arguments: '{}', depth: 0, started: 0 };
  const row = prepareRow(tool);

  for (const [status, elapsedMs, expected] of [
    ['running', null, 'Running · 2s'],
    ['ok', 1234, 'Done · 1.2s'],
    ['failed', 5678, 'Failed · 5.6s'],
  ]) {
    tool.status = status;
    tool.elapsedMs = elapsedMs;
    c.updateLiveTool(row, 0);
    const name = accessibleText(row.summary);
    assert.ok(name.startsWith(expected + ', tool: check_timer, arguments: args'), name);
    assert.doesNotMatch(name, /progress:/);
    assert.doesNotMatch(name, /(?:2s|1\.2s|5\.6s)check_timer/);
    assert.equal((name.match(/(?:2s|1\.2s|5\.6s)/g) || []).length, 1, name);
  }
  c.summarizeArgs = () => '';
  tool.status = 'running';
  tool.elapsedMs = null;
  c.updateLiveTool(row, 0);
  assert.doesNotMatch(accessibleText(row.summary), /arguments:/);

  const subagent = { status: 'running', name: 'sub-agent: worker', version: 0, arguments: '', depth: 0, started: 0, subagent: 'child', step: 'step 2: answering' };
  const subagentRow = prepareRow(subagent);
  c.updateLiveTool(subagentRow, 0);
  const subagentName = accessibleText(subagentRow.summary);
  assert.ok(subagentName.startsWith('Running · 2s, tool: sub-agent: worker, progress: step 2: answering'), subagentName);
  assert.doesNotMatch(subagentName, /arguments:/);
});

test('live top-level terminal disclosures retain existing focus without announcing', () => {
  for (const [ok, status] of [[true, 'Done'], [false, 'Failed']]) {
    const tool = { invocationId: 'top-level', status: 'running', name: 'check_timer', version: 0, arguments: '{}', depth: 0, started: 0 };
    const { c, live, announcements } = liveRendererFixture([tool]);
    c.renderLive();
    const row = c.liveRender.rows.get(tool);
    c.document.activeElement = row.summary;

    c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: tool.invocationId, tool_name: tool.name, ok, elapsed_ms: 1234 });

    assert.equal(c.S.running.has('session'), true, 'the run can remain active after the tool ends');
    assert.ok(live.children.includes(row.node), 'the focused live row remains rendered');
    assert.equal(c.document.activeElement, row.summary, 'rendering the terminal state does not move focus');
    assert.ok(accessibleText(row.summary).startsWith(status + ' · 1.2s, tool: check_timer'), accessibleText(row.summary));
    assert.match(row.node.props.class, /\blive\b/, 'this is live-row evidence, not the historical chip');
    const statusRegion = findElement(row.summary, child => child.props?.['data-live-status'] !== undefined);
    assert.ok(statusRegion);
    assert.equal(statusRegion.props.role, undefined);
    assert.equal(statusRegion.props['aria-live'], undefined);
    assert.deepEqual(announcements, [], 'tool completion does not add live-region announcements');

    if (ok) {
      c.onEvent({ type: 'run_finished', session_id: 'session', run_id: 'run', status: 'completed' });
      c.renderLive();
      assert.ok(c.S.liveTools.includes(tool), 'a focused live row survives the run-end state transition');
      assert.ok(live.children.includes(row.node));
      assert.equal(c.document.activeElement, row.summary);
    }
  }
});

test('nested terminal rows remain readable and completion does not steal focus elsewhere', () => {
  for (const [ok, status] of [[true, 'Done'], [false, 'Failed']]) {
    const parent = { invocationId: 'parent', status: 'running', name: 'run_agent', version: 0, arguments: '{}', depth: 0, started: 0 };
    const child = { invocationId: 'nested', status: 'running', name: 'nested_task', version: 0, arguments: '{}', depth: 1, started: 0 };
    const { c, live, announcements } = liveRendererFixture([parent, child]);
    c.renderLive();
    const row = c.liveRender.rows.get(child);
    c.document.activeElement = row.summary;

    c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: child.invocationId, tool_name: child.name, ok, elapsed_ms: 1234 });

    assert.ok(live.children.includes(row.node), 'a nested terminal row remains visible under its running parent');
    assert.equal(c.document.activeElement, row.summary);
    assert.ok(accessibleText(row.summary).includes(status + ' · 1.2s, tool: nested_task'), accessibleText(row.summary));
    assert.deepEqual(announcements, []);
  }

  const tool = { invocationId: 'unfocused', status: 'running', name: 'check_timer', version: 0, arguments: '{}', depth: 0, started: 0 };
  const { c, live, outside } = liveRendererFixture([tool]);
  c.renderLive();
  const row = c.liveRender.rows.get(tool);
  c.document.activeElement = outside;
  c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: tool.invocationId, tool_name: tool.name, ok: true, elapsed_ms: 1234 });
  assert.equal(live.children.includes(row.node), false, 'an unfocused top-level terminal row is not retained');
  assert.equal(c.document.activeElement, outside, 'completion does not steal focus from another control');
});

test('parallel completion removes an obsolete earlier row without moving the focused terminal sibling', () => {
  const earlier = { invocationId: 'earlier', status: 'running', name: 'first_task', version: 0, arguments: '{}', depth: 0, started: 0 };
  const later = { invocationId: 'later', status: 'running', name: 'second_task', version: 0, arguments: '{}', depth: 0, started: 0 };
  const { c, live, announcements, movedFocusedNodes } = liveRendererFixture([earlier, later]);
  c.renderLive();
  const earlierRow = c.liveRender.rows.get(earlier);
  const laterRow = c.liveRender.rows.get(later);
  c.document.activeElement = laterRow.summary;

  c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: later.invocationId, tool_name: later.name, ok: true, elapsed_ms: 1234 });
  assert.equal(c.document.activeElement, laterRow.summary, 'the already-completed focused row remains focused');
  c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: earlier.invocationId, tool_name: earlier.name, ok: true, elapsed_ms: 1234 });

  assert.ok(live.children.includes(laterRow.node));
  assert.equal(live.children.includes(earlierRow.node), false);
  assert.equal(c.document.activeElement, laterRow.summary, 'removing the earlier sibling does not blur the focused row');
  assert.deepEqual(movedFocusedNodes, [], 'reconciliation removes stale siblings before considering moves');
  assert.deepEqual(announcements, [], 'parallel completion adds no live-region announcements');
});

test('a retained terminal row is cleaned up after focus leaves without stealing focus or announcing', () => {
  const tool = { invocationId: 'terminal', status: 'running', name: 'check_timer', version: 0, arguments: '{}', depth: 0, started: 0 };
  const { c, live, outside, announcements } = liveRendererFixture([tool]);
  c.renderLive();
  const row = c.liveRender.rows.get(tool);
  c.document.activeElement = row.summary;
  c.onEvent({ type: 'tool_call_finished', session_id: 'session', run_id: 'run', invocation_id: tool.invocationId, tool_name: tool.name, ok: true, elapsed_ms: 1234 });
  c.onEvent({ type: 'run_finished', session_id: 'session', run_id: 'run', status: 'completed' });

  assert.ok(c.S.liveTools.includes(tool), 'the focused result remains available through run end');
  assert.ok(live.children.includes(row.node));
  const announcementsBeforeFocusout = announcements.length;
  c.document.activeElement = outside;
  live.dispatchEvent({ type: 'focusout', target: row.summary, relatedTarget: outside });
  assert.ok(c.S.liveTools.includes(tool), 'cleanup waits for the focus transition to settle');
  c.flushAnimationFrames();

  assert.equal(c.S.liveTools.includes(tool), false, 'the terminal row is removed from live state');
  assert.equal(live.children.includes(row.node), false, 'the row is no longer visible or tabbable');
  assert.equal(c.document.activeElement, outside, 'cleanup does not move focus');
  assert.equal(announcements.length, announcementsBeforeFocusout, 'focus cleanup adds no live-region announcement');
});

test('toast dismissal, timer pause, and retry actions are accessible', async () => {
  const { c, root, timers, advance } = fixture();
  c.toast('offline', true, { label: 'Retry', run: () => {} });
  const error = root.children.at(-1);
  assert.equal(error.props.role, 'alert');
  assert.equal(timers.size, 1, 'errors only schedule their delayed message insertion');
  advance(0);
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

test('toast live message is populated after its empty status node is mounted', () => {
  const { c, root, advance } = fixture();
  const mountedMessages = [];
  const append = root.append.bind(root);
  root.append = child => {
    mountedMessages.push(child.children[0].children.length);
    child.isConnected = true;
    append(child);
  };
  c.toast('Saved', false);
  const toast = root.children.at(-1);
  assert.deepEqual(mountedMessages, [0]);
  assert.equal(toast.children[0].children.length, 0);
  advance(0);
  assert.equal(toast.children[0].textContent, 'Saved');
});
