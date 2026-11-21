// Daemon wire, consent, stale-reply and harness-routing regression coverage.
// Exercise shipped functions with controlled protocol replies, without UI mocks
// replacing any orchestration action under test.
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('./web/app.js', import.meta.url), 'utf8');
function fn(name) {
  const start = source.search(new RegExp('  (?:async )?function ' + name + '\\('));
  assert.ok(start >= 0, name);
  return source.slice(start, source.indexOf('\n  }', start) + 4);
}
const topic = (status = 'pending_approval') => ({ id: 'topic', name: 'Formless', workspace: null, workspace_kind: 'scratch', parent_topic_id: null,
  source_session_id: 'main', topic_session_id: status === 'pending_approval' ? null : 'side', status,
  outline: [{ title: 'Implement', harness: 'claude', access: 'write' }, { title: 'Review', harness: 'codex', access: 'read_only' }],
  features: [{ id: 'x', request: 'Add X', status: 'ready', outputs: [{ kind: 'git_revision', revision: 'abc123' }], attempt_count: 2 },
    { id: 'y', request: 'Add Y', status: 'running', outputs: [] }],
  attempts: [{ id: 'build-x', step_title: 'Implement', access: 'write', harness: 'claude', conversation_session_id: 'build-transcript', harness_session_id: 'external-native-id', feature_id: 'x' },
    { id: 'review-x', step_title: 'Review', access: 'read_only', harness: 'codex', conversation_session_id: 'review-transcript', feature_id: 'x' }] });
const defer = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };
function setup(names, extra = {}) {
  const c = vm.createContext({ S: { current: 'main', mainChat: { id: 'main' }, sessions: [], topicsAvailable: true,
    topics: [topic()], channel: {}, daemonOnline: true, messages: [], sessionLoad: { id: 'main', status: 'loaded' } },
    toast() {}, note() {}, renderSessions() {}, renderComposer() {}, renderTopicContext() {}, renderTopicNavigation() {},
    refreshSessions: async () => {}, refreshTopics: async () => {}, openSession: async id => { c.S.current = id; },
    setView() {}, closeSidebar() {}, ...extra });
  vm.runInContext(names.map(fn).join('\n'), c);
  return c;
}
const json = value => JSON.parse(JSON.stringify(value));

test('main/topic/attempt hierarchy uses Formless transcript IDs, not external harness IDs', () => {
  const c = setup(['topicSessionIds', 'topicForSession', 'currentTopicAttempt']);
  c.S.topics = [topic('running')];
  assert.deepEqual([...c.topicSessionIds()], ['main', 'side', 'build-transcript', 'review-transcript']);
  c.S.current = 'review-transcript';
  assert.equal(c.topicForSession().id, 'topic');
  assert.equal(c.currentTopicAttempt().harness, 'codex');
  c.S.current = 'side';
  assert.equal(c.currentTopicAttempt(), undefined);
  assert.equal(c.topicForSession('external-native-id'), undefined);
});

test('approval moves first, opens the real topic, then asks the daemon to start', async () => {
  const calls = [], approved = defer();
  const c = setup(['topicAction', 'approveTopic', 'storeTopic', 'openTopic'], {
    request: async req => { calls.push(json(req)); return req.type === 'approve_topic' ? approved.promise : topic('running'); },
    refreshSessions: async () => calls.push('refresh-sessions'), openSession: async id => { calls.push(['open', id]); c.S.current = id; } });
  const pending = c.approveTopic(topic());
  assert.deepEqual(calls, [{ type: 'approve_topic', approval: { topic_id: 'topic' } }]);
  // Rapid repeated clicks do not approve/start the same topic twice.
  await c.approveTopic(topic());
  assert.equal(calls.length, 1);
  approved.resolve(topic('approved'));
  await pending;
  assert.deepEqual(calls.slice(1), ['refresh-sessions', ['open', 'side'], { type: 'start_topic_workflow', topic_id: 'topic' }]);
  assert.equal(c.S.topics[0].status, 'running');
  assert.equal(c.S.topicBusy, null);
});

