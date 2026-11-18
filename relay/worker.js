// Formless relay (SPEC §12, v2): a Cloudflare Worker + Durable Object room.
// The daemon dials out to /relay/<room>?role=daemon and browsers to
// ?role=client. Frames between them are AES-256-GCM ciphertext envelopes;
// this worker only forwards opaque text and can never read session data.
// The Web frontend lives in web/ and is served as static assets with the
// headers below (the page URL carries the key, so no inline script runs).
// Room v3 is a truncated SHA-256 of a domain-separated HKDF daemon claim.
// A daemon proves that claim in an Authorization header before upgrade;
// knowing the room does not reveal the claim or pairing/E2E key. Deployed at formless.athiwat.xyz.
//
// Transport v1 requires transport=1 (and mux=1 for daemons). Each CID has
// ordered opaque 64KiB fragments; grants reserve capacity before source sends,
// distinct destination receipts prove drainage. The Worker never reassembles.
// Initial/handshake/lifecycle controls are bounded separately. No legacy raw
// broadcast fallback is admitted; matching endpoint versions are required.
//
// Web Push (opt-in): a browser sends {"relay":"push_subscribe","sub"} or
// {"relay":"push_unsubscribe","endpoint"} in the clear (a subscription is
// not Session data); the room keeps them. The daemon sends {"relay":"push"}
// when an approval or question waits, and the room sends each subscription
// a payload-free VAPID push (push.mjs).

import { verifyDaemonClaim } from "./daemon-claim.mjs";
import { isConfigFailure, loadVapid, sendPush, validSubscription } from "./push.mjs";

import { RoomBudget } from "./room-budget.mjs";
import { LIMITS, TransportError, wireBytes, randomMessageID, validateFragment } from "./web/transport.mjs";
const MAX_SUBSCRIPTIONS = 10;
// Leave room for other outbound I/O during a push event.
const MAX_PUSH_IN_FLIGHT = 4;
function handshake(text) {
  wireBytes(text, LIMITS.preReady);
  let value; try { value=JSON.parse(text); } catch (_) { throw new TransportError("invalid handshake"); }
  const b64=(s,n)=>typeof s==="string"&&s.length===4*Math.ceil(n/3)&&/^[A-Za-z0-9+/]+={0,2}$/.test(s)&&atob(s).length===n&&btoa(atob(s))===s;
  if(!value||Array.isArray(value)||value.v!==2||Object.keys(value).some(k=>!["v","hello","confirm"].includes(k))||(!value.hello&&!value.confirm)||(value.hello&&!b64(value.hello,16))||(value.confirm&&!b64(value.confirm,32)))throw new TransportError("only bounded v2 handshake before readiness");
  return text;
}

// Checked once per isolate (#838); `refused` records a push service
// rejecting our signature, so /health can say so.
let pushConfig = null;
let refused = null;
function pushFor(env) {
  if (!pushConfig) pushConfig = loadVapid(env).then((c) => {
    if (c.state === "misconfigured") console.error("relay: push is off: " + c.reason);
    return c;
  });
  return pushConfig;
}

