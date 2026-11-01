// Room teardown and controls use the production bounded protocol over real
// SQLite, with only native WebSocket runtime objects replaced by fixtures.
import assert from 'node:assert/strict';
import {SECURITY_HEADERS} from './worker.js';
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
const worker=(await import('./worker.js')).default;
const health=await(await worker.fetch(new Request('https://relay.test/health'),{VAPID_PUBLIC_KEY:'x',VAPID_PRIVATE_JWK:'{bad'})).json();
assert.equal(health.push,'misconfigured');assert.ok(!JSON.stringify(health).includes('{bad'));console.log('health ok');
