import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import * as transport from "./web/transport.mjs";
const source=readFileSync(new URL('./web/app.js',import.meta.url),'utf8');
const fn=name=>{const start=source.search(new RegExp('  (?:async )?function '+name+'\\('));assert.ok(start>=0,name);return source.slice(start,source.indexOf('\n  }',start)+4);};
const generation='a'.repeat(32);
test('authenticated browser requests reach the daemon send path within the plaintext boundary',async()=>{
  for(const size of [0,transport.LIMITS.plaintext,transport.LIMITS.plaintext+1]){
    const received=[],closes=[];
    const key=await webcrypto.subtle.generateKey({name:'AES-GCM',length:256},false,['encrypt','decrypt']);
    const ws={readyState:1,close:code=>closes.push(code)};
    const S={ws,channel:{key,sent:0},local:false,relayReady:true,relayModule:transport,relayMemory:new transport.MemoryBudget(512*1024*1024),pending:new Map(),sendChain:Promise.resolve(),nextId:1};
    S.relayLink={ws,endpoint:{async start(frame){
      const value=JSON.parse(frame);
      const plaintext=await webcrypto.subtle.decrypt({name:'AES-GCM',iv:Buffer.from(value.iv,'base64'),additionalData:new TextEncoder().encode('formless-relay/2 b2d '+value.seq)},key,Buffer.from(value.ct,'base64'));
      const request=JSON.parse(new TextDecoder().decode(plaintext));
      received.push(request);
      S.pending.get(request.id).resolve('pong');S.pending.delete(request.id);
    }}};
    const c=vm.createContext({S,PROTOCOL:1,FRAME_VERSION:2,crypto:webcrypto,Uint8Array,btoa,enc:s=>new TextEncoder().encode(s),aad:(direction,seq)=>new TextEncoder().encode('formless-relay/2 '+direction+' '+seq),setTimeout(){return 1;},clearTimeout(){}});
    vm.runInContext(['request','encrypt','bytesToB64'].map(fn).join('\n'),c);
    const request={type:'ping'};
    if(size){request.padding='';request.padding='x'.repeat(size-JSON.stringify({id:1,protocol:1,request}).length);}
    const result=c.request(request);
    if(size>transport.LIMITS.plaintext) await assert.rejects(result,/relay frame too large/);
    else assert.equal(await result,'pong');
    await S.sendChain;
    assert.equal(received.length,size>transport.LIMITS.plaintext?0:1);
    assert.equal(S.channel.sent,size>transport.LIMITS.plaintext?0:1);
    assert.deepEqual(closes,[]);assert.equal(S.relayMemory.used,0);
  }
});
const deferred=()=>{let resolve;const promise=new Promise(r=>{resolve=r;});return{promise,resolve};};
async function until(predicate){const end=Date.now()+2000;while(!predicate()){assert.ok(Date.now()<end,'bounded browser fixture');await new Promise(r=>setTimeout(r,2));}}
async function browser(keyGate=null,decryptGate=null){
  const sent=[],closes=[],readyAcks=[],messages=[],elements=new Map();let loaded=0,flushed=0,reconnects=0;
  const S={ws:null,channel:null,daemonOnline:false,room:'room',pending:new Map(),sendChain:Promise.resolve(),nextId:1,relayModule:transport,relayMemory:new transport.MemoryBudget(512*1024*1024),userClosed:false};
  class Socket{
    constructor(){this.readyState=1;}
    send(frame){const value=JSON.parse(frame);sent.push(value);if(value.relay==='ack_control'&&value.receipt==='D'.repeat(22))readyAcks.push(Boolean(S.relayReady&&S.relayLocalProof&&S.channel));}
    close(code){this.readyState=3;closes.push(code);this.onclose?.({code});}
  }
  const crypto=keyGate||decryptGate?{getRandomValues:webcrypto.getRandomValues.bind(webcrypto),subtle:{deriveBits:webcrypto.subtle.deriveBits.bind(webcrypto.subtle),deriveKey:async(...args)=>{if(keyGate){keyGate.entered.resolve();await keyGate.release.promise;}return webcrypto.subtle.deriveKey(...args);},decrypt:async(...args)=>{if(decryptGate){decryptGate.entered.resolve();await decryptGate.release.promise;}return webcrypto.subtle.decrypt(...args);}}}:webcrypto;
  const c=vm.createContext({S,WebSocket:Socket,location:{protocol:'https:',host:'fixture.invalid'},crypto,Uint8Array,TextEncoder,TextDecoder,btoa,atob,
    enc:s=>new TextEncoder().encode(s),aad:(direction,seq)=>new TextEncoder().encode('formless-relay/2 '+direction+' '+seq),FRAME_VERSION:2,HANDSHAKE_MS:8000,HANDSHAKE_TRIES:3,
    $:id=>{if(!elements.has(id))elements.set(id,{textContent:'',classList:{add(){},remove(){},contains(){return true;}}});return elements.get(id);},
    setTimeout(){return 1;},clearTimeout(){},renderAll(){},renderChrome(){},renderHistory(){},clearLiveState(){},scheduleReconnect(){reconnects++;},note(){},markStable(){},restorePushSubscription(){},loadAll(){loaded++;},refreshOutbox(){},flushOutbox(){flushed++;},onMessage(message){messages.push(message);},
  });
  vm.runInContext(['openRelay','resetRelayConnection','finishRelayReady','startHandshake','sendHello','confirmTag','finishHandshake','bytesToB64','b64ToBytes','decrypt','pushRefused'].map(fn).join('\n'),c);
  S.key=await webcrypto.subtle.importKey('raw',new Uint8Array(32).fill(7),'HKDF',false,['deriveBits','deriveKey']);
  c.openRelay();const ws=S.ws;ws.onopen();
  const control=(body,receipt)=>ws.onmessage({data:JSON.stringify({relay:'control',g:generation,receipt,f:JSON.stringify(body)})});
  control({relay:'transport',v:1,g:generation,cid:'private-client'},'A'.repeat(22));
  await until(()=>sent.length===1);
  return{c,S,ws,sent,closes,readyAcks,messages,control,loaded:()=>loaded,flushed:()=>flushed,reconnects:()=>reconnects};
}
async function readyBrowser(b){
  b.control({relay:'daemon_online'},'B'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.v===2&&frame.hello));
  const daemonHello=new Uint8Array(16).fill(9),proof=await b.c.confirmTag(daemonHello,'d2b');
  b.control({v:2,hello:b.c.bytesToB64(daemonHello),confirm:b.c.bytesToB64(proof)},'C'.repeat(22));
  b.control({relay:'ready',g:generation},'D'.repeat(22));
  await until(()=>b.S.relayReady);
}
test('shipped browser waits for local E2E proof and consumed room readiness before bulk/grants',async()=>{
  const b=await browser();b.control({relay:'daemon_online'},'B'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.v===2&&frame.hello));
  const daemonHello=new Uint8Array(16).fill(9);
  const proof=await b.c.confirmTag(daemonHello,'d2b');
  b.control({v:2,hello:b.c.bytesToB64(daemonHello),confirm:b.c.bytesToB64(proof)},'C'.repeat(22));
  b.control({relay:'ready',g:generation},'D'.repeat(22));
  assert.equal(b.S.relayReady,false,'ready cannot overtake async key verification');
  await until(()=>b.S.relayReady&&b.readyAcks.length===1);
  assert.deepEqual(b.readyAcks,[true]);assert.equal(b.loaded(),1);assert.equal(b.flushed(),1);assert.equal(b.S.relayLink.endpoint.ready,true);
  assert.deepEqual(b.closes,[]);await until(()=>b.S.relayMemory.used===0);
  b.ws.close(1000);
});
test('shipped browser rejects a declared bulk frame before E2E ready without bulk allocation',async()=>{
  const b=await browser();assert.equal(b.S.relayMemory.used,0);
  const fragment={relay:'data',v:1,id:'b'.repeat(32),index:0,bytes:transport.LIMITS.encrypted,data:'A'.repeat(transport.LIMITS.chunk)};
  b.ws.onmessage({data:JSON.stringify({relay:'delivery',g:generation,receipt:'B'.repeat(22),fragment})});
  assert.equal(b.S.relayMemory.used,0);assert.deepEqual(b.closes,[1008]);assert.equal(b.loaded(),0);
});
test('an expired queued browser request is never encrypted/transmitted after flow control clears',async()=>{
  const gate=deferred(),timers=[],sent=[];let encrypted=0;
  const ws={readyState:1,send:frame=>sent.push(frame),close(){}};
  const S={ws,channel:{},local:false,relayReady:true,relayModule:transport,relayMemory:new transport.MemoryBudget(8*1024*1024),pending:new Map(),sendChain:gate.promise,nextId:1,relayLink:{ws,endpoint:{start:async frame=>sent.push(frame)}}};
  const c=vm.createContext({S,PROTOCOL:1,setTimeout:fn=>{timers.push(fn);return timers.length;},clearTimeout(){},encrypt:async text=>{encrypted++;return text;}});
  vm.runInContext(fn('request'),c);
  const request=c.request({type:'fixture'});const rejected=assert.rejects(request,/did not answer/);
  timers[0]();await rejected;gate.resolve();await S.sendChain;
  assert.equal(encrypted,0);assert.equal(sent.length,0);assert.equal(S.relayMemory.used,0);
});
test('service worker install actually precaches the shared browser transport',async()=>{
  const handlers=new Map(),cached=[];let pending;
  const self={addEventListener:(event,handler)=>handlers.set(event,handler),skipWaiting:async()=>{},registration:{},clients:{claim:async()=>{}}};
  vm.runInNewContext(readFileSync(new URL('./web/sw.js',import.meta.url),'utf8'),{self,caches:{open:async()=>({addAll:async files=>cached.push(...files)})}});
  handlers.get('install')({waitUntil:promise=>{pending=promise;}});await pending;
  assert.ok(cached.includes('/transport.mjs'),'offline app must have its dynamic import');
  assert.ok(cached.includes('/icon-maskable-192.png'),'offline app must have its 192px maskable icon');
  assert.ok(cached.includes('/icon-maskable-512.png'),'offline app must have its 512px maskable icon');
});

