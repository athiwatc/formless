// Bounded opaque transport primitives, shared by the room/dev/browser. These
// do not decrypt data or grant E2E readiness. Wiring owns readiness, socket
// identity, timers, task cancellation and release after actual socket teardown.
export const LIMITS = Object.freeze({
  version: 1,
  clients: 16,
  plaintext: 32 * 1024 * 1024,
  encrypted: 4 * Math.ceil((32 * 1024 * 1024 + 16) / 3) + 256,
  chunk: 64 * 1024,
  wire: 64 * 1024 + 1024,
  preReady: 2048,
  windowFrames: 8,
  // One complete fragment per CID/direction cannot be monopolized by peers.
  reservedWire: 64 * 1024 + 1024,
  sharedWire: 1024 * 1024,
  wireCopyCharge: 4,
  roomMemory: 16 * 1024 * 1024,
  ackMs: 10000,
  assemblyMs: 30000,
  handshakeMs: 30000,
});
export const MAX_WINDOW_BYTES = LIMITS.windowFrames * LIMITS.wire;
export const MAX_ROOM_WIRE = 2 * LIMITS.clients * LIMITS.reservedWire + LIMITS.sharedWire;
export const ROOM_ACCOUNTED_BYTES = MAX_ROOM_WIRE * LIMITS.wireCopyCharge;
export class TransportError extends Error {
  constructor(message, code = 1008) { super(message); this.code = code; }
}
const fail = (message, code) => { throw new TransportError(message, code); };
const integer = (value) => Number.isSafeInteger(value) && value >= 0;
const messageID = (value) => typeof value === "string" && /^[a-f0-9]{32}$/.test(value);
const deliveryToken = (value) => typeof value === "string" && /^[A-Za-z0-9_-]{22}$/.test(value);

/** Count bounded input without allocating a second full UTF-8 copy. */
export function wireBytes(text, max = LIMITS.wire) {
  if (typeof text !== "string") fail("text frames required", 1008);
  if (text.length > max) fail("relay frame too large", 1009);
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code <= 0x7f) bytes++;
    else if (code <= 0x7ff) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length && text.charCodeAt(i + 1) >= 0xdc00 && text.charCodeAt(i + 1) <= 0xdfff) { bytes += 4; i++; }
    else bytes += 3;
    if (bytes > max) fail("relay frame too large", 1009);
  }
  return bytes;
}
export function randomToken() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let binary = ""; for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
export function randomMessageID() {
  return [...crypto.getRandomValues(new Uint8Array(16))].map(b => b.toString(16).padStart(2, "0")).join("");
}
function ascii(text, max) {
  if (typeof text !== "string" || text.length > max) fail("encrypted fragment too large", 1009);
  for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) > 127) fail("encrypted fragments must be ASCII");
  return text.length;
}
/** A lazy iterator: no array of copied fragments or extra whole-message clone. */
export function* fragments(frame, id = randomMessageID()) {
  const bytes = ascii(frame, LIMITS.encrypted);
  if (!bytes || !messageID(id)) fail("invalid encrypted message identity");
  for (let index = 0, offset = 0; offset < bytes; index++, offset += LIMITS.chunk) {
    yield { relay: "data", v: LIMITS.version, id, index, bytes, data: frame.slice(offset, offset + LIMITS.chunk) };
  }
}
export function validateFragment(part) {
  if (!part || part.relay !== "data" || part.v !== LIMITS.version || !messageID(part.id)
      || !integer(part.index) || !integer(part.bytes) || !part.bytes || part.bytes > LIMITS.encrypted) fail("invalid encrypted fragment", part?.bytes > LIMITS.encrypted ? 1009 : 1008);
  const expected = Math.min(LIMITS.chunk, part.bytes - part.index * LIMITS.chunk);
  if (expected <= 0 || ascii(part.data, LIMITS.chunk) !== expected) fail("invalid encrypted fragment length");
  return part;
}

