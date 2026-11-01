// Owned loopback-only VAPID service; completion is acknowledged after storage,
// rather than guessed from a sleep between subscribe and push.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {b64u} from './push.mjs';
import {server,endpoints,deadline,deferred} from './test-bounded-wire.mjs';
const pair=await crypto.subtle.generateKey({name:'ECDSA',namedCurve:'P-256'},true,['sign','verify']);
const env={VAPID_PUBLIC_KEY:b64u(await crypto.subtle.exportKey('raw',pair.publicKey)),VAPID_PRIVATE_JWK:JSON.stringify(await crypto.subtle.exportKey('jwk',pair.privateKey)),VAPID_SUBJECT:'mailto:private-fixture@example.invalid',NO_PROXY:'127.0.0.1,localhost',no_proxy:'127.0.0.1,localhost'};
const got=deferred();
const service=createServer((req,res)=>{let body='';assert.equal(req.socket.remoteAddress,'127.0.0.1');req.on('data',c=>body+=c);req.on('end',()=>{res.writeHead(201);res.end();got.resolve({method:req.method,auth:req.headers.authorization,body,ttl:req.headers.ttl});});});
service.listen(0,'127.0.0.1');await once(service,'listening');
const relay=await server({env});
try{
 const httpOrigin=new URL(relay.base).origin.replace(/^ws/,'http');const key=await(await fetch(httpOrigin+'/push/key')).json();assert.ok(key.key===env.VAPID_PUBLIC_KEY,'owned fixture key must match');
 const{daemon,browser}=await endpoints(relay);
 const endpoint=`http://127.0.0.1:${service.address().port}/push/abc`;assert.equal(new URL(endpoint).hostname,'127.0.0.1');
 browser.peer.send(JSON.stringify({relay:'push_subscribe',sub:{endpoint,keys:{p256dh:env.VAPID_PUBLIC_KEY,auth:b64u(crypto.getRandomValues(new Uint8Array(16)))}}}));
 await browser.waitFor(()=>browser.controls.some(p=>p.relay==='push_subscribed'));
 daemon.peer.send(JSON.stringify({relay:'push',kind:'approval'}));const request=await deadline(got.promise,'owned payload-free push',5000);
 assert.equal(request.method,'POST');assert.equal(request.body,'');assert.ok(/^vapid t=[\w-]+\.[\w-]+\.[\w-]+, k=[\w-]+$/.test(request.auth),'request must carry the ephemeral fixture VAPID proof');assert.equal(request.ttl,'3600');
 browser.peer.send(JSON.stringify({relay:'push_unsubscribe',endpoint}));await browser.waitFor(()=>browser.controls.some(p=>p.relay==='push_unsubscribed'));
 console.log('dev relay push ok');
}finally{await relay.close();service.close();}