test('push registration receipts do not impersonate daemon offline controls',async()=>{
  const b=await browser();b.S.daemonOnline=true;const channel={fixture:true};b.S.channel=channel;
  const registration=[],removal=[];
  b.S.pushRegistration={finish:socket=>registration.push(socket)};
  b.S.pushRemovalAck=(socket,id)=>removal.push({socket,id});
  const id='f0ae9f18-2a8b-45c9-a338-53dfdc62c127';
  for(const [body,receipt] of [[{relay:'push_subscribed'},'B'.repeat(22)],[{relay:'push_unsubscribed',id},'C'.repeat(22)]]){
    b.control(body,receipt);await until(()=>b.sent.some(frame=>frame.relay==='ack_control'&&frame.receipt===receipt));
    assert.equal(b.S.daemonOnline,true);assert.equal(b.S.channel,channel);
  }
  assert.deepEqual(registration,[b.ws]);
  assert.deepEqual(removal,[{socket:b.ws,id}]);
  assert.deepEqual(b.closes,[]);b.ws.close(1000);
});

test('push refusals settle only the matching operation and leave the socket open',async()=>{
  const b=await browser();
  b.S.daemonOnline=true;
  b.S.channel={fixture:true};
  const channel=b.S.channel;
  const registration=[],removal=[];
  b.S.pushRegistration={finish:(socket,outcome)=>registration.push({socket,outcome})};
  b.S.pushRemovalAck=(socket,id,confirmed)=>removal.push({socket,id,confirmed});
  const id='f0ae9f18-2a8b-45c9-a338-53dfdc62c127';
  b.control({relay:'push_refused',operation:'subscribe',reason:'capacity'},'B'.repeat(22));
  b.control({relay:'push_refused',operation:'unsubscribe',reason:'busy',id},'C'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.relay==='ack_control'&&frame.receipt==='C'.repeat(22)));
  assert.deepEqual(registration,[{socket:b.ws,outcome:'The relay has reached its notification device limit.'}]);
  assert.deepEqual(removal,[{socket:b.ws,id,confirmed:false}]);
  assert.equal(b.S.channel,channel);
  assert.equal(b.S.daemonOnline,true);
  assert.deepEqual(b.closes,[]);
  b.ws.close(1000);
});