test('failed launch retains the moved, approved topic so Start workflow can retry', async () => {
  const errors = [], calls = [];
  const c = setup(['topicAction', 'approveTopic', 'storeTopic', 'openTopic'], {
    toast: message => errors.push(message), request: async req => {
      calls.push(req.type);
      if (req.type === 'approve_topic') return topic('approved');
      throw new Error('Claude is not installed');
    } });
  await c.approveTopic(topic());
  assert.deepEqual(calls, ['approve_topic', 'start_topic_workflow']);
  assert.equal(c.S.topics[0].status, 'approved');
  assert.equal(c.S.current, 'side');
  assert.deepEqual(errors, ['Claude is not installed']);
});

test('reject keeps requests in place and never starts a workflow', async () => {
  const calls = [];
  const c = setup(['topicAction', 'rejectTopic', 'storeTopic'], { request: async req => { calls.push(json(req)); return topic('rejected'); } });
  await c.rejectTopic(topic());
  assert.equal(c.S.current, 'main');
  assert.equal(c.S.topics[0].status, 'rejected');
  assert.deepEqual(calls.map(req => req.type), ['reject_topic']);
});

test('feature retry requires consent and resets just that feature before restarting orchestration', async () => {
  const calls = [];
  const c = setup(['topicAction', 'retryTopicFeature', 'storeTopic'], { ask: async () => false,
    request: async req => { calls.push(json(req)); return topic('running'); } });
  await c.retryTopicFeature(topic(), topic().features[1]);
  assert.equal(calls.length, 0);
  c.ask = async () => true;
  await c.retryTopicFeature(topic(), topic().features[1]);
  assert.deepEqual(calls, [{ type: 'retry_topic_feature', retry: { topic_id: 'topic', feature_id: 'y' } },
    { type: 'start_topic_workflow', topic_id: 'topic' }]);
});

test('polling ignores disconnected replies, retains known status on error, and coalesces reads', async () => {
  const wait = defer(), calls = [];
  const c = setup(['refreshTopics'], { request: async req => { calls.push(req.type); return wait.promise; } });
  vm.runInContext('let topicRefresh = null;', c);
  const first = c.refreshTopics(), second = c.refreshTopics();
  assert.deepEqual(calls, ['list_topics']);
  c.S.channel = {};
  wait.resolve([topic('complete')]);
  await Promise.all([first, second]);
  assert.equal(c.S.topics[0].status, 'pending_approval');
  c.request = async () => { throw new Error('offline'); };
  await c.refreshTopics();
  assert.equal(c.S.topicsAvailable, true);
  assert.equal(c.S.topicsError, true);
  assert.equal(c.S.topics.length, 1);
  await c.refreshTopics(true);
  assert.equal(c.S.topicsAvailable, true, 'transient startup failure keeps known topics');
  c.request = async () => { throw Object.assign(new Error('unknown request'), { daemonResponse: true, code: 'bad_request' }); };
  await c.refreshTopics(true);
  assert.equal(c.S.topicsAvailable, false, 'older daemons retain the existing Sessions UI');
  assert.equal(c.S.topicsUnsupported, true);
  assert.equal(c.S.mainChat, null);
});

test('transient first topic load is retried without reconnecting', async () => {
  const calls = [];
  const c = setup(['refreshTopics'], { request: async () => { throw new Error('request timed out'); } });
  vm.runInContext('let topicRefresh = null;', c);
  c.S.mainChat = null; c.S.topics = []; c.S.topicsAvailable = false; c.S.topicsUnsupported = true;
  await c.refreshTopics(true);
  assert.equal(c.S.topicsUnsupported, false, 'a reconnect retries discovery even after an older daemon');
  c.request = async req => { calls.push(req.type); return req.type === 'ping' ? { features: ['topic_orchestration_v2'] } : req.type === 'get_main_chat' ? { id: 'main' } : [topic()]; };
  await c.refreshTopics();
  assert.deepEqual(calls, ['ping', 'get_main_chat', 'list_topics']);
  assert.equal(c.S.topicsAvailable, true);
  assert.equal(c.S.topicsUnsupported, false);
});

