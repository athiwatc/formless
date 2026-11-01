import test from 'node:test';
import assert from 'node:assert/strict';
import {RelayRoom} from './worker.js';
import {DevStorage} from './dev-storage.mjs';
import {LIMITS,fragments} from './web/transport.mjs';
function socket(){return{readyState:1,att:null,sent:[],seen:0,closed:null,serializeAttachment(a){this.att=structuredClone(a);},deserializeAttachment(){return structuredClone(this.att);},send(s){if(this.readyState!==1)throw Error('closed');this.sent.push(JSON.parse(s));},close(code,reason){this.closed={code,reason};this.readyState=2;},setMessageLimit(n){this.max=n;}};}
function fixture(){const storage=new DevStorage(),sockets=[];const state={storage,acceptWebSocket(ws,tags){ws.tags=tags;sockets.push(ws);},getWebSockets(tag){return sockets.filter(ws=>ws.readyState!==3&&(!tag||ws.tags.includes(tag)));},waitUntil(p){p.catch(()=>{});}};const room=new RelayRoom(state,{});return{room,state,storage,sockets,close(){storage.close();}};}
async function ackControls(room,ws){while(ws.seen<ws.sent.length){const p=ws.sent[ws.seen++];if(p.relay==='control')await room.webSocketMessage(ws,JSON.stringify({relay:'ack_control',g:p.g,receipt:p.receipt}));}}
async function pair(f){const d=socket(),b=socket();f.room.admit(d,'daemon',true,'',true);await ackControls(f.room,d);f.room.admit(b,'client',true,'fixture');await ackControls(f.room,d);await ackControls(f.room,b);return{d,b};}
async function ready(f,d,b){await f.room.webSocketMessage(d,JSON.stringify({relay:'ready',cid:b.att.cid,g:b.att.g}));await ackControls(f.room,b);await ackControls(f.room,d);}
function send(f,ws,p){return f.room.webSocketMessage(ws,JSON.stringify(p));}
function last(ws,kind){return ws.sent.filter(p=>p.relay===kind).at(-1);}