/** Logical retained-memory accounting; not an OS/RSS or garbage-collector cap. */
export class MemoryBudget {
  constructor(limit) { this.limit = limit; this.used = 0; }
  reserve(bytes) {
    if (!integer(bytes) || bytes > this.limit - this.used) fail("relay memory budget exhausted");
    this.used += bytes; let held = true;
    return {
      shrink: remaining => {
        if (!held || !integer(remaining) || remaining > bytes) fail("invalid relay lease shrink");
        this.used -= bytes - remaining; bytes = remaining;
      },
      release: () => { if (held) { held = false; this.used -= bytes; } },
    };
  }
}
/** One message per direction. Caller retains the returned lease through
 * decryption/consumption and must not ACK its final fragment before release. */
export class Assembler {
  constructor(budget) { this.budget = budget; this.current = null; this.consuming = false; }
  push(part, ready, now = Date.now()) {
    try { return this.pushChecked(part, ready, now); }
    catch (error) { this.abort(); throw error; }
  }
  pushChecked(part, ready, now) {
    if (!ready) fail("bulk data before authenticated readiness");
    validateFragment(part);
    if (this.consuming) fail("previous encrypted message is still being consumed");
    let state = this.current;
    if (!state) {
      if (part.index !== 0) fail("encrypted message starts out of order");
      // One byte array plus a worst-case two-byte JS decoded string. No
      // plaintext/decrypt allocation is hidden in this wire-only reservation.
      const lease = this.budget.reserve(part.bytes * 3);
      try { state = { id:part.id, bytes:part.bytes, index:0, deadline:now + LIMITS.assemblyMs, lease, buffer:new Uint8Array(part.bytes) }; }
      catch (error) { lease.release(); throw error; }
      this.current = state;
    }
    if (state.deadline <= now || state.id !== part.id || state.bytes !== part.bytes || state.index !== part.index) { this.abort(); fail("encrypted fragment reordered, duplicated or expired"); }
    const offset = part.index * LIMITS.chunk;
    for (let i = 0; i < part.data.length; i++) state.buffer[offset + i] = part.data.charCodeAt(i);
    state.index++;
    if (offset + part.data.length !== state.bytes) return null;
    let frame = new TextDecoder("utf-8", { fatal:true }).decode(state.buffer);
    // The returned handle can outlive release. Sever the completed backing
    // buffer now and capture only the lease, never the whole assembly state.
    state.buffer = null;
    const lease = state.lease;
    state.lease = null;
    this.current = null; this.consuming = true;
    let held = true;
    return {
      get frame() { return frame; },
      release: () => { if (held) { held = false; frame = null; lease.release(); this.consuming = false; } },
    };
  }
  abort() {
    if (this.current) this.current.lease.release();
    this.current = null;
    // An already-transferred consumer lease is not refunded while its async
    // operation still retains data. Its owner releases it in its finally.
  }
}

/** Only the excess beyond each direction's progress reserve is shared. The
 * caller counts closing sockets against the client cap until actual teardown. */
export class RoomCredits {
  constructor() { this.excess = 0; this.raw = 0; }
  change(before, after) {
    if (!integer(before) || !integer(after)) fail("invalid credit accounting");
    const delta = Math.max(0, after - LIMITS.reservedWire) - Math.max(0, before - LIMITS.reservedWire);
    if (this.excess + delta > LIMITS.sharedWire) return false;
    if (this.excess + delta < 0 || this.raw + after - before < 0) fail("invalid credit release");
    this.excess += delta; this.raw += after - before; return true;
  }
}
function equalToken(a, b) {
  if (!deliveryToken(a) || !deliveryToken(b)) return false;
  let difference = 0; for (let i = 0; i < a.length; i++) difference |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return difference === 0;
}
/** Socket/generation identity is supplied from the actual transport, never an
 * ACK payload's cid. Persist compact entries in that socket's attachment. */