// Small structural DOM for the proposal editor and recursive navigation. No
// orchestration RPCs are replaced by UI logic in these tests.
function element(tag, props, ...children) {
  props ||= {};
  return { tag, props, children: children.flat(Infinity).filter(v => v != null && v !== false), value: props.value || '',
    classList: { add() {}, remove() {}, toggle() {} }, focus() {},
    replaceChildren(...kids) { this.children = kids.flat(Infinity).filter(v => v != null && v !== false); } };
}
function descendants(node) { return node && typeof node === 'object' ? [node, ...(node.children || []).flatMap(descendants)] : []; }
function press(node, name) {
  const button = descendants(node).find(item => item.tag === 'button' && item.children.includes(name));
  assert.ok(button, name); (button.onclick || button.props.onclick)();
}

test('v1 capability falls back without submitting a v2 request to a legacy daemon', async () => {
  const calls = [];
  const c = setup(['refreshTopics'], { request: async req => { calls.push(req.type); return { features: ['topic_orchestration_v1'] }; } });
  vm.runInContext('let topicRefresh = null;', c);
  await c.refreshTopics(true);
  assert.deepEqual(calls, ['ping']);
  assert.equal(c.S.topicsAvailable, false);
  assert.equal(c.S.topicsUnsupported, true);
});

test('outline is optional, editable and supports any harness on any step including the same harness', () => {
  const c = setup(['topicOutlineEditor', 'harnessLabel', 'recordValue'], { h: element });
  const editor = c.topicOutlineEditor();
  assert.deepEqual(json(editor.read()), []);
  press(editor.element, 'Accounting preset');
  assert.deepEqual(json(editor.read()).map(step => step.title), ['Scan receipts', 'Do accounting', 'Create reports']);
  assert.ok(editor.read().every(step => step.harness === null));
  press(editor.element, 'Code preset');
  let fields = descendants(editor.element);
  const harnesses = fields.filter(node => node.props?.['aria-label'] === 'Step harness');
  assert.deepEqual(harnesses[0].children.map(node => node.props.value), ['', 'claude', 'codex', 'pi', 'opencode', 'antigravity', 'acp', 'muse']);
  for (const kind of ['claude', 'codex', 'pi', 'opencode', 'antigravity', 'muse']) {
    harnesses.forEach(node => { node.value = kind; });
    assert.ok(editor.read().every(step => step.harness === kind && step.acp_agent === null));
  }
  harnesses[0].value = 'acp'; harnesses[0].onchange();
  assert.throws(() => editor.read(), /ACP agent/);
  fields.find(node => node.props?.['aria-label'] === 'ACP agent').value = 'local-agent';
  assert.equal(editor.read()[0].acp_agent, 'local-agent');
  fields.find(node => node.props?.['aria-label'] === 'Step instructions').value = 'Keep exact instructions';
  const down = fields.find(node => node.props?.['aria-label'] === 'Move step 1 down'); down.props.onclick();
  assert.equal(editor.read()[1].instructions, 'Keep exact instructions');
  press(editor.element, 'Remove');
  assert.equal(editor.read().length, 1);
  press(editor.element, 'Let Formless plan');
  assert.deepEqual(json(editor.read()), []);
});

test('topic navigation nests daemon parents at arbitrary levels and guards bad cycles', () => {
  let rendered;
  const nodes = new Map();
  const c = setup(['renderTopicNavigation', 'topicForSession', 'currentTopicAttempt', 'harnessLabel', 'topicStatusLabel'], {
    h: element, $: id => { if (!nodes.has(id)) nodes.set(id, element('nav')); return nodes.get(id); },
    replaceTopicContent: (_, signature, children) => { rendered = element('nav', {}, children); } });
  c.S.topics = Array.from({ length: 12 }, (_, i) => ({ ...topic('running'), id: 't' + i, name: 'Level ' + i,
    parent_topic_id: i ? 't' + (i - 1) : null, attempts: [] }));
  c.renderTopicNavigation();
  let node = descendants(rendered).find(item => item.props?.['data-topic-id'] === 't0');
  for (let i = 1; i < 12; i++) {
    node = descendants(node).find(item => item.props?.['data-topic-id'] === 't' + i);
    assert.ok(node, 'level ' + i + ' remains nested');
  }
  c.S.topics[0].parent_topic_id = 't11';
  c.renderTopicNavigation();
  assert.equal(descendants(rendered).filter(item => item.props?.['data-topic-id']).length, 12);
});