test('Worker refuses bulk before readiness before JSON parsing and keeps daemon healthy',async()=>{const f=fixture();try{const{d,b}=await pair(f);const ds=d.sent.length;await f.room.webSocketMessage(b,'x'.repeat(LIMITS.preReady+1));assert.equal(b.closed.code,1009);assert.equal(d.closed,null);assert.equal(d.sent.length,ds+1);assert.equal(d.sent.at(-1).relay,'control');assert.deepEqual(JSON.parse(d.sent.at(-1).f),{relay:'client_close',cid:b.att.cid,g:b.att.g});assert.equal(f.room.budget.describe().wireBytes,0);}finally{f.close();}});
test('Worker distinct receipt owns credits; browser-close + hibernation cannot refund daemon UP',async()=>{const f=fixture();try{const{d,b}=await pair(f);await ready(f,d,b);await send(f,b,{relay:'need',g:b.att.g,send_id:1});const grant=last(b,'grant');assert.ok(grant);await send(f,b,{...fragments('opaque frame','a'.repeat(32)).next().value,g:b.att.g,permit:grant.permit});const delivery=last(d,'delivery');assert.notEqual(delivery.receipt,grant.permit);assert.equal(delivery.cid,b.att.cid);const att=structuredClone(b.att);b.readyState=3;b.att=null;await f.room.webSocketClose(b);assert.equal(f.room.budget.describe().wireBytes,LIMITS.wire);const woke=new RelayRoom(f.state,{});await ackControls(woke,d);await send({room:woke},d,{relay:'ack',cid:att.cid,g:att.g,receipt:delivery.receipt});assert.equal(woke.budget.describe().wireBytes,0);assert.equal(d.closed,null);}finally{f.close();}});
test('Worker ready delivery waits destination control ACK and final receipt blocks next logical message',async()=>{const f=fixture();try{const{d,b}=await pair(f);await send(f,d,{relay:'ready',cid:b.att.cid,g:b.att.g});await send(f,b,{relay:'need',g:b.att.g,send_id:1});assert.equal(last(b,'grant'),undefined);await ackControls(f.room,d);const grant=last(b,'grant');assert.ok(grant);await send(f,b,{...fragments('one','a'.repeat(32)).next().value,g:b.att.g,permit:grant.permit});await send(f,b,{relay:'need',g:b.att.g,send_id:2});const second=last(b,'grant');await send(f,b,{...fragments('two','b'.repeat(32)).next().value,g:b.att.g,permit:second.permit});assert.equal(b.closed.code,1008);assert.equal(d.closed,null);assert.equal(d.sent.filter(p=>p.relay==='delivery').length,1);}finally{f.close();}});
test('Daemon replacement keeps immutable old attachment and revokes all old writes/control authority',async()=>{const f=fixture();try{const{d,b}=await pair(f);const original=structuredClone(d.att),replacement=socket();f.room.admit(replacement,'daemon',true,'',true);assert.deepEqual(d.att,original);assert.equal(d.closed.code,4003);assert.equal(b.closed.code,4002);const before=replacement.sent.length;await send(f,d,{relay:'ready',cid:b.att.cid,g:b.att.g});await send(f,d,{cid:b.att.cid,g:b.att.g,f:JSON.stringify({v:2,hello:btoa('x'.repeat(16))})});assert.equal(replacement.sent.length,before);assert.equal(f.room.muxDaemon(),replacement);}finally{f.close();}});
test('Closing sockets stay counted until actual close; absolute idle deadline has no refund shortcut',async()=>{const f=fixture();try{const{d,b}=await pair(f);await ready(f,d,b);await send(f,b,{relay:'need',g:b.att.g,send_id:1});const grant=last(b,'grant');await send(f,b,{...fragments('held','a'.repeat(32)).next().value,g:b.att.g,permit:grant.permit});f.room.requestClose(d,1008,'fixture timeout');assert.equal(f.room.budget.describe().wireBytes,LIMITS.wire);assert.ok(f.room.budget.describe().natives.some(n=>n.sid===d.att.sid&&n.closing));d.readyState=3;await f.room.webSocketClose(d);assert.equal(f.room.budget.describe().wireBytes,0);}finally{f.close();}});
test('handshake flood closes only the source and leaves another CID handshake progress',async()=>{const f=fixture();try{const{d,b}=await pair(f);const good=socket();f.room.admit(good,'client',true,'good');await ackControls(f.room,d);await ackControls(f.room,good);const hello=JSON.stringify({v:2,hello:btoa('h'.repeat(16))});for(let i=0;i<7;i++){await f.room.webSocketMessage(b,hello);await ackControls(f.room,d);}assert.equal(b.closed.code,1008);assert.equal(d.closed,null);const before=d.sent.length;await f.room.webSocketMessage(good,hello);assert.ok(d.sent.length>before);assert.equal(good.closed,null);}finally{f.close();}});

test('a granted daemon response racing actual browser close cannot disconnect other CIDs',async()=>{const f=fixture();try{
 const{d,b}=await pair(f);await ready(f,d,b);
 const healthy=socket();f.room.admit(healthy,'client',true,'healthy');await ackControls(f.room,d);await ackControls(f.room,healthy);await ready(f,d,healthy);
 await send(f,d,{relay:'need',cid:b.att.cid,g:b.att.g,send_id:1});const grant=last(d,'grant');assert.equal(grant.cid,b.att.cid);
 b.readyState=3;await f.room.webSocketClose(b);
 await send(f,d,{...fragments('late response','a'.repeat(32)).next().value,cid:b.att.cid,g:b.att.g,permit:grant.permit});
 assert.equal(d.closed,null);assert.equal(healthy.closed,null);
 await send(f,healthy,{relay:'need',g:healthy.att.g,send_id:1});const goodGrant=last(healthy,'grant');assert.ok(goodGrant);
 await send(f,healthy,{...fragments('healthy request','b'.repeat(32)).next().value,g:healthy.att.g,permit:goodGrant.permit});assert.equal(last(d,'delivery').cid,healthy.att.cid);assert.equal(d.closed,null);
 }finally{f.close();}});