test('duplicate handshake replies around readiness never send a late confirm or replace the working channel',async()=>{
  const b=await browser();b.control({relay:'daemon_online'},'B'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.v===2&&frame.hello));
  const hello=new Uint8Array(16).fill(9),proof=await b.c.confirmTag(hello,'d2b');
  const reply={v:2,hello:b.c.bytesToB64(hello),confirm:b.c.bytesToB64(proof)};
  // Both replies may already be queued before the first async HKDF finishes.
  b.control(reply,'C'.repeat(22));b.control(reply,'E'.repeat(22));
  b.control({relay:'ready',g:generation},'D'.repeat(22));
  await until(()=>b.S.relayReady&&b.sent.some(frame=>frame.receipt==='D'.repeat(22)));
  const channel=b.S.channel;
  b.control(reply,'F'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.receipt==='F'.repeat(22)));
  assert.equal(b.sent.filter(frame=>frame.v===2&&frame.confirm).length,1,'one browser proof, no post-ready raw handshake');
  assert.equal(b.S.channel,channel);assert.equal(b.loaded(),1);assert.equal(b.flushed(),1);
  // A genuine authenticated next frame still opens on exactly that channel.
  const iv=new Uint8Array(12).fill(3),message={type:'response',id:7,result:{pong:true}};
  const ciphertext=await webcrypto.subtle.encrypt({name:'AES-GCM',iv,additionalData:new TextEncoder().encode('formless-relay/2 d2b 1')},channel.key,new TextEncoder().encode(JSON.stringify(message)));
  const encrypted=JSON.stringify({v:2,seq:1,iv:b.c.bytesToB64(iv),ct:b.c.bytesToB64(new Uint8Array(ciphertext))});
  const parts=[...transport.fragments(encrypted,'b'.repeat(32))];assert.equal(parts.length,1);
  b.ws.onmessage({data:JSON.stringify({relay:'delivery',g:generation,receipt:'G'.repeat(22),fragment:parts[0]})});
  await until(()=>b.sent.some(frame=>frame.relay==='ack'&&frame.receipt==='G'.repeat(22)));
  assert.equal(JSON.stringify(b.messages),JSON.stringify([message]));assert.equal(channel.received,1);
  assert.deepEqual(b.closes,[]);await until(()=>b.S.relayMemory.used===0);b.ws.close(1000);
});

