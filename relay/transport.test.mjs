import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { LIMITS, MAX_WINDOW_BYTES, MAX_ROOM_WIRE, ROOM_ACCOUNTED_BYTES, TransportError, wireBytes, fragments, validateFragment, MemoryBudget, Assembler, RoomCredits, DeliveryWindow, SendPermits, WireEndpoint } from "./web/transport.mjs";
const id = "a".repeat(32), generation = "b".repeat(32);
const invalid = (fn,code=1008) => assert.throws(fn,error=>error instanceof TransportError && error.code===code);

test("wire UTF8 byte limit is exact and checked without a second encoding allocation",()=>{
  assert.equal(wireBytes("x".repeat(LIMITS.wire)),LIMITS.wire);
  invalid(()=>wireBytes("x".repeat(LIMITS.wire+1)),1009);
  assert.equal(wireBytes("🦀",4),4);invalid(()=>wireBytes("🦀",3),1009);
  assert.equal(wireBytes("\ud800",3),3);invalid(()=>wireBytes(new Uint8Array(1)),1008);
});
test("pre-ready traffic cannot reserve bulk memory, and announced lengths reject before allocation",()=>{
  const budget=new MemoryBudget(LIMITS.encrypted*3),assembler=new Assembler(budget);
  const part=fragments("hello",id).next().value;
  invalid(()=>assembler.push(part,false));assert.equal(budget.used,0);
  invalid(()=>assembler.push({...part,bytes:LIMITS.encrypted+1},true),1009);assert.equal(budget.used,0);
  invalid(()=>assembler.push({...part,index:1},true));assert.equal(budget.used,0);
  invalid(()=>validateFragment({...part,data:"é"}));
});
test("opaque encrypted maximum preserves the full 32MiB logical envelope with lazy fragments",()=>{
  // Wire-only maximum; actual AES/IPC roundtrips belong to endpoint fixtures.
  const budget=new MemoryBudget(LIMITS.encrypted*3),assembler=new Assembler(budget);
  const frame=("0123456789ABCDEF".repeat(Math.ceil(LIMITS.encrypted/16))).slice(0,LIMITS.encrypted);
  let result, count=0;
  for(const part of fragments(frame,id)){
    assert.ok(wireBytes(JSON.stringify(part))<=LIMITS.wire);
    result=assembler.push(part,true,0);count++;
  }
  assert.equal(count,Math.ceil(LIMITS.encrypted/LIMITS.chunk));
  assert.equal(result.frame,frame);assert.equal(budget.used,LIMITS.encrypted*3);
  assembler.abort();assert.equal(budget.used,LIMITS.encrypted*3,"consumer still owns its retained payload");
  invalid(()=>assembler.push(fragments("next",id).next().value,true));
  result.release();result.release();assert.equal(budget.used,0);
  invalid(()=>fragments("x".repeat(LIMITS.encrypted+1),id).next(),1009);
});
test("duplicate/reordered/expired fragments abort only their own bounded assembler",()=>{
  const budget=new MemoryBudget(12*LIMITS.chunk),a=new Assembler(budget),b=new Assembler(budget);
  const parts=[...fragments("x".repeat(LIMITS.chunk+1),id)];
  assert.equal(a.push(parts[0],true,0),null);assert.equal(b.push(parts[0],true,0),null);
  const charged=budget.used;invalid(()=>a.push(parts[0],true,1));assert.equal(budget.used,charged/2);
  const result=b.push(parts[1],true,1);assert.equal(result.frame.length,LIMITS.chunk+1);result.release();assert.equal(budget.used,0);
  a.push(parts[0],true,0);invalid(()=>a.push(parts[1],true,LIMITS.assemblyMs));assert.equal(budget.used,0);
});
test("delivery ACK requires an unpredictable received token and actual socket/generation",()=>{
  const room=new RoomCredits(),owner={},window=new DeliveryWindow(room,owner,generation);
  assert.equal(window.ack(owner,generation,"0".repeat(22)),null,"pre-ACK grants nothing");
  const token=window.issue(100,7,0);assert.match(token,/^[A-Za-z0-9_-]{22}$/);
  assert.equal(window.ack({},generation,token),null);assert.equal(window.ack(owner,"c".repeat(32),token),null);
  assert.equal(window.ack(owner,generation,"0".repeat(22)),null);assert.equal(room.raw,100);
  assert.equal(window.ack(owner,generation,token),7);assert.equal(room.raw,0);
  assert.equal(window.ack(owner,generation,token),null,"replayed ACK cannot refund twice");
  const second=window.issue(100,8,0);assert.notEqual(second,token);window.teardown();assert.equal(room.raw,0);
});
test("shared excess cannot consume a healthy CID's progress reserve",()=>{
  const room=new RoomCredits();
  const windows=[0,1,2].map(()=>new DeliveryWindow(room,{},generation));
  for(const window of windows){for(let i=0;i<LIMITS.windowFrames;i++)if(!window.issue(LIMITS.wire,i,0))break;}
  assert.ok(room.excess>0);assert.ok(room.excess<=LIMITS.sharedWire);
  const healthy=new DeliveryWindow(room,{},generation);
  assert.ok(healthy.issue(LIMITS.wire,1,0),"one full fragment remains available despite stalled peers");
  assert.ok(room.raw<=MAX_ROOM_WIRE);assert.ok(ROOM_ACCOUNTED_BYTES<LIMITS.roomMemory);
  for(const window of [...windows,healthy]){assert.ok(window.bytes<=MAX_WINDOW_BYTES);window.teardown();}
  assert.equal(room.excess,0);assert.equal(room.raw,0);
});
test("closing/expired native deliveries remain charged until actual teardown or proven receipt",()=>{
  const room=new RoomCredits(),window=new DeliveryWindow(room,{},generation);
  window.issue(LIMITS.wire,1,0);assert.equal(window.expired(LIMITS.ackMs-1),false);assert.equal(window.expired(LIMITS.ackMs),true);
  window.close();assert.equal(room.raw,LIMITS.wire);assert.equal(window.issue(1,2,0),null);
  window.teardown();window.teardown();assert.equal(room.raw,0);
});
test("hibernation restore retains token, generation, closing and expiration without payload storage",()=>{
  const room=new RoomCredits(),owner={},window=new DeliveryWindow(room,owner,generation);
  const token=window.issue(LIMITS.wire,4,100);window.close();const snapshot=window.snapshot();
  const rehydratedRoom=new RoomCredits();const restored=DeliveryWindow.restore(rehydratedRoom,owner,JSON.parse(JSON.stringify(snapshot)),100);
  assert.equal(rehydratedRoom.raw,LIMITS.wire);assert.equal(restored.expired(100+LIMITS.ackMs),true);
  assert.equal(restored.issue(1,5,100),null);assert.equal(restored.ack(owner,generation,token),4);assert.equal(rehydratedRoom.raw,0);
  invalid(()=>DeliveryWindow.restore(new RoomCredits(),owner,{...snapshot,e:[...snapshot.e,...snapshot.e]},100));
  for(const c of [undefined,null,1,"false"]){const budget=new RoomCredits();invalid(()=>DeliveryWindow.restore(budget,owner,{...snapshot,c},100));assert.equal(budget.raw,0,"bad lifecycle cannot acquire/reopen credit");}
});
test("max compact per-socket delivery tables leave bounded attachment metadata headroom",()=>{
  const room=new RoomCredits(),a=new DeliveryWindow(room,{},generation),b=new DeliveryWindow(room,{},generation);
  for(let i=0;i<8;i++){assert.ok(a.issue(LIMITS.wire,i,0));assert.ok(b.issue(LIMITS.wire,i,0));}
  const attachment={role:"client",cid:"c".repeat(36),ua:"U".repeat(200),ready:true,g:generation,up:a.snapshot(),down:b.snapshot(),deadline:1234567890123,rate:[1,2,3,4]};
  assert.ok(JSON.stringify(attachment).length<2048,"compact state contains receipt metadata, never ciphertext");
  a.teardown();b.teardown();
});