export class RelayRoom {
  constructor(state, env) {
    this.state=state;this.env=env;this.gone=new WeakSet();this.budget=new RoomBudget(state.storage?.sql);
    this.scheduledAlarm=null;this.alarmPending=false;this.alarmWanted=null;this.pushJobs=new Set();this.pushPending=false;this.pushAgain=false;this.failed=false;
    // Read an existing alarm before initialization schedules anything. A wake
    // for an already-due alarm must not postpone that handler in a constructor.
    const finish=at=>{this.scheduledAlarm=at??null;this.reconcile();};
    if(state.blockConcurrencyWhile)this.ready=state.blockConcurrencyWhile(async()=>finish(await state.storage.getAlarm()));
    else{const alarm=state.storage.getAlarm?.();if(alarm?.then)this.ready=alarm.then(finish);else{finish(alarm);this.ready=Promise.resolve();}}
  }
  attachment(ws) { try { return ws.deserializeAttachment()||{}; } catch (_) { return {}; } }
  all(){return this.state.getWebSockets();}
  socket(sid){return this.all().find(ws=>{const a=this.attachment(ws);return a.sid===sid&&ws.readyState!==3;})||null;}
  native(ws){const a=this.attachment(ws),n=this.budget.describe().natives.find(n=>n.sid===a.sid&&n.g===a.g&&n.role===a.role&&(a.role!=="daemon"||a.authenticated===3));return n||null;}
  sockets(role){return this.all().filter(ws=>{const a=this.attachment(ws),n=this.native(ws);return ws.readyState===1&&a.role===role&&n&&!n.closing&&(role!=="daemon"||a.authenticated===3);});}
  muxDaemon(){return this.sockets("daemon")[0]||null;}
  effects(effects){if(this.failed||this.blocked)return;for(const effect of effects||[]){const ws=this.socket(effect.sid);if(!ws||!this.native(ws)||this.native(ws).closing||ws.readyState!==1)continue;try{const text=JSON.stringify(effect.packet);wireBytes(text);ws.send(text);}catch(_){this.requestClose(ws,1011,"relay write failed");}}}
  sendControl(ws,value,{readyCID=null,originCID=null,lifecycle=null}={}){
    const n=this.native(ws);if(this.failed||this.blocked||!n||n.closing||ws.readyState!==1)throw new TransportError("control destination closed");
    const f=JSON.stringify(value);wireBytes(f,LIMITS.preReady);
    const bytes=wireBytes(JSON.stringify({relay:"control",g:n.g,receipt:"0".repeat(22),f}),3072);
    const receipt=this.budget.control(n.sid,bytes,readyCID,Date.now(),originCID,lifecycle);
    try{ws.send(JSON.stringify({relay:"control",g:n.g,receipt,f}));}catch(error){this.requestClose(ws,1011,"relay control write failed");throw error;}
    this.arm();
  }
  pumpLifecycle(){
    const daemon=this.muxDaemon();if(!daemon)return;
    const sid=this.attachment(daemon).sid;
    for(const p of this.budget.describe().peers){
      if(p.daemon!==sid)continue;
      try{
        if(p.needsClose)this.sendControl(daemon,{relay:"client_close",cid:p.cid,g:p.g},{originCID:p.cid,lifecycle:"close"});
        else if(p.needsOpen&&!p.closed){
          const browser=this.socket(p.browser);if(!browser)continue;
          const ua=this.attachment(browser).ua;
          this.sendControl(daemon,{relay:"client_open",cid:p.cid,g:p.g,ua},{originCID:p.cid,lifecycle:"open"});
          this.sendControl(browser,{relay:"daemon_online"});
        }
      }catch(_){/* Metadata-only pending lifecycle remains charged/bounded. Retry
        only this idempotent notification when another receipt frees capacity. */}
    }
    this.arm();
  }
  notifyClients(kind){for(const ws of this.sockets("client")){try{this.sendControl(ws,{relay:kind});}catch(_){this.requestClose(ws,1008,"control backlog exceeded");}}}
  requestClose(ws,code=1008,reason="relay transport refused"){
    try{const n=this.native(ws);if(n){
      if(n.role==='client'){const p=this.budget.describe().peers.find(p=>p.browser===n.sid);if(p&&!p.closed)this.budget.expireCID(p.cid);}
      if(!n.closing)this.budget.closing(n.sid);
    }}catch(_){this.failStorage();}
    // Even a failed storage transition must still request native closure; it
    // must never skip this action or pretend the old charged queue disappeared.
    try{ws.close(code,reason);}catch(_){}
    try{this.pumpLifecycle();this.arm();}catch(_){this.failed=true;this.blocked=true;}
  }
  failStorage(){this.failed=true;this.blocked=true;for(const ws of this.all())try{ws.close(1011,"relay storage failed");}catch(_){}}