test('a dropped encrypted frame closes the channel and schedules a state-resync connection',async()=>{
  const b=await browser();
  await readyBrowser(b);
  const channel=b.S.channel;
  const gap='{"v":2,"seq":2}';
  const [fragment]=transport.fragments(gap,'c'.repeat(32));
  b.ws.onmessage({data:JSON.stringify({relay:'delivery',g:generation,receipt:'H'.repeat(22),fragment})});
  await until(()=>b.closes.includes(4002));
  assert.equal(channel.received,0);
  assert.equal(b.S.channel,null);
  assert.equal(b.reconnects(),1);
  await until(()=>b.S.relayMemory.used===0);
});


test('an old handshake deriving its key cannot overwrite a replacement connection',async()=>{
  const keyGate={entered:deferred(),release:deferred()},b=await browser(keyGate);
  b.control({relay:'daemon_online'},'B'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.v===2&&frame.hello));
  const hello=new Uint8Array(16).fill(9),proof=await b.c.confirmTag(hello,'d2b');
  b.control({v:2,hello:b.c.bytesToB64(hello),confirm:b.c.bytesToB64(proof)},'C'.repeat(22));
  await keyGate.entered.promise;
  b.ws.close(1000);
  const replacement={readyState:1,send(){throw new Error('old handshake wrote to replacement socket');}},channel={replacement:true};
  b.S.ws=replacement;b.S.hello=new Uint8Array(16).fill(4);b.S.channel=channel;
  const sentBefore=b.sent.length;keyGate.release.resolve();
  await until(()=>b.S.relayMemory.used===0);
  assert.equal(b.S.channel,channel);assert.equal(b.S.relayLocalProof,false);
  assert.equal(b.sent.length,sentBefore,'disposed generation cannot ACK or send more proofs');
  assert.equal(b.loaded(),0);
});


