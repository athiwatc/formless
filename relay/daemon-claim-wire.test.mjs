import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { daemonClaim, claimRoom, claimAuthorization } from "./daemon-claim.mjs";
import { wireOpen, wireWait } from "./test-wire.mjs";

test("real dev Upgrade rejects squats/wrong proofs and authenticated restart reclaims slot",{timeout:10000},async()=>{
  const env={...process.env};delete env.VAPID_PUBLIC_KEY;delete env.VAPID_PRIVATE_JWK;
  const child=spawn(process.execPath,[new URL("./dev.mjs",import.meta.url).pathname,"--port","0"],{env,stdio:["ignore","pipe","pipe"]});
  const peers=[];
  try{
    const port=await new Promise((resolve,reject)=>{
      let output="";
      const timer=setTimeout(()=>reject(new Error("dev listener deadline")),3000);
      child.stdout.on("data",chunk=>{output+=chunk;const match=output.match(/relay on http:\/\/127\.0\.0\.1:(\d+)/);if(match){clearTimeout(timer);resolve(Number(match[1]));}});
      child.once("error",reject);child.once("exit",()=>{clearTimeout(timer);reject(new Error("dev exited before listener"));});
    });
    const claim=await daemonClaim(Uint8Array.from({length:32},(_,i)=>i));
    const room=await claimRoom(claim),authorization=claimAuthorization(claim);
    const base=`ws://127.0.0.1:${port}/relay/${room}`;
    const open=async(role,auth)=>{const peer=await wireOpen(base+`?role=${role}&mux=1`,auth);peers.push(peer);return peer;};
    const client=await open("client");
    await wireWait(client,p=>p.got.some(f=>JSON.parse(f).relay==="daemon_offline"));
    await assert.rejects(open("daemon"),/401/);
    await assert.rejects(open("daemon","Bearer "+"00".repeat(32)),/401/);
    const first=await open("daemon",authorization);
    const opened=await wireWait(first,p=>p.got.map(JSON.parse).find(f=>f.relay==="client_open"));
    assert.ok(opened.cid);
    await wireWait(client,p=>p.got.some(f=>JSON.parse(f).relay==="daemon_online"));
    await assert.rejects(open("daemon","Bearer "+"00".repeat(32)),/401/);
    assert.equal(first.closed,null,"wrong proof cannot evict authenticated slot");
    const before=client.got.length;
    const second=await open("daemon",authorization);
    assert.equal(await wireWait(first,p=>p.closed),4003,"restart preempts without rotating secret");
    await wireWait(second,p=>p.got.map(JSON.parse).find(f=>f.cid===opened.cid));
    await wireWait(client,p=>p.got.slice(before).some(f=>JSON.parse(f).relay==="daemon_online"));
    assert.deepEqual(client.got.slice(before).map(JSON.parse),[{relay:"daemon_offline"},{relay:"daemon_online"}]);
    client.send("opaque ciphertext");
    await wireWait(second,p=>p.got.map(JSON.parse).find(f=>f.f==="opaque ciphertext"&&f.cid===opened.cid));
    second.send(JSON.stringify({cid:opened.cid,f:"opaque reply"}));
    await wireWait(client,p=>p.got.includes("opaque reply"));
    assert.equal(client.closed,null);
    assert.equal(client.got.filter(f=>f==='{"relay":"daemon_offline"}').length,2,"initial offline and intentional preemption only");
    second.send(JSON.stringify({relay:"disconnect",cid:opened.cid,code:4002}));
    assert.equal(await wireWait(client,p=>p.closed),4002,"authenticated control path remains usable");
  }finally{
    for(const peer of peers)peer.terminate();
    if(child.exitCode===null){child.kill();await once(child,"exit");}
  }
});
