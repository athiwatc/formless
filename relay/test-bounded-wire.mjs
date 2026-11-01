import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {daemonClaim,claimRoom,claimAuthorization} from './daemon-claim.mjs';
import {wireOpen,wireWait} from './test-wire.mjs';
import {LIMITS,MemoryBudget,WireEndpoint} from './web/transport.mjs';
const endpointPeers=new WeakMap();
export const deferred=()=>{let resolve;const promise=new Promise(r=>resolve=r);return{promise,resolve};};
export async function server(options={}){
 const env={...process.env,...options.env};if(!options.env){delete env.VAPID_PUBLIC_KEY;delete env.VAPID_PRIVATE_JWK;}
 const child=spawn(process.execPath,[fileURLToPath(new URL('./dev.mjs',import.meta.url)),'--port','0'],{env,stdio:['ignore','pipe','pipe']});
 const peers=[];
 const port=await new Promise((resolve,reject)=>{let out='';const timeout=setTimeout(()=>reject(Error('owned relay listener deadline')),3000);child.stdout.on('data',bytes=>{out+=bytes;if(out.length>4096)out=out.slice(-4096);const m=out.match(/relay on http:\/\/127\.0\.0\.1:(\d+)/);if(m){clearTimeout(timeout);resolve(Number(m[1]));}});child.once('error',reject);child.once('exit',()=>{clearTimeout(timeout);reject(Error('owned relay exited'));});});
 const key=Uint8Array.from({length:32},(_,i)=>i),claim=await daemonClaim(key),room=await claimRoom(claim),authorization=claimAuthorization(claim),base=`ws://127.0.0.1:${port}/relay/${room}`;
 return{base,authorization,async open(role,auth=role==='daemon'?authorization:undefined){const p=await wireOpen(base+`?role=${role}&mux=1&transport=1`,auth);peers.push(p);return p;},async close(){const wrappers=peers.map(p=>endpointPeers.get(p)).filter(Boolean);for(const wrapper of wrappers)wrapper.dispose();for(const p of peers)p.terminate();try{await Promise.all(wrappers.map(wrapper=>wrapper.settled()));}finally{if(child.exitCode===null){child.kill();await once(child,'exit');}}}};
}
export class EndpointPeer {
 constructor(peer,role,consume=()=>{}) {
  this.peer=peer;this.role=role;this.g=null;this.cid=null;this.channels=new Map();this.controls=[];
  this.index=0;this.chain=Promise.resolve();this.consume=consume;this.waiters=new Set();
  this.memory=new MemoryBudget(512*1024*1024);this.disposed=false;this.error=null;this.active=new Set();
  endpointPeers.set(peer,this);this.unsubscribe=peer.subscribe(()=>this.drain());this.drain();
 }
 alive(){return !this.disposed&&this.peer.closed===null;}
 notify(){for(const check of this.waiters)check();}
 fail(error){if(!this.error)this.error=error;this.notify();}
 send(text){if(!this.alive())return false;this.peer.send(text);return true;}
 channel(cid,g){
  const key=this.role==='daemon'?cid:'browser';
  if(!this.channels.has(key)){
   assert.ok(this.alive(),'closed fixture cannot create a new logical channel');
   this.channels.set(key,new WireEndpoint({generation:g,cid:this.role==='daemon'?cid:null,memory:this.memory,
    send:text=>this.send(text),close:(code,reason)=>this.fail(Error(`fixture endpoint rejected ${code}: ${reason}`)),
    consume:frame=>{
     const task=Promise.resolve().then(()=>this.consume(frame,cid));this.active.add(task);
     return task.catch(error=>{this.fail(error);throw error;}).finally(()=>this.active.delete(task));
    }}));
  }
  return this.channels.get(key);
 }
 drain(){
  try{
   while(this.index<this.peer.got.length){
    const packet=JSON.parse(this.peer.got[this.index++]);
    if(packet.relay==='control'){
     this.chain=this.chain.then(()=>{
      // A close may share a TCP read with the preceding lifecycle control.
      // Keep its history for the preemption oracle, but never send a queued
      // ACK or create channels after actual close/explicit fixture disposal.
      if(this.g!==null)assert.equal(packet.g,this.g);
      const value=JSON.parse(packet.f);this.controls.push(value);
      if(this.alive()){
       if(value.relay==='transport'){this.g=value.g;this.cid=value.cid;if(this.role==='client')this.channel(this.cid,this.g);}
       else if(value.relay==='client_open')this.channel(value.cid,value.g);
       else if(value.relay==='ready'){const key=this.role==='daemon'?value.cid:this.cid;this.channel(key,value.g).setReady();}
       this.send(JSON.stringify({relay:'ack_control',g:packet.g,receipt:packet.receipt}));
      }
      this.notify();
     }).catch(error=>this.fail(error));
    }else if(this.alive()){
     const channel=this.channels.get(this.role==='daemon'?packet.cid:'browser');
     assert.ok(channel,'bounded packet must have a known endpoint');channel.receive(JSON.stringify(packet));
    }
   }
   if(this.peer.closed!==null)this.dispose();
  }catch(error){this.fail(error);}
 }
 dispose(){
  if(this.disposed)return;this.disposed=true;this.unsubscribe?.();
  for(const channel of this.channels.values())channel.dispose();
  this.notify();
 }
 async settled(){
  await this.chain;
  await deadline(Promise.allSettled([...this.active]),'owned fixture consumers',3000);
  // Consumer release/ACK continuations run after the wrapped consumer promise.
  await Promise.resolve();
  if(this.error)throw this.error;
 }
 async waitFor(predicate){
  let check;
  const work=new Promise((resolve,reject)=>{
   check=()=>{try{const result=predicate();if(result)resolve(result);else if(this.error)reject(this.error);else if(!this.alive())reject(Error('fixture endpoint closed before condition'));}catch(error){reject(error);}};
   this.waiters.add(check);check();
  });
  try{return await deadline(work,'native control',3000);}finally{this.waiters.delete(check);}
 }
 async waitChannel(cid){return this.waitFor(()=>this.channels.get(cid));}
 async waitReady(cid){return this.waitFor(()=>this.channels.get(this.role==='daemon'?cid:'browser')?.ready);}
}
export async function endpoints(s,consume){const daemon=new EndpointPeer(await s.open('daemon'),'daemon',consume);await daemon.waitFor(()=>daemon.g);const browser=new EndpointPeer(await s.open('client'),'client');await browser.waitFor(()=>browser.cid);const channel=await daemon.waitChannel(browser.cid);daemon.peer.send(JSON.stringify({relay:'ready',cid:browser.cid,g:channel.generation}));await Promise.all([daemon.waitReady(browser.cid),browser.waitReady(browser.cid)]);return{daemon,browser};}
export const deadline=(work,name,ms=10000)=>{let timer;return Promise.race([work,new Promise((_,reject)=>timer=setTimeout(()=>reject(Error(name+' deadline')),ms))]).finally(()=>clearTimeout(timer));};

