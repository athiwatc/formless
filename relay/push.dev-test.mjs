// Owned loopback-only VAPID service; completion is acknowledged after storage,
// rather than guessed from a sleep between subscribe and push.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {once} from 'node:events';
import {b64u} from './push.mjs';
import {server,endpoints,deadline,deferred} from './test-bounded-wire.mjs';
const pair=await crypto.subtle.generateKey({name:'ECDSA',namedCurve:'P-256'},true,['sign','verify']);
const env={VAPID_PUBLIC_KEY:b64u(await crypto.subtle.exportKey('raw',pair.publicKey)),VAPID_PRIVATE_JWK:JSON.stringify(await crypto.subtle.exportKey('jwk',pair.privateKey)),VAPID_SUBJECT:'mailto:private-fixture@example.invalid',NO_PROXY:'127.0.0.1,localhost',no_proxy:'127.0.0.1,localhost'};
const first = deferred(), later = deferred();
const deliveries = [];
const expected = new Set(Array.from({length:10}, (_, i) => "/push/" + (i + 1)));
const service = createServer((req, res) => {
  let body = "";
  assert.equal(req.socket.remoteAddress, "127.0.0.1");
  req.on("data", chunk => body += chunk);
  req.on("end", () => {
    res.writeHead(201);
    res.end();
    deliveries.push(req.url);
    first.resolve({method:req.method,auth:req.headers.authorization,body,ttl:req.headers.ttl});
    expected.delete(req.url);
    if (!expected.size) later.resolve();
  });
});
service.listen(0,'127.0.0.1');await once(service,'listening');
const relay=await server({env});
try{
 const httpOrigin=new URL(relay.base).origin.replace(/^ws/,'http');const key=await(await fetch(httpOrigin+'/push/key')).json();assert.ok(key.key===env.VAPID_PUBLIC_KEY,'owned fixture key must match');
 const{daemon,browser}=await endpoints(relay);
 const url = index => `http://127.0.0.1:${service.address().port}/push/${index}`;
 async function subscribe(index) {
   const count = browser.controls.length;
   browser.peer.send(JSON.stringify({relay:"push_subscribe",sub:{endpoint:url(index),
     keys:{p256dh:env.VAPID_PUBLIC_KEY,auth:b64u(crypto.getRandomValues(new Uint8Array(16)))}}}));
   await browser.waitFor(()=>browser.controls.slice(count).some(p=>p.relay==="push_subscribed"));
 }
 await subscribe(0);
 daemon.peer.send(JSON.stringify({relay:'push',kind:'approval'}));const request=await deadline(first.promise,'owned payload-free push',5000);
 assert.equal(request.method,'POST');assert.equal(request.body,'');assert.ok(/^vapid t=[\w-]+\.[\w-]+\.[\w-]+, k=[\w-]+$/.test(request.auth),'request must carry the ephemeral fixture VAPID proof');assert.equal(request.ttl,'3600');
 assert.deepEqual(deliveries, ["/push/0"]);

 // Fill the room, remove the forgotten browser, then fill the freed slot.
 for (let i = 1; i < 10; i++) await subscribe(i);
 const id = crypto.randomUUID();
 browser.peer.send(JSON.stringify({relay:"push_unsubscribe",endpoint:url(0),id}));
 const ack = await browser.waitFor(()=>browser.controls.find(p=>p.relay==="push_unsubscribed"));
 assert.equal(ack.id, id);
 await subscribe(10);
 daemon.peer.send(JSON.stringify({relay:"push",kind:"approval"}));
 await deadline(later.promise, "approval reaches remaining subscriptions", 5000);
 assert.deepEqual(new Set(deliveries.slice(1)), new Set(Array.from({length:10}, (_, i) => "/push/" + (i + 1))));
 assert.equal(deliveries.length, 11);
 console.log("dev relay push ok: acknowledged removal releases the slot and later approval reaches only remaining endpoints");
}finally{await relay.close();service.close();}
