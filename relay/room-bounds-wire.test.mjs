import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {wireWait} from './test-wire.mjs';
import {LIMITS} from './web/transport.mjs';
import {server,endpoints,EndpointPeer,deadline,deferred} from './test-bounded-wire.mjs';
test('real dev native 127 frames carry maximum opaque logical envelope under bounded credits',{timeout:30000},async()=>{
 const s=await server();try{let received;const{browser}=await endpoints(s,frame=>{received={length:frame.length,hash:createHash('sha256').update(frame).digest('hex')};});
 const frame='X'.repeat(LIMITS.encrypted),expected=createHash('sha256').update(frame).digest('hex');
 await deadline(browser.channels.get('browser').start(frame),'maximum encrypted envelope',25000);
 assert.deepEqual(received,{length:LIMITS.encrypted,hash:expected});assert.equal(browser.peer.closed,null);
 }finally{await s.close();}
});
test('one CID final consumer stall cannot block another CID request or refund its pending receipt',{timeout:15000},async()=>{
 const s=await server(),entered=deferred(),release=deferred();try{
 let stalledCID,healthyReceived=false;const{daemon,browser:stalled}=await endpoints(s,async(frame,cid)=>{if(cid===stalledCID){entered.resolve();await release.promise;}else healthyReceived=frame==='healthy';});stalledCID=stalled.cid;
 let finished=false;const pending=stalled.channels.get('browser').start('S'.repeat(1024*1024)).then(()=>{finished=true;});
 await deadline(entered.promise,'stalled receiver entered');
 const healthy=new EndpointPeer(await s.open('client'),'client');await healthy.waitFor(()=>healthy.cid);const channel=await daemon.waitChannel(healthy.cid);daemon.peer.send(JSON.stringify({relay:'ready',cid:healthy.cid,g:channel.generation}));await deadline(Promise.all([daemon.waitReady(healthy.cid),healthy.waitReady(healthy.cid)]),'healthy ready');
 await deadline(healthy.channels.get('browser').start('healthy'),'healthy progress');assert.equal(healthyReceived,true);assert.equal(finished,false);
 release.resolve();await deadline(pending,'released stalled consumer');assert.equal(finished,true);
 }finally{release.resolve();await s.close();}
});
test('real pre-ready native declared oversize is rejected from header without a body',{timeout:10000},async()=>{const s=await server();try{const p=await s.open('client');p.declareTextBytes(64*1024*1024);assert.equal(await wireWait(p,p=>p.closed),1009);}finally{await s.close();}});

test('native ping flood cannot accumulate unbounded Pong writes or close a healthy CID',{timeout:15000},async()=>{const s=await server();try{
 let received;const{daemon,browser}=await endpoints(s,frame=>{received=frame;});const flooding=await s.open('client');
 for(let i=0;i<8193;i++)flooding.ping();
 assert.ok(await wireWait(flooding,p=>p.closed),'bounded native protocol-control queue/rate must close the source');
 await deadline(browser.channels.get('browser').start('healthy after ping flood'),'healthy CID after native control flood');
 assert.equal(received,'healthy after ping flood');assert.equal(daemon.peer.closed,null);assert.equal(browser.peer.closed,null);
 }finally{await s.close();}});
