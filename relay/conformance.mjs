// Same HTTP/control matrix for dev or an explicitly supplied matching relay.
import assert from 'node:assert/strict';
import {daemonClaim,claimRoom,claimAuthorization} from './daemon-claim.mjs';
import {wireOpen,wireWait} from './test-wire.mjs';
import {server,endpoints,EndpointPeer,deadline} from './test-bounded-wire.mjs';
import {conformanceBase} from './conformance-base.mjs';
let s,base=process.argv[2];
let explicit;
try {
 if(process.argv.length>3)throw new Error('expected at most one BASE argument');
 if(base!==undefined)explicit=conformanceBase(base);
}catch(error){
 console.error('Usage: node relay/conformance.mjs [HTTP(S)_ORIGIN]\n'+error.message);
 process.exit(2);
}
if(!explicit){s=await server();base=new URL(s.base).origin.replace(/^ws/,'http');}
else{
 base=explicit.http;
 const claim=await daemonClaim(crypto.getRandomValues(new Uint8Array(32))),room=await claimRoom(claim),authorization=claimAuthorization(claim),peers=[];
 const target=explicit.websocket+'/relay/'+room;
 s={base:target,authorization,async open(role){const peer=await wireOpen(target+`?role=${role}&mux=1&transport=1`,role==='daemon'?authorization:undefined);peers.push(peer);return peer;},async close(){for(const p of peers)p.terminate();}};
}
try{
 const health=await(await fetch(base+'/health')).json();assert.deepEqual([health.ok,health.service,health.version],[true,'formless-relay',2]);
 const key=await fetch(base+'/push/key');assert.equal(key.status,404);assert.match(key.headers.get('content-type')||'',/^text\/plain/);
 const missing=await fetch(base+'/no-such-file.js');assert.equal(missing.status,404);assert.ok(missing.headers.get('content-security-policy'));assert.equal(missing.headers.get('x-content-type-options'),'nosniff');
 const module=await fetch(base+'/transport.mjs');assert.equal(module.status,200);assert.match(module.headers.get('content-type')||'',/^text\/javascript/);
 const received=[],{daemon,browser}=await endpoints(s,frame=>received.push(frame));const replies=[];browser.consume=frame=>replies.push(frame);
 // The callback captured by WireEndpoint calls the current wrapper consumer.
 daemon.peer.send('{"relay": "push"}');await deadline(daemon.channels.get(browser.cid).start('hello'),'routed reply');assert.deepEqual(replies,['hello'],'spaced push controls cannot be broadcast as transcript data');
 const broken='{"relay"';browser.peer.send(broken);assert.equal(await wireWait(browser.peer,p=>p.closed),1008,'malformed pre/post-ready text is refused rather than routed as ciphertext');assert.equal(daemon.peer.closed,null,'one bad CID cannot close the shared daemon');
 const good=new EndpointPeer(await s.open('client'),'client',frame=>replies.push(frame));await good.waitFor(()=>good.cid);const channel=await daemon.waitChannel(good.cid);daemon.peer.send(JSON.stringify({relay:'ready',cid:good.cid,g:channel.generation}));await deadline(Promise.all([good.waitReady(good.cid),daemon.waitReady(good.cid)]),'replacement client ready');
 await deadline(good.channels.get('browser').start('healthy request'),'healthy client');assert.deepEqual(received,['healthy request']);
 daemon.peer.send(JSON.stringify({relay:'disconnect',cid:good.cid,code:4002}));assert.equal(await wireWait(good.peer,p=>p.closed),4002);
 await assert.rejects(wireOpen(s.base+'?role=daemon&mux=1&transport=1'),/401/);
 const replacement=await s.open('daemon');assert.equal(await wireWait(daemon.peer,p=>p.closed),4003);replacement.close();
 console.log('conformance ok');
}finally{await s.close();}