export class DeliveryWindow {
  constructor(room, owner, generation) { this.room = room; this.owner = owner; this.generation = generation; this.entries = []; this.bytes = 0; this.closing = false; }
  issue(bytes, sourceID, now = Date.now()) {
    if (this.closing || !integer(bytes) || !bytes || bytes > LIMITS.wire || !integer(sourceID)
        || this.entries.length >= LIMITS.windowFrames || this.bytes + bytes > MAX_WINDOW_BYTES) return null;
    if (!this.room.change(this.bytes, this.bytes + bytes)) return null;
    let token;
    try { token = randomToken(); } catch (error) { this.room.change(this.bytes + bytes, this.bytes); throw error; }
    this.entries.push([token, bytes, sourceID, now + LIMITS.ackMs]); this.bytes += bytes;
    return token;
  }
  ack(owner, generation, token) {
    if (owner !== this.owner || generation !== this.generation) return null;
    const index = this.entries.findIndex(entry => equalToken(entry[0], token));
    if (index < 0) return null;
    const [,bytes,sourceID] = this.entries[index];
    this.entries.splice(index,1); this.room.change(this.bytes, this.bytes - bytes); this.bytes -= bytes;
    return sourceID;
  }
  expired(now = Date.now()) { return this.entries.some(entry => entry[3] <= now); }
  close() { this.closing = true; } // Native buffered writes may still exist.
  teardown() { this.room.change(this.bytes,0); this.bytes = 0; this.entries = []; this.closing = true; }
  snapshot() { return { g:this.generation, c:this.closing, e:this.entries.map(row=>row.slice()) }; }
  static restore(room, owner, snapshot, now = Date.now()) {
    if (!snapshot || !messageID(snapshot.g) || typeof snapshot.c !== "boolean" || !Array.isArray(snapshot.e) || snapshot.e.length > LIMITS.windowFrames) fail("invalid delivery window state");
    const result = new DeliveryWindow(room,owner,snapshot.g);
    const tokens = new Set();
    for (const row of snapshot.e) {
      if (!Array.isArray(row) || row.length !== 4 || !deliveryToken(row[0]) || tokens.has(row[0]) || !integer(row[1]) || !row[1] || row[1] > LIMITS.wire || !integer(row[2]) || !integer(row[3]) || row[3] > now + LIMITS.ackMs) fail("invalid persisted receipt");
      tokens.add(row[0]); result.entries.push(row.slice()); result.bytes += row[1];
    }
    if (result.bytes > MAX_WINDOW_BYTES || !room.change(0,result.bytes)) fail("persisted delivery window exceeds budget");
    result.closing = snapshot.c === true;
    return result;
  }
}

/** Pre-send reservation prevents honest endpoints oversubscribing shared
 * space. The sender-visible permit is NOT a destination receipt: redemption
 * creates a separate random token disclosed only in the delivered packet.
 * Each reservation is charged one maximum wire envelope until its receipt. */
