import test from 'node:test';
import assert from 'node:assert/strict';
import {createConnection} from 'node:net';
import {once} from 'node:events';
import {server} from './dev.mjs';
import {wireOpen} from './test-wire.mjs';
import {deadline,deferred} from './test-bounded-wire.mjs';

test('actual production dev socket evicts a hostile half-open close and releases only after native close',{timeout:10000},async()=>{
 const clients=[],accepted=[],peers=[];const onConnection=s=>accepted.push(s);server.on('connection',onConnection);
 server.listen(0,'127.0.0.1');await once(server,'listening');const port=server.address().port,room='a'.repeat(32),path=`/relay/${room}?role=client&transport=1`;
 async function raw(){
  const socket=createConnection({host:'127.0.0.1',port,allowHalfOpen:true}),headers=deferred(),ended=deferred();clients.push(socket);socket.on('error',()=>{});socket.on('end',()=>ended.resolve());
  let bytes=Buffer.alloc(0),done=false;socket.on('data',chunk=>{if(done)return;bytes=Buffer.concat([bytes,chunk]);assert.ok(bytes.length<8192);const end=bytes.indexOf('\r\n\r\n');if(end>=0){done=true;headers.resolve(bytes.subarray(0,end).toString('latin1'));}});
  await deadline(once(socket,'connect'),'private TCP connect',3000);
  socket.write(`GET ${path} HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ${Buffer.from('private-fixture!').toString('base64')}\r\n\r\n`);
  assert.match(await deadline(headers.promise,'private Upgrade',3000),/^HTTP\/1\.1 101 /);
  return{socket,ended,server:accepted.find(s=>s.remotePort===socket.localPort)};
 }
 try{
  const sockets=[];for(let i=0;i<16;i++)sockets.push(await raw());
  await assert.rejects(wireOpen(`ws://127.0.0.1:${port}${path}`),/upgrade status 429/,'connection17 must be rejected before any 101/native WebSocket allocation');
  const bad=sockets[0],actualClose=once(bad.server,'close');
  bad.socket.write(Buffer.from([129,0])); // Unmasked: production parser requests close.
  await deadline(bad.ended.promise,'server FIN',3000);
  assert.equal(bad.socket.writableEnded,false,'the peer deliberately withholds its FIN and WebSocket close reply');
  await deadline(actualClose,'bounded actual server socket eviction',3000);
  assert.equal(bad.server.destroyed,true,'the production close grace must destroy the actual server-side native socket');
  const replacement=await wireOpen(`ws://127.0.0.1:${port}${path}`);peers.push(replacement);assert.equal(replacement.closed,null,'only the observed actual close releases the retained slot');
 }finally{
  for(const p of peers)p.terminate();for(const c of clients)c.destroy();for(const s of accepted)s.destroy();
  await deadline(new Promise(resolve=>server.close(resolve)),'owned dev server cleanup',3000);server.off('connection',onConnection);
 }
});