test('a Main Chat proposal can choose a deeply nested parent without a repository', async () => {
  let request;
  const fields = new Map();
  const c = setup(['proposeTopic', 'recordValue', 'harnessLabel', 'topicPath', 'topicOutlineEditor'], {
    currentTopicAttempt: () => null, prefs: { get: () => ({}), set() {} }, storeTopic() {}, openTopic() {},
    h: (...args) => { const node = element(...args); if (node.props?.['aria-label']) fields.set(node.props['aria-label'], node); return node; },
    ask: async opts => { fields.get('Topic name').value = 'Reports'; fields.get('Parent topic').value = 'story'; request = json(opts.saveRequest()); return topic(); } });
  c.S.topics = [{ ...topic('approved'), id: 'project', name: 'Project' }, { ...topic('approved'), id: 'story', parent_topic_id: 'project', name: 'Story' }];
  c.S.messages = [{ id: 'm', role: 'user', content: 'Prepare monthly reports' }];
  await c.proposeTopic();
  assert.equal(request.proposal.parent_topic_id, 'story');
  assert.equal(request.proposal.workspace, null);
  assert.deepEqual(request.proposal.outline, []);
  assert.equal(c.topicPath(c.S.topics[1]), 'Project / Story');
});

test('cancel is explicit, preserves daemon results, and never launches new work', async () => {
  const calls = [];
  const c = setup(['cancelTopic', 'topicAction', 'storeTopic'], { ask: async () => false,
    request: async req => { calls.push(json(req)); return topic('cancelled'); } });
  await c.cancelTopic(topic('running'));
  assert.equal(calls.length, 0);
  c.ask = async () => true;
  await c.cancelTopic(topic('running'));
  assert.deepEqual(calls, [{ type: 'cancel_topic', topic_id: 'topic' }]);
  assert.equal(c.S.topics[0].status, 'cancelled');
});

test('permissions preserve native choices, require exact explicit decisions, and coalesce double clicks', async () => {
  const calls = [], wait = defer();
  const c = setup(['addApproval', 'decideHarnessPermission', 'decide'], { renderApproval() {}, renderStatus() {},
    request: async req => { calls.push(json(req)); return wait.promise; } });
  c.S.approvals = [];
  const ev = { type: 'harness_permission_requested', permission_id: 'p', topic_id: 'topic', attempt_id: 'a', session_id: 'transcript',
    harness: 'acp', acp_agent: 'local', title: 'Write file?', options: [{ option_id: 'allow-custom', name: 'Allow writing', kind: 'allow_always' }] };
  c.addApproval(ev); c.addApproval(ev);
  const req = c.S.approvals[0];
  assert.equal(c.S.approvals.length, 1);
  assert.deepEqual(json(req.options), ev.options);
  await c.decide(true, 'always');
  await c.decideHarnessPermission(req, 'allow');
  assert.equal(calls.length, 0, 'generic shortcuts and invented options cannot grant access');
  const decision = c.decideHarnessPermission(req, 'allow-custom');
  await c.decideHarnessPermission(req, 'allow-custom');
  assert.deepEqual(calls, [{ type: 'harness_permission_decision', permission_id: 'p', option_id: 'allow-custom' }]);
  wait.resolve({ answered: true }); await decision;
  assert.equal(c.S.approvals.length, 0);
});

test('permission failures stay pending and old-connection results cannot remove reconnected requests', async () => {
  const wait = defer();
  const c = setup(['decideHarnessPermission'], { renderApproval() {}, renderStatus() {}, request: async () => { throw new Error('offline'); } });
  const req = { kind: 'harness', id: 'p', options: [] }; c.S.approvals = [req];
  await c.decideHarnessPermission(req, null);
  assert.equal(c.S.approvals[0], req); assert.equal(req.deciding, false);
  c.request = () => wait.promise;
  const pending = c.decideHarnessPermission(req, null);
  c.S.channel = {}; const reconnected = { ...req }; c.S.approvals = [reconnected];
  wait.resolve({ answered: true }); await pending;
  assert.equal(c.S.approvals[0], reconnected);
});