export class SendPermits {
  constructor(room, source, sourceGeneration, destination, destinationGeneration) {
    if (!messageID(sourceGeneration) || !messageID(destinationGeneration)) fail("invalid transport generation");
    this.room=room;this.source=source;this.sourceGeneration=sourceGeneration;
    this.destination=destination;this.destinationGeneration=destinationGeneration;
    this.entries=[];this.bytes=0;this.closing=false;
  }
  grant(sourceID,now=Date.now()) {
    if(this.closing || !integer(sourceID) || this.entries.some(row=>row[2]===sourceID) || this.entries.length>=LIMITS.windowFrames) return null;
    if(!this.room.change(this.bytes,this.bytes+LIMITS.wire))return null;
    let permit;
    try{permit=randomToken();}catch(error){this.room.change(this.bytes+LIMITS.wire,this.bytes);throw error;}
    // [sender permit, destination receipt, source packet ID, deadline].
    this.entries.push([permit,null,sourceID,now+LIMITS.ackMs]);this.bytes+=LIMITS.wire;
    return permit;
  }
  redeem(source,generation,permit,actualBytes,now=Date.now()) {
    if(this.closing || source!==this.source || generation!==this.sourceGeneration || !integer(actualBytes) || !actualBytes || actualBytes>LIMITS.wire)return null;
    const row=this.entries.find(row=>row[0]!==null && equalToken(row[0],permit));
    if(!row || row[3]<=now)return null;
    const receipt=randomToken();row[0]=null;row[1]=receipt;row[3]=now+LIMITS.ackMs;
    return {receipt,sourceID:row[2]};
  }
  acknowledge(destination,generation,receipt) {
    if(destination!==this.destination || generation!==this.destinationGeneration)return null;
    const index=this.entries.findIndex(row=>row[1]!==null && equalToken(row[1],receipt));
    if(index<0)return null;
    const row=this.entries[index];this.entries.splice(index,1);
    this.room.change(this.bytes,this.bytes-LIMITS.wire);this.bytes-=LIMITS.wire;
    return row[2];
  }
  expired(now=Date.now()){return this.entries.some(row=>row[3]<=now);}
  closeSource(){
    this.closing=true;
    // Unused grants never entered a destination's native queue. Already
    // delivered UP packets remain charged after Browser source teardown.
    const held=this.entries.filter(row=>row[1]!==null);
    this.room.change(this.bytes,held.length*LIMITS.wire);this.bytes=held.length*LIMITS.wire;this.entries=held;
  }
  teardownDestination(){this.room.change(this.bytes,0);this.bytes=0;this.entries=[];this.closing=true;}
  snapshot(){return{sg:this.sourceGeneration,dg:this.destinationGeneration,c:this.closing,e:this.entries.map(row=>row.slice())};}
  static restore(room,source,destination,snapshot,now=Date.now()){
    if(!snapshot || !messageID(snapshot.sg)||!messageID(snapshot.dg)||typeof snapshot.c!=="boolean"||!Array.isArray(snapshot.e)||snapshot.e.length>LIMITS.windowFrames)fail("invalid send-permit lifecycle");
    const result=new SendPermits(room,source,snapshot.sg,destination,snapshot.dg),tokens=new Set(),ids=new Set();
    for(const row of snapshot.e){
      if(!Array.isArray(row)||row.length!==4||!integer(row[2])||ids.has(row[2])||!integer(row[3])||row[3]>now+LIMITS.ackMs
          ||!((deliveryToken(row[0])&&row[1]===null)||(row[0]===null&&deliveryToken(row[1])))
          ||tokens.has(row[0]??row[1])||(snapshot.c&&row[0]!==null))fail("invalid persisted send permit");
      ids.add(row[2]);tokens.add(row[0]??row[1]);result.entries.push(row.slice());
    }
    result.bytes=result.entries.length*LIMITS.wire;
    if(!room.change(0,result.bytes))fail("persisted send permits exceed room budget");
    result.closing=snapshot.c;return result;
  }
}

/** One already-authenticated logical CID at an endpoint. The owner performs
 * E2E crypto and grants readiness only after its proof plus room readiness.
 * A single queued message is allowed: callers must bound/charge producers
 * before encrypting and transfer their lease here. No plaintext is inspected. */
