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

export class RelayRoom {
  constructor(state, env) {
    this.state = state;
    this.env = env;
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

  notifyDaemon(daemon, kind, cid) {
    try { daemon.send(JSON.stringify({ relay: kind, cid: cid })); } catch (_) {}
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
    if (role === "daemon" && this.sockets("daemon").length > 0) {
      return new Response("a daemon is already connected to this room", { status: 409 });
    }
    const pair = new WebSocketPair();
    const client = pair[0];
    const server = pair[1];
    if (role === "daemon") {
      const mux = url.searchParams.get("mux") === "1";
      this.state.acceptWebSocket(server, [role]);
      server.serializeAttachment({ role: role, mux: mux });
      if (mux) {
        for (const ws of this.sockets("client")) this.notifyDaemon(server, "client_open", this.attachment(ws).cid);
      }
      this.notifyClients("daemon_online");
    } else {
      const cid = crypto.randomUUID();
      this.state.acceptWebSocket(server, [role, cid]);
      server.serializeAttachment({ role: role, cid: cid });
      const daemon = this.muxDaemon();
      if (daemon) this.notifyDaemon(daemon, "client_open", cid);
      const online = this.sockets("daemon").length > 0;
      try { server.send(JSON.stringify({ relay: online ? "daemon_online" : "daemon_offline" })); } catch (_) {}
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws, message) {
    if (typeof message !== "string") return;
    const att = this.attachment(ws);
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

  async webSocketClose(ws) {
    const att = this.attachment(ws);
    if (att.role === "daemon") {
      this.notifyClients("daemon_offline");
      return;
    }
    const daemon = this.muxDaemon();
    if (daemon && att.cid) this.notifyDaemon(daemon, "client_close", att.cid);
  }

  async webSocketError(ws) {
    try { ws.close(1011, "error"); } catch (_) {}
  }
}

/** Security headers for every frontend response (also used by dev.mjs). */
export const SECURITY_HEADERS = {
  "content-security-policy":
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
    "connect-src 'self' wss: ws:; manifest-src 'self'; worker-src 'self'; base-uri 'none'; " +
    "form-action 'none'; frame-ancestors 'none'",
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "cache-control": "no-cache",
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ ok: true, service: "formless-relay", version: 2 });
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
