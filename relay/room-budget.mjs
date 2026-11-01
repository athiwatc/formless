// Metadata-only SQLite authority. Never stores ciphertext or plaintext bodies.
// Rebuild credits from one bounded record on every transition; persist before
// returning any send/close effect. A callback never crosses an await boundary.
import { LIMITS, RoomCredits, SendPermits, TransportError, randomToken, validateFragment } from './web/transport.mjs';
export const ROOM_LIMITS = Object.freeze({ stateBytes:64*1024, nativeSockets:18,
  controlBytes:128*1024, controlReserve:3072, controlShared:64*1024,
  controlEntries:32, controlWire:3072, rateFrames:8192, rateBytes:128*1024*1024 });
const bad = message => {throw new TransportError(message);};
const id = x => typeof x==='string' && /^[a-f0-9]{32}$/.test(x);
const integer = x => Number.isSafeInteger(x) && x>=0;
const cid = x => typeof x==='string' && /^[a-f0-9-]{36}$/.test(x);
const token = x => typeof x==='string' && /^[A-Za-z0-9_-]{22}$/.test(x);
function sameToken(a,b){if(!token(a)||!token(b))return false;let difference=0;for(let i=0;i<22;i++)difference|=a.charCodeAt(i)^b.charCodeAt(i);return difference===0;}
const empty = () => ({v:1,natives:[],peers:[],cursor:0});
const blank = g => ({sg:g,dg:g,c:false,e:[]});
function hydrate(snapshot,now) {
  if(!snapshot || snapshot.v!==1 || !Array.isArray(snapshot.natives) || snapshot.natives.length>ROOM_LIMITS.nativeSockets || !Array.isArray(snapshot.peers) || snapshot.peers.length>LIMITS.clients || !integer(snapshot.cursor) || snapshot.cursor>=2*LIMITS.clients)bad('invalid room ledger');
  const natives=new Map(), peers=new Map(), wire=new RoomCredits();let controlBytes=0,excess=0;
  for(const n of snapshot.natives){
    if(!id(n.sid)||!id(n.g)||!['daemon','client'].includes(n.role)||typeof n.closing!=='boolean'||!integer(n.deadline)||!Array.isArray(n.controls)||n.controls.length>ROOM_LIMITS.controlEntries||!Array.isArray(n.rate)||n.rate.length!==3||!n.rate.every(integer)||natives.has(n.sid))bad('invalid native lifecycle');
    let bytes=0;const tokens=new Set();
    for(const c of n.controls){if(!Array.isArray(c)||c.length!==5||!(c[3]===null||cid(c[3]))||!(c[4]===null||cid(c[4]))||!token(c[0])||tokens.has(c[0])||!integer(c[1])||!c[1]||c[1]>ROOM_LIMITS.controlWire||!integer(c[2])||c[2]>now+LIMITS.ackMs)bad('invalid control receipt');tokens.add(c[0]);bytes+=c[1];}
    controlBytes+=bytes;excess+=Math.max(0,bytes-ROOM_LIMITS.controlReserve);natives.set(n.sid,{...n,bytes});
  }
  if(controlBytes>ROOM_LIMITS.controlBytes||excess>ROOM_LIMITS.controlShared)bad('control budget exceeded');
  for(const p of snapshot.peers){
    if(!cid(p.cid)||!id(p.g)||!id(p.browser)||!(p.daemon===null||id(p.daemon))||typeof p.closed!=='boolean'||typeof p.ready!=='boolean'||typeof p.needsOpen!=='boolean'||typeof p.needsClose!=='boolean'||!integer(p.closeGrace)||p.closeGrace>now+LIMITS.ackMs||!integer(p.deadline)||peers.has(p.cid)||!Array.isArray(p.wait)||p.wait.length!==2||!Array.isArray(p.last)||p.last.length!==2||!p.last.every(integer)||!Array.isArray(p.assembly)||p.assembly.length!==2||!Array.isArray(p.receive)||p.receive.length!==2||!p.receive.every(x=>typeof x==='boolean')||!Array.isArray(p.hello)||p.hello.length!==2||!p.hello.every(x=>integer(x)&&x<=6))bad('invalid CID lifecycle');
    const browser=natives.get(p.browser),daemon=p.daemon===null?null:natives.get(p.daemon);
    if((!browser&&!p.closed)||(browser&&(browser.role!=='client'||browser.g!==p.g))||(p.daemon!==null&&(!daemon||daemon.role!=='daemon')))bad('unknown or mismatched persisted socket owner');
    const up=SendPermits.restore(wire,p.browser,p.daemon,p.up,now), down=SendPermits.restore(wire,p.daemon,p.browser,p.down,now);
    if(p.up.sg!==p.g||p.up.dg!==p.g||p.down.sg!==p.g||p.down.dg!==p.g)bad('CID generation mismatch');
    for(let d=0;d<2;d++){
      if(!Array.isArray(p.wait[d])||p.wait[d].length+(d?down:up).entries.length>LIMITS.windowFrames||!p.wait[d].every(integer)||new Set(p.wait[d]).size!==p.wait[d].length)bad('invalid permit waiters');
      const a=p.assembly[d];if(a!==null&&(!(a.finalID===null||integer(a.finalID))||!id(a.id)||!integer(a.bytes)||!a.bytes||a.bytes>LIMITS.encrypted||!integer(a.next)||!integer(a.deadline)||a.deadline>now+LIMITS.assemblyMs))bad('invalid assembly metadata');
    }
    peers.set(p.cid,{...p,windows:[up,down]});
  }
  for(const n of natives.values())for(const c of n.controls)if(c[4]!==null){const p=peers.get(c[4]);if(!p||(n.sid!==p.browser&&n.sid!==p.daemon)||(c[3]!==null&&c[3]!==c[4]))bad('unknown persisted control owner');}
  return{natives,peers,wire,cursor:snapshot.cursor};
}
function snapshot(state){return{v:1,cursor:state.cursor,natives:[...state.natives.values()].map(({bytes,...n})=>n),peers:[...state.peers.values()].map(({windows,...p})=>({...p,up:windows[0].snapshot(),down:windows[1].snapshot()}))};}
function native(state,sid){const n=state.natives.get(sid);if(!n)bad('unknown native socket');return n;}
function peer(state,cid){const p=state.peers.get(cid);if(!p)bad('unknown CID');return p;}
function prune(state){for(const [key,p]of state.peers)if(p.closed&&!state.natives.has(p.browser)&&!p.needsClose&&p.windows.every(w=>!w.entries.length)&&![...state.natives.values()].some(n=>n.controls.some(c=>c[4]===p.cid)))state.peers.delete(key);}
function closeNative(state,sid,now){
  if(!state.natives.has(sid))return;
  state.natives.delete(sid);
  for(const p of state.peers.values()){
    if(p.browser===sid){const wasClosed=p.closed;p.closed=true;p.ready=false;p.needsOpen=false;p.needsClose=p.needsClose||(!wasClosed&&p.daemon!==null&&state.natives.has(p.daemon)&&!native(state,p.daemon).closing);p.closeGrace=p.closeGrace||now+LIMITS.ackMs;p.wait=[[],[]];p.windows[0].closeSource();p.windows[1].teardownDestination();}
    if(p.daemon===sid){p.ready=false;p.wait=[[],[]];p.windows[0].teardownDestination();p.windows[1].closeSource();p.daemon=null;p.needsOpen=false;p.needsClose=false;}
  }
  prune(state);
}
function grants(state,now){
  const effects=[],lanes=[...state.peers.values()].flatMap(p=>[0,1].map(d=>[p,d]));
  if(!lanes.length)return effects;
  // One grant per lane per round; a stalled peer cannot borrow another lane's
  // first-fragment reserve, nor permanently win all shared capacity on ACK.
  for(let round=0;round<LIMITS.windowFrames;round++){
    let progress=false;
    for(let i=0;i<lanes.length;i++){
      const [p,d]=lanes[(state.cursor+i)%lanes.length];const w=p.windows[d];
      if(p.closed||!p.ready||!p.receive[d?0:1]||!p.wait[d].length||native(state,w.source).closing||native(state,w.destination).closing)continue;
      const send_id=p.wait[d][0],permit=w.grant(send_id,now);if(!permit)continue;
      p.wait[d].shift();effects.push({sid:w.source,packet:{relay:'grant',g:p.g,...(d?{cid:p.cid}:{}),send_id,permit}});progress=true;
    }
    state.cursor=(state.cursor+1)%(2*LIMITS.clients);if(!progress)break;
  }
  return effects;
}
export class RoomBudget {
  constructor(sql){
    if(!sql?.exec)bad('SQLite room storage unavailable');this.sql=sql;
    sql.exec('CREATE TABLE IF NOT EXISTS relay_transport_state (id INTEGER PRIMARY KEY CHECK(id=1), state TEXT NOT NULL CHECK(length(state)<=65536))');
    const rows=Array.from(sql.exec('SELECT state FROM relay_transport_state WHERE id=1'));
    this.encoded=rows.length?rows[0].state:JSON.stringify(empty());
    if(typeof this.encoded!=='string'||this.encoded.length>ROOM_LIMITS.stateBytes)bad('room ledger exceeds bound');
    hydrate(JSON.parse(this.encoded),Date.now());
  }
  read(now=Date.now()){return hydrate(JSON.parse(this.encoded),now);}
  change(fn,now=Date.now()){
    const state=this.read(now),result=fn(state),encoded=JSON.stringify(snapshot(state));
    if(encoded.length>ROOM_LIMITS.stateBytes)bad('room lifecycle metadata exhausted');
    hydrate(JSON.parse(encoded),now); // Validate the full next state before publication.
    // A single synchronous SQL write is the durable authority. Cloudflare's
    // normal output gate protects the later send; allowUnconfirmed is not used.
    this.sql.exec('INSERT INTO relay_transport_state(id,state) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET state=excluded.state',encoded);
    this.encoded=encoded;return result;
  }
  describe(now=Date.now()){const s=this.read(now);return{natives:[...s.natives.values()],peers:[...s.peers.values()].map(({windows,...p})=>p),wireBytes:s.wire.raw,wireExcess:s.wire.excess,metadataBytes:this.encoded.length};}
  canAdmit(role,now=Date.now()){const s=this.read(now);return s.natives.size<ROOM_LIMITS.nativeSockets&&(role==='client'?s.peers.size<LIMITS.clients:[...s.natives.values()].filter(n=>n.role==='daemon').length<2);}
  addNative(info,now=Date.now()){return this.change(s=>{
    if(s.natives.size>=ROOM_LIMITS.nativeSockets||s.natives.has(info.sid)||!id(info.sid)||!id(info.g)||!['client','daemon'].includes(info.role))bad('native socket limit');
    s.natives.set(info.sid,{...info,closing:false,deadline:0,controls:[],rate:[now,0,0],bytes:0});
  },now);}
  addPeer(browser,daemon,identity,now=Date.now()){return this.change(s=>{
    if(s.peers.size>=LIMITS.clients||s.peers.has(identity.cid)||!cid(identity.cid)||!id(identity.g)||native(s,browser).role!=='client'||native(s,browser).g!==identity.g)bad('CID admission limit');
    if(daemon!==null&&(native(s,daemon).role!=='daemon'||native(s,daemon).closing))bad('daemon unavailable');
    const p={...identity,browser,daemon,closed:false,ready:false,needsOpen:daemon!==null,needsClose:false,closeGrace:0,deadline:now+LIMITS.handshakeMs,wait:[[],[]],last:[0,0],assembly:[null,null],receive:[false,false],hello:[0,0],up:blank(identity.g),down:blank(identity.g)};
    p.windows=[new SendPermits(s.wire,browser,p.g,daemon,p.g),new SendPermits(s.wire,daemon,p.g,browser,p.g)];s.peers.set(p.cid,p);
  },now);}
  bindDaemon(sid,now=Date.now()){return this.change(s=>{native(s,sid);for(const p of s.peers.values())if(!p.closed&&p.daemon===null&&s.natives.has(p.browser)&&!native(s,p.browser).closing&&p.windows.every(w=>!w.entries.length)){p.daemon=sid;p.needsOpen=true;p.deadline=now+LIMITS.handshakeMs;p.receive=[false,false];p.hello=[0,0];p.windows=[new SendPermits(s.wire,p.browser,p.g,sid,p.g),new SendPermits(s.wire,sid,p.g,p.browser,p.g)];}},now);}
  authorize(sid,g,cidValue,ready=true,now=Date.now()){
    const s=this.read(now),n=native(s,sid),p=peer(s,cidValue);
    if(n.closing||p.closed||p.g!==g||(!ready?false:!p.ready)||!(p.browser===sid||p.daemon===sid))bad('stale or unready CID owner');
    return{...p,windows:undefined,direction:p.browser===sid?0:1};
  }
  handshake(sid,cidValue,g,now=Date.now()){return this.change(s=>{
    const p=peer(s,cidValue),d=p.browser===sid?0:1;if(p.closed||p.ready||p.g!==g||native(s,sid).closing||!(p.browser===sid||p.daemon===sid)||p.deadline<=now)bad('invalid handshake owner');
    if(++p.hello[d]>6)bad('handshake rate exceeded');return d?p.browser:p.daemon;
  },now);}
  ready(sid,cidValue,g,now=Date.now()){return this.change(s=>{const p=peer(s,cidValue);if(p.daemon!==sid||native(s,sid).role!=='daemon'||native(s,sid).closing||p.closed||p.g!==g||p.deadline<=now)bad('invalid ready authority');if(!p.ready){p.ready=true;p.deadline=0;p.receive=[false,false];}return p.browser;},now);}
  rate(sid,bytes,now=Date.now()){return this.change(s=>{const n=native(s,sid);if(!integer(bytes))bad('invalid rate charge');if(now-n.rate[0]>=1000)n.rate=[now,0,0];n.rate[1]++;n.rate[2]+=bytes;if(n.rate[1]>ROOM_LIMITS.rateFrames||n.rate[2]>ROOM_LIMITS.rateBytes)bad('relay rate exceeded');},now);}
  need(sid,cidValue,g,sendID,now=Date.now()){return this.change(s=>{
    const p=peer(s,cidValue),d=p.browser===sid?0:1,w=p.windows[d];
    if(!p.ready||p.closed||p.g!==g||w.source!==sid||native(s,sid).closing||!integer(sendID)||sendID!==p.last[d]+1||p.wait[d].length+w.entries.length>=LIMITS.windowFrames)bad('invalid send request');
    p.last[d]=sendID;p.wait[d].push(sendID);return grants(s,now);
  },now);}
  deliver(sid,cidValue,g,packet,wireBytes,now=Date.now()){return this.change(s=>{
    validateFragment(packet);
    const p=peer(s,cidValue),d=p.browser===sid?0:1,w=p.windows[d];
    if(!p.ready||p.closed||!p.receive[d?0:1]||p.g!==g||w.source!==sid||native(s,sid).closing)bad('invalid bulk sender');
    let a=p.assembly[d];if(!a){if(packet.index!==0)bad('out-of-order fragment');a={id:packet.id,bytes:packet.bytes,next:0,deadline:now+LIMITS.assemblyMs,finalID:null};}
    if(a.finalID!==null||a.id!==packet.id||a.bytes!==packet.bytes||a.next!==packet.index||a.deadline<=now)bad('replayed/reordered/expired fragment');
    const redeemed=w.redeem(sid,g,packet.permit,wireBytes,now);if(!redeemed)bad('missing send permit');
    a.next++;if(packet.index*LIMITS.chunk+packet.data.length===packet.bytes)a.finalID=redeemed.sourceID;p.assembly[d]=a;
    const fragment={relay:'data',v:packet.v,id:packet.id,index:packet.index,bytes:packet.bytes,data:packet.data};
    return{sid:w.destination,packet:{relay:'delivery',g:p.g,...(!d?{cid:p.cid}:{}),receipt:redeemed.receipt,fragment}};
  },now);}
  ack(sid,cidValue,g,receipt,now=Date.now()){return this.change(s=>{
    const p=peer(s,cidValue);if(p.g!==g)bad('stale receipt generation');
    const d=p.daemon===sid?0:p.browser===sid?1:-1;if(d<0)bad('wrong receipt owner');
    const w=p.windows[d],send_id=w.acknowledge(sid,g,receipt);if(send_id===null)bad('unknown receipt');if(p.assembly[d]?.finalID===send_id)p.assembly[d]=null;
    const effects=s.natives.has(w.source)&&!native(s,w.source).closing&&!p.closed?[{sid:w.source,packet:{relay:'credit',g:p.g,...(d?{cid:p.cid}:{}),send_id}}]:[];
    prune(s);return effects.concat(grants(s,now));
  },now);}
  control(sid,bytes,readyCID=null,now=Date.now(),originCID=readyCID,lifecycle=null){return this.change(s=>{
    const n=native(s,sid);if(n.closing||!integer(bytes)||!bytes||bytes>ROOM_LIMITS.controlWire||n.controls.length>=ROOM_LIMITS.controlEntries)bad('control backlog exceeded');
    if(originCID!==null){
      if(!s.peers.has(originCID)||n.controls.filter(c=>c[4]===originCID).length>=2)bad('CID handshake/control backlog exceeded');
      const reserved=[...s.peers.values()].filter(p=>!p.closed&&p.cid!==originCID&&(p.browser===sid||p.daemon===sid)&&!n.controls.some(c=>c[4]===p.cid)).length;
      if(n.controls.length+1>ROOM_LIMITS.controlEntries-reserved)bad('reserved control progress unavailable');
    }else if(n.controls.filter(c=>c[4]===null).length>=4)bad('native system control backlog exceeded');
    let raw=0,extra=0;for(const other of s.natives.values()){const total=other.bytes+(other===n?bytes:0);raw+=total;extra+=Math.max(0,total-ROOM_LIMITS.controlReserve);}
    if(raw>ROOM_LIMITS.controlBytes||extra>ROOM_LIMITS.controlShared)bad('control room budget exceeded');
    if(lifecycle!==null){const p=peer(s,originCID);if(p.daemon!==sid||!['open','close'].includes(lifecycle))bad('invalid lifecycle control');if(lifecycle==='open')p.needsOpen=false;else p.needsClose=false;}const receipt=randomToken();if(readyCID!==null&&!s.peers.has(readyCID))bad('unknown ready control CID');n.controls.push([receipt,bytes,now+LIMITS.ackMs,readyCID,originCID]);n.bytes+=bytes;return receipt;
  },now);}
  ackControl(sid,g,receipt,now=Date.now()){return this.change(s=>{const n=native(s,sid);if(n.g!==g)bad('stale control generation');const i=n.controls.findIndex(row=>sameToken(row[0],receipt));if(i<0)bad('unknown control receipt');const readyCID=n.controls[i][3];n.controls.splice(i,1);const p=s.peers.get(readyCID);if(p&&!p.closed){if(p.browser===sid)p.receive[0]=true;else if(p.daemon===sid)p.receive[1]=true;else bad('wrong ready consumer');}prune(s);return grants(s,now);},now);}
  expireCID(cidValue,now=Date.now()){return this.change(s=>{
    const p=peer(s,cidValue);
    if(!p.closed){p.closed=true;p.ready=false;p.needsOpen=false;p.closeGrace=now+LIMITS.ackMs;p.wait=[[],[]];p.windows.forEach(w=>w.closeSource());
      p.needsClose=p.daemon!==null&&s.natives.has(p.daemon)&&!native(s,p.daemon).closing;}
    return p.browser;
  },now);}
  closing(sid,now=Date.now()){return this.change(s=>{
    const n=native(s,sid);n.closing=true;n.deadline=now+LIMITS.ackMs;
    for(const p of s.peers.values())if(p.browser===sid||p.daemon===sid){p.ready=false;p.wait=[[],[]];p.windows[p.browser===sid?0:1].closeSource();}
  },now);}
  actualClose(sid,now=Date.now()){return this.change(s=>{closeNative(s,sid,now);return grants(s,now);},now);}
  reconcile(present,now=Date.now()){return this.change(s=>{for(const sid of s.natives.keys())if(!present.has(sid))closeNative(s,sid,now);return grants(s,now);},now);}
  deadlines(now=Date.now()){
    const s=this.read(now),close=new Set(),cids=new Set();let next=Infinity;
    const nativeExpiry=(at,sid)=>{if(at<=now)close.add(sid);else next=Math.min(next,at);};
    const cidExpiry=(at,p)=>{if(at<=now)cids.add(p.cid);else next=Math.min(next,at);};
    for(const n of s.natives.values()){
      if(n.closing&&n.deadline)nativeExpiry(n.deadline,n.sid);
      for(const c of n.controls){
        const p=c[4]===null?null:s.peers.get(c[4]);
        if(n.role==='daemon'&&p){
          if(!p.closed)cidExpiry(c[2],p);
          // Give the logical cancellation path its own bounded control-drain
          // grace. Data receipts alone never kill an otherwise healthy daemon.
          else nativeExpiry(Math.max(c[2],p.closeGrace),n.sid);
        }else nativeExpiry(c[2],n.sid);
      }
    }
    for(const p of s.peers.values()){
      if(!p.closed&&!p.ready)cidExpiry(p.deadline,p);
      if(!p.closed){
        p.windows.forEach(w=>w.entries.forEach(row=>cidExpiry(row[3],p)));
        p.assembly.forEach(a=>{if(a)cidExpiry(a.deadline,p);});
      }
      // Closed UP/controls remain charged/admission-counted until their actual
      // receipts or native destination teardown. Never refund an orphan timeout.
    }
    return{close:[...close].filter(Boolean),cids:[...cids],next:Number.isFinite(next)?next:null};
  }
}
