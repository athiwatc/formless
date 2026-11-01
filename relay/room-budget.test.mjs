import test from 'node:test';
import {mkdtempSync,rmSync,statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {RoomBudget,ROOM_LIMITS} from './room-budget.mjs';
import {DevStorage} from './dev-storage.mjs';
import {LIMITS,fragments,TransportError} from './web/transport.mjs';
const browser='b'.repeat(32),daemon='d'.repeat(32),generation='a'.repeat(32),dg='e'.repeat(32),identity='11111111-1111-4111-8111-111111111111';
const now=Date.now();
function setup(storage=new DevStorage()){const budget=new RoomBudget(storage.sql);budget.addNative({sid:daemon,g:dg,role:'daemon'},now);budget.addNative({sid:browser,g:generation,role:'client'},now);budget.addPeer(browser,daemon,{cid:identity,g:generation},now);return{storage,budget};}
function ready(budget){budget.ready(daemon,identity,generation,now);const a=budget.control(browser,200,identity,now),b=budget.control(daemon,200,identity,now);budget.ackControl(browser,generation,a,now);budget.ackControl(daemon,dg,b,now);}
function packet(permit){return{...fragments('opaque','f'.repeat(32)).next().value,permit};}
const invalid=fn=>assert.throws(fn,e=>e instanceof TransportError);

test('readiness is daemon-owned and grants wait actual destination ready-control consumption',()=>{
 const{storage,budget}=setup();try{
  invalid(()=>budget.ready(browser,identity,generation,now));invalid(()=>budget.need(browser,identity,generation,1,now));
  budget.ready(daemon,identity,generation,now);
  assert.deepEqual(budget.need(browser,identity,generation,1,now),[]);
  const token=budget.control(daemon,200,identity,now);
  const effects=budget.ackControl(daemon,dg,token,now);assert.equal(effects.length,1);assert.equal(effects[0].packet.relay,'grant');
  assert.equal(effects[0].sid,browser);assert.equal(budget.describe(now).wireBytes,LIMITS.wire);
 }finally{storage.close();}
});
test('sender permit cannot pre-ACK; actual destination and generation alone settle sent bytes',()=>{
 const{storage,budget}=setup();try{ready(budget);
  const grant=budget.need(browser,identity,generation,1,now)[0];
  invalid(()=>budget.ack(browser,identity,generation,grant.packet.permit,now));
  const delivered=budget.deliver(browser,identity,generation,packet(grant.packet.permit),100,now);
  assert.notEqual(delivered.packet.receipt,grant.packet.permit);
  invalid(()=>budget.ack(browser,identity,generation,delivered.packet.receipt,now));
  invalid(()=>budget.ack(daemon,identity,'c'.repeat(32),delivered.packet.receipt,now));
  assert.equal(budget.describe(now).wireBytes,LIMITS.wire);
  const effects=budget.ack(daemon,identity,generation,delivered.packet.receipt,now);
  assert.equal(effects[0].packet.relay,'credit');assert.equal(budget.describe(now).wireBytes,0);
  invalid(()=>budget.ack(daemon,identity,generation,delivered.packet.receipt,now));
 }finally{storage.close();}
});
test('browser actual-close preserves daemon-owned UP across SQLite rehydrate; wrong and replay ACKs cannot refund',()=>{
 const{storage,budget}=setup();try{ready(budget);
  const grant=budget.need(browser,identity,generation,1,now)[0];
  const delivered=budget.deliver(browser,identity,generation,packet(grant.packet.permit),100,now);
  budget.closing(browser,now);assert.equal(budget.describe(now).wireBytes,LIMITS.wire);
  budget.actualClose(browser,now);assert.equal(budget.describe(now).wireBytes,LIMITS.wire);assert.equal(budget.describe(now).peers.length,1);
  const woke=new RoomBudget(storage.sql);
  const closeReceipt=woke.control(daemon,100,null,now,identity,'close');woke.ackControl(daemon,dg,closeReceipt,now);
  invalid(()=>woke.ack(browser,identity,generation,delivered.packet.receipt,now));
  invalid(()=>woke.ack(daemon,identity,'c'.repeat(32),delivered.packet.receipt,now));
  assert.equal(woke.describe(now).wireBytes,LIMITS.wire);
  woke.ack(daemon,identity,generation,delivered.packet.receipt,now);assert.equal(woke.describe(now).wireBytes,0);assert.equal(woke.describe(now).peers.length,0);
 }finally{storage.close();}
});
test('actual daemon destination close, not close request, proves UP queue teardown',()=>{
 const{storage,budget}=setup();try{ready(budget);const grant=budget.need(browser,identity,generation,1,now)[0];budget.deliver(browser,identity,generation,packet(grant.packet.permit),100,now);
 budget.closing(daemon,now);assert.equal(budget.describe(now).wireBytes,LIMITS.wire);budget.actualClose(daemon,now);assert.equal(budget.describe(now).wireBytes,0);
 }finally{storage.close();}
});
test('bounded waiter count, duplicate sequence, timeout and reordering fail closed without payload storage',()=>{
 const{storage,budget}=setup();try{ready(budget);
 const first=budget.need(browser,identity,generation,1,now)[0];invalid(()=>budget.need(browser,identity,generation,1,now));
 for(let i=2;i<=8;i++)budget.need(browser,identity,generation,i,now);invalid(()=>budget.need(browser,identity,generation,9,now));
 invalid(()=>budget.deliver(browser,identity,generation,{...packet(first.packet.permit),index:1},100,now));
 assert.ok(budget.deadlines(now+LIMITS.ackMs).cids.includes(identity));
 const serialized=storage.sql.exec('SELECT state FROM relay_transport_state WHERE id=1')[0].state;
 assert.ok(!serialized.includes('opaque'));assert.ok(serialized.length<ROOM_LIMITS.stateBytes);
 }finally{storage.close();}
});
test('failed SQL commit publishes neither memory credit nor token, so restore retains prior authority',()=>{
 const{storage,budget}=setup();try{ready(budget);const old=budget.encoded,exec=storage.sql.exec;storage.sql.exec=()=>{throw new Error('owned commit failure');};
 assert.throws(()=>budget.need(browser,identity,generation,1,now),/owned commit failure/);assert.equal(budget.encoded,old);
 storage.sql.exec=exec;assert.equal(new RoomBudget(storage.sql).describe(now).wireBytes,0);
 }finally{storage.close();}
});
test('unknown persisted lifecycle refuses restore instead of resetting credit authority',()=>{
 const{storage,budget}=setup();try{ready(budget);budget.need(browser,identity,generation,1,now);const state=JSON.parse(budget.encoded);state.peers[0].up.c=null;
 storage.sql.exec('UPDATE relay_transport_state SET state=? WHERE id=1',JSON.stringify(state));invalid(()=>new RoomBudget(storage.sql));
 }finally{storage.close();}
});

test('all 16 orphan CIDs retain admission slots until actual daemon receipts drain them',()=>{
 const{storage,budget}=setup();try{
  // Replace the initial idle CID so every retained slot is a genuine sent UP.
  budget.actualClose(browser,now);const initialClose=budget.control(daemon,100,null,now,identity,'close');budget.ackControl(daemon,dg,initialClose,now);
  const receipts=[];
  for(let i=0;i<LIMITS.clients;i++){
   const sid=(i+1).toString(16).padStart(32,'0'),g=(i+100).toString(16).padStart(32,'0'),c=crypto.randomUUID();
   budget.addNative({sid,g,role:'client'},now);budget.addPeer(sid,daemon,{cid:c,g},now);budget.ready(daemon,c,g,now);
   const control=budget.control(daemon,100,c,now);budget.ackControl(daemon,dg,control,now);
   const grant=budget.need(sid,c,g,1,now)[0];const delivery=budget.deliver(sid,c,g,packet(grant.packet.permit),100,now);
   budget.actualClose(sid,now);const closeReceipt=budget.control(daemon,100,null,now,c,'close');budget.ackControl(daemon,dg,closeReceipt,now);receipts.push([c,g,delivery.packet.receipt]);
  }
  const newcomer='9'.repeat(32),newg='8'.repeat(32);budget.addNative({sid:newcomer,g:newg,role:'client'},now);
  invalid(()=>budget.addPeer(newcomer,daemon,{cid:crypto.randomUUID(),g:newg},now));
  assert.equal(budget.describe(now).peers.length,16);assert.equal(budget.describe(now).wireBytes,16*LIMITS.wire);
  const woke=new RoomBudget(storage.sql);woke.ack(daemon,...receipts[0],now);
  woke.addPeer(newcomer,daemon,{cid:crypto.randomUUID(),g:newg},now);assert.equal(woke.describe(now).peers.length,16);
 }finally{storage.close();}
});
test('a healthy CID retains one full fragment reserve after other lanes exhaust shared excess',()=>{
 const{storage,budget}=setup();try{ready(budget);
  for(let i=1;i<=8;i++)budget.need(browser,identity,generation,i,now);
  const peers=[];
  for(let i=1;i<=3;i++){
   const sid=String(i).repeat(32),g=String(i+3).repeat(32),c=crypto.randomUUID();peers.push({sid,g,c});
   budget.addNative({sid,g,role:'client'},now);budget.addPeer(sid,daemon,{cid:c,g},now);budget.ready(daemon,c,g,now);
   budget.ackControl(daemon,dg,budget.control(daemon,100,c,now),now);
   if(i<3)for(let send=1;send<=8;send++)budget.need(sid,c,g,send,now);
  }
  const healthy=peers.at(-1),granted=budget.need(healthy.sid,healthy.c,healthy.g,1,now);
  assert.ok(granted.some(effect=>effect.sid===healthy.sid));assert.ok(budget.describe(now).wireExcess<=LIMITS.sharedWire);
 }finally{storage.close();}
});

test('a final fragment remains a source logical barrier until its destination receipt',()=>{
 const{storage,budget}=setup();try{ready(budget);const first=budget.need(browser,identity,generation,1,now)[0];const delivery=budget.deliver(browser,identity,generation,packet(first.packet.permit),100,now);const next=budget.need(browser,identity,generation,2,now)[0];
 invalid(()=>budget.deliver(browser,identity,generation,{...packet(next.packet.permit),id:'c'.repeat(32)},100,now));
 budget.ack(daemon,identity,generation,delivery.packet.receipt,now);assert.ok(budget.deliver(browser,identity,generation,{...packet(next.packet.permit),id:'c'.repeat(32)},100,now));
 }finally{storage.close();}
});
test('closed CID with outstanding daemon controls retains its admission slot until every control settles',()=>{
 const{storage,budget}=setup();try{const opened=budget.control(daemon,100,null,now,identity,'open');budget.actualClose(browser,now);assert.equal(budget.describe(now).peers.length,1);
 const closed=budget.control(daemon,100,null,now,identity,'close');budget.ackControl(daemon,dg,closed,now);assert.equal(budget.describe(now).peers.length,1);
 budget.ackControl(daemon,dg,opened,now);assert.equal(budget.describe(now).peers.length,0);
 }finally{storage.close();}
});
test('bounded handshake attempts do not become a generic pre-auth bulk allowance',()=>{
 const{storage,budget}=setup();try{for(let i=0;i<6;i++)assert.equal(budget.handshake(browser,identity,generation,now),daemon);invalid(()=>budget.handshake(browser,identity,generation,now));assert.equal(budget.describe(now).wireBytes,0);assert.ok(budget.deadlines(now+LIMITS.handshakeMs).cids.includes(identity));
 }finally{storage.close();}
});

test('real private on-disk SQLite reopen preserves orphan credit without payloads or live cache',()=>{
 const root=mkdtempSync(join(tmpdir(),'fl-relay-ledger-')),file=join(root,'room.db');let storage=new DevStorage(()=>{},file);
 try{
  const{budget}=setup(storage);ready(budget);const grant=budget.need(browser,identity,generation,1,now)[0],delivery=budget.deliver(browser,identity,generation,packet(grant.packet.permit),100,now);
  budget.actualClose(browser,now);assert.ok(statSync(file).size>0);storage.close();storage=null;
  storage=new DevStorage(()=>{},file);const woke=new RoomBudget(storage.sql);
  assert.equal(woke.describe(now).wireBytes,LIMITS.wire);assert.equal(woke.describe(now).peers.length,1);
  invalid(()=>woke.ack(browser,identity,generation,delivery.packet.receipt,now));assert.equal(woke.describe(now).wireBytes,LIMITS.wire);
  const closed=woke.control(daemon,100,null,now,identity,'close');woke.ackControl(daemon,dg,closed,now);woke.ack(daemon,identity,generation,delivery.packet.receipt,now);
  assert.equal(woke.describe(now).wireBytes,0);assert.equal(woke.describe(now).peers.length,0);
  assert.equal(storage.sql.exec('PRAGMA integrity_check')[0].integrity_check,'ok');assert.ok(!woke.encoded.includes('opaque'));
 }finally{storage?.close();rmSync(root,{recursive:true,force:true});}
});

test('logical cancellation receipts cannot free a CID while its browser native socket remains present',()=>{const{storage,budget}=setup();try{
 budget.expireCID(identity,now);const closeReceipt=budget.control(daemon,100,null,now,identity,'close');budget.ackControl(daemon,dg,closeReceipt,now);
 assert.equal(budget.describe(now).peers.length,1);assert.equal(budget.canAdmit('client',now),true,'other free slots remain available but this closing CID is still counted');
 budget.actualClose(browser,now);assert.equal(budget.describe(now).peers.length,0);
 }finally{storage.close();}});
test('persisted owner role, missing active browser and generation mismatches fail before restoring credit',()=>{
 for(const mutate of [s=>{s.peers[0].daemon='f'.repeat(32);},s=>{s.natives.find(n=>n.sid===daemon).role='client';},s=>{s.peers[0].browser='f'.repeat(32);},s=>{s.natives.find(n=>n.sid===browser).g='f'.repeat(32);}]){
  const{storage,budget}=setup();try{ready(budget);budget.need(browser,identity,generation,1,now);const corrupt=JSON.parse(budget.encoded);mutate(corrupt);storage.sql.exec('UPDATE relay_transport_state SET state=? WHERE id=1',JSON.stringify(corrupt));invalid(()=>new RoomBudget(storage.sql));}finally{storage.close();}
 }
});