test('hibernation-restored absolute handshake expiry requests close without falsely claiming teardown',async()=>{const f=fixture();const original=Date.now;try{
 const b=socket();f.room.admit(b,'client',true,'idle');const sid=b.att.sid;const deadlineAt=f.room.budget.describe().peers[0].deadline;Date.now=()=>deadlineAt;
 const woke=new RelayRoom(f.state,{});await woke.alarm();assert.equal(b.closed.code,1008);assert.ok(woke.budget.describe().natives.some(n=>n.sid===sid&&n.closing),'CLOSING remains admitted');
 b.readyState=3;await woke.webSocketClose(b);assert.ok(!woke.budget.describe().natives.some(n=>n.sid===sid));
 }finally{Date.now=original;f.close();}});

test('failed SQLite transition still requests native closure and never publishes fresh delivery authority',async()=>{const f=fixture();try{
 const{d,b}=await pair(f);await ready(f,d,b);const encoded=f.room.budget.encoded,before=d.sent.length,exec=f.storage.sql.exec;
 f.storage.sql.exec=()=>{throw Error('private fixture SQLite commit failure');};
 await send(f,b,{relay:'need',g:b.att.g,send_id:1});assert.ok(b.closed);assert.ok(d.closed);assert.equal(f.room.budget.encoded,encoded);assert.equal(d.sent.length,before);assert.equal(f.room.failed,true);
 f.storage.sql.exec=exec;assert.equal(f.room.budget.describe().wireBytes,0);
 }finally{f.close();}});

test('constructor hydration preserves an existing earlier alarm through the actual async initialization branch',async()=>{const f=fixture();try{
 const b=socket();f.room.admit(b,'client',true,'idle');const existing=Date.now()+5000;await f.storage.setAlarm(existing);
 let replacements=0;const set=f.storage.setAlarm.bind(f.storage);f.storage.setAlarm=async at=>{replacements++;return set(at);};
 f.state.blockConcurrencyWhile=callback=>callback();const woke=new RelayRoom(f.state,{});await woke.ready;
 assert.equal(f.storage.getAlarm(),existing);assert.equal(woke.scheduledAlarm,existing);assert.equal(replacements,0,'hydration must not postpone an already scheduled alarm');
 }finally{f.close();}});

test('actual daemon departure closes bound browser generation; a replacement cannot recycle it',async()=>{const f=fixture();try{
 const{d,b}=await pair(f);await ready(f,d,b);const old={cid:b.att.cid,g:b.att.g};d.readyState=3;await f.room.webSocketClose(d);
 assert.equal(b.closed.code,4002);assert.ok(b.sent.some(p=>p.relay==='control'&&JSON.parse(p.f).relay==='daemon_offline'));
 const replacement=socket();f.room.admit(replacement,'daemon',true,'',true);
 assert.ok(!replacement.sent.some(p=>p.relay==='control'&&JSON.parse(p.f).cid===old.cid),'closing generation cannot attach to a new daemon');
 b.readyState=3;await f.room.webSocketClose(b);const fresh=socket();f.room.admit(fresh,'client',true,'');
 assert.notEqual(fresh.att.cid,old.cid);assert.notEqual(fresh.att.g,old.g);
 }finally{f.close();}});