export class WireEndpoint {
  constructor({generation,cid=null,send,close,consume,control=()=>{},memory}) {
    if(!messageID(generation))fail("invalid endpoint generation");
    this.generation=generation;this.cid=cid;this.sendWire=send;this.closeWire=close;
    this.consume=consume;this.control=control;this.memory=memory;this.assembler=new Assembler(memory);
    this.ready=false;this.closed=false;this.current=null;this.nextID=1;
    this.pending=new Map();this.controls=0;this.controlChain=Promise.resolve();
  }
  setReady(){if(!this.closed)this.ready=true;}
  route(value){return{...value,g:this.generation,...(this.cid?{cid:this.cid}:{})};}
  send(value){const text=JSON.stringify(this.route(value));wireBytes(text);this.sendWire(text);}
  start(frame,lease={release(){}}){
    if(this.closed||!this.ready||this.current){lease.release();return Promise.reject(new TransportError("endpoint not ready or message already pending"));}
    let iterator;
    try{iterator=fragments(frame);}catch(error){lease.release();return Promise.reject(error);}
    return new Promise((resolve,reject)=>{
      this.current={iterator,lease,resolve,reject,done:false};
      try{this.pump();}catch(error){this.fail(error);}
    });
  }
  pump(){
    const current=this.current;if(!current||this.closed)return;
    while(!current.done&&this.pending.size<LIMITS.windowFrames){
      const next=current.iterator.next();
      if(next.done){current.done=true;break;}
      if(!Number.isSafeInteger(this.nextID))fail("transport sequence exhausted");
      const sendID=this.nextID++;
      this.pending.set(sendID,{part:next.value,sent:false});
      this.send({relay:"need",send_id:sendID});
    }
    if(current.done&&!this.pending.size){
      this.current=null;current.iterator.return?.();current.lease.release();current.resolve();
    }
  }
  receive(message,now=Date.now()){
    if(this.closed)return;
    try{
      wireBytes(message);const packet=JSON.parse(message);
      if(packet.g!==this.generation)fail("stale transport generation");
      switch(packet.relay){
        case "grant":{
          const entry=this.pending.get(packet.send_id);
          if(!entry||entry.sent||!deliveryToken(packet.permit))fail("unexpected send permit");
          entry.sent=true;this.send({...entry.part,permit:packet.permit});break;
        }
        case "credit":{
          const entry=this.pending.get(packet.send_id);
          if(!entry||!entry.sent)fail("unexpected delivery credit");
          this.pending.delete(packet.send_id);this.pump();break;
        }
        case "delivery":{
          if(!deliveryToken(packet.receipt))fail("invalid delivery receipt");
          const owned=this.assembler.push(packet.fragment,this.ready,now);
          if(!owned){this.send({relay:"ack",receipt:packet.receipt});break;}
          // Do not queue arbitrary callbacks or ACK the final piece while
          // application processing still retains the completed frame.
          Promise.resolve().then(()=>this.consume(owned.frame)).then(()=>{
            owned.release();if(!this.closed)this.send({relay:"ack",receipt:packet.receipt});
          }).catch(error=>{owned.release();this.fail(error);});
          break;
        }
        case "control":{
          if(!deliveryToken(packet.receipt)||typeof packet.f!=="string")fail("invalid control delivery");
          wireBytes(packet.f,LIMITS.preReady);
          if(this.controls>=32)fail("control backlog exceeded");
          const lease=this.memory.reserve(packet.f.length*2+512);
          this.controls++;
          this.controlChain=this.controlChain.then(async()=>{
            if(!this.closed)await this.control(packet.f);
            if(!this.closed)this.send({relay:"ack_control",receipt:packet.receipt});
          }).catch(error=>this.fail(error)).finally(()=>{this.controls--;lease.release();});
          break;
        }
        default:fail("unknown bounded transport packet");
      }
    }catch(error){this.fail(error);}
  }
  tick(now=Date.now()){
    if(this.assembler.current?.deadline<=now)this.fail(new TransportError("encrypted assembly expired"));
  }
  fail(error){
    if(this.closed)return;
    this.dispose(error instanceof Error?error:new TransportError("transport failed"));
    this.closeWire(error instanceof TransportError?error.code:4002,"relay transport failed");
  }
  dispose(error=new TransportError("relay endpoint closed")){
    if(this.closed)return;this.closed=true;this.ready=false;this.assembler.abort();
    const current=this.current;this.current=null;this.pending.clear();
    if(current){current.iterator.return?.();current.lease.release();current.reject(error);}
    // An active consumer keeps its lease until its own finally settles.
  }
}
