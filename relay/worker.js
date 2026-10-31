// Formless relay (SPEC §12, v2): a Cloudflare Worker + Durable Object room.
// The daemon dials out to /relay/<room>?role=daemon and browsers to
// ?role=client. Frames between them are AES-256-GCM ciphertext envelopes;
// this worker only forwards opaque text and can never read session data.
// The Web frontend lives in web/ and is served as static assets with the
// headers below (the page URL carries the key, so no inline script runs).
// The room id is a truncated SHA-256 of the pairing key, so knowing a
// room does not reveal the key. Deployed at formless.athiwat.xyz.
//
// A daemon that connects with ?mux=1 gets one logical connection per
// browser: each client socket has an id (cid), the daemon is told
// {"relay":"client_open"|"client_close","cid"}, browser frames reach it as
// {"cid","f":<frame>}, and its {"cid","f"} replies go to that browser only.
// Older daemons (no mux) still get raw frames fanned out to every client.
//
// Web Push (opt-in): a browser sends {"relay":"push_subscribe","sub"} or
// {"relay":"push_unsubscribe","endpoint"} in the clear (a subscription is
// not Session data); the room keeps them. The daemon sends {"relay":"push"}
// when an approval or question waits, and the room sends each subscription
// a payload-free VAPID push (push.mjs).

import { isConfigFailure, loadVapid, sendPush, validSubscription } from "./push.mjs";

