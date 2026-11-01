import {RelayRoom} from './worker.js';
import {DevStorage} from './dev-storage.mjs';
export function roomFixture(env={}){
 const sockets=[],storage=new DevStorage();
 const state={storage,getWebSockets:tag=>sockets.filter(s=>s.readyState!==3&&(!tag||s.tags.includes(tag))),acceptWebSocket:(s,tags)=>{s.tags=tags;sockets.push(s);},waitUntil:p=>p.catch(()=>{})};
 const room=new RelayRoom(state,env);
 const socket=()=>({att:{},readyState:1,sent:[],seen:0,closed:null,serializeAttachment(a){this.att=structuredClone(a);},deserializeAttachment(){return structuredClone(this.att);},send(frame){if(this.readyState!==1)throw Error('fixture socket closed');this.sent.push(JSON.parse(frame));},close(code,reason){this.closed={code,reason};this.readyState=2;},finish(){this.readyState=3;}});
 return{room,state,socket,storage,close(){storage.close();}};
}
export function controls(ws){return ws.sent.filter(p=>p.relay==='control').map(p=>JSON.parse(p.f));}
export async function ackControls(room,ws){while(ws.seen<ws.sent.length){const p=ws.sent[ws.seen++];if(p.relay==='control')await room.webSocketMessage(ws,JSON.stringify({relay:'ack_control',g:p.g,receipt:p.receipt}));}}
