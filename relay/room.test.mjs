// Room teardown and controls use the production bounded protocol over real
// SQLite, with only native WebSocket runtime objects replaced by fixtures.
import assert from 'node:assert/strict';
import worker, {SECURITY_HEADERS} from './worker.js';
import {roomFixture,controls,ackControls} from './test-room-state.mjs';
const f=roomFixture();
try{
 const daemon=f.socket(),browser=f.socket();f.room.admit(daemon,'daemon',true,'',true);f.room.admit(browser,'client',true,'');await ackControls(f.room,daemon);await ackControls(f.room,browser);
 await f.room.webSocketError(browser);
 assert.equal(controls(daemon).filter(m=>m.relay==='client_close').length,1,'error alone retires logical device work without requiring a close callback');
 assert.ok(f.room.budget.describe().natives.some(n=>n.sid===browser.att.sid&&n.closing),'logical cleanup is not native teardown proof');
 browser.finish();await f.room.webSocketClose(browser);await f.room.webSocketClose(browser);
 assert.equal(controls(daemon).filter(m=>m.relay==='client_close'&&m.cid===browser.att.cid).length,1);
 const b2=f.socket();f.room.admit(b2,'client',true,'');await ackControls(f.room,b2);
 await f.room.webSocketError(daemon);daemon.finish();await f.room.webSocketClose(daemon);await f.room.webSocketClose(daemon);
 assert.equal(controls(b2).filter(m=>m.relay==='daemon_offline').length,1);
 const d2=f.socket();f.room.admit(d2,'daemon',true,'',true);const b3=f.socket(),b4=f.socket();f.room.admit(b3,'client',true,'');f.room.admit(b4,'client',true,'');
 await f.room.webSocketMessage(d2,JSON.stringify({relay:'disconnect',cid:b3.att.cid,code:4002}));await f.room.webSocketMessage(d2,JSON.stringify({relay:'disconnect',cid:b4.att.cid,code:4001}));
 assert.equal(b3.closed.code,4002);assert.equal(b4.closed.code,4001);
 assert.match(SECURITY_HEADERS['content-security-policy'],/connect-src 'self';/);
 console.log('room ok');
}finally{f.close();}

function assertFrontendHeaders(response, https = true) {
  assert.equal(response.headers.get('strict-transport-security'), https ? 'max-age=31536000; includeSubDomains' : null);
  assert.equal(response.headers.get('permissions-policy'), 'camera=(), microphone=(), geolocation=(), payment=(), usb=()');
  assert.equal(response.headers.get('cross-origin-opener-policy'), 'same-origin');
  assert.equal(response.headers.get('x-content-type-options'), 'nosniff');
  assert.match(response.headers.get('content-security-policy'), /connect-src 'self';/);
}

const healthResponse = await worker.fetch(new Request('https://relay.test/health'), {VAPID_PUBLIC_KEY: 'x', VAPID_PRIVATE_JWK: '{bad'});
assertFrontendHeaders(healthResponse);
assert.match(healthResponse.headers.get('content-type'), /^application\/json/);
const health = await healthResponse.json();
assert.equal(health.push, 'misconfigured');
assert.ok(!JSON.stringify(health).includes('{bad'));
const noPush = await worker.fetch(new Request('https://relay.test/push/key'), {});
assert.equal(noPush.status, 404);
assertFrontendHeaders(noPush);

const source = new Response('fixture', {headers: {'content-type': 'text/html', 'etag': '"fixture"'}});
const asset = await worker.fetch(new Request('https://relay.test/'), {ASSETS: {fetch: () => source}});
assertFrontendHeaders(asset);
assert.equal(asset.headers.get('content-type'), 'text/html');
assert.equal(asset.headers.get('etag'), '"fixture"');
assert.equal(await asset.text(), 'fixture');
assert.equal(source.headers.get('strict-transport-security'), null);
const localHealth = await worker.fetch(new Request('http://127.0.0.1:8787/health'), {});
assertFrontendHeaders(localHealth, false);
console.log('frontend response headers ok');