test('full-size queued RPC reduces its encryption peak before a full-size response is consumed',async()=>{
  const memory=new transport.MemoryBudget(512*1024*1024),ws={readyState:1,close(){}};
  const S={ws,channel:{},local:false,relayReady:true,relayModule:transport,relayMemory:memory,pending:new Map(),sendChain:Promise.resolve(),nextId:1};
  let sendCharge=0;
  S.relayLink={ws,endpoint:{start:async frame=>{
    sendCharge=memory.used;
    const response=memory.reserve(3*transport.LIMITS.encrypted+9*transport.LIMITS.plaintext+1024*1024);
    assert.ok(sendCharge<90*1024*1024,'encoded producer is not charged its discarded plaintext/encryption peak');
    assert.ok(memory.used<=memory.limit);
    response.release();S.pending.get(1).resolve({pong:true});
  }}};
  const c=vm.createContext({S,PROTOCOL:1,setTimeout:()=>1,clearTimeout(){},encrypt:async()=> 'A'.repeat(transport.LIMITS.encrypted)});
  vm.runInContext(fn('request'),c);
  const req={type:'fixture',value:''},overhead=JSON.stringify({id:1,protocol:1,request:req}).length;
  req.value='x'.repeat(transport.LIMITS.plaintext-overhead);
  const result=await c.request(req);assert.equal(result.pong,true);await S.sendChain;
  assert.equal(memory.used,0);assert.ok(sendCharge>0);
});


test('a frame decrypting on a disconnected generation cannot publish into the replacement',async()=>{
  const decryptGate={entered:deferred(),release:deferred()},b=await browser(null,decryptGate);
  b.control({relay:'daemon_online'},'B'.repeat(22));
  await until(()=>b.sent.some(frame=>frame.v===2&&frame.hello));
  const hello=new Uint8Array(16).fill(9),proof=await b.c.confirmTag(hello,'d2b');
  b.control({v:2,hello:b.c.bytesToB64(hello),confirm:b.c.bytesToB64(proof)},'C'.repeat(22));
  b.control({relay:'ready',g:generation},'D'.repeat(22));
  await until(()=>b.S.relayReady&&b.sent.some(frame=>frame.receipt==='D'.repeat(22)));
  const channel=b.S.channel,iv=new Uint8Array(12).fill(3),message={type:'response',id:7,result:{privateOldConnection:true}};
  const ciphertext=await webcrypto.subtle.encrypt({name:'AES-GCM',iv,additionalData:new TextEncoder().encode('formless-relay/2 d2b 1')},channel.key,new TextEncoder().encode(JSON.stringify(message)));
  const encrypted=JSON.stringify({v:2,seq:1,iv:b.c.bytesToB64(iv),ct:b.c.bytesToB64(new Uint8Array(ciphertext))});
  b.ws.onmessage({data:JSON.stringify({relay:'delivery',g:generation,receipt:'G'.repeat(22),fragment:[...transport.fragments(encrypted,'b'.repeat(32))][0]})});
  await decryptGate.entered.promise;
  b.ws.close(1000);
  assert.ok(b.S.relayMemory.used>0,'active crypto consumer remains charged after disposal');
  const replacement={replacement:true};b.S.ws={readyState:1};b.S.channel=replacement;
  decryptGate.release.resolve();await until(()=>b.S.relayMemory.used===0);
  assert.equal(b.S.channel,replacement);assert.deepEqual(b.messages,[]);
  assert.equal(b.sent.some(frame=>frame.relay==='ack'&&frame.receipt==='G'.repeat(22)),false);
});
