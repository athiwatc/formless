import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
function fn(name){const start=source.indexOf('  function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
function asyncFn(name){const start=source.indexOf('  async function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
function transcriptScrollHandler(){const marker='  $("transcript").addEventListener("scroll", () => {';const start=source.indexOf(marker);assert.ok(start>=0);const end=source.indexOf('\n  });',start);assert.ok(end>=0);return source.slice(start,end+'\n  });'.length);}
const text='A distinct streamed paragraph. '.repeat(20);
const stamp=()=>({block:1,length:text.length,head:text.slice(0,128),tail:text.slice(-128),afterSeq:10});
function fixture(){const roots=[],live={querySelectorAll:()=>[]};const box={contains:n=>roots.some(r=>r===n||r.blocks.includes(n)),querySelector:()=>live,querySelectorAll:()=>roots};
 const c=vm.createContext({S:{messages:[]},reading:{session:'A',anchor:null,saved:new Map()},$:()=>box,anchorBlocks:'p'});
 vm.runInContext(['readingStreamMatches','remapReadingStream','commitReadingStream','resolveReadingBlock'].map(fn).join('\n'),c);return{c,roots,live};}
test('missing semantic block never resolves to an empty live or history root',()=>{
 const{c,roots,live}=fixture();const history={dataset:{messageId:'old'},blocks:[],querySelectorAll(){return this.blocks;}};roots.push(history);
 assert.equal(c.resolveReadingBlock({live:true,block:4}),undefined);
 assert.equal(c.resolveReadingBlock({live:false,id:'old',block:4}),undefined);
 assert.equal(c.resolveReadingBlock({live:true,block:-1}),live);
 assert.equal(c.resolveReadingBlock({live:false,id:'old',block:-1}),history);
});
test('saved live bookmark remaps to its first matching committed step, excluding old and unrelated messages',()=>{
 const{c,roots}=fixture();const blocks=[{},{}];const history={dataset:{messageId:'step'},blocks,querySelectorAll(){return this.blocks;}};roots.push(history);
 c.S.messages=[{id:'old',role:'assistant',seq:9,content:text},{id:'other',role:'assistant',seq:11,content:'unrelated'},
  {id:'step',role:'assistant',seq:12,content:text+'Later output.'},{id:'next',role:'assistant',seq:14,content:text}];
 const candidate={live:true,block:8,node:null,stream:stamp()};assert.equal(c.resolveReadingBlock(candidate),blocks[1]);
 assert.equal(candidate.id,'step');assert.equal(candidate.live,false);assert.equal(candidate.block,1);
});
test('background commit maps only that Session bookmark and never retargets an already committed anchor',()=>{
 const{c}=fixture();const a={live:true,stream:stamp()},b={live:true,stream:stamp()};c.reading.session='B';c.reading.anchor={candidates:[b]};c.reading.saved.set('A',{anchor:{candidates:[a]}});
 c.commitReadingStream('A',{id:'step',role:'assistant',seq:11,content:text});assert.equal(a.id,'step');assert.equal(b.live,true);
 c.commitReadingStream('A',{id:'next',role:'assistant',seq:12,content:text});assert.equal(a.id,'step');
 assert.equal(c.readingStreamMatches({role:'tool',seq:11,content:text},stamp()),false);
 assert.equal(c.readingStreamMatches({role:'assistant',seq:11,content:'x'+text.slice(1)},stamp()),false);
});
test('hover-only visibility does not schedule recovery; actual removal is coalesced and respects intervening focus',()=>{
 const body={},frames=[],returns=[];const c=vm.createContext({document:{body,activeElement:body},requestAnimationFrame:cb=>{frames.push(cb);return frames.length;},focusReturn:target=>returns.push(target)});
 vm.runInContext('let focusRecoveryFrame=0,lastFocusTarget=null;\n'+['focusRemovedByState','scheduleFocusRecovery'].map(fn).join('\n')+'\nfunction target(value){lastFocusTarget=value;}',c);
 const hover={isConnected:true,closest:()=>null,getClientRects(){throw new Error('unexpected layout read');}};c.target(hover);c.scheduleFocusRecovery();assert.equal(frames.length,0);
 const removed={isConnected:false};c.target(removed);c.scheduleFocusRecovery();c.scheduleFocusRecovery();assert.equal(frames.length,1);
 c.document.activeElement={};frames.shift()();assert.equal(returns.length,0);
 c.document.activeElement=body;c.scheduleFocusRecovery();frames.shift()();assert.equal(returns[0],removed);
});

function captureFixture(rows, bounds, visible = true) {
  const reads = [];
  let queries = 0;
  let currentBounds = bounds;
  const roots = [];
  const rowData = [];
  const pendingGeometry = [];
  const contained = new Set();
  const listeners = new Map();
  const frames = new Map();
  let isVisible = visible;
  let frameId = 0;
  let settles = 0;
  let box;
  const transcriptTop = 10;
  const makeRoot = (row, index) => {
    rowData.push(row);
    const rect = () => ({ top: transcriptTop + row.top - box.scrollTop + reading.adjustment,
      bottom: transcriptTop + row.bottom - box.scrollTop + reading.adjustment, height: row.height });
    const inner = { getBoundingClientRect: () => { const r = rect(); return { ...r, top: r.top + (row.blockOffset ?? 2) }; },
      getClientRects: () => row.height ? [{}] : [], contains: () => false };
    const block = { getBoundingClientRect: rect, getClientRects: () => row.height ? [{}] : [], contains: child => child === inner };
    let root;
    const streamText = { nodeType: 3, parentElement: { closest: selector => selector === "[data-message-id], [data-live-root]" ? root : null } };
    root = {
      dataset: row.live ? {} : { messageId: String(row.id ?? index) },
      hasAttribute: attr => row.live && attr === "data-live-root",
      getBoundingClientRect: () => { reads.push(index); return rect(); },
      querySelectorAll: () => [block, inner],
    };
    if (row.live) root.streamText = streamText;
    return root;
  };
  rows.forEach((row, index) => roots.push(makeRoot(row, index)));
  const reading = { session: "A", saved: new Map(), anchor: null, unseen: new Map(), unseenIds: new Set(), deferred: new Map(),
    pendingContent: false, pendingLive: false, afterSeq: -1, depth: 0, changed: false, adjustment: 0,
    intentUntil: 0, inputFrame: 0, observed: new Set(), roots: null, rootTree: null, rootTreeSize: 0 };
  box = {
    scrollTop: 500, scrollHeight: 500, getBoundingClientRect: () => ({ top: transcriptTop }),
    querySelectorAll: () => { queries++; return roots; },
    contains: node => roots.includes(node) || contained.has(node),
    addEventListener: (type, listener, options) => { listeners.set(type, [...(listeners.get(type) || []), { listener, options }]); },
    classList: { add() {}, toggle() {} },
    style: { setProperty: () => pendingGeometry.push({ type: "attributes", target: box, attributeName: "style" }) },
  };
  const c = vm.createContext({
    S: { current: "A", messages: [], follow: true }, reading,
    readingVisible: () => isVisible, $: () => box, visibleReadingBounds: () => currentBounds,
    anchorBlocks: "p, li", readingStreamStamp: message => message.hasAttribute("data-live-root") ? "live" : null,
    readingRootChanges: { takeRecords: () => pendingGeometry.splice(0) },
    readingChanges: { takeRecords: () => [] }, readingResize: { observe() {}, unobserve() {} },
    scheduleViewportSync() {}, viewportGeometryPending: false,
    markNewOutput() {}, updateReadingPill() {}, settleReadingLayout() { settles++; },
    performance: { now: () => 0 }, Node: { TEXT_NODE: 3 },
    requestAnimationFrame(callback) { const id = ++frameId; frames.set(id, callback); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
  });
  vm.runInContext(["invalidateReadingRoots", "setReadingRootMaximum", "markReadingRootDirty", "markLiveReadingRootDirty",
    "readingGeometryChanged", "readingRoots", "refreshReadingRoot", "firstReadingRoot", "captureReadingAnchor",
    "readingStreamMatches", "remapReadingStream", "resolveReadingBlock", "writeReadingTop", "restoreReadingAnchor",
    "readingInput",
    "observeReadingNodes", "readingDiagramLoaded", "readingDetailsWillToggle", "watchReadingDetails", "readingResizeChanged",
    "setReadingAdjustment", "resetReadingSession", "withTranscriptLayout", "scheduleViewportGeometrySync", "syncViewport"].map(fn).join("\n"), c);
  vm.runInContext(transcriptScrollHandler(), c);
  c.watchReadingDetails(box);
  return {
    capture(nextBounds, scrollTop) {
      if (nextBounds) currentBounds = nextBounds;
      if (scrollTop !== undefined) box.scrollTop = scrollTop;
      const result = c.captureReadingAnchor();
      return {
        top: result?.top,
        candidates: result && Array.from(result.candidates, ({ id, block, offset, stream }) => ({ id, block, offset, stream })),
      };
    },
    append(row) { roots.push(makeRoot(row, roots.length)); pendingGeometry.push({ type: "childList", target: box }); },
    setRow(index, row) { Object.assign(rowData[index], row); },
    replaceRows(nextRows) {
      roots.splice(0, roots.length);
      rowData.splice(0, rowData.length);
      nextRows.forEach((row, index) => roots.push(makeRoot(row, index)));
    },
    queueMutation(record) { pendingGeometry.push(record); },
    root(index) { return roots[index]; },
    setAdjustment(value) { c.setReadingAdjustment(value); },
    switchSession(id) { c.resetReadingSession(id); c.S.current = id; },
    resetSession(id) { c.resetReadingSession(id); },
    setCurrent(id) { c.S.current = id; },
    setVisible(value) { isVisible = value; },
    setFollow(value) { c.S.follow = value; },
    setScrollTop(value) { box.scrollTop = value; },
    input(direction) { c.readingInput(direction); },
    flushFrames() { while (frames.size) { const [id, callback] = frames.entries().next().value; frames.delete(id); callback(); } },
    runSyncViewport() {
      const root = { clientWidth: 800, setAttribute() {}, toggleAttribute() {} };
      const composer = { contains: () => false, getBoundingClientRect: () => ({ top: 550, bottom: 600, height: 50, width: 800 }),
        style: { removeProperty() {} } };
      const chat = { getClientRects: () => [], getBoundingClientRect: () => ({ top: 0, bottom: 600 }) };
      const input = { getBoundingClientRect: () => ({ width: 800 }) };
      c.document = { hidden: false, documentElement: root, activeElement: null };
      c.matchMedia = () => ({ matches: false });
      c.viewportFrame = 1; c.viewportPrinting = false; c.viewportGeometryPending = false;
      c.viewportBounds = () => ({ left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600, scale: 1 });
      c.safeViewportBounds = () => ({ left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600 });
      c.getComputedStyle = () => ({ fontSize: "16px" });
      c.viewportVariable = () => false; c.positionPopover = () => {}; c.activeDialog = () => null;
      c.$ = id => ({ transcript: box, composer, "view-chat": chat, input })[id] || box;
      c.syncViewport();
    },
    saveAnchor() { reading.anchor = c.captureReadingAnchor(); return reading.anchor; },
    restoreAnchor() { return c.restoreReadingAnchor(); },
    savedAnchor(id) {
      const anchor = reading.saved.get(id)?.anchor;
      return anchor && { top: anchor.top,
        candidates: Array.from(anchor.candidates, ({ id: candidateId, block, offset, stream }) =>
          ({ id: candidateId, block, offset, stream })) };
    },
    viewportResize() { c.scheduleViewportGeometrySync(); },
    layout(update, options) { c.withTranscriptLayout(update, options); },
    addContained(node) { contained.add(node); },
    loadDiagram(image, isCurrent = () => true) { c.readingDiagramLoaded(image, isCurrent); },
    resizeChanged() { c.readingResizeChanged(); },
    streamMutation() { pendingGeometry.push({ type: "characterData", target: roots.at(-1).streamText }); },
    streamResize() { c.readingResizeChanged([{ target: roots.at(-1) }]); },
    dispatch(type, event) { for (const { listener } of listeners.get(type) || []) listener(event); },
    summaryTarget() {
      const details = {};
      const summary = { closest: selector => selector === "details" ? details : null };
      return { closest: selector => selector === "summary" ? summary : null };
    },
    reads,
    get settles() { return settles; },
    get pendingFrames() { return frames.size; },
    get session() { return reading.session; },
    context: c,
    get queries() { return queries; },
    get rootCount() { return roots.length; },
  };
}
const rect = (top, bottom, height = bottom - top, live = false) => ({ top, bottom, height, live });

function sessionLifecycleFixture(rowsBySession, bounds) {
  const rows = Object.values(rowsBySession).flat();
  const fixture = captureFixture([], bounds);
  const c = fixture.context, box = c.$("transcript"), rowById = new Map(rows.map(row => [row.id, row]));
  const mutationRecords = [], pending = new Map(), trace = [];
  const fake = (tag, props = {}, ...kids) => {
    const node = { tag, props: props || {}, children: [], dataset: {}, hidden: false,
      append(...children) { this.children.push(...children.flat(Infinity).filter(child => child != null)); },
      replaceChildren(...children) { this.children = []; this.append(...children); },
      remove() { this.removed = true; },
      classList: { add() {}, remove() {}, toggle() {} },
      getAttribute(name) { return this.props[name] ?? null; },
    };
    if (props?.["data-message-id"] != null) node.dataset.messageId = props["data-message-id"];
    if (props?.["data-live-root"] != null) node.dataset.liveRoot = props["data-live-root"];
    node.append(...kids);
    return node;
  };
  Object.assign(c.S, { current: "long", messages: [], follow: true, view: "chat", sessions: Object.keys(rowsBySession).map(id => ({ id })),
    sessionLoad: { id: "long", status: "loaded" }, storageScope: "fixture", drafts: {}, editMessageId: null,
    liveTools: [], liveRun: null, queued: [], plan: [], planState: null, step: null, recap: null, recapRunId: null,
    running: new Set(), daemonOnline: true, topicsAvailable: false });
  Object.assign(c.reading, { session: "long", saved: new Map(), anchor: null });
  box.scrollHeight = 30000;
  c.historyRender = { session: null, turns: new Map(), live: null, welcome: null, banner: null, loadNotice: null };
  c.rendered = new Map();
  c.WHO = { "system-note": "System note", assistant: "Assistant", user: "User" };
  c.h = fake;
  c.welcome = () => fake("div");
  c.isRunning = () => false;
  c.uiLocale = () => "en";
  c.speaker = role => role;
  c.sentElsewhere = () => null;
  c.turnTimestamp = () => null;
  c.currentTopicAttempt = () => false;
  c.steerStep = () => null;
  c.withMentions = value => value;
  c.attachmentTags = () => [];
  c.markdown = () => fake("div");
  c.assistantStopNote = () => null;
  c.toolCallChip = () => fake("div");
  c.rateButtons = () => fake("div");
  c.recapBlock = () => fake("div");
  c.renderLive = () => {};
  c.renderChrome = () => {};
  c.renderOutbox = () => {};
  c.renderComposer = () => {};
  c.renderView = () => {};
  c.renderApproval = () => {};
  c.renderNotes = () => {};
  c.messageBody = (message, fill) => {
    const node = fake("div", { "data-message-id": message.id });
    c.rendered.set(message.id, node);
    fill?.(node);
    return node;
  };
  c.reconcileChildren = (parent, wanted) => {
    if (parent === box) parent.children = [...wanted];
    else parent.replaceChildren(...wanted);
    if (parent !== box) return;
    const ids = [];
    const collect = node => {
      if (node?.dataset?.messageId) { ids.push(node.dataset.messageId); return; }
      for (const child of node?.children || []) collect(child);
    };
    for (const node of wanted) collect(node);
    fixture.replaceRows(ids.map(id => rowById.get(id)).filter(Boolean));
    if (!ids.length && c.S.current === "long" && c.S.sessionLoad?.status === "pending") {
      box.scrollTop = 0;
      fixture.replaceRows([{ ...rect(0, 24), live: true }]);
    }
    mutationRecords.push({ type: "childList", target: box, addedNodes: [], removedNodes: [] });
  };
  c.readingChanges = { takeRecords: () => mutationRecords.splice(0) };
  c.saveDraft = () => {};
  c.invalidateBranchSummary = () => {};
  c.cancelEventRender = () => {};
  c.closeSidebar = () => {};
  c.clearLiveText = () => {};
  c.renderSessions = () => {};
  c.renderRunTools = () => {};
  c.renderStatus = () => {};
  c.renderChips = () => {};
  c.renderQueue = () => {};
  c.renderPlan = () => {};
  c.renderStep = () => {};
  c.renderRunBanner = () => {};
  c.restoreSessionRunState = () => {};
  c.applySessionLiveState = () => {};
  c.autosize = () => {};
  c.refreshAttachmentInventory = () => Promise.resolve();
  c.sessionCacheKey = () => "draft";
  c.sessionStorageKey = () => "session";
  c.attachmentDrafts = {};
  c.loadDraft = () => "";
  c.CLIENT_LABEL = "fixture";
  c.prefs = { set() {} };
  c.request = () => Promise.resolve({ used_tokens: 0, context_tokens: 1, runs: 0, total: {} });
  c.act = request => {
    if (request.type !== "get_session") return Promise.resolve({});
    record(`get_session:${request.id}`);
    return new Promise(resolve => pending.set(request.id, resolve));
  };
  c.flushOutbox = () => Promise.resolve();
  c.isRunning = () => false;
  c.sessionSeq = 0;
  vm.runInContext([fn("sessionLoadNotice"), fn("renderHistory"), fn("renderAll"), asyncFn("openSession")].join("\n"), c);
  const snapshotAnchor = anchor => anchor && ({ top: anchor.top,
    candidates: Array.from(anchor.candidates, ({ id, block, offset }) => ({ id, block, offset })) });
  function record(phase) {
    trace.push({ phase, current: c.S.current, session: c.reading.session, follow: c.S.follow,
      scrollTop: box.scrollTop, status: c.S.sessionLoad?.status, messageCount: c.S.messages.length,
      anchor: snapshotAnchor(c.reading.anchor), saved: Object.fromEntries(Array.from(c.reading.saved, ([id, value]) =>
        [id, { follow: value.follow, anchor: snapshotAnchor(value.anchor) }])) });
  }
  c.record = record;
  vm.runInContext(`
    const rawResetReadingSession = resetReadingSession;
    resetReadingSession = function(id) { record("reset-before:" + id); const result = rawResetReadingSession(id); record("reset-after:" + id); return result; };
    const rawRenderHistory = renderHistory;
    renderHistory = function(...args) { record("render-before"); const result = rawRenderHistory(...args); record("render-after"); return result; };
    const rawRenderAll = renderAll;
    renderAll = function(...args) { record("renderAll-before"); const result = rawRenderAll(...args); record("renderAll-after"); return result; };
  `, c);
  return {
    c, fixture, trace,
    messages(id) { return rowsBySession[id].map((row, seq) => ({ id: row.id, seq,
      role: seq % 2 ? "assistant" : "user", content: row.id, meta: "{}",
      ...(id === "long" && seq === 234 ? { generated_by: "instructions" } : {}), created_at: "2026-11-24T19:00:00Z" })); },
    resolve(id) { pending.get(id)({ messages: this.messages(id), session: { id }, queued_items: [] }); pending.delete(id); },
  };
}

test("anchor capture reuses indexed geometry for nearby and large jumps, preserving ordered lower-bound candidates", () => {
  const rows = [rect(0, 541), rect(40, 140, 0), rect(300, 700), rect(45, 560),
    rect(1000, 1100, 100, true), rect(65, 545), ...Array(1000).fill(rect(100, 700))];
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  const result = fixture.capture();
  assert.deepEqual(result.candidates, [
    { id: "0", block: 1, offset: -498, stream: null },
    { id: "2", block: 1, offset: -198, stream: null },
    { id: "3", block: 1, offset: -453, stream: null },
    { id: null, block: 1, offset: 502, stream: "live" },
  ]);
  assert.equal(result.top, 500);
  assert.equal(fixture.queries, 1);
  assert.deepEqual(fixture.reads, Array.from({ length: rows.length }, (_, index) => index));

  const nearby = fixture.capture(null, 560);
  assert.deepEqual(nearby.candidates, [
    { id: "2", block: 1, offset: -258, stream: null },
    { id: null, block: 1, offset: 442, stream: "live" },
    { id: "6", block: 1, offset: -458, stream: null },
    { id: "7", block: 1, offset: -458, stream: null },
  ]);
  const jump = fixture.capture(null, 720);
  assert.deepEqual(jump.candidates, [{ id: null, block: 1, offset: 282, stream: "live" }]);
  assert.equal(fixture.queries, 1);
  assert.equal(fixture.reads.length, rows.length, "unchanged roots are not remeasured while scrolling");

  const appendedIndex = fixture.rootCount;
  fixture.append(rect(1400, 1600));
  const appended = fixture.capture({ top: 1200, bottom: 1260 }, 300);
  assert.deepEqual(appended.candidates, [{ id: String(appendedIndex), block: 1, offset: 1102, stream: null }]);
  assert.equal(fixture.queries, 2);
  assert.equal(fixture.reads.length, rows.length * 2 + 1, "capture drains undelivered structural records before using cached geometry");
});

test("live-root mutation invalidation refreshes changed geometry without rescanning the history prefix", () => {
  const rows = [rect(0, 600), rect(10, 610), rect(20, 620), rect(30, 630),
    ...Array(1000).fill(rect(500, 600, 0)), rect(1000, 1200, 200, true)];
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  const measured = fixture.reads.length;
  fixture.setRow(rows.length - 1, rect(1500, 2300, 800, true));
  fixture.streamMutation();
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  assert.equal(fixture.reads.length, measured, "mutation notification does not remeasure roots past the history prefix");
  assert.deepEqual(fixture.capture({ top: 1200, bottom: 1260 }).candidates,
    [{ id: null, block: 1, offset: 1002, stream: "live" }]);
  assert.equal(fixture.reads.length, measured + 1, "mutation notification refreshes only the live root when selection reaches it");
  assert.equal(fixture.queries, 1);
});

test("live-root ResizeObserver invalidation refreshes changed geometry without rescanning the history prefix", () => {
  const rows = [rect(0, 600), rect(10, 610), rect(20, 620), rect(30, 630),
    ...Array(1000).fill(rect(500, 600, 0)), rect(1000, 1200, 200, true)];
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  const measured = fixture.reads.length;
  fixture.setRow(rows.length - 1, rect(1500, 2300, 800, true));
  fixture.streamResize();
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  assert.equal(fixture.reads.length, measured, "resize notification does not remeasure roots past the history prefix");
  assert.deepEqual(fixture.capture({ top: 1200, bottom: 1260 }).candidates,
    [{ id: null, block: 1, offset: 1002, stream: "live" }]);
  assert.equal(fixture.reads.length, measured + 1, "resize notification refreshes only the live root when selection reaches it");
  assert.equal(fixture.queries, 1);
});

test("viewport layout invalidates synchronously when ResizeObserver has not delivered", () => {
  const rows = [rect(0, 520), rect(100, 620)];
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  fixture.setRow(0, rect(0, 540));
  fixture.viewportResize();
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1"]);
  assert.equal(fixture.queries, 2);
  fixture.setRow(0, rect(0, 520));
  fixture.layout(() => {}, { newContent: false, geometryChanged: true });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  assert.equal(fixture.queries, 3);
  assert.match(fn("syncViewport"), /newContent: false, geometryChanged/);
  assert.match(source, /window\.addEventListener\("resize", scheduleViewportGeometrySync\)/);
  assert.match(source, /visualViewport\?\.addEventListener\("resize", scheduleViewportGeometrySync\)/);
});

test("paused reading bookmarks restore independently across repeated switches between two Sessions", () => {
  const rows = (session, top, bottom) => [
    ...Array.from({ length: 4 }, (_, index) => ({ ...rect(index * 100, index * 100 + 80), id: `${session}-${index}` })),
    { ...rect(top, bottom), id: `${session}-004` },
  ];
  const aRows = rows("A", 450, 650), bRows = rows("B", 900, 1150);
  const fixture = captureFixture(aRows, { top: 40, bottom: 100 });
  fixture.setScrollTop(0);
  fixture.setFollow(false);
  fixture.saveAnchor();
  assert.deepEqual(fixture.capture().candidates[0], { id: "A-0", block: 1, offset: 2, stream: null });
  fixture.input(-1);
  fixture.setScrollTop(497);
  assert.deepEqual(fixture.capture().candidates, [{ id: "A-004", block: 1, offset: -45, stream: null }]);
  assert.equal(fixture.pendingFrames, 1);

  fixture.switchSession("B");
  assert.equal(fixture.pendingFrames, 0);
  fixture.replaceRows(bRows);
  fixture.setScrollTop(812);
  fixture.setFollow(false);
  fixture.saveAnchor();
  assert.deepEqual(fixture.capture().candidates, [{ id: "B-004", block: 1, offset: 90, stream: null }]);

  for (let round = 0; round < 2; round++) {
    fixture.switchSession("A");
    fixture.setScrollTop(0);
    fixture.replaceRows([]);
    fixture.restoreAnchor();
    fixture.replaceRows(aRows);
    fixture.restoreAnchor();
    assert.equal(fixture.capture().top, 497);
    assert.deepEqual(fixture.capture().candidates, [{ id: "A-004", block: 1, offset: -45, stream: null }]);

    fixture.switchSession("B");
    fixture.setScrollTop(0);
    fixture.replaceRows([]);
    fixture.restoreAnchor();
    fixture.replaceRows(bRows);
    fixture.restoreAnchor();
    assert.equal(fixture.capture().top, 812);
    assert.deepEqual(fixture.capture().candidates, [{ id: "B-004", block: 1, offset: 90, stream: null }]);
  }

  const image = {};
  fixture.addContained(image);
  fixture.setRow(4, rect(1100, 1300));
  fixture.loadDiagram(image);
  fixture.restoreAnchor();
  assert.equal(fixture.capture().top, 1012);
  assert.deepEqual(fixture.capture().candidates, [{ id: "B-004", block: 1, offset: 90, stream: null }]);
});

test("session navigation does not remeasure a paused bookmark without pending scroll input", () => {
  const rows = Array.from({ length: 1005 }, (_, index) => rect(index * 100, index * 100 + 80));
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  fixture.capture();
  fixture.setFollow(false);
  fixture.saveAnchor();
  const reads = fixture.reads.length, queries = fixture.queries;

  fixture.switchSession("B");

  assert.equal(fixture.reads.length, reads);
  assert.equal(fixture.queries, queries);
});

test("real openSession and renderHistory preserve Long and Short bookmarks across async loads", async () => {
  const makeRows = (id, count, stride, target, offset) => Array.from({ length: count }, (_, index) => {
    const top = index * stride;
    return { ...rect(top, top + 80), id: `${id}-${String(index).padStart(3, "0")}`,
      blockOffset: index === target ? offset : 2 };
  });
  const longRows = makeRows("long", 240, 100, 6, 24.34);
  const shortRows = makeRows("short", 84, 86, 65, 41.05);
  const app = sessionLifecycleFixture({ long: longRows, short: shortRows }, { top: 40, bottom: 100 });
  const { c, fixture, trace } = app;
  const assertBookmark = (candidate, id, offset) => {
    assert.equal(candidate.id, id);
    assert.equal(candidate.block, 1);
    assert.equal(Number(candidate.offset.toFixed(2)), offset);
  };
  c.S.messages = app.messages("long");
  fixture.replaceRows(longRows);
  c.renderHistory();
  fixture.setFollow(false);
  fixture.setScrollTop(600);
  fixture.saveAnchor();
  assertBookmark(fixture.capture().candidates[0], "long-006", 24.34);

  const shortLoad = c.openSession("short");
  assert.equal(c.S.current, "short");
  assert.equal(c.S.messages.length, 0);
  assertBookmark(fixture.savedAnchor("long").candidates[0], "long-006", 24.34);
  app.resolve("short");
  await shortLoad;
  assert.equal(c.S.messages.length, 84);
  fixture.setFollow(false);
  fixture.setScrollTop(5604);
  fixture.saveAnchor();
  assertBookmark(fixture.capture().candidates[0], "short-065", 27.05);

  const longLoad = c.openSession("long");
  assert.equal(c.S.current, "long");
  assert.equal(c.S.messages.length, 0);
  assert.equal(fixture.rootCount, 1);
  assertBookmark(fixture.savedAnchor("short").candidates[0], "short-065", 27.05);
  const pendingLong = trace.findLast(entry => entry.phase === "get_session:long");
  assert.equal(pendingLong.status, "pending");
  assert.equal(pendingLong.anchor.top, 600);
  assertBookmark(pendingLong.anchor.candidates[0], "long-006", 24.34);
  assertBookmark(pendingLong.saved.long.anchor.candidates[0], "long-006", 24.34);
  assertBookmark(pendingLong.saved.short.anchor.candidates[0], "short-065", 27.05);
  const emptyLongRender = trace.findLast(entry => entry.phase === "render-after" && entry.current === "long" && entry.status === "pending");
  assertBookmark(emptyLongRender.anchor.candidates[0], "long-006", 24.34);
  assert.equal(c.reading.inputFrame, 0);
  assert.equal(c.reading.intentUntil, 0);
  assert.equal(c.captureReadingAnchor(), null);
  fixture.runSyncViewport();
  assertBookmark(c.reading.anchor.candidates[0], "long-006", 24.34);
  assert.equal(c.S.follow, false);
  const box = c.$("transcript");
  box.clientHeight = 300;
  box.scrollHeight = 30000;
  c.reading.intentUntil = 100;
  c.reading.direction = -1;
  fixture.dispatch("scroll", {});
  assertBookmark(c.reading.anchor.candidates[0], "long-006", 24.34);
  assert.equal(c.S.follow, false);
  assert.equal(c.reading.intentUntil, 100);
  c.performance.now = () => 200;
  app.resolve("long");
  await longLoad;
  assert.equal(fixture.capture().top, 600, JSON.stringify({ anchor: { top: c.reading.anchor?.top,
    candidates: c.reading.anchor?.candidates.map(({ id, block, offset }) => ({ id, block, offset })) },
    inputFrame: c.reading.inputFrame, intentUntil: c.reading.intentUntil, follow: c.S.follow,
    trace: trace.slice(-6).map(({ phase, current, status, scrollTop, anchor }) => ({ phase, current, status, scrollTop, anchor })) }));
  assertBookmark(fixture.capture().candidates[0], "long-006", 24.34);

  const returnShort = c.openSession("short");
  app.resolve("short");
  await returnShort;
  assert.equal(fixture.capture().top, 5604);
  assertBookmark(fixture.capture().candidates[0], "short-065", 27.05);
  assert.ok(trace.some(entry => entry.phase === "renderAll-after" && entry.current === "long"
    && entry.status === "loaded" && entry.scrollTop === 600));
});

test("opening a Session from another view selects and clears it before Chat renders", () => {
  const rows = (session, top, bottom) => [
    ...Array.from({ length: 4 }, (_, index) => ({ ...rect(index * 100, index * 100 + 80), id: `${session}-${index}` })),
    { ...rect(top, bottom), id: `${session}-004` },
  ];
  const aRows = rows("A", 450, 650), bRows = rows("B", 900, 1150);
  const fixture = captureFixture(aRows, { top: 40, bottom: 100 });
  fixture.setScrollTop(497);
  fixture.setFollow(false);
  fixture.saveAnchor();
  fixture.switchSession("B");
  fixture.replaceRows(bRows);
  fixture.setScrollTop(812);
  fixture.setFollow(false);
  fixture.saveAnchor();
  fixture.switchSession("A");
  fixture.replaceRows(aRows);
  fixture.setScrollTop(0);
  fixture.restoreAnchor();
  fixture.setVisible(false);

  fixture.resetSession("B");
  fixture.setCurrent("B");
  fixture.replaceRows([]);
  fixture.setVisible(true);
  fixture.layout(() => {});

  assert.equal(fixture.session, "B");
  assert.deepEqual(fixture.savedAnchor("B").candidates, [{ id: "B-004", block: 1, offset: 90, stream: null }]);
  const start = source.indexOf("  async function openSession(");
  const end = source.indexOf("\n  async function newSession(", start);
  const openSession = source.slice(start, end);
  const showChat = openSession.indexOf('setView("chat")');
  assert.ok(openSession.indexOf("S.current = id;") < showChat);
  assert.ok(openSession.indexOf("S.messages = [];") < showChat);
});

test("details click and keyboard expansion invalidate before synchronous anchor capture", () => {
  const fixture = captureFixture([rect(0, 520), rect(100, 620)], { top: 40, bottom: 100 });
  const target = fixture.summaryTarget();
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  fixture.dispatch("click", { target });
  fixture.setRow(0, rect(0, 540));
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1"]);
  fixture.dispatch("keydown", { target, key: " " });
  fixture.setRow(0, rect(0, 520));
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  assert.equal(fixture.queries, 3);
});

test("diagram load and root resize callbacks invalidate changed heights", () => {
  const fixture = captureFixture([rect(0, 520), rect(100, 620)], { top: 40, bottom: 100 });
  const image = {};
  fixture.addContained(image);
  fixture.capture();
  fixture.setRow(0, rect(0, 540));
  fixture.loadDiagram(image);
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1"]);
  assert.equal(fixture.settles, 1);
  fixture.setRow(0, rect(0, 520));
  fixture.resizeChanged();
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  assert.equal(fixture.settles, 2);
  assert.equal(fixture.queries, 3);
});

test("reading adjustment transforms update thresholds without remeasuring content-space roots", () => {
  const fixture = captureFixture([rect(0, 531), rect(100, 620)], { top: 40, bottom: 100 });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  fixture.setAdjustment(15);
  assert.deepEqual(fixture.capture().candidates, [
    { id: "0", block: 1, offset: -483, stream: null },
    { id: "1", block: 1, offset: -383, stream: null },
  ]);
  assert.equal(fixture.queries, 1);
  assert.equal(fixture.reads.length, 2);
});

test("no matches still measure all roots, while hidden captures do not query them", () => {
  const rows = Array.from({ length: 50 }, (_, i) => rect(i * 20, i * 20 + 20));
  const bounds = { top: 1200, bottom: 1300 };
  const result = captureFixture(rows, bounds);
  assert.deepEqual(result.capture().candidates, []);
  assert.equal(result.reads.length, rows.length);
  const hidden = captureFixture(rows, bounds, false);
  assert.equal(hidden.capture().candidates, null);
  assert.equal(hidden.queries, 0);
});