test('authoritative snapshots replay permissions and racing closure cannot resurrect one', () => {
  const c = setup(['applySessionLiveState', 'addApproval'], { showQuestion() {}, renderApproval() {}, renderStatus() {}, cancelAsk() {} });
  c.S.approvals = []; c.S.running = new Set(); c.S.questions = [];
  const ev = { type: 'harness_permission_requested', permission_id: 'p', session_id: 'transcript', options: [{ option_id: 'deny', kind: 'reject_once' }] };
  c.applySessionLiveState('main', { running: false, pending_events: [ev] }, []);
  assert.equal(c.S.approvals[0].id, 'p');
  c.applySessionLiveState('main', { running: false, pending_events: [ev] }, [{ type: 'harness_permission_closed', permission_id: 'p' }]);
  assert.equal(c.S.approvals.length, 0);
});

test('a topic snapshot from before rejection cannot restore a rejected proposal', async () => {
  const reads = [];
  const c = setup(['refreshTopics', 'topicAction', 'storeTopic', 'rejectTopic'], { request: req => {
    if (req.type === 'reject_topic') return Promise.resolve(topic('rejected'));
    const wait = defer(); reads.push(wait); return wait.promise;
  } });
  vm.runInContext('let topicRefresh = null;', c);
  const oldRead = c.refreshTopics();
  await c.rejectTopic(topic());
  assert.equal(c.S.topics[0].status, 'rejected');
  const newRead = c.refreshTopics();
  assert.equal(reads.length, 2, 'a mutation prevents coalescing into the old read');
  reads[0].resolve([topic()]);
  await oldRead;
  assert.equal(c.S.topics[0].status, 'rejected');
  reads[1].resolve([topic('rejected')]);
  await newRead;
  assert.equal(c.S.topics[0].status, 'rejected');
});

test('nested harness transcripts cannot dispatch native send, attachment, edit or fork actions', async () => {
  const c = setup(['topicForSession', 'currentTopicAttempt', 'submit', 'attachFiles', 'beginEdit', 'forkSession', 'stop', 'ensureRunTarget', 'retryRun', 'resumeRun', 'setSessionModel', 'openModelPicker'], {
    $: () => { throw new Error('Read-only action touched the composer'); },
    act: () => { throw new Error('Read-only action mutated a session'); } });
  c.S.current = 'build-transcript';
  await c.submit(false);
  await c.attachFiles([{ name: 'secret.txt' }]);
  c.beginEdit({ id: 'x', content: 'rewrite' });
  await c.forkSession(c.S.current);
  c.stop();
  assert.equal(c.ensureRunTarget({ session: c.S.current }), false);
  await c.retryRun('', { session: c.S.current });
  await c.resumeRun();
  await c.setSessionModel('native-model');
  await c.openModelPicker();
});

test('attempt snapshot polling refreshes only the open transcript and rejects late navigation replies', async () => {
  const replies = [], rendered = [];
  const c = setup(['topicForSession', 'currentTopicAttempt', 'refreshAttemptTranscript'], {
    request: req => { const wait = defer(); replies.push({ req: json(req), wait }); return wait.promise; },
    renderHistory: opts => rendered.push(opts),
  });
  vm.runInContext('let attemptRefresh = null; let sessionSeq = 1;', c);
  c.S.current = 'build-transcript'; c.S.sessionLoad = { id: c.S.current, status: 'loaded' };
  const first = c.refreshAttemptTranscript(), duplicate = c.refreshAttemptTranscript();
  assert.deepEqual(replies.map(item => item.req), [{ type: 'get_session', id: 'build-transcript' }]);
  replies[0].wait.resolve({ messages: [{ id: 'm', content: 'Claude progress' }] });
  await Promise.all([first, duplicate]);
  assert.equal(c.S.messages[0].content, 'Claude progress');
  assert.equal(rendered.length, 1);
  const stale = c.refreshAttemptTranscript();
  c.S.current = 'main'; c.S.messages = [];
  replies[1].wait.resolve({ messages: [{ id: 'old', content: 'old attempt' }] });
  await stale;
  assert.equal(c.S.messages.length, 0);
  assert.equal(rendered.length, 1);
});

