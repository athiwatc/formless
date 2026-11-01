import test from 'node:test';
import assert from 'node:assert/strict';
import {EndpointPeer,deferred} from './test-bounded-wire.mjs';

function peer({writeFailure=null}={}) {
 const listeners=new Set();
 return {got:[],closed:null,attempts:0,sent:[],
  subscribe(fn){listeners.add(fn);return()=>listeners.delete(fn);},
  send(text){this.attempts++;if(this.closed!==null)throw Error('fixture socket closed');if(writeFailure)throw writeFailure;this.sent.push(JSON.parse(text));},
  deliver(value){this.got.push(JSON.stringify(value));for(const listener of listeners)listener();},
  finish(code){this.closed=code;for(const listener of listeners)listener();},
 };
}
const generation='a'.repeat(32);
function control(body,receipt='A'.repeat(22)) {
 return {relay:'control',g:generation,receipt,f:JSON.stringify(body)};
}

test('queued lifecycle controls sharing a native close never ACK the retired socket',async()=>{
 const raw=peer(),endpoint=new EndpointPeer(raw,'client'),release=deferred();
 endpoint.chain=release.promise;
 raw.deliver(control({relay:'transport',v:1,g:generation,cid:'private-client'}));
 raw.deliver(control({relay:'daemon_offline'},'B'.repeat(22)));
 // Deterministically hold the callback microtasks until the same native read
 // has ended with a preemption close. Old code writes ACKs after this point.
 raw.finish(4002);release.resolve();await endpoint.chain;await endpoint.settled();
 assert.equal(raw.attempts,0,'no write was even attempted after observed close');
 assert.deepEqual(endpoint.controls.map(value=>value.relay),['transport','daemon_offline']);
 assert.equal(endpoint.channels.size,0,'queued welcome cannot allocate a fresh channel after close');
 assert.equal(endpoint.memory.used,0);
});

test('explicit fixture disposal cancels queued control ACKs before terminating an open peer',async()=>{
 const raw=peer(),endpoint=new EndpointPeer(raw,'client'),release=deferred();
 endpoint.chain=release.promise;
 raw.deliver(control({relay:'transport',v:1,g:generation,cid:'private-client'}));
 endpoint.dispose();release.resolve();await endpoint.settled();
 assert.equal(raw.closed,null,'this is explicit cleanup, not an observed native close');
 assert.equal(raw.attempts,0);assert.equal(endpoint.channels.size,0);
});

test('an unexpected active control write failure remains a fixture failure',async()=>{
 const error=Error('unexpected active fixture write failure'),raw=peer({writeFailure:error});
 const endpoint=new EndpointPeer(raw,'client');
 raw.deliver(control({relay:'transport',v:1,g:generation,cid:'private-client'}));
 await assert.rejects(endpoint.settled(),failure=>failure===error);
 assert.equal(raw.closed,null,'failure occurred on an active socket');
 assert.equal(raw.attempts,1,'the active failure cannot be skipped as teardown');
 endpoint.dispose();
});