test("keeping a released result handle cannot retain the completed assembly array",()=>{
  const script=`
    import {Assembler,MemoryBudget,fragments,LIMITS} from ${JSON.stringify(new URL('./web/transport.mjs',import.meta.url).href)};
    const Native=globalThis.Uint8Array;let weak;
    globalThis.Uint8Array=class extends Native { constructor(...args){super(...args);weak=new WeakRef(this);} };
    const budget=new MemoryBudget(3*LIMITS.chunk),assembler=new Assembler(budget);
    globalThis.retained=assembler.push(fragments('X'.repeat(LIMITS.chunk),'a'.repeat(32)).next().value,true);
    globalThis.retained.release();
    if(budget.used!==0||globalThis.retained.frame!==null)throw new Error('release failed');
    for(let i=0;i<8;i++){await new Promise(setImmediate);globalThis.gc();}
    await new Promise(setImmediate);
    if(weak.deref())throw new Error('released handle retained assembly array');
  `;
  execFileSync(process.execPath,['--expose-gc','--input-type=module','-e',script],{timeout:3000,stdio:'pipe'});
});

test("sender grants reserve space but cannot pre-ACK the independent destination receipt",()=>{
  const room=new RoomCredits(),source={},destination={},permits=new SendPermits(room,source,generation,destination,id);
  const permit=permits.grant(1,0);assert.ok(permit);assert.equal(room.raw,LIMITS.wire);
  assert.equal(permits.acknowledge(destination,id,permit),null,'sender-visible token is not receipt');
  assert.equal(permits.redeem({},generation,permit,100,0),null);
  assert.equal(permits.redeem(source,id,permit,100,0),null);
  const sent=permits.redeem(source,generation,permit,100,0);assert.ok(sent);assert.notEqual(sent.receipt,permit);
  assert.equal(permits.redeem(source,generation,permit,100,0),null,'permit is single-use');
  assert.equal(permits.acknowledge(source,id,sent.receipt),null,'actual destination identity required');
  assert.equal(permits.acknowledge(destination,generation,sent.receipt),null);
  assert.equal(permits.acknowledge(destination,id,sent.receipt),1);assert.equal(room.raw,0);
  assert.equal(permits.acknowledge(destination,id,sent.receipt),null);
});
test("closed browser source leaves sent UP charged across wake until actual daemon receipt or teardown",()=>{
  const room=new RoomCredits(),browser={},daemon={},permits=new SendPermits(room,browser,generation,daemon,id);
  const used=permits.grant(10,0);permits.grant(11,0);
  const sent=permits.redeem(browser,generation,used,200,0);
  permits.closeSource();assert.equal(room.raw,LIMITS.wire,'only unused grant can be freed');
  assert.equal(permits.grant(12,0),null);
  const wakeRoom=new RoomCredits(),wakeDaemon={},restored=SendPermits.restore(wakeRoom,Symbol('closed Browser'),wakeDaemon,JSON.parse(JSON.stringify(permits.snapshot())),0);
  assert.equal(wakeRoom.raw,LIMITS.wire);
  assert.equal(restored.acknowledge(wakeDaemon,generation,sent.receipt),null,'wrong daemon generation cannot refund orphan');
  assert.equal(restored.acknowledge({},id,sent.receipt),null);
  assert.equal(restored.acknowledge(wakeDaemon,id,sent.receipt),10);assert.equal(wakeRoom.raw,0);
  restored.teardownDestination();assert.equal(wakeRoom.raw,0);
});
test("pending grants are bounded, expire absolutely and preserve another CID progress reserve",()=>{
  const room=new RoomCredits(),source={},destination={},stalled=new SendPermits(room,source,generation,destination,id);
  for(let i=0;i<LIMITS.windowFrames;i++)assert.ok(stalled.grant(i,0));
  assert.equal(stalled.grant(99,0),null);assert.equal(stalled.expired(LIMITS.ackMs),true);
  const healthy=new SendPermits(room,{},generation,{},id);assert.ok(healthy.grant(1,0));
  const malformed={...stalled.snapshot(),c:null};const other=new RoomCredits();invalid(()=>SendPermits.restore(other,source,destination,malformed,0));assert.equal(other.raw,0);
  stalled.closeSource();assert.equal(stalled.bytes,0,'unused grants have no native writes');
  healthy.closeSource();assert.equal(room.raw,0);
});

