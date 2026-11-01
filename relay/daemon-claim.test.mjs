import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";
import { daemonClaim, claimRoom, claimAuthorization, verifyDaemonClaim } from "./daemon-claim.mjs";
import { RelayRoom } from "./worker.js";
import { roomFixture, controls, ackControls } from "./test-room-state.mjs";

const vector = {
  key: Uint8Array.from({ length:32 }, (_,i)=>i),
  authorization: "Bearer dd8607ecc7b952bb2c46e93f9387c239f50599aaad3a67036b743df4ab64b061",
  room: "c24c18b87313893139ea9647c754be6d",
};
test("Rust/Worker/browser room-v3 fixed vector and strict header proof", async()=>{
  const claim = await daemonClaim(vector.key);
  assert.equal(claimAuthorization(claim),vector.authorization);
  assert.equal(await claimRoom(claim),vector.room);
  const source = await readFile(new URL("./web/app.js",import.meta.url),"utf8");
  const body = source.slice(source.indexOf("  async function roomFor("),source.indexOf("  // Relay frame protocol"));
  const browserRoom = vm.runInNewContext(`(async()=>{${body}; return roomFor(key);})()`,{crypto,TextEncoder,Uint8Array,key:vector.key});
  assert.equal(await browserRoom,vector.room,"execute the shipped browser derivation");
  assert.equal(await verifyDaemonClaim(vector.room,vector.authorization),true);
  for(const invalid of [null,"",vector.authorization.toUpperCase(),vector.authorization+" ","Bearer "+"00".repeat(32),"Bearer "+"0".repeat(63),"Bearer "+"0".repeat(65)]){
    assert.equal(await verifyDaemonClaim(vector.room,invalid),false);
  }
  assert.equal(await verifyDaemonClaim("0".repeat(32),vector.authorization),false,"proof bound to room");
  assert.notEqual(Buffer.from(claim).toString("hex"),Buffer.from(vector.key).toString("hex"));
});
function fixture(){const f=roomFixture();let pushes=0;f.room.pushAll=async()=>{pushes++;};return{...f,pushes:()=>pushes};}
test("unauthenticated daemon rejected before upgrade, admission and control authority",async(t)=>{
  const f=fixture();t.after(()=>f.close());
  const browser=f.socket();f.room.admit(browser,"client",false,"private-browser");
  for(const header of [null,"Bearer "+"00".repeat(32)]){
    const response=await f.room.fetch(new Request(`https://relay.test/relay/${vector.room}?role=daemon`,{headers:{Upgrade:"websocket",...(header?{Authorization:header}:{})}}));
    assert.equal(response.status,401,"WebSocketPair must not even be constructed");
  }
  const squatter=f.socket();
  assert.throws(()=>f.room.admit(squatter,"daemon",true,"squatter"),/authentication/);
  squatter.att={role:"daemon",mux:true};
  await f.room.webSocketMessage(squatter,'{"relay":"push"}');
  await f.room.webSocketMessage(squatter,JSON.stringify({relay:"disconnect",cid:browser.att.cid}));
  assert.equal(f.pushes(),0);assert.equal(browser.closed,null);
  assert.deepEqual(f.state.getWebSockets("daemon"),[]);
});
test("authenticated restart preempts, retires old controls and survives hibernation/late teardown",async()=>{
 const f=fixture();try{
 const browser=f.socket();f.room.admit(browser,"client",true,"private-browser");
 const first=f.socket();f.room.admit(first,"daemon",true,"",true);const oldIdentity=structuredClone(first.att);
 const second=f.socket();f.room.admit(second,"daemon",true,"",true);
 assert.equal(first.closed.code,4003);assert.deepEqual(first.att,oldIdentity,"supersession must preserve immutable socket identity");
 assert.equal(browser.closed.code,4002,"a new daemon requires a fresh bounded channel");assert.deepEqual(f.room.sockets("daemon"),[second]);
 const fresh=f.socket();f.room.admit(fresh,"client",true,"private-browser");
 assert.ok(controls(second).some(m=>m.relay==="client_open"&&m.cid===fresh.att.cid&&m.ua==="private-browser"));
 assert.ok(controls(fresh).some(m=>m.relay==="daemon_online"));
 const resumed=new RelayRoom(f.state,{});let pushes=0;resumed.pushAll=async()=>{pushes++;};const before=fresh.sent.length;
 await resumed.webSocketMessage(first,'{"relay":"push"}');await resumed.webSocketMessage(first,JSON.stringify({relay:"disconnect",cid:fresh.att.cid}));
 await resumed.webSocketMessage(first,JSON.stringify({cid:fresh.att.cid,g:fresh.att.g,f:"stale"}));
 first.finish();await resumed.webSocketClose(first);await resumed.webSocketError(first);
 assert.equal(pushes,0);assert.equal(fresh.closed,null);assert.equal(fresh.sent.length,before);
 await resumed.webSocketMessage(second,'{"relay":"push"}');assert.equal(pushes,1);
 await ackControls(resumed,second);await ackControls(resumed,fresh);
 const hello=JSON.stringify({v:2,hello:btoa("x".repeat(16))});await resumed.webSocketMessage(fresh,hello);
 assert.ok(controls(second).some(m=>m.cid===fresh.att.cid&&m.f===hello),"only the new authenticated daemon receives current browser handshakes");
 }finally{f.close();}
});

test("production fetch verifies before Upgrade and admits a restart through the same room policy",async()=>{
  const f=fixture();
  const savedResponse=globalThis.Response,savedPair=globalThis.WebSocketPair;
  const pairs=[];
  // Node has no Workers 101 response/WebSocketPair; only these runtime
  // constructors are replaced, while the production fetch/policy executes.
  globalThis.WebSocketPair=class{constructor(){this[0]={client:true};this[1]=f.socket();pairs.push(this);}};
  globalThis.Response=class{constructor(body,init){this.status=init.status;this.webSocket=init.webSocket;this.body=body;}};
  try{
    const request=()=>new Request(`https://relay.test/relay/${vector.room}?role=daemon&mux=1&transport=1`,{headers:{Upgrade:"websocket",Authorization:vector.authorization}});
    const first=await f.room.fetch(request());assert.equal(first.status,101);assert.equal(first.webSocket,pairs[0][0]);
    assert.equal(pairs[0][1].att.authenticated,3);
    const second=await f.room.fetch(request());assert.equal(second.status,101);
    assert.equal(pairs[0][1].closed.code,4003);
    assert.deepEqual(f.room.sockets("daemon"),[pairs[1][1]]);
    assert.ok(!JSON.stringify(pairs.map(p=>p[1].att)).includes(vector.authorization.slice(7)),"no claim in socket attachments");
  }finally{f.close();globalThis.Response=savedResponse;if(savedPair===undefined)delete globalThis.WebSocketPair;else globalThis.WebSocketPair=savedPair;}
});
