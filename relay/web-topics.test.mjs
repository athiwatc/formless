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
const topic = (status = 'pending_approval') => ({ id: 'topic', name: 'Formless', repo: '/repos/formless',
  source_session_id: 'main', topic_session_id: status === 'pending_approval' ? null : 'side', status,
  builder: 'claude', reviewer: 'codex', features: [{ id: 'x', request: 'Add X', status: 'ready', ready_revision: 'abc123' },
    { id: 'y', request: 'Add Y', status: 'building' }],
  attempts: [{ id: 'build-x', role: 'build', harness: 'claude', conversation_session_id: 'build-transcript', harness_session_id: 'external-native-id', feature_id: 'x' },
    { id: 'review-x', role: 'review', harness: 'codex', conversation_session_id: 'review-transcript', feature_id: 'x' }] });
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
  assert.equal(c.S.topicsAvailable, false, 'older daemons retain the existing Sessions UI');
  assert.equal(c.S.mainChat, null);
});

test('nested harness transcripts cannot dispatch native send, attachment, edit or fork actions', async () => {
  const c = setup(['topicForSession', 'currentTopicAttempt', 'submit', 'attachFiles', 'beginEdit', 'forkSession', 'stop', 'ensureRunTarget', 'retryRun', 'resumeRun'], {
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

test('topic panel shows independently ready features and exact reviewed revision', () => {
  const root = { classList: { toggle() {} } }; let nodes;
  const c = setup(['topicForSession', 'currentTopicAttempt', 'topicStatusLabel', 'harnessLabel', 'topicBadge', 'topicWorkflowLabel', 'renderTopicContext'], {
    $: () => root, h: (tag, props, ...children) => ({ tag, props, children }),
    replaceTopicContent: (_, signature, children) => { nodes = children; } });
  c.S.topics = [topic('running')]; c.S.current = 'side';
  c.renderTopicContext();
  const content = JSON.stringify(nodes);
  assert.match(content, /Ready for you/);
  assert.match(content, /Building/);
  assert.match(content, /abc123/);
  assert.match(content, /nothing merges automatically/);
  assert.doesNotMatch(content, /Move & start/);
});

test('proposal request binds selected messages and workflow without moving or starting work', async () => {
  const calls = [], fields = new Map();
  const c = setup(['proposeTopic', 'recordValue', 'harnessLabel'], {
    currentTopicAttempt: () => null, prefs: { get: () => ({}), set() {} }, storeTopic() {}, openTopic() {},
    h: (tag, props, ...children) => {
      props ||= {};
      const node = { tag, props, children, value: props.value || '', focus() {} };
      if (tag === 'select') node.value = children.flat()[0]?.props.value;
      if (props['aria-label']) fields.set(props['aria-label'], node);
      return node;
    },
    ask: async opts => {
      fields.get('Topic name').value = 'Formless'; fields.get('Repository on your machine').value = '/repos/formless';
      calls.push(json(opts.saveRequest()));
      return topic();
    } });
  c.S.messages = [{ id: 'x', role: 'user', content: 'Add X' }, { id: 'a', role: 'assistant', content: 'Sure' }, { id: 'y', role: 'user', content: 'Add Y' }];
  await c.proposeTopic('x');
  assert.deepEqual(calls, [{ type: 'propose_topic', proposal: { source_session_id: 'main', message_ids: ['x'], name: 'Formless', repo: '/repos/formless',
    builder: 'claude', builder_model: null, reviewer: 'codex', reviewer_model: null } }]);
});

test('existing topic chats surface proposals and preview independent features from one source message', () => {
  let nodes;
  const c = setup(['topicForSession', 'currentTopicAttempt', 'topicStatusLabel', 'harnessLabel', 'topicBadge', 'topicWorkflowLabel', 'renderTopicContext'], {
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