test('UP receipt expiry cancels only its CID; a responsive daemon keeps healthy progress and late receipt ownership',async()=>{const f=fixture(),clock=Date.now;try{
 const{d,b}=await pair(f);await ready(f,d,b);const good=socket();f.room.admit(good,'client',true,'healthy');await ackControls(f.room,d);await ackControls(f.room,good);await ready(f,d,good);
 await send(f,b,{relay:'need',g:b.att.g,send_id:1});const grant=last(b,'grant');await send(f,b,{...fragments('held private CID','a'.repeat(32)).next().value,g:b.att.g,permit:grant.permit});const delivery=last(d,'delivery');
 const expiry=JSON.parse(f.room.budget.encoded).peers.find(p=>p.cid===b.att.cid).up.e[0][3];Date.now=()=>expiry;await f.room.alarm();
 assert.equal(b.closed.code,1008);assert.equal(d.closed,null);assert.equal(good.closed,null);assert.equal(f.room.budget.describe().wireBytes,LIMITS.wire,'logical cancellation cannot refund the retained UP delivery');
 assert.ok(d.sent.some(p=>p.relay==='control'&&JSON.parse(p.f).relay==='client_close'&&JSON.parse(p.f).cid===b.att.cid));
 await ackControls(f.room,d);b.readyState=3;await f.room.webSocketClose(b);
 Date.now=()=>expiry+LIMITS.ackMs+1;await f.room.alarm();assert.equal(d.closed,null,'a consumed cancellation control must not turn an orphan data timeout into a shared link failure');
 await send(f,good,{relay:'need',g:good.att.g,send_id:1});const healthyGrant=last(good,'grant');assert.ok(healthyGrant);
 await send(f,good,{...fragments('healthy after expiry','b'.repeat(32)).next().value,g:good.att.g,permit:healthyGrant.permit});assert.equal(last(d,'delivery').cid,good.att.cid);assert.equal(d.closed,null);
 const before=f.room.budget.describe().wireBytes;await send(f,d,{relay:'ack',cid:b.att.cid,g:b.att.g,receipt:delivery.receipt});assert.equal(f.room.budget.describe().wireBytes,before-LIMITS.wire,'late exact destination receipt frees only its own charge');
 }finally{Date.now=clock;f.close();}});
test('unconsumed shared cancellation control has a separate drain deadline and still cannot fake native teardown',async()=>{const f=fixture(),clock=Date.now;try{
 const{d,b}=await pair(f);await ready(f,d,b);await send(f,b,{relay:'need',g:b.att.g,send_id:1});await send(f,b,{...fragments('retained','a'.repeat(32)).next().value,g:b.att.g,permit:last(b,'grant').permit});
 const first=JSON.parse(f.room.budget.encoded).peers[0].up.e[0][3];Date.now=()=>first;await f.room.alarm();assert.equal(d.closed,null);assert.equal(f.room.budget.describe().wireBytes,LIMITS.wire);
 const snapshot=JSON.parse(f.room.budget.encoded),control=snapshot.natives.find(n=>n.sid===d.att.sid).controls.find(c=>c[4]===b.att.cid);assert.ok(control);
 Date.now=()=>Math.max(control[2],snapshot.peers[0].closeGrace);await f.room.alarm();assert.equal(d.closed.code,1008,'shared control drain failure is distinct from the original CID timeout');assert.equal(f.room.budget.describe().wireBytes,LIMITS.wire);
 d.readyState=3;await f.room.webSocketClose(d);assert.equal(f.room.budget.describe().wireBytes,0,'actual destination teardown, rather than close request, proves the remaining charge gone');
 }finally{Date.now=clock;f.close();}});

test('lost live socket attachment cannot masquerade as absent destination teardown or mint a new slot',async()=>{const f=fixture();try{
 const{d,b}=await pair(f);await ready(f,d,b);await send(f,d,{relay:'need',cid:b.att.cid,g:b.att.g,send_id:1});const grant=last(d,'grant');await send(f,d,{...fragments('held DOWN','a'.repeat(32)).next().value,cid:b.att.cid,g:b.att.g,permit:grant.permit});
 const original=structuredClone(b.att);b.att=null;const woke=new RelayRoom(f.state,{});
 assert.equal(woke.blocked,true);assert.equal(woke.budget.describe().wireBytes,LIMITS.wire,'unidentifiable but still-live socket may own that queued delivery');
 assert.throws(()=>woke.admit(socket(),'client',true,''));assert.equal(woke.budget.describe().wireBytes,LIMITS.wire);
 b.readyState=3;await woke.webSocketClose(b);assert.equal(woke.budget.describe().wireBytes,0,'only unambiguous actual inventory may settle the absent owner');assert.ok(!woke.budget.describe().natives.some(n=>n.sid===original.sid));
 let pushes=0;woke.pushAll=async()=>{pushes++;};await woke.webSocketMessage(d,JSON.stringify({relay:'push'}));assert.equal(pushes,0,'physical CLOSING cannot regain authority when the unknown inventory clears');
 }finally{f.close();}});
