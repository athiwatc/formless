import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
function fn(name){const start=source.indexOf('  function '+name+'(');assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);}
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
  let settles = 0;
  let box;
  const transcriptTop = 10;
  const makeRoot = (row, index) => {
    rowData.push(row);
    const rect = () => ({ top: transcriptTop + row.top - box.scrollTop + reading.adjustment,
      bottom: transcriptTop + row.bottom - box.scrollTop + reading.adjustment, height: row.height });
    const inner = { getBoundingClientRect: () => { const r = rect(); return { ...r, top: r.top + 2 }; }, contains: () => false };
    const block = { getBoundingClientRect: rect, contains: child => child === inner };
    let root;
    const streamText = { nodeType: 3, parentElement: { closest: selector => selector === "[data-message-id], [data-live-root]" ? root : null } };
    root = {
      dataset: row.live ? {} : { messageId: String(index) },
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
    scrollTop: 500, getBoundingClientRect: () => ({ top: transcriptTop }),
    querySelectorAll: () => { queries++; return roots; },
    contains: node => roots.includes(node) || contained.has(node),
    addEventListener: (type, listener, options) => { listeners.set(type, [...(listeners.get(type) || []), { listener, options }]); },
    classList: { add() {} },
    style: { setProperty: () => pendingGeometry.push({ type: "attributes", target: box, attributeName: "style" }) },
  };
  const c = vm.createContext({
    S: { current: "A", messages: [], follow: true }, reading,
    readingVisible: () => visible, $: () => box, visibleReadingBounds: () => currentBounds,
    anchorBlocks: "p, li", readingStreamStamp: message => message.hasAttribute("data-live-root") ? "live" : null,
    readingRootChanges: { takeRecords: () => pendingGeometry.splice(0) },
    readingChanges: { takeRecords: () => [] }, readingResize: { observe() {}, unobserve() {} },
    scheduleViewportSync() {}, viewportGeometryPending: false,
    restoreReadingAnchor() {}, markNewOutput() {}, updateReadingPill() {}, settleReadingLayout() { settles++; },
    performance: { now: () => 0 }, Node: { TEXT_NODE: 3 }, cancelAnimationFrame() {},
  });
  vm.runInContext(["invalidateReadingRoots", "setReadingRootMaximum", "markReadingRootDirty", "markLiveReadingRootDirty",
    "readingGeometryChanged", "readingRoots", "refreshReadingRoot", "firstReadingRoot", "captureReadingAnchor",
    "observeReadingNodes", "readingDiagramLoaded", "readingDetailsWillToggle", "watchReadingDetails", "readingResizeChanged",
    "setReadingAdjustment", "resetReadingSession", "withTranscriptLayout", "scheduleViewportGeometrySync"].map(fn).join("\n"), c);
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
    get queries() { return queries; },
    get rootCount() { return roots.length; },
  };
}
const rect = (top, bottom, height = bottom - top, live = false) => ({ top, bottom, height, live });

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

test("live stream invalidations do not remeasure roots past the cached history prefix", () => {
  const rows = [rect(0, 600), rect(10, 610), rect(20, 620), rect(30, 630),
    ...Array(1000).fill(rect(500, 600, 0)), rect(1000, 1200, 200, true)];
  const fixture = captureFixture(rows, { top: 40, bottom: 100 });
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  const measured = fixture.reads.length;
  for (let update = 0; update < 4; update++) {
    fixture.setRow(rows.length - 1, rect(1000, 2300 + update * 100, 1300 + update * 100, true));
    fixture.streamMutation();
    assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
    fixture.streamResize();
    assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0", "1", "2", "3"]);
  }
  assert.equal(fixture.queries, 1);
  assert.equal(fixture.reads.length, measured, "stream mutations and live-root resizes do not trigger a full geometry scan");
  assert.deepEqual(fixture.capture({ top: 1200, bottom: 1260 }).candidates.map(({ id }) => id), [null]);
  assert.equal(fixture.reads.length, measured + 1, "the dirty live root is refreshed only when selection reaches it");
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

test("session switch and return cannot reuse the prior Session's root cache", () => {
  const fixture = captureFixture([rect(0, 520), rect(100, 620)], { top: 40, bottom: 100 });
  fixture.capture();
  fixture.switchSession("B");
  fixture.replaceRows([rect(300, 650)]);
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["0"]);
  fixture.switchSession("A");
  fixture.replaceRows([rect(0, 520), rect(100, 620)]);
  assert.deepEqual(fixture.capture().candidates.map(({ id }) => id), ["1"]);
  assert.equal(fixture.queries, 3);
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
