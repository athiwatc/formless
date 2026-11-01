import test from 'node:test';
import assert from 'node:assert/strict';
import {wireOpen,wireWait} from './test-wire.mjs';
import {server,EndpointPeer,deadline} from './test-bounded-wire.mjs';

test('real dev Upgrade rejects squats/wrong proofs and authenticated restart reclaims bounded slot',{timeout:10000},async()=>{
 const s=await server();try{
  const client=new EndpointPeer(await s.open('client'),'client');await client.waitFor(()=>client.controls.some(p=>p.relay==='daemon_offline'));
  const url=s.base+'?role=daemon&mux=1&transport=1';
  await assert.rejects(wireOpen(url),/401/);await assert.rejects(wireOpen(url,'Bearer '+'00'.repeat(32)),/401/);
  const first=new EndpointPeer(await s.open('daemon'),'daemon');await first.waitChannel(client.cid);await client.waitFor(()=>client.controls.some(p=>p.relay==='daemon_online'));
  await assert.rejects(wireOpen(url,'Bearer '+'00'.repeat(32)),/401/);assert.equal(first.peer.closed,null,'wrong proof cannot evict authenticated daemon');
  const requests=[],replies=[],second=new EndpointPeer(await s.open('daemon'),'daemon',frame=>requests.push(frame));
  assert.equal(await wireWait(first.peer,p=>p.closed),4003);assert.equal(await wireWait(client.peer,p=>p.closed),4002,'matching versions must perform a fresh generation/E2E handshake');
  assert.equal(client.controls.filter(p=>p.relay==='daemon_offline').length,2,'only initial offline and intentional preemption');
  const fresh=new EndpointPeer(await s.open('client'),'client',frame=>replies.push(frame));await fresh.waitFor(()=>fresh.cid);const channel=await second.waitChannel(fresh.cid);
  second.peer.send(JSON.stringify({relay:'ready',cid:fresh.cid,g:channel.generation}));await deadline(Promise.all([fresh.waitReady(fresh.cid),second.waitReady(fresh.cid)]),'new bounded channel ready');
  await deadline(fresh.channels.get('browser').start('opaque ciphertext'),'current browser delivery');assert.deepEqual(requests,['opaque ciphertext']);
  await deadline(channel.start('opaque reply'),'current daemon delivery');assert.deepEqual(replies,['opaque reply']);assert.equal(fresh.peer.closed,null);
  second.peer.send(JSON.stringify({relay:'disconnect',cid:fresh.cid,code:4002}));assert.equal(await wireWait(fresh.peer,p=>p.closed),4002,'authenticated control still routes to its browser');
 }finally{await s.close();}
});