test('topic panel shows independently ready work items and generic outputs', () => {
  const root = { classList: { toggle() {} } }; let nodes;
  const c = setup(['topicForSession', 'currentTopicAttempt', 'topicStatusLabel', 'harnessLabel', 'topicBadge', 'topicWorkflowLabel', 'topicPath', 'topicArtifacts', 'renderTopicContext'], {
    $: () => root, h: (tag, props, ...children) => ({ tag, props, children }),
    replaceTopicContent: (_, signature, children) => { nodes = children; } });
  c.S.topics = [topic('running')]; c.S.current = 'side';
  c.renderTopicContext();
  const content = JSON.stringify(nodes);
  assert.match(content, /Ready for you/);
  assert.match(content, /In progress/);
  assert.match(content, /abc123/);
  assert.match(content, /nothing merges automatically/);
  assert.doesNotMatch(content, /Move & start/);
});

test('proposal request binds selected messages and workflow without moving or starting work', async () => {
  const calls = [], fields = new Map();
  const c = setup(['proposeTopic', 'recordValue', 'harnessLabel', 'topicPath', 'topicOutlineEditor'], {
    currentTopicAttempt: () => null, prefs: { get: () => ({}), set() {} }, storeTopic() {}, openTopic() {},
    h: (tag, props, ...children) => {
      props ||= {};
      const node = { tag, props, children, value: props.value || '', focus() {}, replaceChildren(...kids) { this.children = kids; } };
      if (tag === 'select') node.value = children.flat()[0]?.props.value;
      if (props['aria-label']) fields.set(props['aria-label'], node);
      return node;
    },
    ask: async opts => {
      fields.get('Topic name').value = 'Accounts';
      calls.push(json(opts.saveRequest()));
      return topic();
    } });
  c.S.messages = [{ id: 'x', role: 'user', content: 'Add X' }, { id: 'a', role: 'assistant', content: 'Sure' }, { id: 'y', role: 'user', content: 'Add Y' }];
  await c.proposeTopic('x');
  assert.deepEqual(calls, [{ type: 'propose_topic', proposal: { source_session_id: 'main', message_ids: ['x'], name: 'Accounts', parent_topic_id: null, workspace: null, outline: [] } }]);
});

test('existing topic chats surface proposals and preview independent features from one source message', () => {
  let nodes;
  const c = setup(['topicForSession', 'currentTopicAttempt', 'topicStatusLabel', 'harnessLabel', 'topicBadge', 'topicWorkflowLabel', 'topicPath', 'topicArtifacts', 'renderTopicContext'], {
    $: () => ({ classList: { toggle() {} } }), h: (tag, props, ...children) => ({ tag, props, children }),
    replaceTopicContent: (_, signature, children) => { nodes = children; } });
  const proposal = { ...topic(), id: 'new-topic', name: 'New focused work', source_session_id: 'side', attempts: [], features: [
    { id: 'x', source_message_id: 'xy', request: 'Build feature X', status: 'pending' },
    { id: 'y', source_message_id: 'xy', request: 'Build feature Y', status: 'pending' }] };
  c.S.topics = [topic('running'), proposal]; c.S.current = 'side';
  c.renderTopicContext();
  assert.match(JSON.stringify(nodes), /Review proposal/);
  c.S.topicFocus = 'new-topic';
  c.renderTopicContext();
  const preview = JSON.stringify(nodes);
  assert.match(preview, /New focused work/);
  assert.match(preview, /Move & start/);
  assert.match(preview, /Keep in original chat/);
  assert.match(preview, /Build feature X/);
  assert.match(preview, /Build feature Y/);
});