  reconcile(){
    const before=this.budget.describe(),present=new Set();let unknown=false;
    for(const ws of this.all()){
      if(ws.readyState===3)continue;
      const n=this.native(ws);if(n)present.add(n.sid);else{unknown=true;try{ws.close(4002,"relay transport upgrade required");}catch(_){}}
    }
    if(unknown){
      // An unidentifiable live socket could itself own a ledger queue. Its
      // presence is not evidence that any unmatched ledger owner has vanished.
      // Close the context, hold all charges, and block new authority until the
      // actual socket inventory is unambiguous again.
      this.blocked=true;for(const ws of this.all())try{ws.close(4002,"relay socket identity unavailable");}catch(_){}this.arm();return;
    }
    this.blocked=this.failed;this.effects(this.budget.reconcile(present));
    for(const p of this.budget.describe().peers){const ws=this.socket(p.browser),n=ws&&this.native(ws);if(ws&&((p.closed&&!n?.closing)||(!p.closed&&n?.closing)))this.requestClose(ws,4002,"relay CID retired");}
    const daemon=this.muxDaemon(),departed=new Set(before.natives.filter(n=>n.role==="daemon"&&!present.has(n.sid)).map(n=>n.sid));
    if(departed.size){
      if(!daemon)this.notifyClients("daemon_offline");
      // A browser already bound to that daemon must never reuse sequence,
      // assembler, permits or readiness with a different daemon identity.
      for(const p of before.peers)if(departed.has(p.daemon)){const browser=this.socket(p.browser);if(browser)this.requestClose(browser,4002,"daemon generation ended");}
    }
    this.pumpLifecycle();this.arm();
  }
  arm(){
    const due=this.budget.deadlines(),at=due.close.length||due.cids.length?Date.now()+1000:due.next;
    if(at===null)return;
    if(this.scheduledAlarm!==null&&this.scheduledAlarm<=at)return;
    this.alarmWanted=this.alarmWanted===null?at:Math.min(this.alarmWanted,at);
    if(this.alarmPending)return;
    const wanted=this.alarmWanted;this.alarmWanted=null;this.alarmPending=true;this.scheduledAlarm=wanted;
    let operation;try{operation=this.state.storage.setAlarm(wanted);}catch(_){this.alarmPending=false;this.failStorage();return;}
    const pending=Promise.resolve(operation).catch(()=>this.failStorage()).finally(()=>{this.alarmPending=false;if(this.alarmWanted!==null&&this.alarmWanted<this.scheduledAlarm){this.scheduledAlarm=null;this.arm();}});
    this.state.waitUntil?.(pending);
  }
  async alarm(){
    this.scheduledAlarm=null;this.reconcile();
    for(const cid of this.budget.deadlines().cids){
      const sid=this.budget.expireCID(cid),ws=this.socket(sid);if(ws)this.requestClose(ws,1008,"relay CID deadline expired");
    }
    this.pumpLifecycle();
    for(const sid of this.budget.deadlines().close){const ws=this.socket(sid);if(ws)this.requestClose(ws,1008,"relay deadline expired");}
    this.arm();
  }
  async fetch(request){
    await this.ready;
    if(request.headers.get("Upgrade")!=="websocket")return new Response("expected websocket",{status:426});
    const url=new URL(request.url),role=url.searchParams.get("role")==="daemon"?"daemon":"client",room=url.pathname.match(/^\/relay\/([a-f0-9]{32})$/)?.[1];
    if(role==="daemon"&&!await verifyDaemonClaim(room||"",request.headers.get("Authorization")))return new Response("daemon authentication required",{status:401});
    if(url.searchParams.get("transport")!=="1"||(role==="daemon"&&url.searchParams.get("mux")!=="1"))return new Response("bounded multiplexed transport required",{status:426});
    this.reconcile();if(this.blocked)return new Response("old relay sockets are closing",{status:503});
    if(!this.budget.canAdmit(role))return new Response("room capacity exhausted",{status:429});
    const pair=new WebSocketPair();try{this.admit(pair[1],role,true,request.headers.get("User-Agent")||"",role==="daemon");}catch(_){return new Response("room capacity exhausted",{status:429});}
    return new Response(null,{status:101,webSocket:pair[0]});
  }
  canAdmit(role){this.reconcile();return !this.blocked&&this.budget.canAdmit(role);}
  admit(server,role,mux,userAgent,authenticated=false){
    if(role==="daemon"&&!authenticated)throw new Error("daemon authentication required");
    this.reconcile();if(this.blocked)throw new TransportError("old sockets closing");
    if(role==="daemon"&&this.budget.describe().natives.filter(n=>n.role==="daemon").length>=2)throw new TransportError("daemon replacement still closing");
    if(role==="client"&&this.budget.describe().peers.length>=LIMITS.clients)throw new TransportError("client limit reached");
    // Immutable identity remains attached during supersession. The durable
    // closing bit revokes authority; an old daemon can only settle its receipts.
    if(role==="daemon"){
      const old=this.sockets("daemon");if(old.length)this.notifyClients("daemon_offline");
      for(const ws of old){const sid=this.attachment(ws).sid;this.requestClose(ws,4003,"daemon connection replaced");for(const p of this.budget.describe().peers)if(p.daemon===sid){const browser=this.socket(p.browser);if(browser)this.requestClose(browser,4002,"daemon generation replaced");}}
    }
    const sid=randomMessageID(),g=randomMessageID(),cid=role==="client"?crypto.randomUUID():null;
    this.budget.addNative({sid,g,role});
    try{
      const daemon=this.muxDaemon();if(cid)this.budget.addPeer(sid,daemon?this.attachment(daemon).sid:null,{cid,g});
      this.state.acceptWebSocket(server,cid?[role,cid]:[role]);
      const ua=(userAgent||"").slice(0,200);server.serializeAttachment({role,sid,g,...(cid?{cid,ua}:{mux:true,authenticated:3})});
      server.setMessageLimit?.(LIMITS.preReady);
      this.sendControl(server,{relay:"transport",v:1,g,...(cid?{cid}:{})});
      if(role==="daemon"){
        this.budget.bindDaemon(sid);
        this.pumpLifecycle();
      }else{
        if(daemon)this.pumpLifecycle();else this.sendControl(server,{relay:"daemon_offline"});
      }
      this.arm();
    }catch(error){
      // Before acceptance there is no native output queue. After acceptance,
      // retain charge until actual close/reconciliation proves teardown.
      if(this.all().includes(server))this.requestClose(server,1008,"relay admission failed");else this.budget.actualClose(sid);
      throw error;
    }
  }
  async subKey(endpoint) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(endpoint));
    return "push:" + [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  async pushAll() {
    if (this.pushPending) {
      this.pushAgain = true;
      return;
    }
    this.pushPending = true;
    try {
      do {
        this.pushAgain = false;
        try {
          await this.pushOnce();
        } catch {
          console.error("relay: push fan-out failed");
        }
      } while (this.pushAgain);
    } finally {
      this.pushPending = false;
    }
  }

  async pushOnce() {
    const { vapid } = await pushFor(this.env);
    if (!vapid) return;
    const subs = await this.state.storage.list({ prefix: "push:", limit: MAX_SUBSCRIPTIONS });
    const eligible = [...subs].filter(([, sub]) => validSubscription(sub, this.env.ALLOW_LOCAL_PUSH === "1", this.env.PUSH_HOSTS));
    let index = 0;
    const deliver = async () => {
      while (index < eligible.length) {
        const [key, sub] = eligible[index++];
        try {
          const status = await sendPush(sub, vapid);
          if (status === 404 || status === 410) await this.state.storage.delete(key);
          else if (isConfigFailure(status) && !refused) {
            refused = "a push service answered " + status + ": check that VAPID_PUBLIC_KEY and VAPID_PRIVATE_JWK are one pair";
            console.error("relay: " + refused);
          } else if (status < 300) refused = null;
        } catch (error) {
          console.error(error?.name === "TimeoutError" ? "relay: push timed out" : "relay: push failed");
        }
      }
    };
    await Promise.allSettled(Array.from({ length: Math.min(MAX_PUSH_IN_FLIGHT, eligible.length) }, deliver));
  }
  async boundedPush(sid,work){
    if(this.pushJobs.has(sid)||this.pushJobs.size>=4)throw new TransportError("push control work limit");
    this.pushJobs.add(sid);try{return await work();}finally{this.pushJobs.delete(sid);}
  }

  async webSocketMessage(ws,message){
    const n=this.native(ws),att=this.attachment(ws);if(!n)return;
    let packet=null;
    try{
      if(this.failed)throw new TransportError("room storage failed");
      const peers=this.budget.describe().peers;
      const current=att.role==="client"?peers.find(p=>p.browser===n.sid):null;
      // Count before JSON parsing or encoding; only an authenticated daemon or
      // an E2E-ready browser can submit full 64KiB wire packets.
      const bulk=att.role==="daemon"&&att.authenticated===3&&peers.some(p=>p.daemon===n.sid&&p.ready)||current?.ready;
      const max=!n.closing&&bulk?LIMITS.wire:LIMITS.preReady;
      const bytes=wireBytes(message,max);this.budget.rate(n.sid,bytes);
      try{packet=JSON.parse(message);}catch(_){throw new TransportError("invalid relay packet");}
      if(packet?.relay==="ack_control"){
        if(packet.g!==n.g)throw new TransportError("stale native control generation");
        this.effects(this.budget.ackControl(n.sid,n.g,packet.receipt));this.pumpLifecycle();this.arm();return;
      }
      if(packet?.relay!=="data")wireBytes(message,LIMITS.preReady);
      const cid=att.role==="client"?att.cid:packet?.cid;
      if(att.role==="client"&&packet?.cid!==undefined&&packet.cid!==att.cid)throw new TransportError("CID spoofing");
      if(packet?.relay==="ack"){
        this.effects(this.budget.ack(n.sid,cid,packet.g,packet.receipt));this.pumpLifecycle();this.arm();return;
      }
      if(this.blocked||n.closing||ws.readyState!==1)return; // Superseded identity has no new authority (#637).
      if(packet?.relay==="ready"&&att.role==="daemon"){
        const browserSID=this.budget.ready(n.sid,cid,packet.g),browser=this.socket(browserSID);
        if(!browser)throw new TransportError("browser closed before ready");
        browser.setMessageLimit?.(LIMITS.wire);ws.setMessageLimit?.(LIMITS.wire);
        this.sendControl(browser,{relay:"ready",g:packet.g},{readyCID:cid,originCID:cid});
        this.sendControl(ws,{relay:"ready",cid,g:packet.g},{readyCID:cid,originCID:cid});return;
      }
      if(packet?.relay==="need"){
        this.budget.authorize(n.sid,packet.g,cid);
        this.effects(this.budget.need(n.sid,cid,packet.g,packet.send_id));this.arm();return;
      }
      if(packet?.relay==="data"){
        validateFragment(packet);this.budget.authorize(n.sid,packet.g,cid);
        this.effects([this.budget.deliver(n.sid,cid,packet.g,packet,bytes)]);this.arm();return;
      }
      if(packet?.relay==="push"&&att.role==="daemon")return this.pushAll();
      if(packet?.relay==="disconnect"&&att.role==="daemon"){
        const p=peers.find(p=>p.cid===cid&&p.daemon===n.sid);const target=p&&this.socket(p.browser);
        if(target)this.requestClose(target,packet.code===4002?4002:4001,"disconnected by daemon");return;
      }
      if(att.role==="client"&&current?.ready&&packet?.relay==="push_subscribe"&&validSubscription(packet.sub,this.env.ALLOW_LOCAL_PUSH==="1",this.env.PUSH_HOSTS)){
        return await this.boundedPush(n.sid,async()=>{
          const subs=await this.state.storage.list({prefix:"push:",limit:MAX_SUBSCRIPTIONS});
          const key=await this.subKey(packet.sub.endpoint);
          if(subs.size>=MAX_SUBSCRIPTIONS&&!subs.has(key))throw new TransportError("push subscription capacity");
          await this.state.storage.put(key,packet.sub);
          this.sendControl(ws,{relay:"push_subscribed"});
        });
      }
      if(att.role==="client"&&current?.ready&&packet?.relay==="push_unsubscribe"&&typeof packet.endpoint==="string"){
        wireBytes(packet.endpoint,LIMITS.preReady);
        return await this.boundedPush(n.sid,async()=>{await this.state.storage.delete(await this.subKey(packet.endpoint));this.sendControl(ws,{relay:"push_unsubscribed"});});
      }
      const p=peers.find(p=>p.cid===cid);if(!p||p.ready)throw new TransportError("unexpected unready control");
      if(att.role==="daemon"&&packet.g!==p.g)throw new TransportError("stale handshake generation");
      const f=att.role==="daemon"?packet.f:message;handshake(f);
      const targetSID=this.budget.handshake(n.sid,cid,p.g),target=targetSID&&this.socket(targetSID);
      if(!target)return;
      // Pressure belongs to this source CID, never another browser or the
      // shared daemon. Each peer has its own bounded handshake allowance.
      this.sendControl(target,att.role==="daemon"?JSON.parse(f):{cid,g:p.g,f},{originCID:cid});
    }catch(error){
      // CID-local failures (including a granted response racing browser close)
      // cannot tear down the authenticated daemon's other logical channels.
      if(att.role==="daemon"&&typeof packet?.cid==="string"){
        const p=this.budget.describe().peers.find(p=>p.cid===packet.cid&&p.daemon===n.sid);
        const target=p&&this.socket(p.browser);if(target)this.requestClose(target,4002,"relay logical channel refused");
      }else this.requestClose(ws,error instanceof TransportError?error.code:1008,"relay transport refused");
    }
  }
  async webSocketClose(ws){this.reconcile();}
  async webSocketError(ws){this.requestClose(ws,1011,"relay socket error");if(ws.readyState===3)this.reconcile();}

}

/** Security headers for every frontend response (also used by dev.mjs). */
export const SECURITY_HEADERS = {
  "content-security-policy":
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
    "connect-src 'self'; manifest-src 'self'; worker-src 'self'; base-uri 'none'; " +
    "form-action 'none'; frame-ancestors 'none'",
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "cache-control": "no-cache",
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/push/key") {
      const { vapid } = await pushFor(env);
      return vapid ? Response.json({ key: vapid.publicKey }) : new Response("push is not set up on this relay", { status: 404 });
    }
    if (url.pathname === "/health") {
      const push = await pushFor(env);
      const state = push.state === "ok" && refused ? "misconfigured" : push.state;
      return Response.json({ ok: true, service: "formless-relay", version: 2, push: state, push_problem: push.reason || refused || undefined });
    }
    const m = url.pathname.match(/^\/relay\/([a-f0-9]{32})$/);
    if (m) {
      const stub = env.ROOMS.get(env.ROOMS.idFromName(m[1]));
      return stub.fetch(request);
    }
    const asset = await env.ASSETS.fetch(request);
    const res = new Response(asset.body, asset);
    for (const [k, v] of Object.entries(SECURITY_HEADERS)) res.headers.set(k, v);
    return res;
  },
};
