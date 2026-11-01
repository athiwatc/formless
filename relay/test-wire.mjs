// Tiny dependency-free RFC6455 fixture peer. Authorization is sent only in
// the Upgrade header; error messages intentionally contain no headers/URL.
import { LIMITS } from "./web/transport.mjs";
import http from "node:http";
import https from "node:https";
import { createHash, randomBytes } from "node:crypto";
export function wireOpen(url, authorization) {
  return new Promise((resolve,reject)=>{
    const endpoint=new URL(url);endpoint.protocol=endpoint.protocol==="wss:"?"https:":"http:";
    const key=randomBytes(16).toString("base64");
    const req=(endpoint.protocol==="https:"?https:http).request(endpoint,{headers:{Upgrade:"websocket",Connection:"Upgrade","Sec-WebSocket-Key":key,"Sec-WebSocket-Version":"13",...(authorization?{Authorization:authorization}:{})}});
    req.setTimeout(3000,()=>req.destroy(new Error("fixture upgrade deadline")));
    req.on("error",reject);
    req.on("response",res=>{res.resume();reject(new Error(`upgrade status ${res.statusCode}`));});
    req.on("upgrade",(res,socket,head)=>{
      const expected=createHash("sha1").update(key+"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
      if(res.headers["sec-websocket-accept"]!==expected){socket.destroy();reject(new Error("invalid fixture upgrade"));return;}
      socket.setTimeout(0);
      let buffer=Buffer.alloc(0),closed=false;
      const listeners=new Set();
      const peer={got:[],closed:null,onmessage:null,onclose:null,
        subscribe(fn){listeners.add(fn);return()=>listeners.delete(fn);},
        send(text){frame(1,Buffer.from(text));},ping(){frame(9,Buffer.alloc(0));},declareTextBytes(n){const length=Buffer.alloc(8);length.writeBigUInt64BE(BigInt(n));socket.write(Buffer.concat([Buffer.from([129,255]),length,Buffer.alloc(4)]));},close(){if(!closed)frame(8,Buffer.from([3,232]));},
        terminate(){socket.destroy();}
      };
      function frame(op,payload){
        if(closed)throw new Error("fixture socket closed");
        const mask=randomBytes(4),n=payload.length;
        if(n>LIMITS.wire)throw new Error("fixture frame too large");
        const header=n<126?Buffer.from([128|op,128|n]):n<65536?Buffer.from([128|op,254,n>>8,n&255]):Buffer.concat([Buffer.from([128|op,255]),(()=>{const length=Buffer.alloc(8);length.writeBigUInt64BE(BigInt(n));return length;})()]);
        const bytes=Buffer.from(payload);for(let i=0;i<n;i++)bytes[i]^=mask[i%4];
        socket.write(Buffer.concat([header,mask,bytes]));
      }
      function finish(code){if(closed)return;closed=true;peer.closed=code;peer.onclose?.({code});for(const fn of listeners)fn();}
      function receive(chunk){
        buffer=Buffer.concat([buffer,chunk]);
        while(buffer.length>=2){
          const op=buffer[0]&15;let n=buffer[1]&127,offset=2;
          if(n===126){if(buffer.length<4)return;n=buffer.readUInt16BE(2);offset=4;}
          if(n===127){if(buffer.length<10)return;const declared=buffer.readBigUInt64BE(2);if(declared>BigInt(LIMITS.wire)){socket.destroy();finish(1009);return;}n=Number(declared);offset=10;}
          if(buffer.length<offset+n)return;
          const payload=buffer.subarray(offset,offset+n);buffer=buffer.subarray(offset+n);
          if(op===8){const code=payload.length>=2?payload.readUInt16BE(0):1000;if(!closed)frame(8,payload.subarray(0,2));finish(code);socket.end();return;}
          if(op===9){frame(10,payload);continue;}
          if(op===1){const text=payload.toString("utf8");peer.got.push(text);peer.onmessage?.({data:text});for(const fn of listeners)fn();}
        }
      }
      socket.on("data",receive);socket.on("end",()=>finish(1006));socket.on("close",()=>finish(1006));socket.on("error",()=>finish(1006));
      resolve(peer);if(head.length)receive(head);
    });
    req.end();
  });
}
export function wireWait(peer,predicate) {
  return new Promise((resolve,reject)=>{
    let unsubscribe=()=>{};
    const timer=setTimeout(()=>{unsubscribe();reject(new Error("fixture frame deadline"));},3000);
    const check=()=>{const value=predicate(peer);if(value){clearTimeout(timer);unsubscribe();resolve(value);}};
    unsubscribe=peer.subscribe(check);check();
  });
}