const MAX_SUBSCRIPTIONS = 10;

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
    this.state = state;
    this.env = env;
    // Sockets already torn down: close and error both end here once (#910).
    this.gone = new WeakSet();
  }

  sockets(role) {
    return this.state.getWebSockets(role).filter((ws) => ws.readyState === 1);
  }

  attachment(ws) {
    try { return ws.deserializeAttachment() || {}; } catch (_) { return {}; }
  }

  /** The connected daemon socket if it speaks the multiplexed protocol. */
  muxDaemon() {
    return this.sockets("daemon").find((ws) => this.attachment(ws).mux) || null;
  }

  notifyDaemon(daemon, kind, cid, ua) {
    const frame = { relay: kind, cid: cid };
    if (ua) frame.ua = ua;
    try { daemon.send(JSON.stringify(frame)); } catch (_) {}
  }

  notifyClients(kind) {
    const frame = JSON.stringify({ relay: kind });
    for (const ws of this.sockets("client")) {
      try { ws.send(frame); } catch (_) {}
    }
  }

  async fetch(request) {
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }
    const url = new URL(request.url);
    const role = url.searchParams.get("role") === "daemon" ? "daemon" : "client";
    const refused = this.refuse(role);
    if (refused) return new Response(refused[1], { status: refused[0] });
    const pair = new WebSocketPair();
    this.admit(pair[1], role, url.searchParams.get("mux") === "1", request.headers.get("User-Agent") || "");
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  /** Why `role` can't join right now, as [status, text], or null. */
  refuse(role) {
    if (role === "daemon" && this.sockets("daemon").length > 0) {
      return [409, "a daemon is already connected to this room"];
    }
    return null;
  }

  /** Admit an accepted server-side socket: the Worker's own, or the one
   * relay/dev.mjs wraps around a Node socket, so both run this code (#836). */
  admit(server, role, mux, userAgent) {
    if (role === "daemon") {
      this.state.acceptWebSocket(server, [role]);
      server.serializeAttachment({ role: role, mux: mux });
      if (mux) {
        for (const ws of this.sockets("client")) { const a = this.attachment(ws); this.notifyDaemon(server, "client_open", a.cid, a.ua); }
      }
      this.notifyClients("daemon_online");
    } else {
      const cid = crypto.randomUUID();
      // The User-Agent only labels this browser in the daemon's device list.
      const ua = (userAgent || "").slice(0, 200);
      this.state.acceptWebSocket(server, [role, cid]);
      server.serializeAttachment({ role: role, cid: cid, ua: ua });
      const daemon = this.muxDaemon();
      if (daemon) this.notifyDaemon(daemon, "client_open", cid, ua);
      const online = this.sockets("daemon").length > 0;
      try { server.send(JSON.stringify({ relay: online ? "daemon_online" : "daemon_offline" })); } catch (_) {}
    }
  }

  /** A plain-text control frame for the room itself, if this is one. */
  control(message) {
    if (!message.startsWith('{"relay"')) return null;
    try { return JSON.parse(message); } catch (_) { return null; }
  }

  async subKey(endpoint) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(endpoint));
    return "push:" + [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
  }

  async pushAll() {
    const { vapid } = await pushFor(this.env);
    if (!vapid) return;
    const subs = await this.state.storage.list({ prefix: "push:" });
    await Promise.all([...subs].map(async ([key, sub]) => {
      try {
        const status = await sendPush(sub, vapid);
        if (status === 404 || status === 410) await this.state.storage.delete(key);
        else if (isConfigFailure(status) && !refused) {
          refused = "a push service answered " + status + ": check that VAPID_PUBLIC_KEY and VAPID_PRIVATE_JWK are one pair";
          console.error("relay: " + refused);
        } else if (status < 300) refused = null;
      } catch (e) {
        console.error("relay: push failed: " + (e && e.message));
      }
    }));
  }

  async webSocketMessage(ws, message) {
    if (typeof message !== "string") return;
    const att = this.attachment(ws);
    const ctl = this.control(message);
    if (ctl && att.role === "daemon" && ctl.relay === "push") return this.pushAll();
    // The daemon closes one browser: 4001 when kicked (it may reconnect;
    // rotating revokes), 4002 when its channel broke (it reconnects, #905).
    if (ctl && att.role === "daemon" && ctl.relay === "disconnect" && typeof ctl.cid === "string") {
      const [code, reason] = ctl.code === 4002 ? [4002, "link integrity failure"] : [4001, "disconnected by the daemon"];
      for (const target of this.state.getWebSockets(ctl.cid)) {
        try { target.close(code, reason); } catch (_) {}
      }
      return;
    }
    if (ctl && att.role === "client") {
      // ALLOW_LOCAL_PUSH: only dev.mjs sets it, for a push service on
      // 127.0.0.1 in its tests.
      if (ctl.relay === "push_subscribe" && validSubscription(ctl.sub, this.env.ALLOW_LOCAL_PUSH === "1", this.env.PUSH_HOSTS)) {
        const subs = await this.state.storage.list({ prefix: "push:" });
        if (subs.size < MAX_SUBSCRIPTIONS) await this.state.storage.put(await this.subKey(ctl.sub.endpoint), ctl.sub);
      } else if (ctl.relay === "push_unsubscribe" && typeof ctl.endpoint === "string") {
        await this.state.storage.delete(await this.subKey(ctl.endpoint));
      }
      return;
    }
    if (att.role === "daemon") {
      let routed = null;
      if (att.mux) {
        try { routed = JSON.parse(message); } catch (_) {}
      }
      if (routed && typeof routed.cid === "string" && typeof routed.f === "string") {
        for (const target of this.state.getWebSockets(routed.cid)) {
          try { target.send(routed.f); } catch (_) {}
        }
        return;
      }
      for (const target of this.sockets("client")) {
        try { target.send(message); } catch (_) {}
      }
      return;
    }
    for (const daemon of this.sockets("daemon")) {
      const frame = this.attachment(daemon).mux ? JSON.stringify({ cid: att.cid, f: message }) : message;
      try { daemon.send(frame); } catch (_) {}
    }
  }

  /** One teardown for close and error, however many times either runs. */
  teardown(ws) {
    if (this.gone.has(ws)) return;
    this.gone.add(ws);
    const att = this.attachment(ws);
    if (att.role === "daemon") {
      this.notifyClients("daemon_offline");
      return;
    }
    const daemon = this.muxDaemon();
    if (daemon && att.cid) this.notifyDaemon(daemon, "client_close", att.cid);
  }

  async webSocketClose(ws) {
    this.teardown(ws);
  }

  async webSocketError(ws) {
    try { ws.close(1011, "error"); } catch (_) {}
    this.teardown(ws);
  }
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