test("endpoint bounds producer fragments and waits for final consumer before acknowledging",async()=>{
  const frames=[],closed=[],memory=new MemoryBudget(3*LIMITS.encrypted);
  let complete;const consume=new Promise(resolve=>{complete=resolve;});
  const endpoint=new WireEndpoint({generation,send:frame=>frames.push(JSON.parse(frame)),close:code=>closed.push(code),consume:()=>consume,memory});
  endpoint.setReady();
  const part=fragments('received',id).next().value;
  endpoint.receive(JSON.stringify({relay:'delivery',fragment:part,g:generation,receipt:'A'.repeat(22)}),0);
  await new Promise(setImmediate);assert.equal(frames.length,0,'final ACK waits actual application consumption');
  assert.equal(memory.used,3*'received'.length);
  complete();await new Promise(setImmediate);
  assert.equal(memory.used,0);assert.equal(frames[0].relay,'ack');assert.equal(frames[0].receipt,'A'.repeat(22));assert.deepEqual(closed,[]);
});
test("endpoint never serializes an unbounded producer queue and releases its owned source lease",async()=>{
  const outgoing=[],endpoint=new WireEndpoint({generation,send:frame=>outgoing.push(JSON.parse(frame)),close(){},consume(){},memory:new MemoryBudget(100)});
  endpoint.setReady();let released=0;
  const sent=endpoint.start('x'.repeat(10*LIMITS.chunk),{release(){released++;}});
  assert.equal(outgoing.length,LIMITS.windowFrames);assert.equal(endpoint.pending.size,LIMITS.windowFrames);
  let denied=0;await assert.rejects(endpoint.start('second',{release(){denied++;}}));assert.equal(denied,1);
  while(endpoint.current){
    const needs=outgoing.splice(0).filter(packet=>packet.relay==='need');
    for(const need of needs){
      endpoint.receive(JSON.stringify({relay:'grant',g:generation,send_id:need.send_id,permit:'A'.repeat(22)}));
      const packet=outgoing.find(packet=>packet.relay==='data');assert.ok(packet);assert.ok(wireBytes(JSON.stringify(packet))<=LIMITS.wire);
      outgoing.splice(outgoing.indexOf(packet),1);
      endpoint.receive(JSON.stringify({relay:'credit',g:generation,send_id:need.send_id}));
    }
  }
  await sent;assert.equal(released,1);endpoint.dispose();assert.equal(released,1);
});
test("endpoint abort during consumption does not refund still-retained incoming memory",async()=>{
  const memory=new MemoryBudget(100);let complete;const wait=new Promise(resolve=>{complete=resolve;});
  const endpoint=new WireEndpoint({generation,send(){},close(){},consume:()=>wait,memory});endpoint.setReady();
  endpoint.receive(JSON.stringify({relay:'delivery',fragment:fragments('held',id).next().value,g:generation,receipt:'A'.repeat(22)}),0);
  await new Promise(setImmediate);endpoint.dispose();assert.equal(memory.used,12);
  complete();await new Promise(setImmediate);assert.equal(memory.used,0);
});


test('downward-only leases retain exact current bytes through release',()=>{
  const budget=new MemoryBudget(100),lease=budget.reserve(90);
  lease.shrink(50);assert.equal(budget.used,50);
  assert.throws(()=>lease.shrink(51));assert.equal(budget.used,50);
  assert.throws(()=>lease.shrink(-1));assert.equal(budget.used,50);
  lease.release();lease.release();assert.equal(budget.used,0);
  assert.throws(()=>lease.shrink(0));assert.equal(budget.used,0);
});
